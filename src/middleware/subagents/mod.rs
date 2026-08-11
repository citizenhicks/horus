//! Durable asynchronous child-agent middleware.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::ActiveCommandContext;
use super::ActiveSubmissionResult;
use super::Middleware;
use super::MiddlewareCommandContext;
use super::MiddlewareCommandOutput;
use super::ModelContext;
use super::PromptSection;
use super::RuntimeContext;
use super::SessionEndContext;
use super::manifest::{MiddlewareManifest, MiddlewareSettingChoices, MiddlewareSettingManifest};
use super::tools::Catalog;
use super::tools::Tool;
use super::tools::ToolContext;
use super::tools::labeled_tool_heading;
use super::tools::render_tool_event;
use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::agent::Agent;
use crate::backend::checkpoint::Checkpoint;
use crate::backend::checkpoint::CheckpointStore;
use crate::backend::model::ToolDefinition;
use crate::backend::model::internal_user_message;
use crate::protocol::EventMsg;
use crate::protocol::FrontendBlock;
use crate::protocol::FrontendCommand;
use crate::protocol::FrontendContribution;
use crate::protocol::FrontendEvent;
use crate::protocol::FrontendPreviewUpdate;
use crate::protocol::FrontendTone;
use crate::protocol::Op;
use crate::protocol::{internal_message_kind, is_internal_message, strip_attachment_references};

use self::runtime::AgentPresentation;
use self::runtime::Followup;
use self::runtime::MAX_MESSAGE_BYTES;
use self::runtime::Shared;
use self::runtime::monitor_agent;

mod runtime;

const MAX_TASK_NAME_BYTES: usize = 64;
const IDENTITY_KEY: &str = "subagents.identity";
const SPAWN_CONTEXT_KEY: &str = "subagents.spawn_context";
const PREVIEW_PAGE_COMMAND: &str = "preview_page";
mod text {
    include!(concat!(
        env!("OUT_DIR"),
        "/src_middleware_subagents_text.rs"
    ));
}

const MIN_WAIT_MS: u64 = 10_000;
const MAX_WAIT_MS: u64 = 3_600_000;
const MAX_CONFIGURED_DEPTH: u8 = 16;
const MAX_CONFIGURED_CONCURRENCY: usize = 64;
const MAX_CONFIGURED_AGENTS: usize = 256;
const _: () = {
    assert!(text::DEFAULTS_WAIT_MS >= MIN_WAIT_MS as i64);
    assert!(text::DEFAULTS_WAIT_MS <= MAX_WAIT_MS as i64);
    assert!(text::DEFAULTS_MAX_DEPTH >= 1);
    assert!(text::DEFAULTS_MAX_DEPTH <= MAX_CONFIGURED_DEPTH as i64);
    assert!(text::DEFAULTS_MAX_CONCURRENCY >= 2);
    assert!(text::DEFAULTS_MAX_CONCURRENCY <= MAX_CONFIGURED_CONCURRENCY as i64);
    assert!(text::DEFAULTS_MAX_AGENTS >= text::DEFAULTS_MAX_CONCURRENCY);
    assert!(text::DEFAULTS_MAX_AGENTS <= MAX_CONFIGURED_AGENTS as i64);
    assert!(text::SETTING_MAX_DEPTH_STEP > 0);
    assert!(text::SETTING_MAX_CONCURRENCY_STEP > 0);
    assert!(text::SETTING_MAX_AGENTS_STEP > 0);
};
const DEFAULT_WAIT_MS: u64 = text::DEFAULTS_WAIT_MS as u64;
/// Default maximum child-agent nesting depth.
pub const DEFAULT_MAX_DEPTH: u8 = text::DEFAULTS_MAX_DEPTH as u8;
/// Default number of concurrently active agents, including the root.
pub const DEFAULT_MAX_CONCURRENCY: usize = text::DEFAULTS_MAX_CONCURRENCY as usize;
/// Default number of retained agents, including the root.
pub const DEFAULT_MAX_AGENTS: usize = text::DEFAULTS_MAX_AGENTS as usize;
const SETTINGS: &[MiddlewareSettingManifest] = &[
    MiddlewareSettingManifest::Select {
        id: "model_route",
        label: text::SETTING_MODEL_ROUTE_LABEL,
        description: text::SETTING_MODEL_ROUTE_DESCRIPTION,
        choices: MiddlewareSettingChoices::ModelRoutes,
        unset_label: Some(text::SETTING_MODEL_ROUTE_UNSET_LABEL),
        default: None,
        max_bytes: 4 * 1024,
    },
    MiddlewareSettingManifest::Integer {
        id: "max_depth",
        label: text::SETTING_MAX_DEPTH_LABEL,
        description: text::SETTING_MAX_DEPTH_DESCRIPTION,
        min: 1,
        max: Some(MAX_CONFIGURED_DEPTH as i64),
        step: text::SETTING_MAX_DEPTH_STEP,
        default: DEFAULT_MAX_DEPTH as i64,
    },
    MiddlewareSettingManifest::Integer {
        id: "max_concurrency",
        label: text::SETTING_MAX_CONCURRENCY_LABEL,
        description: text::SETTING_MAX_CONCURRENCY_DESCRIPTION,
        min: 2,
        max: Some(MAX_CONFIGURED_CONCURRENCY as i64),
        step: text::SETTING_MAX_CONCURRENCY_STEP,
        default: DEFAULT_MAX_CONCURRENCY as i64,
    },
    MiddlewareSettingManifest::Integer {
        id: "max_agents",
        label: text::SETTING_MAX_AGENTS_LABEL,
        description: text::SETTING_MAX_AGENTS_DESCRIPTION,
        min: 2,
        max: Some(MAX_CONFIGURED_AGENTS as i64),
        step: text::SETTING_MAX_AGENTS_STEP,
        default: DEFAULT_MAX_AGENTS as i64,
    },
];

/// Configuration and presentation metadata for child-agent collaboration.
pub const MANIFEST: MiddlewareManifest = MiddlewareManifest {
    id: "subagents",
    label: text::MANIFEST_LABEL,
    description: text::MANIFEST_DESCRIPTION,
    required: false,
    default_enabled: true,
    settings: SETTINGS,
};

/// Child-agent parameters owned by the subagent capability.
#[derive(Clone)]
pub struct SubagentLaunch {
    pub session_id: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub metadata: BTreeMap<String, Value>,
}

/// Creates one child agent for this capability.
pub type SubagentLauncher =
    Arc<dyn Fn(SubagentLaunch) -> BoxFuture<'static, Result<Agent>> + Send + Sync>;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ForkTurns {
    #[default]
    None,
    All,
    Last(usize),
}

impl ForkTurns {
    fn label(self) -> String {
        match self {
            Self::None => "No context".into(),
            Self::All => "Full context".into(),
            Self::Last(1) => "Last 1 turn".into(),
            Self::Last(turns) => format!("Last {turns} turns"),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentIdentity {
    root_session_id: String,
    agent_path: String,
    depth: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PreviewPosition {
    BeforeSequence { before_sequence: u64 },
    BeforeItem { sequence: u64, before_item: usize },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewCursor {
    path: String,
    position: PreviewPosition,
}

impl PreviewPosition {
    fn validate(self) -> Result<Self> {
        let valid = match self {
            Self::BeforeSequence { before_sequence } => before_sequence > 0,
            Self::BeforeItem {
                before_item,
                sequence: _,
            } => before_item > 0,
        };
        if valid {
            Ok(self)
        } else {
            Err(Error::Tool("invalid subagent preview cursor".into()))
        }
    }

    fn page_id(self, path: &str) -> String {
        match self {
            Self::BeforeSequence { before_sequence } => {
                format!("{path}:before-sequence-{before_sequence}")
            }
            Self::BeforeItem {
                sequence,
                before_item,
            } => format!("{path}:sequence-{sequence}-before-item-{before_item}"),
        }
    }
}

impl AgentIdentity {
    fn read(session_id: &str, metadata: &BTreeMap<String, Value>) -> Result<Self> {
        let Some(value) = metadata.get(IDENTITY_KEY) else {
            return Ok(Self {
                root_session_id: session_id.into(),
                agent_path: "/root".into(),
                depth: 0,
            });
        };
        Ok(serde_json::from_value(value.clone())?)
    }

    fn metadata(&self, mut metadata: BTreeMap<String, Value>) -> BTreeMap<String, Value> {
        metadata.insert(
            IDENTITY_KEY.into(),
            serde_json::json!({
                "root_session_id": self.root_session_id,
                "agent_path": self.agent_path,
                "depth": self.depth,
            }),
        );
        metadata
    }
}

#[derive(Clone)]
struct AgentScope {
    checkpoints: Arc<dyn CheckpointStore>,
    launch_agent: SubagentLauncher,
    session_id: String,
    root_session_id: String,
    agent_path: String,
    depth: u8,
    model: String,
    metadata: BTreeMap<String, Value>,
}

impl AgentScope {
    fn new(runtime: &RuntimeContext, launch_agent: SubagentLauncher) -> Result<Self> {
        let identity = AgentIdentity::read(&runtime.session_id, &runtime.metadata)?;
        Ok(Self {
            checkpoints: Arc::clone(&runtime.checkpoints),
            launch_agent,
            session_id: runtime.session_id.clone(),
            root_session_id: identity.root_session_id,
            agent_path: identity.agent_path,
            depth: identity.depth,
            model: runtime.model_route.clone(),
            metadata: runtime.metadata.clone(),
        })
    }

    async fn fork(
        &self,
        session_id: String,
        agent_path: String,
        model: String,
        reasoning_effort: Option<String>,
        turns: ForkTurns,
    ) -> Result<Agent> {
        let parent = self
            .checkpoints
            .load(&self.session_id)
            .await?
            .ok_or_else(|| Error::Checkpoint("parent checkpoint is missing".into()))?;
        let parent_sequence = parent.sequence;
        let pending = parent
            .pending_tools
            .iter()
            .map(|call| call.call_id.clone())
            .collect::<BTreeSet<_>>();
        let context = parent
            .context
            .into_iter()
            .filter(|item| {
                item.get("type").and_then(Value::as_str) != Some("function_call")
                    || item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .is_none_or(|call_id| !pending.contains(call_id))
            })
            .collect::<Vec<_>>();
        let mut checkpoint = Checkpoint::empty(&session_id);
        checkpoint.catalog_visible = false;
        checkpoint.context = fork_context(&context, turns);
        checkpoint.session_context = parent.session_context;
        let mut metadata = AgentIdentity {
            root_session_id: self.root_session_id.clone(),
            agent_path: agent_path.clone(),
            depth: self.depth + 1,
        }
        .metadata(self.metadata.clone());
        metadata.insert(SPAWN_CONTEXT_KEY.into(), Value::String(turns.label()));
        checkpoint.metadata.clone_from(&metadata);
        self.checkpoints
            .fork(&self.session_id, parent_sequence, &checkpoint)
            .await?;
        (self.launch_agent)(SubagentLaunch {
            session_id,
            model,
            reasoning_effort,
            metadata,
        })
        .await
    }

    async fn resume(
        &self,
        session_id: String,
        agent_path: String,
        depth: u8,
        model: String,
    ) -> Result<Agent> {
        let checkpoint = self.checkpoints.load(&session_id).await?.ok_or_else(|| {
            Error::Checkpoint(format!("checkpoint for `{agent_path}` is missing"))
        })?;
        (self.launch_agent)(SubagentLaunch {
            session_id,
            model,
            reasoning_effort: None,
            metadata: AgentIdentity {
                root_session_id: self.root_session_id.clone(),
                agent_path,
                depth,
            }
            .metadata(checkpoint.metadata),
        })
        .await
    }
}

/// Contributes asynchronous collaboration tools.
pub struct Subagents {
    max_depth: u8,
    launch_agent: SubagentLauncher,
    default_model: Option<String>,
    default_reasoning: Option<String>,
    prompt: String,
    shared: Arc<Shared>,
}

impl Subagents {
    /// Creates a child-agent capability with hard depth, concurrency, and agent limits.
    ///
    /// `max_concurrency` counts active agents and `max_agents` counts retained agents;
    /// both include the root.
    pub fn new(
        max_depth: u8,
        max_concurrency: usize,
        max_agents: usize,
        launch_agent: SubagentLauncher,
    ) -> Result<Self> {
        if max_depth == 0 || max_depth > MAX_CONFIGURED_DEPTH {
            return Err(Error::Config(format!(
                "subagent max depth must be between 1 and {MAX_CONFIGURED_DEPTH}"
            )));
        }
        if max_concurrency > MAX_CONFIGURED_CONCURRENCY {
            return Err(Error::Config(format!(
                "subagent max concurrency cannot exceed {MAX_CONFIGURED_CONCURRENCY}"
            )));
        }
        if max_agents > MAX_CONFIGURED_AGENTS {
            return Err(Error::Config(format!(
                "subagent max agents cannot exceed {MAX_CONFIGURED_AGENTS}"
            )));
        }
        Ok(Self {
            max_depth,
            launch_agent,
            default_model: None,
            default_reasoning: None,
            prompt: text::PROMPT_DEFAULT.into(),
            shared: Arc::new(Shared::new(max_concurrency, max_agents)?),
        })
    }

    /// Selects a registered provider/model route for children by default.
    #[must_use]
    pub fn default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = Some(model.into());
        self
    }

    /// Selects a reasoning effort for children by default.
    pub fn default_reasoning(mut self, reasoning: impl Into<String>) -> Result<Self> {
        let reasoning = reasoning.into();
        if reasoning.trim().is_empty() {
            return Err(Error::Config(
                "subagent reasoning effort cannot be empty".into(),
            ));
        }
        self.default_reasoning = Some(reasoning);
        Ok(self)
    }

    /// Overrides the instruction given to child agents.
    pub fn prompt(mut self, prompt: impl Into<String>) -> Result<Self> {
        let prompt = prompt.into();
        if prompt.trim().is_empty() {
            return Err(Error::Config("subagent prompt cannot be empty".into()));
        }
        self.prompt = prompt;
        Ok(self)
    }

    fn section(&self, identity: &AgentIdentity) -> PromptSection {
        let body = if identity.depth == 0 {
            text::PROMPT_ROOT.into()
        } else {
            format!(
                "You are `{}`, a child agent.\n{}",
                identity.agent_path,
                self.prompt.trim()
            )
        };
        PromptSection::new(body)
    }

    async fn read_command(
        &self,
        session_id: &str,
        metadata: &BTreeMap<String, Value>,
        arguments: &str,
    ) -> Result<MiddlewareCommandOutput> {
        let identity = AgentIdentity::read(session_id, metadata)?;
        let path = arguments.trim();
        if !path.is_empty() {
            return self
                .preview_page(&identity.root_session_id, path, None)
                .await;
        }
        let options = self
            .shared
            .resume_options(&identity.root_session_id)
            .await?;
        if options.is_empty() {
            return Ok(MiddlewareCommandOutput::render(
                "subagents",
                text::RENDER_EMPTY,
                FrontendTone::Neutral,
            ));
        }
        Ok(MiddlewareCommandOutput::events(vec![
            FrontendEvent::Picker {
                title: text::RENDER_OPEN.into(),
                options,
            },
        ]))
    }

    async fn read_preview_page(
        &self,
        session_id: &str,
        metadata: &BTreeMap<String, Value>,
        arguments: &str,
    ) -> Result<MiddlewareCommandOutput> {
        let identity = AgentIdentity::read(session_id, metadata)?;
        let cursor: PreviewCursor = serde_json::from_str(arguments)
            .map_err(|_| Error::Tool("invalid subagent preview cursor".into()))?;
        if cursor.path.trim() != cursor.path || cursor.path.is_empty() {
            return Err(Error::Tool("invalid subagent preview cursor".into()));
        }
        self.preview_page(
            &identity.root_session_id,
            &cursor.path,
            Some(cursor.position.validate()?),
        )
        .await
    }

    async fn preview_page(
        &self,
        root_session_id: &str,
        path: &str,
        position: Option<PreviewPosition>,
    ) -> Result<MiddlewareCommandOutput> {
        let page = self.shared.preview(root_session_id, path, position).await?;
        let next = page
            .next
            .map(|position| -> Result<Op> {
                Ok(Op::CapabilityCommand {
                    capability: MANIFEST.id.into(),
                    command: PREVIEW_PAGE_COMMAND.into(),
                    arguments: serde_json::to_string(&PreviewCursor {
                        path: path.into(),
                        position,
                    })?,
                    input: None,
                    target: None,
                })
            })
            .transpose()?;
        Ok(MiddlewareCommandOutput::events(vec![
            FrontendEvent::Preview {
                id: path.into(),
                title: path.rsplit('/').next().unwrap_or(path).into(),
                subtitle: page.subtitle,
                page_id: page.page_id,
                update: if position.is_some() {
                    FrontendPreviewUpdate::Prepend
                } else {
                    FrontendPreviewUpdate::Replace
                },
                events: page.events,
                next,
            },
        ]))
    }
}

impl Middleware for Subagents {
    fn name(&self) -> &'static str {
        MANIFEST.id
    }

    fn initialize<'a>(&'a self, context: RuntimeContext) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.shared.initialize(context))
    }

    fn register(&self, catalog: &mut Catalog, runtime: &RuntimeContext) -> Result<()> {
        let scope = Arc::new(AgentScope::new(runtime, Arc::clone(&self.launch_agent))?);
        if scope.depth < self.max_depth {
            catalog.register(Arc::new(SpawnAgent {
                default_model: self.default_model.clone(),
                default_reasoning: self.default_reasoning.clone(),
                shared: Arc::clone(&self.shared),
                scope: Arc::clone(&scope),
            }))?;
        }
        catalog.register(Arc::new(SendMessage {
            shared: Arc::clone(&self.shared),
            scope: Arc::clone(&scope),
        }))?;
        catalog.register(Arc::new(FollowupTask {
            shared: Arc::clone(&self.shared),
            scope: Arc::clone(&scope),
        }))?;
        catalog.register(Arc::new(ListAgents {
            shared: Arc::clone(&self.shared),
            scope: Arc::clone(&scope),
        }))?;
        catalog.register(Arc::new(InterruptAgent {
            shared: Arc::clone(&self.shared),
            scope: Arc::clone(&scope),
        }))?;
        catalog.register(Arc::new(WaitAgent {
            shared: Arc::clone(&self.shared),
            scope,
        }))
    }

    fn prompt_section(&self, runtime: &RuntimeContext) -> Result<Option<PromptSection>> {
        let identity = AgentIdentity::read(&runtime.session_id, &runtime.metadata)?;
        Ok(Some(self.section(&identity)))
    }

    fn frontend(&self) -> FrontendContribution {
        FrontendContribution {
            capability: self.name().into(),
            accepts_file_attachments: false,
            count: None,
            commands: vec![FrontendCommand {
                name: "subagents".into(),
                arguments: String::new(),
                description: text::COMMAND_DESCRIPTION.into(),
            }],
            widgets: Vec::new(),
            references: Vec::new(),
            active_input: None,
        }
    }

    fn render(&self, event: &EventMsg, _session_id: &str) -> Option<FrontendBlock> {
        render_tool_event(
            event,
            |name| {
                matches!(
                    name,
                    "spawn_agent"
                        | "send_message"
                        | "followup_task"
                        | "list_agents"
                        | "interrupt_agent"
                        | "wait_agent"
                )
            },
            |name, arguments| match name {
                "spawn_agent" => labeled_tool_heading(text::RENDER_AGENT, "task_name", arguments),
                "send_message" => labeled_tool_heading(text::RENDER_MESSAGE, "target", arguments),
                "followup_task" => {
                    labeled_tool_heading(text::RENDER_FOLLOW_UP, "target", arguments)
                }
                "list_agents" => {
                    labeled_tool_heading(text::RENDER_AGENTS, "path_prefix", arguments)
                }
                "interrupt_agent" => {
                    labeled_tool_heading(text::RENDER_INTERRUPT, "target", arguments)
                }
                "wait_agent" => labeled_tool_heading(text::RENDER_WAIT, "timeout_ms", arguments),
                _ => name.to_string().into(),
            },
        )
    }

    fn command<'a>(
        &'a self,
        context: MiddlewareCommandContext<'a>,
    ) -> BoxFuture<'a, Result<MiddlewareCommandOutput>> {
        Box::pin(async move {
            match context.command {
                "subagents" => {
                    self.read_command(
                        context.session_id,
                        &context.checkpoint.metadata,
                        context.arguments,
                    )
                    .await
                }
                PREVIEW_PAGE_COMMAND => {
                    self.read_preview_page(
                        context.session_id,
                        &context.checkpoint.metadata,
                        context.arguments,
                    )
                    .await
                }
                command => Err(Error::Unknown(format!("subagents command `{command}`"))),
            }
        })
    }

    fn active_command<'a>(
        &'a self,
        context: &'a mut ActiveCommandContext<'_>,
    ) -> BoxFuture<'a, Result<Option<ActiveSubmissionResult>>> {
        Box::pin(async move {
            let output = match context.command {
                "subagents" => {
                    self.read_command(context.session_id, context.metadata, context.arguments)
                        .await
                }
                PREVIEW_PAGE_COMMAND => {
                    self.read_preview_page(context.session_id, context.metadata, context.arguments)
                        .await
                }
                _ => return Ok(None),
            };
            match output {
                Ok(output) => {
                    context
                        .events
                        .extend(output.events.into_iter().map(EventMsg::Frontend));
                    Ok(Some(ActiveSubmissionResult::Handled))
                }
                Err(error) => Ok(Some(ActiveSubmissionResult::Rejected(error.to_string()))),
            }
        })
    }

    fn before_model<'a>(&'a self, context: &'a mut ModelContext<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let identity = AgentIdentity::read(context.session_id, context.metadata)?;
            let acknowledged = context
                .input()
                .iter()
                .filter_map(internal_message_kind)
                .filter_map(|kind| kind.strip_prefix("subagent_mail:"))
                .map(str::to_owned)
                .collect();
            let mail = self
                .shared
                .receive_mail(
                    &identity.root_session_id,
                    &identity.agent_path,
                    &acknowledged,
                )
                .await?;
            if !mail.is_empty() {
                *context.checkpoint_changed = true;
            }
            for mail in mail {
                context.push_input(internal_user_message(&mail.internal_kind(), &mail.render()));
            }
            Ok(())
        })
    }

    fn shutdown<'a>(&'a self, context: SessionEndContext) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let identity = AgentIdentity::read(&context.session_id, &context.metadata)?;
            if identity.depth == 0 {
                self.shared.remove_root(&identity.root_session_id).await;
            }
            Ok(())
        })
    }
}

struct SpawnAgent {
    default_model: Option<String>,
    default_reasoning: Option<String>,
    shared: Arc<Shared>,
    scope: Arc<AgentScope>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArgs {
    task_name: String,
    message: String,
    fork_turns: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
}

impl Tool for SpawnAgent {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "spawn_agent".into(),
            description: text::TOOL_SPAWN_AGENT_DESCRIPTION.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_name": {
                        "type": "string",
                        "description": text::TOOL_SPAWN_AGENT_PARAMETER_TASK_NAME_DESCRIPTION
                    },
                    "message": {"type": "string"},
                    "fork_turns": {
                        "type": "string",
                        "description": text::TOOL_SPAWN_AGENT_PARAMETER_FORK_TURNS_DESCRIPTION
                    },
                    "model": {
                        "type": "string",
                        "description": text::TOOL_SPAWN_AGENT_PARAMETER_MODEL_DESCRIPTION
                    },
                    "reasoning_effort": {
                        "type": "string",
                        "description": text::TOOL_SPAWN_AGENT_PARAMETER_REASONING_EFFORT_DESCRIPTION
                    }
                },
                "required": ["task_name", "message"],
                "additionalProperties": false
            }),
        }
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: SpawnArgs = serde_json::from_value(arguments)?;
            validate_task_name(&arguments.task_name)?;
            let message = validate_message("message", arguments.message)?;
            let turns = parse_fork_turns(arguments.fork_turns.as_deref())?;
            let model = arguments
                .model
                .or_else(|| self.default_model.clone())
                .unwrap_or_else(|| self.scope.model.clone());
            let reasoning_effort = arguments
                .reasoning_effort
                .or_else(|| self.default_reasoning.clone());
            let path = format!(
                "{}/{}",
                self.scope.agent_path.trim_end_matches('/'),
                arguments.task_name
            );
            let session_id = Uuid::new_v4().to_string();
            let shared = Arc::clone(&self.shared);
            let scope = Arc::clone(&self.scope);
            supervise(async move {
                shared
                    .reserve(
                        &scope.root_session_id,
                        &path,
                        &scope.agent_path,
                        session_id.clone(),
                        scope.depth + 1,
                        AgentPresentation {
                            model: model.clone(),
                            spawn_context: turns.label(),
                        },
                    )
                    .await?;
                let agent = match scope
                    .fork(session_id, path.clone(), model, reasoning_effort, turns)
                    .await
                {
                    Ok(agent) => agent,
                    Err(error) => {
                        return Err(cleanup_error(
                            error,
                            shared.remove(&scope.root_session_id, &path).await,
                        ));
                    }
                };
                let model = agent.model_route().to_string();
                let (sender, events) = agent.into_parts();
                if let Err(error) = shared
                    .attach(&scope.root_session_id, &path, sender.clone(), Some(model))
                    .await
                    .and_then(|()| {
                        sender
                            .submit(Op::UserInput {
                                text: message,
                                attachments: Vec::new(),
                            })
                            .map(|_| ())
                    })
                {
                    return Err(cleanup_error(
                        error,
                        shared.remove(&scope.root_session_id, &path).await,
                    ));
                }
                tokio::spawn(monitor_agent(
                    Arc::clone(&shared),
                    scope.root_session_id.clone(),
                    path.clone(),
                    events,
                ));
                Ok(serde_json::json!({"task_name": path}).to_string())
            })
            .await
        })
    }
}

struct SendMessage {
    shared: Arc<Shared>,
    scope: Arc<AgentScope>,
}

struct FollowupTask {
    shared: Arc<Shared>,
    scope: Arc<AgentScope>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageArgs {
    target: String,
    message: String,
}

impl Tool for SendMessage {
    fn definition(&self) -> ToolDefinition {
        message_definition("send_message", text::TOOL_SEND_MESSAGE_DESCRIPTION)
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: MessageArgs = serde_json::from_value(arguments)?;
            self.shared
                .queue_message(
                    &self.scope.root_session_id,
                    &self.scope.agent_path,
                    &arguments.target,
                    validate_message("message", arguments.message)?,
                )
                .await?;
            Ok(String::new())
        })
    }
}

impl Tool for FollowupTask {
    fn definition(&self) -> ToolDefinition {
        message_definition("followup_task", text::TOOL_FOLLOWUP_TASK_DESCRIPTION)
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: MessageArgs = serde_json::from_value(arguments)?;
            let message = validate_message("message", arguments.message)?;
            let shared = Arc::clone(&self.shared);
            let scope = Arc::clone(&self.scope);
            supervise(async move {
                let followup = shared
                    .prepare_followup(
                        &scope.root_session_id,
                        &scope.agent_path,
                        &arguments.target,
                        message.clone(),
                    )
                    .await?;
                let Followup::Start {
                    record,
                    sender,
                    previous,
                } = followup
                else {
                    return Ok(String::new());
                };
                let (sender, events, model) = match sender {
                    Some(sender) => (sender, None, None),
                    None => {
                        let agent = match scope
                            .resume(
                                record.session_id,
                                arguments.target.clone(),
                                record.depth,
                                record.model,
                            )
                            .await
                        {
                            Ok(agent) => agent,
                            Err(error) => {
                                return Err(cleanup_error(
                                    error,
                                    shared
                                        .rollback(
                                            &scope.root_session_id,
                                            &arguments.target,
                                            previous.clone(),
                                        )
                                        .await,
                                ));
                            }
                        };
                        let model = agent.model_route().to_string();
                        let (sender, events) = agent.into_parts();
                        (sender, Some(events), Some(model))
                    }
                };
                if let Err(error) = shared
                    .attach(
                        &scope.root_session_id,
                        &arguments.target,
                        sender.clone(),
                        model,
                    )
                    .await
                    .and_then(|()| {
                        sender
                            .submit(Op::UserInput {
                                text: message,
                                attachments: Vec::new(),
                            })
                            .map(|_| ())
                    })
                {
                    return Err(cleanup_error(
                        error,
                        shared
                            .rollback(&scope.root_session_id, &arguments.target, previous)
                            .await,
                    ));
                }
                if let Some(events) = events {
                    tokio::spawn(monitor_agent(
                        Arc::clone(&shared),
                        scope.root_session_id.clone(),
                        arguments.target,
                        events,
                    ));
                }
                Ok(String::new())
            })
            .await
        })
    }
}

fn message_definition(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "target": {"type": "string"},
                "message": {"type": "string"}
            },
            "required": ["target", "message"],
            "additionalProperties": false
        }),
    }
}

struct ListAgents {
    shared: Arc<Shared>,
    scope: Arc<AgentScope>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    path_prefix: Option<String>,
}

impl Tool for ListAgents {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_agents".into(),
            description: text::TOOL_LIST_AGENTS_DESCRIPTION.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path_prefix": {"type": "string"}},
                "additionalProperties": false
            }),
        }
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: ListArgs = serde_json::from_value(arguments)?;
            let agents = self
                .shared
                .list(
                    &self.scope.root_session_id,
                    arguments.path_prefix.as_deref(),
                )
                .await?;
            Ok(serde_json::json!({"agents": agents}).to_string())
        })
    }
}

struct InterruptAgent {
    shared: Arc<Shared>,
    scope: Arc<AgentScope>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetArgs {
    target: String,
}

impl Tool for InterruptAgent {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "interrupt_agent".into(),
            description: text::TOOL_INTERRUPT_AGENT_DESCRIPTION.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"target": {"type": "string"}},
                "required": ["target"],
                "additionalProperties": false
            }),
        }
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: TargetArgs = serde_json::from_value(arguments)?;
            let previous_status = self
                .shared
                .interrupt(&self.scope.root_session_id, &arguments.target)
                .await?;
            Ok(serde_json::json!({"previous_status": previous_status}).to_string())
        })
    }
}

struct WaitAgent {
    shared: Arc<Shared>,
    scope: Arc<AgentScope>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    timeout_ms: Option<u64>,
}

impl Tool for WaitAgent {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "wait_agent".into(),
            description: text::TOOL_WAIT_AGENT_DESCRIPTION.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": MIN_WAIT_MS,
                        "maximum": MAX_WAIT_MS
                    }
                },
                "additionalProperties": false
            }),
        }
    }

    fn interrupt_on_active_input(&self) -> bool {
        true
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: WaitArgs = serde_json::from_value(arguments)?;
            let timeout_ms = arguments.timeout_ms.unwrap_or(DEFAULT_WAIT_MS);
            if !(MIN_WAIT_MS..=MAX_WAIT_MS).contains(&timeout_ms) {
                return Err(Error::Tool(format!(
                    "timeout_ms must be between {MIN_WAIT_MS} and {MAX_WAIT_MS}"
                )));
            }
            let agents = self
                .shared
                .wait(
                    &self.scope.root_session_id,
                    &self.scope.agent_path,
                    Duration::from_millis(timeout_ms),
                )
                .await?;
            Ok(serde_json::json!({
                "updated": !agents.is_empty(),
                "agents": agents
            })
            .to_string())
        })
    }
}

fn fork_context(context: &[Value], turns: ForkTurns) -> Vec<Value> {
    let mut fork = match turns {
        ForkTurns::None => Vec::new(),
        ForkTurns::All => context.to_vec(),
        ForkTurns::Last(turns) => {
            let start = context
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, item)| {
                    item.get("role").and_then(Value::as_str) == Some("user")
                        && !is_internal_message(item)
                })
                .nth(turns.saturating_sub(1))
                .map_or(0, |(index, _)| index);
            context[start..].to_vec()
        }
    };
    strip_attachment_references(&mut fork);
    fork
}

fn parse_fork_turns(value: Option<&str>) -> Result<ForkTurns> {
    let Some(value) = value else {
        return Ok(ForkTurns::default());
    };
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Ok(ForkTurns::None);
    }
    if value.eq_ignore_ascii_case("all") {
        return Ok(ForkTurns::All);
    }
    let turns = value.parse::<usize>().map_err(|_| {
        Error::Tool("fork_turns must be `none`, `all`, or a positive integer string".into())
    })?;
    if turns == 0 {
        return Err(Error::Tool(
            "fork_turns must be `none`, `all`, or a positive integer string".into(),
        ));
    }
    Ok(ForkTurns::Last(turns))
}

fn validate_task_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > MAX_TASK_NAME_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(Error::Tool(
            "task_name must contain 1-64 lowercase letters, digits, or underscores".into(),
        ));
    }
    Ok(())
}

fn validate_message(name: &str, message: String) -> Result<String> {
    if message.trim().is_empty() {
        return Err(Error::Tool(format!("{name} cannot be empty")));
    }
    if message.len() > MAX_MESSAGE_BYTES {
        return Err(Error::Tool(format!(
            "{name} exceeded {MAX_MESSAGE_BYTES} bytes"
        )));
    }
    Ok(message)
}

fn cleanup_error(error: Error, cleanup: Result<()>) -> Error {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => Error::Rollback {
            primary: Box::new(error),
            rollback: Box::new(cleanup),
        },
    }
}

async fn supervise<T>(operation: impl Future<Output = Result<T>> + Send + 'static) -> Result<T>
where
    T: Send + 'static,
{
    tokio::spawn(operation)
        .await
        .map_err(|error| Error::Stopped(format!("subagent lifecycle task failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::QueuedInputBaseline;
    use crate::middleware::QueuedInputQueue;
    use crate::protocol::{ToolCallBeginEvent, ToolCallEndEvent};

    fn test_middleware() -> Subagents {
        Subagents::new(
            1,
            2,
            2,
            Arc::new(|_| Box::pin(async { Err(Error::Stopped("unused".into())) })),
        )
        .expect("subagents middleware")
    }

    #[test]
    fn prompt_section_guides_root_to_delegate_parallel_work() {
        let identity = AgentIdentity::read("root", &BTreeMap::new()).expect("root identity");
        let section = test_middleware().section(&identity);

        assert_eq!(
            section.body,
            "Delegate independent work to subagents when it can run in parallel. Spawn with fresh context by default; include recent turns only when the task requires them, and full history only when essential. They share your workspace; continue your own work while they run, and wait only when you need their results."
        );
    }

    #[test]
    fn prompt_section_identifies_child_with_default_instruction() {
        let identity = AgentIdentity {
            root_session_id: "root".into(),
            agent_path: "/root/reviewer".into(),
            depth: 1,
        };
        let section = test_middleware().section(&identity);

        assert_eq!(
            section.body,
            "You are `/root/reviewer`, a child agent.\nComplete the task and report concisely to your parent."
        );
    }

    #[test]
    fn prompt_section_uses_configured_child_instruction() {
        let identity = AgentIdentity {
            root_session_id: "root".into(),
            agent_path: "/root/reviewer".into(),
            depth: 1,
        };
        let middleware = test_middleware()
            .prompt("Review the parser and report findings.")
            .expect("custom child prompt");

        let section = middleware.section(&identity);

        assert_eq!(
            section.body,
            "You are `/root/reviewer`, a child agent.\nReview the parser and report findings."
        );
    }

    #[test]
    fn renders_every_subagent_tool_call() {
        let middleware = Subagents::new(
            1,
            2,
            2,
            Arc::new(|_| Box::pin(async { Err(Error::Stopped("unused".into())) })),
        )
        .expect("subagents middleware");

        for name in [
            "spawn_agent",
            "send_message",
            "followup_task",
            "list_agents",
            "interrupt_agent",
            "wait_agent",
        ] {
            assert!(
                middleware
                    .render(
                        &EventMsg::ToolCallBegin(ToolCallBeginEvent {
                            turn_id: "turn".into(),
                            call_id: "call".into(),
                            name: name.into(),
                            arguments: serde_json::json!({}),
                        }),
                        "session"
                    )
                    .is_some(),
                "missing begin renderer for {name}"
            );
            assert!(
                middleware
                    .render(
                        &EventMsg::ToolCallEnd(ToolCallEndEvent {
                            turn_id: "turn".into(),
                            call_id: "call".into(),
                            name: name.into(),
                            output: String::new(),
                            is_error: false,
                        }),
                        "session"
                    )
                    .is_some(),
                "missing end renderer for {name}"
            );
        }
    }

    #[tokio::test]
    async fn active_command_emits_a_subagent_transcript_preview() {
        let workspace = tempfile::tempdir().expect("workspace");
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(
            crate::backend::checkpoint::sqlite::SqliteCheckpoint::new(
                workspace.path().join("checkpoints.sqlite3"),
            )
            .expect("checkpoint store"),
        );
        let root = Checkpoint::empty("root");
        checkpoints.save(&root, &[], None).await.expect("save root");
        let transcript = serde_json::json!({"role": "user", "content": "review this"});
        let mut child = Checkpoint::empty("child");
        child.sequence = 1;
        child.context.push(transcript.clone());
        checkpoints
            .save(&child, &[transcript], None)
            .await
            .expect("save child");
        let middleware = Subagents::new(
            1,
            2,
            2,
            Arc::new(|_| Box::pin(async { Err(Error::Stopped("unused".into())) })),
        )
        .expect("subagents middleware");
        middleware
            .shared
            .initialize(RuntimeContext {
                checkpoints,
                session_id: root.session_id.clone(),
                model_route: "test".into(),
                session_context: root.session_context.clone(),
                metadata: root.metadata.clone(),
                queued_input: crate::middleware::QueuedInputSnapshot::default(),
                frontend: Arc::new(|_| Ok(())),
            })
            .await
            .expect("initialize runtime");
        middleware
            .shared
            .reserve(
                "root",
                "/root/reviewer",
                "/root",
                "child".into(),
                1,
                AgentPresentation {
                    model: "test".into(),
                    spawn_context: String::new(),
                },
            )
            .await
            .expect("reserve child");
        let mut queued = Vec::new();
        let mut events = Vec::new();

        let result = middleware
            .active_command(&mut ActiveCommandContext {
                submission_id: "preview-1",
                session_id: "root",
                metadata: &root.metadata,
                active_turn_id: "turn-1",
                command: "subagents",
                arguments: "/root/reviewer",
                input: None,
                target: None,
                queued_input: QueuedInputQueue::new(&mut queued, QueuedInputBaseline::default()),
                events: &mut events,
            })
            .await
            .expect("active command");

        assert_eq!(result, Some(ActiveSubmissionResult::Handled));
        assert!(matches!(
            events.as_slice(),
            [EventMsg::Frontend(FrontendEvent::Preview {
                id,
                title,
                subtitle,
                page_id,
                update: FrontendPreviewUpdate::Replace,
                events,
                next: None,
            })]
                if id == "/root/reviewer"
                    && title == "reviewer"
                    && subtitle.is_empty()
                    && page_id == "/root/reviewer:latest"
                    && matches!(events.as_slice(), [EventMsg::UserMessage(message)] if message.message == "review this")
        ));
        assert!(queued.is_empty());
    }

    #[tokio::test]
    async fn preview_pages_are_bounded_omit_the_latest_seed_and_do_not_overlap() {
        let workspace = tempfile::tempdir().expect("workspace");
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(
            crate::backend::checkpoint::sqlite::SqliteCheckpoint::new(
                workspace.path().join("checkpoints.sqlite3"),
            )
            .expect("checkpoint store"),
        );
        let root = Checkpoint::empty("root");
        checkpoints.save(&root, &[], None).await.expect("save root");
        let inherited = serde_json::json!({"role": "user", "content": "inherited parent"});
        let mut child = Checkpoint::empty("child");
        child.context.push(inherited);
        child.metadata.insert(
            SPAWN_CONTEXT_KEY.into(),
            Value::String("Full context".into()),
        );
        checkpoints
            .fork("root", root.sequence, &child)
            .await
            .expect("fork child");
        for sequence in 1..=runtime::PREVIEW_TRANSCRIPT_BATCHES + 1 {
            child.sequence = u64::try_from(sequence).expect("test sequence");
            let item = serde_json::json!({
                "role": "user",
                "content": format!("child {sequence}")
            });
            child.context.push(item.clone());
            checkpoints
                .save(&child, &[item], None)
                .await
                .expect("save child batch");
        }
        let middleware = Subagents::new(
            1,
            2,
            2,
            Arc::new(|_| Box::pin(async { Err(Error::Stopped("unused".into())) })),
        )
        .expect("subagents middleware");
        middleware
            .shared
            .initialize(RuntimeContext {
                checkpoints,
                session_id: root.session_id.clone(),
                model_route: "test".into(),
                session_context: root.session_context.clone(),
                metadata: root.metadata.clone(),
                queued_input: crate::middleware::QueuedInputSnapshot::default(),
                frontend: Arc::new(|_| Ok(())),
            })
            .await
            .expect("initialize runtime");
        middleware
            .shared
            .reserve(
                "root",
                "/root/reviewer",
                "/root",
                "child".into(),
                1,
                AgentPresentation {
                    model: "test".into(),
                    spawn_context: "Full context".into(),
                },
            )
            .await
            .expect("reserve child");

        let initial = middleware
            .read_command("root", &root.metadata, "/root/reviewer")
            .await
            .expect("initial preview");
        let [
            FrontendEvent::Preview {
                id,
                title,
                subtitle,
                page_id,
                update: FrontendPreviewUpdate::Replace,
                events,
                next:
                    Some(Op::CapabilityCommand {
                        command, arguments, ..
                    }),
                ..
            },
        ] = initial.events.as_slice()
        else {
            panic!("expected a replace preview with a continuation");
        };
        let initial_messages = events
            .iter()
            .filter_map(|event| match event {
                EventMsg::UserMessage(message) => Some(message.message.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(id, "/root/reviewer");
        assert_eq!(title, "reviewer");
        assert_eq!(subtitle, "Full context");
        assert_eq!(page_id, "/root/reviewer:latest");
        assert_eq!(command, PREVIEW_PAGE_COMMAND);
        assert_eq!(initial_messages.len(), runtime::PREVIEW_TRANSCRIPT_BATCHES);
        assert_eq!(initial_messages.first(), Some(&"child 2"));
        assert_eq!(initial_messages.last(), Some(&"child 51"));
        assert!(!initial_messages.contains(&"inherited parent"));

        let older = middleware
            .read_preview_page("root", &root.metadata, arguments)
            .await
            .expect("older preview");
        let [
            FrontendEvent::Preview {
                page_id,
                update: FrontendPreviewUpdate::Prepend,
                events,
                next: None,
                ..
            },
        ] = older.events.as_slice()
        else {
            panic!("expected a terminal prepend preview");
        };
        let older_messages = events
            .iter()
            .filter_map(|event| match event {
                EventMsg::UserMessage(message) => Some(message.message.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(page_id, "/root/reviewer:before-sequence-2");
        assert_eq!(older_messages, ["inherited parent", "child 1"]);
        assert!(
            initial_messages
                .iter()
                .all(|message| !older_messages.contains(message))
        );
    }

    #[tokio::test]
    async fn inherited_seed_uses_item_cursors_and_byte_bounded_pages() {
        let workspace = tempfile::tempdir().expect("workspace");
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(
            crate::backend::checkpoint::sqlite::SqliteCheckpoint::new(
                workspace.path().join("checkpoints.sqlite3"),
            )
            .expect("checkpoint store"),
        );
        let root = Checkpoint::empty("root");
        checkpoints.save(&root, &[], None).await.expect("save root");
        let mut child = Checkpoint::empty("child");
        let body = "x".repeat(3 * 1024 * 1024);
        for marker in ['a', 'b', 'c'] {
            child.context.push(serde_json::json!({
                "role": "user",
                "content": format!("{marker}{body}")
            }));
        }
        checkpoints
            .fork("root", root.sequence, &child)
            .await
            .expect("fork child");
        let middleware = Subagents::new(
            1,
            2,
            2,
            Arc::new(|_| Box::pin(async { Err(Error::Stopped("unused".into())) })),
        )
        .expect("subagents middleware");
        middleware
            .shared
            .initialize(RuntimeContext {
                checkpoints,
                session_id: root.session_id.clone(),
                model_route: "test".into(),
                session_context: root.session_context.clone(),
                metadata: root.metadata.clone(),
                queued_input: crate::middleware::QueuedInputSnapshot::default(),
                frontend: Arc::new(|_| Ok(())),
            })
            .await
            .expect("initialize runtime");
        middleware
            .shared
            .reserve(
                "root",
                "/root/reviewer",
                "/root",
                "child".into(),
                1,
                AgentPresentation {
                    model: "test".into(),
                    spawn_context: "Full context".into(),
                },
            )
            .await
            .expect("reserve child");

        let initial = middleware
            .read_command("root", &root.metadata, "/root/reviewer")
            .await
            .expect("initial preview");
        let [
            FrontendEvent::Preview {
                events,
                next:
                    Some(Op::CapabilityCommand {
                        arguments, command, ..
                    }),
                ..
            },
        ] = initial.events.as_slice()
        else {
            panic!("expected an inherited-context continuation");
        };
        assert!(events.is_empty());
        assert_eq!(command, PREVIEW_PAGE_COMMAND);
        assert!(matches!(
            serde_json::from_str::<PreviewCursor>(arguments)
                .expect("decode initial inherited cursor")
                .position,
            PreviewPosition::BeforeSequence { before_sequence: 1 }
        ));

        let mut arguments = arguments.clone();
        let mut transcript = Vec::new();
        let mut pages = 0;
        let mut used_item_cursor = false;
        loop {
            let output = middleware
                .read_preview_page("root", &root.metadata, &arguments)
                .await
                .expect("inherited page");
            let [FrontendEvent::Preview { events, next, .. }] = output.events.as_slice() else {
                panic!("expected one inherited preview page");
            };
            assert!(
                serde_json::to_vec(events)
                    .expect("encode inherited preview page")
                    .len()
                    <= runtime::MAX_PREVIEW_PAGE_BYTES
            );
            let markers = events
                .iter()
                .filter_map(|event| match event {
                    EventMsg::UserMessage(message) => message.message.chars().next(),
                    _ => None,
                })
                .collect::<Vec<_>>();
            transcript.splice(0..0, markers);
            pages += 1;
            let Some(Op::CapabilityCommand {
                arguments: next_arguments,
                ..
            }) = next
            else {
                break;
            };
            let cursor: PreviewCursor =
                serde_json::from_str(next_arguments).expect("decode inherited item cursor");
            used_item_cursor |= matches!(
                cursor.position,
                PreviewPosition::BeforeItem {
                    sequence: 0,
                    before_item: 1
                }
            );
            arguments.clone_from(next_arguments);
        }

        assert_eq!(pages, 2);
        assert!(used_item_cursor);
        assert_eq!(transcript, ['a', 'b', 'c']);
    }

    #[tokio::test]
    async fn preview_page_keeps_a_tool_call_with_its_boundary_output() {
        let workspace = tempfile::tempdir().expect("workspace");
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(
            crate::backend::checkpoint::sqlite::SqliteCheckpoint::new(
                workspace.path().join("checkpoints.sqlite3"),
            )
            .expect("checkpoint store"),
        );
        let root = Checkpoint::empty("root");
        checkpoints.save(&root, &[], None).await.expect("save root");
        let mut child = Checkpoint::empty("child");
        checkpoints
            .save(&child, &[], None)
            .await
            .expect("save child");
        let call = serde_json::json!({
            "type": "function_call",
            "call_id": "call-1",
            "name": "inspect",
            "arguments": "{}"
        });
        child.sequence = 1;
        child.context.push(call.clone());
        checkpoints
            .save(&child, &[call], None)
            .await
            .expect("save tool call");
        let output = serde_json::json!({
            "type": "function_call_output",
            "call_id": "call-1",
            "output": "done"
        });
        child.sequence = 2;
        child.context.push(output.clone());
        checkpoints
            .save(&child, &[output], None)
            .await
            .expect("save tool output");
        for sequence in 3..=runtime::PREVIEW_TRANSCRIPT_BATCHES + 1 {
            child.sequence = u64::try_from(sequence).expect("test sequence");
            let item = serde_json::json!({
                "role": "user",
                "content": format!("child {sequence}")
            });
            child.context.push(item.clone());
            checkpoints
                .save(&child, &[item], None)
                .await
                .expect("save child batch");
        }
        let middleware = Subagents::new(
            1,
            2,
            2,
            Arc::new(|_| Box::pin(async { Err(Error::Stopped("unused".into())) })),
        )
        .expect("subagents middleware");
        middleware
            .shared
            .initialize(RuntimeContext {
                checkpoints,
                session_id: root.session_id.clone(),
                model_route: "test".into(),
                session_context: root.session_context.clone(),
                metadata: root.metadata.clone(),
                queued_input: crate::middleware::QueuedInputSnapshot::default(),
                frontend: Arc::new(|_| Ok(())),
            })
            .await
            .expect("initialize runtime");
        middleware
            .shared
            .reserve(
                "root",
                "/root/reviewer",
                "/root",
                "child".into(),
                1,
                AgentPresentation {
                    model: "test".into(),
                    spawn_context: "No context".into(),
                },
            )
            .await
            .expect("reserve child");

        let preview = middleware
            .read_command("root", &root.metadata, "/root/reviewer")
            .await
            .expect("preview");
        let [
            FrontendEvent::Preview {
                events, next: None, ..
            },
        ] = preview.events.as_slice()
        else {
            panic!("expected one terminal preview page");
        };
        let begin = events.iter().find_map(|event| match event {
            EventMsg::ToolCallBegin(event) => Some(event),
            _ => None,
        });
        let end = events.iter().find_map(|event| match event {
            EventMsg::ToolCallEnd(event) => Some(event),
            _ => None,
        });

        assert!(matches!(
            (begin, end),
            (Some(begin), Some(end))
                if begin.call_id == "call-1"
                    && end.call_id == "call-1"
                    && begin.name == "inspect"
                    && end.name == "inspect"
                    && begin.turn_id == end.turn_id
        ));
    }

    #[tokio::test]
    async fn preview_cursor_rejects_unknown_fields_and_zero_offsets() {
        let middleware = test_middleware();
        for arguments in [
            r#"{"path":"/root/reviewer","position":{"kind":"before_sequence","before_sequence":2},"extra":true}"#,
            r#"{"path":"/root/reviewer","position":{"kind":"before_sequence","before_sequence":2,"extra":true}}"#,
            r#"{"path":"/root/reviewer","position":{"kind":"before_item","sequence":0,"before_item":0}}"#,
        ] {
            let error = middleware
                .read_preview_page("root", &BTreeMap::new(), arguments)
                .await
                .err()
                .expect("invalid cursor must fail closed");

            assert!(
                matches!(error, Error::Tool(message) if message == "invalid subagent preview cursor")
            );
        }
    }

    #[tokio::test]
    async fn fork_persists_the_metadata_passed_to_the_child() {
        let workspace = tempfile::tempdir().expect("workspace");
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(
            crate::backend::checkpoint::sqlite::SqliteCheckpoint::new(
                workspace.path().join("checkpoints.sqlite3"),
            )
            .expect("checkpoint store"),
        );
        let mut parent = Checkpoint::empty("parent");
        parent.metadata.insert(
            "gateway.chat".into(),
            serde_json::json!({"workspace": "/srv/project"}),
        );
        checkpoints
            .save(&parent, &[], None)
            .await
            .expect("save parent");
        let launched = Arc::new(std::sync::Mutex::new(None));
        let launcher: SubagentLauncher = Arc::new({
            let launched = Arc::clone(&launched);
            move |launch| {
                *launched.lock().expect("launch metadata lock") = Some(launch.metadata);
                Box::pin(async { Err(Error::Stopped("test launch stopped".into())) })
            }
        });
        let runtime = RuntimeContext {
            checkpoints: Arc::clone(&checkpoints),
            session_id: parent.session_id.clone(),
            model_route: "test".into(),
            session_context: parent.session_context.clone(),
            metadata: parent.metadata.clone(),
            queued_input: crate::middleware::QueuedInputSnapshot::default(),
            frontend: Arc::new(|_| Ok(())),
        };
        let scope = AgentScope::new(&runtime, launcher).expect("agent scope");

        let result = scope
            .fork(
                "child".into(),
                "/root/child".into(),
                "test".into(),
                None,
                ForkTurns::None,
            )
            .await;
        assert!(matches!(result, Err(Error::Stopped(_))));
        let child = checkpoints
            .load("child")
            .await
            .expect("load child")
            .expect("child checkpoint");
        let launched = launched
            .lock()
            .expect("launch metadata lock")
            .clone()
            .expect("launched metadata");
        let identity = AgentIdentity::read("child", &child.metadata).expect("child identity");

        assert_eq!(child.metadata, launched);
        assert_eq!(
            child.metadata.get("gateway.chat"),
            parent.metadata.get("gateway.chat")
        );
        assert_eq!(identity.root_session_id, "parent");
        assert_eq!(identity.agent_path, "/root/child");
        assert_eq!(identity.depth, 1);
        assert_eq!(
            child.metadata.get(SPAWN_CONTEXT_KEY),
            Some(&Value::String("No context".into()))
        );
    }

    #[test]
    fn cleanup_failures_preserve_both_errors() {
        let error = cleanup_error(
            Error::Tool("launch failed".into()),
            Err(Error::Checkpoint("cleanup failed".into())),
        );

        assert!(matches!(
            error,
            Error::Rollback { primary, rollback }
                if matches!(*primary, Error::Tool(_))
                    && matches!(*rollback, Error::Checkpoint(_))
        ));
    }

    #[test]
    fn forked_context_drops_session_owned_attachment_references() {
        let context = vec![serde_json::json!({
            "role": "user",
            "content": [{"type": "input_text", "text": "inspect"}],
            "_horus_attachments": [{
                "id": "378b8581-e96c-4413-a138-93e74561cb87",
                "name": "photo.png",
                "size": 1,
                "media_type": "image/png"
            }]
        })];

        let fork = fork_context(&context, ForkTurns::All);

        assert!(fork[0].get("_horus_attachments").is_none());
    }

    #[tokio::test]
    async fn supervised_lifecycle_outlives_a_cancelled_caller() {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn(supervise(async move {
            entered_tx.send(()).expect("signal lifecycle start");
            release_rx.await.expect("release lifecycle");
            completed_tx.send(()).expect("signal lifecycle completion");
            Ok(())
        }));

        entered_rx.await.expect("lifecycle started");
        caller.abort();
        assert!(
            caller
                .await
                .expect_err("caller should be cancelled")
                .is_cancelled(),
            "caller should stop before the lifecycle is released"
        );
        release_tx.send(()).expect("release lifecycle");

        tokio::time::timeout(Duration::from_secs(1), completed_rx)
            .await
            .expect("lifecycle continued after caller cancellation")
            .expect("lifecycle completion signal");
    }
}
