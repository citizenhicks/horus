//! Ordered middleware and capability registration.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io;
use std::io::Write;
use std::sync::Arc;

use serde_json::Value;

use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::backend::checkpoint::Checkpoint;
use crate::backend::checkpoint::CheckpointStore;
use crate::backend::model::ModelOutput;
use crate::backend::model::ModelRouter;
use crate::backend::sandbox::Sandbox;
use crate::protocol::EventMsg;
use crate::protocol::FrontendBlock;
use crate::protocol::FrontendContribution;
use crate::protocol::FrontendEvent;
use crate::protocol::FrontendTone;
use crate::protocol::SessionContext;
use crate::protocol::TokenUsage;
use crate::protocol::ToolCallBeginEvent;
use crate::protocol::ToolCallEndEvent;

pub mod compaction;
pub mod context_offloading;
pub mod cron;
pub mod instructions;
pub mod sessions;
pub mod skills;
pub mod steering;
pub mod subagents;
pub mod tasks;
pub mod tools;

use tools::Catalog;

const ESTIMATED_BYTES_PER_TOKEN: usize = 4;

/// Sends middleware-owned UI updates without depending on a concrete frontend.
pub type FrontendEventSink = Arc<dyn Fn(FrontendEvent) -> Result<()> + Send + Sync>;

/// Durable runtime identity exposed while middleware is initialized.
#[derive(Clone)]
pub struct RuntimeContext {
    pub checkpoints: Arc<dyn CheckpointStore>,
    pub session_id: String,
    pub model_route: String,
    pub session_context: SessionContext,
    pub metadata: BTreeMap<String, Value>,
    pub frontend: FrontendEventSink,
}

/// Mutable state exposed immediately before a model request.
pub struct ModelContext<'a> {
    pub model: &'a ModelRouter,
    pub provider: &'a str,
    pub session_id: &'a str,
    pub session_context: &'a SessionContext,
    pub metadata: &'a BTreeMap<String, Value>,
    pub turn_id: &'a str,
    pub model_step: usize,
    pub context_window: i64,
    pub instructions: &'a str,
    pub(crate) input: &'a mut Vec<Value>,
    pub(crate) transcript_delta: &'a mut Vec<Value>,
    pub queued_input: &'a mut Vec<String>,
    pub last_usage: Option<&'a TokenUsage>,
    pub tools: &'a Catalog,
    pub events: &'a mut Vec<EventMsg>,
    pub usage: &'a mut Vec<TokenUsage>,
    /// Set when this hook changes durable checkpoint state.
    pub checkpoint_changed: &'a mut bool,
}

impl ModelContext<'_> {
    /// Returns the provider-neutral input for the next model request.
    #[must_use]
    pub fn input(&self) -> &[Value] {
        self.input
    }

    /// Replaces model context without adding synthetic history to the transcript.
    pub fn replace_input(&mut self, input: Vec<Value>) {
        *self.input = input;
        *self.checkpoint_changed = true;
    }

    /// Appends durable input to model context and its transcript journal.
    pub fn push_input(&mut self, item: Value) {
        self.input.push(item.clone());
        self.transcript_delta.push(item);
    }

    /// Estimates serialized model input at four bytes per token.
    #[must_use]
    pub fn estimated_input_tokens(&self) -> i64 {
        let mut bytes = ByteCounter::default();
        if serde_json::to_writer(&mut bytes, self.input).is_err() {
            return i64::MAX;
        }
        i64::try_from(approximate_tokens(bytes.0)).unwrap_or(i64::MAX)
    }
}

#[derive(Default)]
struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Read-only normalized output exposed after a successful model response.
pub struct AfterModelContext<'a> {
    pub provider: &'a str,
    pub session_id: &'a str,
    pub session_context: &'a SessionContext,
    pub metadata: &'a BTreeMap<String, Value>,
    pub turn_id: &'a str,
    pub model_step: usize,
    pub context_window: i64,
    pub queued_input_count: usize,
    pub output: &'a ModelOutput,
    pub events: &'a mut Vec<EventMsg>,
}

/// Mutable turn state exposed to the middleware owning an active operation.
pub struct ActiveSubmissionContext<'a> {
    pub operation: &'a str,
    pub active_turn_id: &'a str,
    pub target_turn_id: &'a str,
    pub text: &'a str,
    pub queued_input: &'a mut Vec<String>,
    pub queued_before: usize,
    pub events: &'a mut Vec<EventMsg>,
}

/// Result of a middleware-owned active operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveSubmissionResult {
    Accepted,
    Rejected(String),
}

/// State exposed when the loop finishes or aborts a turn.
pub struct TurnEndContext<'a> {
    pub session_id: &'a str,
    pub turn_id: &'a str,
    pub events: &'a mut Vec<EventMsg>,
}

/// Durable identity exposed when one agent runtime stops.
#[derive(Clone)]
pub struct SessionEndContext {
    pub session_id: String,
    pub metadata: BTreeMap<String, Value>,
}

/// State available to a middleware-owned frontend command.
pub struct MiddlewareCommandContext<'a> {
    pub command: &'a str,
    pub arguments: &'a str,
    pub session_id: &'a str,
    pub session_context: &'a SessionContext,
    pub checkpoint: &'a Checkpoint,
    pub checkpoints: Arc<dyn CheckpointStore>,
}

/// Result of a middleware-owned frontend command.
pub struct MiddlewareCommandOutput {
    pub events: Vec<FrontendEvent>,
}

/// Read-only middleware UI surface consumed by a frontend shell.
#[derive(Clone)]
pub struct FrontendExtensions {
    stack: MiddlewareStack,
    contributions: Arc<[FrontendContribution]>,
}

impl FrontendExtensions {
    pub(crate) fn new(stack: MiddlewareStack) -> Result<Self> {
        let contributions = stack.frontend()?;
        Ok(Self {
            stack,
            contributions: contributions.into(),
        })
    }

    /// Returns command and widget manifests in capability order.
    #[must_use]
    pub fn contributions(&self) -> &[FrontendContribution] {
        &self.contributions
    }

    /// Lets installed middleware render capability-specific events.
    #[must_use]
    pub fn render(&self, event: &EventMsg) -> Vec<FrontendBlock> {
        self.stack
            .entries
            .iter()
            .filter_map(|entry| {
                entry
                    .render(event)
                    .map(|block| block.namespaced(entry.name()))
            })
            .collect()
    }
}

impl MiddlewareCommandOutput {
    /// Returns UI updates without replacing the active session.
    #[must_use]
    pub fn events(events: Vec<FrontendEvent>) -> Self {
        Self { events }
    }

    /// Returns one capability-scoped transcript block.
    #[must_use]
    pub fn render(
        capability: impl Into<String>,
        text: impl Into<String>,
        tone: FrontendTone,
    ) -> Self {
        Self::events(vec![FrontendEvent::Render {
            capability: capability.into(),
            block: FrontendBlock {
                id: None,
                group: None,
                append: false,
                pending: false,
                text: text.into(),
                format: crate::protocol::FrontendBlockFormat::PlainText,
                tone,
            },
        }])
    }
}

/// A capability contribution to the single ordered agent pipeline.
pub trait Middleware: Send + Sync {
    /// Stable ID used to reject duplicate registrations.
    fn name(&self) -> &'static str;

    /// Adds tools to the catalog while the agent is created.
    fn register(&self, _catalog: &mut Catalog, _runtime: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    /// Contributes immutable system instructions once while the agent is created.
    fn prompt_fragment(&self, _runtime: &RuntimeContext) -> Result<Option<String>> {
        Ok(None)
    }

    /// Declares commands and status data that any frontend may render.
    fn frontend(&self) -> FrontendContribution {
        FrontendContribution::default()
    }

    /// Renders an event owned by this capability for thin frontend shells.
    fn render(&self, _event: &EventMsg) -> Option<FrontendBlock> {
        None
    }

    /// Handles a command declared by this middleware's frontend contribution.
    fn command<'a>(
        &'a self,
        context: MiddlewareCommandContext<'a>,
    ) -> BoxFuture<'a, Result<MiddlewareCommandOutput>> {
        Box::pin(async move {
            Err(Error::Unknown(format!(
                "middleware command `{}/{}`",
                self.name(),
                context.command
            )))
        })
    }

    /// Restores middleware-owned durable state for this agent tree.
    fn initialize<'a>(&'a self, _context: RuntimeContext) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Declares active-turn operations owned by this middleware.
    fn active_operations(&self) -> &'static [&'static str] {
        &[]
    }

    /// Handles one declared active-turn operation.
    fn active_submission(
        &self,
        _context: &mut ActiveSubmissionContext<'_>,
    ) -> Result<ActiveSubmissionResult> {
        Err(Error::Config(format!(
            "middleware `{}` declared but did not handle an active operation",
            self.name()
        )))
    }

    /// Observes a turn ending and may clear capability-owned transient UI.
    fn turn_ended(&self, _context: &mut TurnEndContext<'_>) -> Result<()> {
        Ok(())
    }

    /// Adjusts the next model request.
    fn before_model<'a>(&'a self, _context: &'a mut ModelContext<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Observes one normalized response before it is checkpointed or dispatched.
    ///
    /// Conditions and defaults belong to the middleware instance. Output is
    /// read-only because streaming deltas may already be visible to frontends.
    fn after_model<'a>(
        &'a self,
        _context: &'a mut AfterModelContext<'_>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Releases session-local state when the agent runtime stops.
    fn shutdown<'a>(&'a self, _context: SessionEndContext) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl Middleware for Sandbox {
    fn name(&self) -> &'static str {
        "sandbox"
    }

    fn frontend(&self) -> FrontendContribution {
        Sandbox::frontend(self)
    }

    fn render(&self, event: &EventMsg) -> Option<FrontendBlock> {
        Sandbox::render(self, event)
    }

    fn command<'a>(
        &'a self,
        context: MiddlewareCommandContext<'a>,
    ) -> BoxFuture<'a, Result<MiddlewareCommandOutput>> {
        Box::pin(async move {
            Sandbox::command(
                self,
                context.session_id,
                &context.checkpoints,
                context.command,
                context.arguments,
            )
            .await
            .map(MiddlewareCommandOutput::events)
        })
    }

    fn initialize<'a>(&'a self, context: RuntimeContext) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            for event in
                Sandbox::initialize(self, &context.session_id, &context.checkpoints).await?
            {
                (context.frontend)(event)?;
            }
            Ok(())
        })
    }

    fn shutdown<'a>(&'a self, context: SessionEndContext) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { Sandbox::shutdown(self, &context.session_id).await })
    }
}

/// A validated, declaration-ordered middleware pipeline.
#[derive(Clone)]
pub struct MiddlewareStack {
    entries: Vec<Arc<dyn Middleware>>,
}

impl MiddlewareStack {
    /// Creates a stack and rejects duplicate middleware IDs.
    pub fn new(entries: Vec<Arc<dyn Middleware>>) -> Result<Self> {
        let mut names = BTreeSet::new();
        let mut active_operations = BTreeMap::new();
        for entry in &entries {
            if !names.insert(entry.name()) {
                return Err(Error::Duplicate(format!("middleware `{}`", entry.name())));
            }
            for operation in entry.active_operations() {
                if operation.is_empty() || operation.chars().any(char::is_whitespace) {
                    return Err(Error::Config(format!(
                        "middleware `{}` declared invalid active operation `{operation}`",
                        entry.name()
                    )));
                }
                if let Some(owner) = active_operations.insert(*operation, entry.name()) {
                    return Err(Error::Config(format!(
                        "active operation `{operation}` is owned by both `{owner}` and `{}`",
                        entry.name()
                    )));
                }
            }
        }
        Ok(Self { entries })
    }

    pub(crate) fn with_sandbox(&self, sandbox: Arc<Sandbox>) -> Result<Self> {
        let mut entries: Vec<Arc<dyn Middleware>> = vec![sandbox];
        entries.extend(self.entries.iter().cloned());
        Self::new(entries)
    }

    /// Builds the immutable tool catalog once.
    pub fn catalog(&self, runtime: &RuntimeContext) -> Result<Catalog> {
        let mut catalog = Catalog::default();
        for entry in &self.entries {
            let registered = catalog.definitions();
            entry.register(&mut catalog, runtime)?;
            for definition in catalog.definitions().iter().filter(|definition| {
                !registered
                    .iter()
                    .any(|registered| registered.name == definition.name)
            }) {
                validate_tool_rendering(entry.as_ref(), &definition.name)?;
            }
        }
        Ok(catalog)
    }

    pub(crate) fn system_prompt(&self, base: &str, runtime: &RuntimeContext) -> Result<String> {
        let mut prompt = base.trim().to_string();
        for entry in &self.entries {
            let Some(fragment) = entry.prompt_fragment(runtime)? else {
                continue;
            };
            let fragment = fragment.trim();
            if fragment.is_empty() {
                return Err(Error::Config(format!(
                    "middleware `{}` returned an empty prompt fragment",
                    entry.name()
                )));
            }
            prompt.push_str("\n\n");
            prompt.push_str(fragment);
        }
        Ok(prompt)
    }

    /// Builds and validates the frontend-neutral capability catalog.
    pub fn frontend(&self) -> Result<Vec<FrontendContribution>> {
        let contributions = self.declared_frontend()?;
        validate_frontend(&contributions)?;
        Ok(contributions)
    }

    fn declared_frontend(&self) -> Result<Vec<FrontendContribution>> {
        let mut contributions = Vec::new();
        for entry in &self.entries {
            let contribution = entry.frontend();
            if contribution.capability.is_empty()
                && contribution.commands.is_empty()
                && contribution.widgets.is_empty()
                && contribution.references.is_empty()
                && contribution.active_input.is_none()
            {
                continue;
            }
            if contribution.capability != entry.name() {
                return Err(Error::Config(format!(
                    "middleware `{}` exported frontend metadata for `{}`",
                    entry.name(),
                    contribution.capability
                )));
            }
            if let Some(input) = &contribution.active_input
                && !entry
                    .active_operations()
                    .contains(&input.operation.as_str())
            {
                return Err(Error::Config(format!(
                    "middleware `{}` exported undeclared active input `{}`",
                    entry.name(),
                    input.operation
                )));
            }
            contributions.push(contribution);
        }
        Ok(contributions)
    }

    pub(crate) fn active_submission(
        &self,
        context: &mut ActiveSubmissionContext<'_>,
    ) -> Result<Option<ActiveSubmissionResult>> {
        self.entries
            .iter()
            .find(|entry| entry.active_operations().contains(&context.operation))
            .map(|entry| entry.active_submission(context))
            .transpose()
    }

    pub(crate) async fn initialize(&self, context: RuntimeContext) -> Result<()> {
        let end = SessionEndContext {
            session_id: context.session_id.clone(),
            metadata: context.metadata.clone(),
        };
        for (index, entry) in self.entries.iter().enumerate() {
            if let Err(error) = entry.initialize(context.clone()).await {
                let mut rollback_error = None;
                for initialized in self.entries[..index].iter().rev() {
                    if let Err(error) = initialized.shutdown(end.clone()).await
                        && rollback_error.is_none()
                    {
                        rollback_error = Some(error);
                    }
                }
                return Err(match rollback_error {
                    Some(rollback) => Error::Rollback {
                        primary: Box::new(error),
                        rollback: Box::new(rollback),
                    },
                    None => error,
                });
            }
        }
        Ok(())
    }

    pub(crate) fn turn_ended(&self, mut context: TurnEndContext<'_>) -> Result<()> {
        for entry in &self.entries {
            entry.turn_ended(&mut context)?;
        }
        Ok(())
    }

    pub(crate) async fn shutdown(&self, context: SessionEndContext) -> Result<()> {
        let mut first_error = None;
        for entry in self.entries.iter().rev() {
            if let Err(error) = entry.shutdown(context.clone()).await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(crate) async fn before_model(&self, mut context: ModelContext<'_>) -> Result<()> {
        for entry in &self.entries {
            entry.before_model(&mut context).await?;
        }
        Ok(())
    }

    pub(crate) async fn after_model(&self, mut context: AfterModelContext<'_>) -> Result<()> {
        for entry in &self.entries {
            entry.after_model(&mut context).await?;
        }
        Ok(())
    }

    pub(crate) async fn command(
        &self,
        middleware: &str,
        context: MiddlewareCommandContext<'_>,
    ) -> Result<MiddlewareCommandOutput> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.name() == middleware)
            .ok_or_else(|| Error::Unknown(format!("middleware `{middleware}`")))?;
        let declared = entry
            .frontend()
            .commands
            .into_iter()
            .any(|command| command.name == context.command);
        if !declared {
            return Err(Error::Unknown(format!(
                "middleware command `{middleware}/{}`",
                context.command
            )));
        }
        entry.command(context).await
    }
}

fn validate_tool_rendering(middleware: &dyn Middleware, tool_name: &str) -> Result<()> {
    let events = [
        (
            "ToolCallBegin",
            EventMsg::ToolCallBegin(ToolCallBeginEvent {
                turn_id: "validation".into(),
                call_id: "validation".into(),
                name: tool_name.into(),
                arguments: serde_json::json!({}),
            }),
        ),
        (
            "successful ToolCallEnd",
            EventMsg::ToolCallEnd(ToolCallEndEvent {
                turn_id: "validation".into(),
                call_id: "validation".into(),
                name: tool_name.into(),
                output: String::new(),
                is_error: false,
            }),
        ),
        (
            "error ToolCallEnd",
            EventMsg::ToolCallEnd(ToolCallEndEvent {
                turn_id: "validation".into(),
                call_id: "validation".into(),
                name: tool_name.into(),
                output: "validation error".into(),
                is_error: true,
            }),
        ),
    ];
    for (event_name, event) in events {
        if middleware.render(&event).is_none() {
            return Err(Error::Config(format!(
                "middleware `{}` registered tool `{tool_name}` but does not render `{event_name}`",
                middleware.name()
            )));
        }
    }
    Ok(())
}

fn validate_frontend(contributions: &[FrontendContribution]) -> Result<()> {
    let mut commands = BTreeSet::new();
    let mut widgets = BTreeSet::new();
    let mut references = BTreeSet::new();
    let mut active_input = false;
    for contribution in contributions {
        for command in &contribution.commands {
            if command.name.is_empty() || command.name.chars().any(char::is_whitespace) {
                return Err(Error::Config(format!(
                    "invalid frontend command `{}`",
                    command.name
                )));
            }
            if !commands.insert(command.name.clone()) {
                return Err(Error::Duplicate(format!(
                    "frontend command `{}`",
                    command.name
                )));
            }
        }
        for item in &contribution.widgets {
            if item.id.is_empty()
                || !widgets.insert((contribution.capability.clone(), item.id.clone()))
            {
                return Err(Error::Duplicate(format!(
                    "frontend status `{}/{}`",
                    contribution.capability, item.id
                )));
            }
        }
        for reference in &contribution.references {
            if reference.trigger.is_control()
                || reference.trigger.is_whitespace()
                || reference.value.is_empty()
                || reference.value.chars().any(char::is_whitespace)
            {
                return Err(Error::Config(format!(
                    "invalid frontend reference `{}{}`",
                    reference.trigger, reference.value
                )));
            }
            if !references.insert((reference.trigger, reference.value.clone())) {
                return Err(Error::Duplicate(format!(
                    "frontend reference `{}{}`",
                    reference.trigger, reference.value
                )));
            }
        }
        if contribution.active_input.is_some() && std::mem::replace(&mut active_input, true) {
            return Err(Error::Duplicate("frontend active input".into()));
        }
    }
    Ok(())
}

pub(crate) const fn approximate_tokens(bytes: usize) -> usize {
    bytes / ESTIMATED_BYTES_PER_TOKEN
}

pub(crate) fn approximate_item_tokens(item: &Value) -> usize {
    serde_json::to_vec(item)
        .map_or(0, |bytes| approximate_tokens(bytes.len()))
        .max(1)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::backend::checkpoint::sqlite::SqliteCheckpoint;
    use crate::backend::model::ModelOutput;
    use crate::backend::model::ToolDefinition;
    use crate::middleware::tools::Tool;
    use crate::middleware::tools::ToolContext;
    use crate::protocol::FrontendReference;

    struct UnrenderedTool;

    impl Tool for UnrenderedTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "unrendered".into(),
                description: String::new(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        fn call<'a>(
            &'a self,
            _context: ToolContext,
            _arguments: Value,
        ) -> BoxFuture<'a, Result<String>> {
            Box::pin(async { Ok(String::new()) })
        }
    }

    struct ToolOwner;

    impl Middleware for ToolOwner {
        fn name(&self) -> &'static str {
            "tool_owner"
        }

        fn register(&self, catalog: &mut Catalog, _runtime: &RuntimeContext) -> Result<()> {
            catalog.register(Arc::new(UnrenderedTool))
        }
    }

    struct CatchAllRenderer;

    impl Middleware for CatchAllRenderer {
        fn name(&self) -> &'static str {
            "catch_all"
        }

        fn render(&self, _event: &EventMsg) -> Option<FrontendBlock> {
            Some(FrontendBlock {
                id: None,
                group: None,
                append: false,
                pending: false,
                text: String::new(),
                format: crate::protocol::FrontendBlockFormat::PlainText,
                tone: FrontendTone::Neutral,
            })
        }
    }

    #[test]
    fn catalog_requires_the_registering_middleware_to_render_its_tools() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let runtime = RuntimeContext {
            checkpoints: Arc::new(
                SqliteCheckpoint::new(temporary.path().join("checkpoints.sqlite3"))
                    .expect("checkpoint store"),
            ),
            session_id: "session".into(),
            model_route: "model".into(),
            session_context: SessionContext::default(),
            metadata: BTreeMap::new(),
            frontend: Arc::new(|_| Ok(())),
        };
        let stack = MiddlewareStack::new(vec![Arc::new(CatchAllRenderer), Arc::new(ToolOwner)])
            .expect("middleware stack");

        assert_eq!(
            stack
                .catalog(&runtime)
                .err()
                .expect("unrendered tool should be rejected")
                .to_string(),
            "configuration error: middleware `tool_owner` registered tool `unrendered` but does not render `ToolCallBegin`"
        );
    }

    struct Extension;

    impl Middleware for Extension {
        fn name(&self) -> &'static str {
            "extension"
        }

        fn frontend(&self) -> FrontendContribution {
            FrontendContribution {
                capability: self.name().into(),
                count: None,
                commands: Vec::new(),
                widgets: Vec::new(),
                references: vec![FrontendReference {
                    trigger: ' ',
                    value: "item".into(),
                    description: String::new(),
                }],
                active_input: None,
            }
        }
    }

    #[test]
    fn frontend_rejects_malformed_reference_triggers() {
        assert_eq!(
            MiddlewareStack::new(vec![Arc::new(Extension)])
                .expect("middleware stack")
                .frontend()
                .expect_err("invalid frontend extension")
                .to_string(),
            "configuration error: invalid frontend reference ` item`"
        );
    }

    struct Observer(&'static str, Arc<Mutex<Vec<&'static str>>>);

    impl Middleware for Observer {
        fn name(&self) -> &'static str {
            self.0
        }

        fn after_model<'a>(
            &'a self,
            _context: &'a mut AfterModelContext<'_>,
        ) -> BoxFuture<'a, Result<()>> {
            Box::pin(async move {
                self.1.lock().expect("observer trace").push(self.0);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn after_model_preserves_middleware_order() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let stack = MiddlewareStack::new(vec![
            Arc::new(Observer("first", Arc::clone(&trace))),
            Arc::new(Observer("second", Arc::clone(&trace))),
        ])
        .expect("middleware stack");
        let output = ModelOutput::from_output(
            vec![serde_json::json!({
                "type": "message",
                "content": [{"type": "output_text", "text": "done"}]
            })],
            true,
            TokenUsage::default(),
        )
        .expect("model output");
        let session_context = SessionContext::default();
        let metadata = BTreeMap::new();
        let mut events = Vec::new();

        stack
            .after_model(AfterModelContext {
                provider: "default",
                session_id: "session",
                session_context: &session_context,
                metadata: &metadata,
                turn_id: "turn",
                model_step: 0,
                context_window: 128_000,
                queued_input_count: 0,
                output: &output,
                events: &mut events,
            })
            .await
            .expect("after model");

        assert_eq!(
            *trace.lock().expect("observer trace"),
            vec!["first", "second"]
        );
    }
}
