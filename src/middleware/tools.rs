//! Tool registry, dispatch, and minimal filesystem tools.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use diffy::DiffOptions;
use futures_util::FutureExt;
use futures_util::future::join_all;
use serde::Deserialize;
use serde_json::Value;

use super::Middleware;
use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::backend::model::ToolCall;
use crate::backend::model::ToolDefinition;
use crate::backend::sandbox::Sandbox;
use crate::backend::sandbox::SandboxPermissions;
use crate::backend::sandbox::ToolPermissions;
use crate::preview_json;
use crate::protocol::EventMsg;
use crate::protocol::FrontendBlock;
use crate::protocol::FrontendBlockFormat;
use crate::protocol::FrontendContribution;
use crate::protocol::FrontendTone;

const MAX_TOOL_OUTPUT_BYTES: usize = 40_000;
const MAX_TOOL_UI_BYTES: usize = 512;
const MAX_TOOL_UI_LINES: usize = 5;
const MAX_MUTATION_BYTES: usize = 40_000;
const MAX_COMMAND_BYTES: usize = 8_000;

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

    /// Creates the default read, write, edit, and sandboxed-bash tool set.
    #[must_use]
    pub fn coding() -> Self {
        Self::new(vec![
            Arc::new(ReadFile),
            Arc::new(WriteFile),
            Arc::new(EditFile),
            Arc::new(Bash),
        ])
    }
}

impl Middleware for Tools {
    fn name(&self) -> &'static str {
        "tools"
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

    fn render(&self, event: &EventMsg) -> Option<FrontendBlock> {
        render_tool_event(event, |name| self.names.contains(name), tool_heading)
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
            group: tool_group(&call.name, &call.turn_id),
            append: false,
            pending: true,
            text: heading(&call.name, &call.arguments),
            format: FrontendBlockFormat::PlainText,
            tone: FrontendTone::Neutral,
        }),
        EventMsg::ToolCallEnd(result) if owns(&result.name) => {
            let is_edit_diff = !result.is_error
                && result.name == "edit_file"
                && diffy::Patch::from_str(&result.output).is_ok();
            if is_edit_diff {
                return Some(FrontendBlock {
                    id: Some(format!("{}/{}", result.turn_id, result.call_id)),
                    group: tool_group(&result.name, &result.turn_id),
                    append: false,
                    pending: false,
                    text: result.output.clone(),
                    format: FrontendBlockFormat::UnifiedDiff,
                    tone: FrontendTone::Success,
                });
            }
            let output = compact_output(&result.output);
            Some(FrontendBlock {
                id: Some(format!("{}/{}", result.turn_id, result.call_id)),
                group: tool_group(&result.name, &result.turn_id),
                append: true,
                pending: false,
                text: if output.is_empty() {
                    String::new()
                } else {
                    format!("\n  {}", output.replace('\n', "\n  "))
                },
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

fn tool_group(name: &str, turn_id: &str) -> Option<String> {
    (name == "read_file").then(|| format!("read:{turn_id}"))
}

fn tool_heading(name: &str, arguments: &Value) -> String {
    let (label, detail) = match name {
        "read_file" => ("Read", "path"),
        "write_file" => ("Write", "path"),
        "edit_file" => ("Edit", "path"),
        "bash" => ("Bash", "command"),
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
            description: "Read a UTF-8 workspace file.".into(),
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
            description: "Write a UTF-8 workspace file.".into(),
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
struct EditArgs {
    path: String,
    old_text: String,
    new_text: String,
}

struct EditFile;

impl Tool for EditFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".into(),
            description: "Replace one exact occurrence in a workspace file.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_text": {"type": "string"},
                    "new_text": {"type": "string"}
                },
                "required": ["path", "old_text", "new_text"],
                "additionalProperties": false
            }),
        }
    }

    fn approval(&self) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    fn call<'a>(&'a self, context: ToolContext, arguments: Value) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: EditArgs = serde_json::from_value(arguments)?;
            if arguments.old_text.is_empty() {
                return Err(Error::Tool("old_text cannot be empty".into()));
            }
            if arguments.old_text == arguments.new_text {
                return Err(Error::Tool("new_text must differ from old_text".into()));
            }
            if arguments
                .old_text
                .len()
                .saturating_add(arguments.new_text.len())
                > MAX_MUTATION_BYTES
            {
                return Err(Error::Tool(format!(
                    "edit exceeds {MAX_MUTATION_BYTES} bytes"
                )));
            }
            let content = context.sandbox.read(&arguments.path).await?;
            if content.match_indices(&arguments.old_text).count() != 1 {
                return Err(Error::Tool(
                    "old_text must occur exactly once in the file".into(),
                ));
            }
            let updated = content.replacen(&arguments.old_text, &arguments.new_text, 1);
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
                format!("edited {} (diff too large to display)", arguments.path)
            })
        })
    }
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
            description: "Run a command in the local sandbox under the active network policy."
                .into(),
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
            if arguments.command.len() > MAX_COMMAND_BYTES {
                return Err(Error::Tool(format!(
                    "command exceeds {MAX_COMMAND_BYTES} bytes"
                )));
            }
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

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
