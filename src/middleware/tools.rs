//! Tool registry, dispatch, and minimal filesystem tools.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use diffy::{DiffOptions, Line, Patch};
use futures_util::FutureExt;
use futures_util::future::join_all;
use serde::Deserialize;
use serde_json::Value;

use super::Middleware;
use super::manifest::MiddlewareManifest;
use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::backend::model::ToolCall;
use crate::backend::model::ToolDefinition;
use crate::backend::sandbox::BackgroundCommandPoll;
use crate::backend::sandbox::Sandbox;
use crate::backend::sandbox::SandboxPermissions;
use crate::backend::sandbox::ToolPermissions;
use crate::preview_json;
use crate::protocol::EventMsg;
use crate::protocol::FrontendBlock;
use crate::protocol::FrontendBlockFormat;
use crate::protocol::FrontendContribution;
use crate::protocol::FrontendTone;

mod text {
    include!(concat!(env!("OUT_DIR"), "/src_middleware_tools_text.rs"));
}

const MAX_TOOL_OUTPUT_BYTES: usize = 40_000;
const MAX_TOOL_UI_BYTES: usize = 512;
const MAX_TOOL_UI_LINES: usize = 5;
const MAX_MUTATION_BYTES: usize = 40_000;
const MAX_COMMAND_BYTES: usize = 8_000;
const MAX_PATCH_MATCH_WORK: usize = 32 * 1024 * 1024;

/// Configuration and presentation metadata for workspace tools.
pub const MANIFEST: MiddlewareManifest = MiddlewareManifest {
    id: "tools",
    label: text::MANIFEST_LABEL,
    description: text::MANIFEST_DESCRIPTION,
    required: false,
    default_enabled: true,
    settings: &[],
};

/// Whether a tool can overlap other calls in its model-produced batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    Parallel,
    Exclusive,
}

/// Whether a tool requires sandbox mutation approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRequirement {
    Never,
    Always,
}

/// Dependencies available only to terminal tool handlers.
pub struct ToolContext {
    pub sandbox: Arc<Sandbox>,
    pub permissions: ToolPermissions,
}

/// A named tool Adapter registered by middleware.
pub trait Tool: Send + Sync {
    /// Returns the provider-facing tool schema.
    fn definition(&self) -> ToolDefinition;

    /// Declares whether calls may overlap.
    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Exclusive
    }

    /// Declares whether this tool requires sandbox mutation approval.
    fn approval(&self) -> ApprovalRequirement {
        ApprovalRequirement::Never
    }

    /// Allows accepted active input to end a blocking wait at a model boundary.
    fn interrupt_on_active_input(&self) -> bool {
        false
    }

    /// Executes one validated provider call.
    fn call<'a>(&'a self, context: ToolContext, arguments: Value) -> BoxFuture<'a, Result<String>>;
}

#[derive(Clone)]
struct RegisteredTool {
    definition: ToolDefinition,
    execution_mode: ExecutionMode,
    approval: ApprovalRequirement,
    interrupt_on_active_input: bool,
    handler: Arc<dyn Tool>,
}

/// The validated tool registry built during agent creation.
#[derive(Clone, Default)]
pub struct Catalog {
    tools: BTreeMap<String, RegisteredTool>,
    definitions: Arc<[ToolDefinition]>,
}

impl Catalog {
    /// Registers one tool and rejects duplicate names.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<()> {
        let definition = tool.definition();
        let name = definition.name.clone();
        let entry = RegisteredTool {
            definition,
            execution_mode: tool.execution_mode(),
            approval: tool.approval(),
            interrupt_on_active_input: tool.interrupt_on_active_input(),
            handler: tool,
        };
        if self.tools.contains_key(&name) {
            return Err(Error::Duplicate(format!("tool `{name}`")));
        }
        self.tools.insert(name, entry);
        self.definitions = self
            .tools
            .values()
            .map(|tool| tool.definition.clone())
            .collect::<Vec<_>>()
            .into();
        Ok(())
    }

    /// Returns model-facing definitions in stable name order.
    #[must_use]
    pub fn definitions(&self) -> Arc<[ToolDefinition]> {
        Arc::clone(&self.definitions)
    }

    /// Returns whether the named tool requires approval.
    #[must_use]
    pub fn requires_approval(&self, name: &str) -> bool {
        self.tools
            .get(name)
            .is_some_and(|tool| tool.approval == ApprovalRequirement::Always)
    }

    pub(crate) fn interrupts_on_active_input(&self, calls: &[ToolCall]) -> bool {
        !calls.is_empty()
            && calls.iter().all(|call| {
                self.tools
                    .get(&call.name)
                    .is_some_and(|tool| tool.interrupt_on_active_input)
            })
    }

    fn get(&self, name: &str) -> Option<&RegisteredTool> {
        self.tools.get(name)
    }
}

/// The result returned to the model for one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub output: String,
    pub is_error: bool,
}

/// Executes a batch concurrently only when every registered tool permits it.
pub(crate) async fn execute_batch(
    catalog: &Catalog,
    calls: &[ToolCall],
    sandbox: Arc<Sandbox>,
    permissions: &SandboxPermissions,
) -> Vec<ToolResult> {
    let parallel = calls.iter().all(|call| {
        catalog
            .get(&call.name)
            .is_some_and(|tool| tool.execution_mode == ExecutionMode::Parallel)
    });
    if !parallel {
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            results.push(
                execute_one(
                    catalog,
                    call.clone(),
                    ToolContext {
                        sandbox: Arc::clone(&sandbox),
                        permissions: permissions.for_call(&call.call_id),
                    },
                )
                .await,
            );
        }
        return results;
    }

    // ModelOutput validation bounds every batch to 128 calls.
    join_all(calls.iter().cloned().map(|call| {
        let context = ToolContext {
            sandbox: Arc::clone(&sandbox),
            permissions: permissions.for_call(&call.call_id),
        };
        execute_one(catalog, call, context)
    }))
    .await
}

async fn execute_one(catalog: &Catalog, call: ToolCall, context: ToolContext) -> ToolResult {
    let tool = catalog.get(&call.name).cloned();
    let ToolCall {
        call_id,
        name,
        arguments,
    } = call;
    let Some(tool) = tool else {
        return ToolResult {
            call_id,
            output: capped(&format!("unknown tool `{name}`"), MAX_TOOL_OUTPUT_BYTES),
            name,
            is_error: true,
        };
    };
    if tool.approval == ApprovalRequirement::Always && !context.permissions.allows_mutation() {
        return ToolResult {
            call_id,
            name,
            output: "tool call is not authorized to mutate state".into(),
            is_error: true,
        };
    }
    let result = AssertUnwindSafe(async move { tool.handler.call(context, arguments).await })
        .catch_unwind()
        .await;
    match result {
        Ok(Ok(output)) => ToolResult {
            call_id,
            name,
            output: capped(&output, MAX_TOOL_OUTPUT_BYTES),
            is_error: false,
        },
        Ok(Err(error)) => ToolResult {
            call_id,
            name,
            output: capped(&error.to_string(), MAX_TOOL_OUTPUT_BYTES),
            is_error: true,
        },
        Err(_) => ToolResult {
            call_id,
            name,
            output: "tool panicked".into(),
            is_error: true,
        },
    }
}

fn capped(output: &str, limit: usize) -> String {
    if output.len() <= limit {
        return output.to_string();
    }

    let left_budget = limit / 2;
    let right_budget = limit - left_budget;
    let left = crate::truncate_utf8(output, left_budget);
    let mut right_start = output.len() - right_budget;
    while !output.is_char_boundary(right_start) {
        right_start += 1;
    }
    let removed = output[left.len()..right_start].chars().count();
    format!(
        "{}…{removed} chars truncated…{}",
        left,
        &output[right_start..]
    )
}

fn compact_output(output: &str) -> String {
    let total_lines = output.lines().count();
    if output.len() <= MAX_TOOL_UI_BYTES && total_lines <= MAX_TOOL_UI_LINES {
        return output.to_string();
    }

    let kept_lines = if total_lines > MAX_TOOL_UI_LINES {
        MAX_TOOL_UI_LINES - 1
    } else {
        total_lines
    };
    let line_budget = MAX_TOOL_UI_BYTES / kept_lines.max(1);
    let mut preview = String::new();
    let mut first = true;
    let mut append = |line: &str| {
        if !first {
            preview.push('\n');
        }
        first = false;
        preview.push_str(&capped(line, line_budget));
    };

    if total_lines <= MAX_TOOL_UI_LINES {
        output.lines().for_each(&mut append);
        return preview;
    }

    let head_lines = (MAX_TOOL_UI_LINES - 1) / 2;
    output.lines().take(head_lines).for_each(&mut append);
    append(&format!(
        "… +{} lines",
        total_lines - (MAX_TOOL_UI_LINES - 1)
    ));
    let mut tail = output
        .lines()
        .rev()
        .take(MAX_TOOL_UI_LINES - 1 - head_lines)
        .collect::<Vec<_>>();
    tail.reverse();
    tail.into_iter().for_each(append);
    preview
}

/// Middleware that contributes an explicit list of tools.
pub struct Tools {
    tools: Vec<Arc<dyn Tool>>,
    names: BTreeSet<String>,
}

impl Tools {
    /// Creates a tool middleware from explicit handlers.
    #[must_use]
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Self {
        let names = tools.iter().map(|tool| tool.definition().name).collect();
        Self { tools, names }
    }

    /// Creates the default file, foreground command, and background command tools.
    #[must_use]
    pub fn coding() -> Self {
        Self::new(vec![
            Arc::new(ReadFile),
            Arc::new(WriteFile),
            Arc::new(ApplyPatch),
            Arc::new(Bash),
            Arc::new(StartCommand),
            Arc::new(PollCommand),
            Arc::new(StopCommand),
        ])
    }
}

impl Middleware for Tools {
    fn name(&self) -> &'static str {
        MANIFEST.id
    }

    fn register(&self, catalog: &mut Catalog, _runtime: &super::RuntimeContext) -> Result<()> {
        for tool in &self.tools {
            catalog.register(Arc::clone(tool))?;
        }
        Ok(())
    }

    fn frontend(&self) -> FrontendContribution {
        FrontendContribution {
            capability: self.name().into(),
            ..FrontendContribution::default()
        }
    }

    fn render(&self, event: &EventMsg, _session_id: &str) -> Option<FrontendBlock> {
        let mut block = render_tool_event(event, |name| self.names.contains(name), tool_heading)?;
        match event {
            EventMsg::ToolCallBegin(call) if call.name == "read_file" => {
                block.group = Some(format!("read:{}", call.turn_id));
            }
            EventMsg::ToolCallEnd(result) if result.name == "read_file" => {
                block.group = Some(format!("read:{}", result.turn_id));
            }
            EventMsg::ToolCallEnd(result)
                if !result.is_error
                    && result.name == "apply_patch"
                    && Patch::from_str(&result.output).is_ok() =>
            {
                block.append = false;
                block.text = result.output.clone();
                block.format = FrontendBlockFormat::UnifiedDiff;
            }
            _ => {}
        }
        Some(block)
    }
}

pub(crate) fn render_tool_event(
    event: &EventMsg,
    owns: impl Fn(&str) -> bool,
    heading: impl Fn(&str, &Value) -> String,
) -> Option<FrontendBlock> {
    match event {
        EventMsg::ToolCallBegin(call) if owns(&call.name) => Some(FrontendBlock {
            id: Some(format!("{}/{}", call.turn_id, call.call_id)),
            group: None,
            append: false,
            pending: true,
            text: heading(&call.name, &call.arguments),
            files: Vec::new(),
            format: FrontendBlockFormat::PlainText,
            tone: FrontendTone::Neutral,
        }),
        EventMsg::ToolCallEnd(result) if owns(&result.name) => {
            let output = compact_output(&result.output);
            Some(FrontendBlock {
                id: Some(format!("{}/{}", result.turn_id, result.call_id)),
                group: None,
                append: true,
                pending: false,
                text: if output.is_empty() {
                    String::new()
                } else {
                    format!("\n  {}", output.replace('\n', "\n  "))
                },
                files: Vec::new(),
                format: FrontendBlockFormat::PlainText,
                tone: if result.is_error {
                    FrontendTone::Error
                } else {
                    FrontendTone::Success
                },
            })
        }
        _ => None,
    }
}

fn tool_heading(name: &str, arguments: &Value) -> String {
    let (label, detail) = match name {
        "read_file" => (text::RENDER_READ_FILE, "path"),
        "write_file" => (text::RENDER_WRITE_FILE, "path"),
        "apply_patch" => (text::RENDER_APPLY_PATCH, "path"),
        "bash" => (text::RENDER_BASH, "command"),
        "start_command" => (text::RENDER_START_COMMAND, "command"),
        "poll_command" => (text::RENDER_POLL_COMMAND, "command_id"),
        "stop_command" => (text::RENDER_STOP_COMMAND, "command_id"),
        _ => return format!("◉ {name} {}", preview_json(arguments)),
    };
    labeled_tool_heading(label, detail, arguments)
}

pub(crate) fn labeled_tool_heading(label: &str, detail: &str, arguments: &Value) -> String {
    arguments
        .get(detail)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || format!("◉ {label}"),
            |value| format!("◉ {label} {value}"),
        )
}

#[derive(Deserialize)]
struct PathArgs {
    path: String,
}

struct ReadFile;

impl Tool for ReadFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: text::TOOL_READ_FILE_DESCRIPTION.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }),
        }
    }

    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }

    fn call<'a>(&'a self, context: ToolContext, arguments: Value) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: PathArgs = serde_json::from_value(arguments)?;
            context.sandbox.read(&arguments.path).await
        })
    }
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

struct WriteFile;

impl Tool for WriteFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".into(),
            description: text::TOOL_WRITE_FILE_DESCRIPTION.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        }
    }

    fn approval(&self) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    fn call<'a>(&'a self, context: ToolContext, arguments: Value) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: WriteArgs = serde_json::from_value(arguments)?;
            if arguments.content.len() > MAX_MUTATION_BYTES {
                return Err(Error::Tool(format!(
                    "content exceeds {MAX_MUTATION_BYTES} bytes"
                )));
            }
            context
                .sandbox
                .write(&arguments.path, &arguments.content, &context.permissions)
                .await?;
            Ok(format!(
                "wrote {} bytes to {}",
                arguments.content.len(),
                arguments.path
            ))
        })
    }
}

#[derive(Deserialize)]
struct ApplyPatchArgs {
    path: String,
    patch: String,
}

struct ApplyPatch;

impl Tool for ApplyPatch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "apply_patch".into(),
            description: text::TOOL_APPLY_PATCH_DESCRIPTION.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "patch": {"type": "string"}
                },
                "required": ["path", "patch"],
                "additionalProperties": false
            }),
        }
    }

    fn approval(&self) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    fn call<'a>(&'a self, context: ToolContext, arguments: Value) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: ApplyPatchArgs = serde_json::from_value(arguments)?;
            if arguments.patch.len() > MAX_MUTATION_BYTES {
                return Err(Error::Tool(format!(
                    "patch exceeds {MAX_MUTATION_BYTES} bytes"
                )));
            }
            let content = context.sandbox.read(&arguments.path).await?;
            let patch = diffy::Patch::from_str(&arguments.patch).map_err(|error| {
                malformed_patch_error(&content, &arguments.patch, &error.to_string())
            })?;
            if patch.hunks().is_empty() {
                return Err(malformed_patch_error(
                    &content,
                    &arguments.patch,
                    "no hunk headers were found",
                ));
            }
            validate_patch_complexity(&content, &patch)?;
            let updated = diffy::apply(&content, &patch)
                .map_err(|error| unmatched_patch_error(&content, &patch, &error))?;
            if updated == content {
                return Err(Error::Tool(
                    "Patch rejected: patch applies but makes no changes.".into(),
                ));
            }
            let mut options = DiffOptions::new();
            options
                .set_original_filename(arguments.path.clone())
                .set_modified_filename(arguments.path.clone());
            let diff = options.create_patch(&content, &updated).to_string();
            context
                .sandbox
                .write(&arguments.path, &updated, &context.permissions)
                .await?;
            Ok(if diff.len() <= MAX_TOOL_OUTPUT_BYTES {
                diff
            } else {
                format!("patched {} (diff too large to display)", arguments.path)
            })
        })
    }
}

fn malformed_patch_error(content: &str, input: &str, reason: &str) -> Error {
    let received = input
        .lines()
        .find(|line| line.trim_start().starts_with("@@"))
        .unwrap_or("<missing>");
    let received = capped(received, MAX_TOOL_UI_BYTES);
    Error::Tool(format!(
        "Patch rejected: malformed unified diff.\n\
         Reason: {reason}.\n\
         Expected hunk header format: @@ -start[,count] +start[,count] @@\n\
         Received: {}\n\
         Actual file has {} lines.",
        received.escape_debug(),
        content.lines().count()
    ))
}

fn unmatched_patch_error(
    content: &str,
    patch: &Patch<'_, str>,
    error: &diffy::ApplyError,
) -> Error {
    let message = error.to_string();
    let Some(hunk_number) = message
        .strip_prefix("error applying hunk #")
        .and_then(|number| number.parse::<usize>().ok())
        .filter(|number| *number > 0 && *number <= patch.hunks().len())
    else {
        return Error::Tool(format!(
            "Patch rejected: a hunk did not match the file.\nReason: {message}."
        ));
    };
    let rejection = if patch.hunks().len() == 1 {
        "Patch rejected: no hunks matched the file.".into()
    } else {
        format!("Patch rejected: hunk #{hunk_number} did not match the file.")
    };
    let Some(hunk) = patch.hunks().get(hunk_number - 1) else {
        return Error::Tool(format!(
            "Patch rejected: a hunk did not match the file.\nReason: {message}."
        ));
    };
    let Some((heading, context)) = hunk.lines().iter().find_map(|line| match line {
        Line::Context(value) if !value.trim().is_empty() => {
            Some(("Failed hunk starts with context:", *value))
        }
        Line::Delete(value) if !value.trim().is_empty() => {
            Some(("Failed hunk starts with deletion:", *value))
        }
        Line::Insert(_) => None,
        Line::Context(_) | Line::Delete(_) => None,
    }) else {
        return Error::Tool(format!(
            "{rejection}\nThe failed hunk has no usable context lines."
        ));
    };
    let nearest = content
        .split_inclusive('\n')
        .enumerate()
        .filter(|(_, line)| *line == context)
        .map(|(index, _)| index + 1)
        .min_by_key(|line| line.abs_diff(hunk.new_range().start()));
    let location = nearest.map_or_else(
        || "No matching context line was found.".into(),
        |line| format!("The nearest match is at line {line}."),
    );
    let context = capped(context.trim_end_matches(['\r', '\n']), MAX_TOOL_UI_BYTES);
    Error::Tool(format!("{rejection}\n{heading}\n{context:?}\n{location}"))
}

fn validate_patch_complexity(content: &str, patch: &Patch<'_, str>) -> Result<()> {
    let image_lines = content.lines().count().saturating_add(
        patch
            .hunks()
            .iter()
            .map(|hunk| hunk.new_range().len())
            .sum::<usize>(),
    );
    let work = patch.hunks().iter().fold(0_usize, |total, hunk| {
        let mut preimage_lines = 0_usize;
        let mut preimage_bytes = 0_usize;
        for line in hunk.lines() {
            if let Line::Context(value) | Line::Delete(value) = line {
                preimage_lines = preimage_lines.saturating_add(1);
                preimage_bytes = preimage_bytes.saturating_add(value.len());
            }
        }
        let hunk_work = if preimage_lines == 0 {
            hunk.lines().len()
        } else {
            image_lines.saturating_mul(preimage_bytes.saturating_add(hunk.lines().len()))
        };
        total.saturating_add(hunk_work)
    });
    if work > MAX_PATCH_MATCH_WORK {
        return Err(Error::Tool("patch is too expensive to match safely".into()));
    }
    Ok(())
}

#[derive(Deserialize)]
struct BashArgs {
    command: String,
}

struct Bash;

impl Tool for Bash {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".into(),
            description: text::TOOL_BASH_DESCRIPTION.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
                "additionalProperties": false
            }),
        }
    }

    fn approval(&self) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    fn call<'a>(&'a self, context: ToolContext, arguments: Value) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: BashArgs = serde_json::from_value(arguments)?;
            validate_command(&arguments.command)?;
            let output = context
                .sandbox
                .execute(&arguments.command, &context.permissions)
                .await?;
            Ok(format!(
                "exit code: {}\nstdout:\n{}\nstderr:\n{}",
                output.exit_code, output.stdout, output.stderr
            ))
        })
    }
}

struct StartCommand;

impl Tool for StartCommand {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "start_command".into(),
            description: text::TOOL_START_COMMAND_DESCRIPTION.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
                "additionalProperties": false
            }),
        }
    }

    fn approval(&self) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    fn call<'a>(&'a self, context: ToolContext, arguments: Value) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: BashArgs = serde_json::from_value(arguments)?;
            validate_command(&arguments.command)?;
            let id = context
                .sandbox
                .start_background(arguments.command, &context.permissions)?;
            Ok(serde_json::json!({"command_id": id, "status": "running"}).to_string())
        })
    }
}

#[derive(Deserialize)]
struct CommandIdArgs {
    command_id: String,
}

struct PollCommand;

impl Tool for PollCommand {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "poll_command".into(),
            description: text::TOOL_POLL_COMMAND_DESCRIPTION.into(),
            parameters: command_id_schema(),
        }
    }

    fn call<'a>(&'a self, context: ToolContext, arguments: Value) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: CommandIdArgs = serde_json::from_value(arguments)?;
            validate_command_id(&arguments.command_id)?;
            let output = context
                .sandbox
                .poll_background(&arguments.command_id, &context.permissions)
                .await?;
            Ok(background_output(output))
        })
    }
}

struct StopCommand;

impl Tool for StopCommand {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "stop_command".into(),
            description: text::TOOL_STOP_COMMAND_DESCRIPTION.into(),
            parameters: command_id_schema(),
        }
    }

    fn call<'a>(&'a self, context: ToolContext, arguments: Value) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: CommandIdArgs = serde_json::from_value(arguments)?;
            validate_command_id(&arguments.command_id)?;
            let output = context
                .sandbox
                .stop_background(&arguments.command_id, &context.permissions)
                .await?;
            Ok(background_output(output))
        })
    }
}

fn validate_command(command: &str) -> Result<()> {
    if command.trim().is_empty() {
        return Err(Error::Tool("command cannot be empty".into()));
    }
    if command.len() > MAX_COMMAND_BYTES {
        return Err(Error::Tool(format!(
            "command exceeds {MAX_COMMAND_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_command_id(id: &str) -> Result<()> {
    uuid::Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| Error::Tool("command_id must be a UUID".into()))
}

fn command_id_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {"command_id": {"type": "string", "format": "uuid"}},
        "required": ["command_id"],
        "additionalProperties": false
    })
}

fn background_output(output: BackgroundCommandPoll) -> String {
    let status = output.status.as_str();
    let exit_code = output.exit_code;
    let rendered = serde_json::json!({
        "status": status,
        "exit_code": exit_code,
        "stdout": output.stdout,
        "stderr": output.stderr,
        "truncated": output.truncated,
        "error": output.error
    })
    .to_string();
    if rendered.len() <= MAX_TOOL_OUTPUT_BYTES {
        return rendered;
    }
    serde_json::json!({
        "status": status,
        "exit_code": exit_code,
        "stdout": "",
        "stderr": "",
        "truncated": true,
        "error": "background output exceeded its serialized limit"
    })
    .to_string()
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
