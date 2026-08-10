//! Model provider interface and routing.

use std::collections::BTreeSet;
use std::io;
use std::io::Write;
use std::sync::Arc;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::protocol::TokenUsage;
use crate::protocol::{ModelStepAnnotation, ModelStepContent, ModelStepContentPhase};

pub mod anthropic;
pub mod deepseek;
pub mod kimi;
pub mod openai;
mod openai_auth;
pub mod openai_codex;
pub mod openai_socket;
pub mod openrouter;
pub mod provider;
mod transport;

use crate::protocol::{ATTACHMENTS_FIELD, INTERNAL_MESSAGE_FIELD, SessionFileReference};
pub(crate) use crate::protocol::{REPLAY_REASONING_FIELD, TOOL_ERROR_FIELD};
// Leaves room for typed lifecycle metadata inside the shipped 20 MiB frontend envelope.
const MAX_MODEL_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOOL_CALLS: usize = 128;
const MAX_TOOL_ARGUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOOL_CALL_ID_BYTES: usize = 4 * 1024;
const MAX_TOOL_NAME_BYTES: usize = 256;

/// Returns one-based context boundaries with no unfinished tool calls.
pub(crate) fn tool_complete_boundaries<'a>(
    input: impl IntoIterator<Item = &'a Value>,
) -> Vec<usize> {
    let mut open_calls = BTreeSet::new();
    let mut complete = Vec::new();
    for (index, item) in input.into_iter().enumerate() {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|call_id| !call_id.is_empty())
                    .map_or_else(|| format!("missing-{index}"), str::to_string);
                open_calls.insert(call_id);
            }
            Some("function_call_output") => {
                if let Some(call_id) = item.get("call_id").and_then(Value::as_str) {
                    open_calls.remove(call_id);
                }
            }
            Some(_) | None => {}
        }
        if open_calls.is_empty() {
            complete.push(index + 1);
        }
    }
    complete
}

/// A function tool definition sent to a model provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// One model-requested function call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
}

/// Input for one model turn.
#[derive(Debug)]
pub struct ModelRequest<'a> {
    pub session_id: &'a str,
    pub instructions: &'a str,
    pub input: &'a [Value],
    pub tools: &'a [ToolDefinition],
    /// Whether provider-hosted tools such as web search may be attached.
    pub allow_hosted_tools: bool,
    /// Whether a transport may continue a previous response for this session.
    pub allow_continuation: bool,
}

/// Durable conversation history for a provider's native compaction endpoint.
///
/// Current system instructions and tool definitions are intentionally excluded.
/// The agent reapplies their current values to each subsequent [`ModelRequest`].
#[derive(Debug)]
pub struct CompactRequest<'a> {
    /// Conversation items to replace with the returned [`CompactOutput`].
    pub input: &'a [Value],
}

/// Fallible synchronous callback used to forward streaming provider events.
pub type ModelEventSink = Arc<dyn Fn(crate::protocol::ModelEvent) -> Result<()> + Send + Sync>;

/// Completed output from a model response.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ModelOutput {
    pub(crate) output: Vec<Value>,
    pub(crate) text: String,
    pub(crate) content: Vec<ModelStepContent>,
    pub(crate) tool_calls: Vec<ToolCall>,
    pub(crate) end_turn: bool,
    pub(crate) usage: TokenUsage,
}

impl ModelOutput {
    /// Validates normalized output and derives its visible text and tool calls.
    pub fn from_output(output: Vec<Value>, end_turn: bool, usage: TokenUsage) -> Result<Self> {
        let content = normalized_step_content(&output)?;
        Self::from_output_with_content(output, end_turn, usage, content)
    }

    pub(super) fn from_output_with_content(
        output: Vec<Value>,
        end_turn: bool,
        usage: TokenUsage,
        content: Vec<ModelStepContent>,
    ) -> Result<Self> {
        ensure_output_size(&output)?;
        validate_usage(&usage)?;
        if output.is_empty() {
            return Err(Error::Provider("model returned no output".into()));
        }

        let text = content
            .iter()
            .filter(|content| content.phase == ModelStepContentPhase::FinalAnswer)
            .map(|content| content.text.as_str())
            .collect();

        let mut call_ids = BTreeSet::new();
        let mut tool_calls = Vec::new();
        for item in output
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        {
            if tool_calls.len() >= MAX_TOOL_CALLS {
                return Err(Error::Provider(
                    format!("model returned more than {MAX_TOOL_CALLS} tool calls").into(),
                ));
            }
            tool_calls.push(decode_tool_call(item, &mut call_ids)?);
        }

        Ok(Self {
            output,
            text,
            content,
            tool_calls,
            end_turn,
            usage,
        })
    }

    /// Returns the provider-neutral output items.
    #[must_use]
    pub fn output(&self) -> &[Value] {
        &self.output
    }

    /// Returns the visible assistant text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the validated tool calls.
    #[must_use]
    pub fn tool_calls(&self) -> &[ToolCall] {
        &self.tool_calls
    }

    /// Reports whether the provider ended the turn.
    #[must_use]
    pub fn end_turn(&self) -> bool {
        self.end_turn
    }

    /// Returns the validated token usage.
    #[must_use]
    pub fn usage(&self) -> &TokenUsage {
        &self.usage
    }

    /// Returns complete, provider-neutral text normalized at the model boundary.
    #[must_use]
    pub fn content(&self) -> &[ModelStepContent] {
        &self.content
    }
}

fn normalized_step_content(output: &[Value]) -> Result<Vec<ModelStepContent>> {
    let mut content = Vec::new();
    for (output_index, item) in output.iter().enumerate() {
        normalize_reasoning_content(output_index, item, &mut content);
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let phase = if item.get("phase").and_then(Value::as_str) == Some("commentary") {
            ModelStepContentPhase::Commentary
        } else {
            ModelStepContentPhase::FinalAnswer
        };
        for (part_index, part) in item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            if part.get("type").and_then(Value::as_str) != Some("output_text") {
                continue;
            }
            let text = part.get("text").and_then(Value::as_str).ok_or_else(|| {
                Error::Provider("output text part omitted text".to_string().into())
            })?;
            if text.is_empty() {
                continue;
            }
            content.push(ModelStepContent {
                output_index,
                part_index,
                phase,
                text: text.into(),
                annotations: normalize_output_text_annotations(part)?,
            });
        }
    }
    Ok(content)
}

fn normalize_reasoning_content(
    output_index: usize,
    item: &Value,
    content: &mut Vec<ModelStepContent>,
) {
    let parts = if item.get("type").and_then(Value::as_str) == Some("reasoning") {
        ["summary", "content"]
            .into_iter()
            .filter_map(|field| item.get(field).and_then(Value::as_array))
            .find(|parts| {
                parts.iter().any(|part| {
                    part.get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty())
                })
            })
    } else {
        None
    };
    if let Some(parts) = parts {
        content.extend(parts.iter().enumerate().filter_map(|(part_index, part)| {
            part.get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(|text| ModelStepContent {
                    output_index,
                    part_index,
                    phase: ModelStepContentPhase::Reasoning,
                    text: text.into(),
                    annotations: Vec::new(),
                })
        }));
        return;
    }
    if let Some(text) = item
        .get(REPLAY_REASONING_FIELD)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        content.push(ModelStepContent {
            output_index,
            part_index: 0,
            phase: ModelStepContentPhase::Reasoning,
            text: text.into(),
            annotations: Vec::new(),
        });
    }
}

fn normalize_output_text_annotations(part: &Value) -> Result<Vec<ModelStepAnnotation>> {
    let Some(annotations) = part.get("annotations") else {
        return Ok(Vec::new());
    };
    if annotations.is_null() {
        return Ok(Vec::new());
    }
    let annotations: Vec<OutputTextAnnotation> = serde_json::from_value(annotations.clone())
        .map_err(|error| {
            Error::Provider(format!("invalid output text annotation: {error}").into())
        })?;
    Ok(annotations.into_iter().map(Into::into).collect())
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum OutputTextAnnotation {
    UrlCitation {
        url: String,
        title: String,
        start_index: usize,
        end_index: usize,
    },
    FileCitation {
        file_id: String,
        filename: String,
        index: usize,
    },
    ContainerFileCitation {
        container_id: String,
        file_id: String,
        filename: String,
        start_index: usize,
        end_index: usize,
    },
    FilePath {
        file_id: String,
        index: usize,
    },
}

impl From<OutputTextAnnotation> for ModelStepAnnotation {
    fn from(annotation: OutputTextAnnotation) -> Self {
        match annotation {
            OutputTextAnnotation::UrlCitation {
                url,
                title,
                start_index,
                end_index,
            } => Self::UrlCitation {
                url,
                title,
                start_index,
                end_index,
            },
            OutputTextAnnotation::FileCitation {
                file_id,
                filename,
                index,
            } => Self::FileCitation {
                file_id,
                filename,
                index,
            },
            OutputTextAnnotation::ContainerFileCitation {
                container_id,
                file_id,
                filename,
                start_index,
                end_index,
            } => Self::ContainerFileCitation {
                container_id,
                file_id,
                filename,
                start_index,
                end_index,
            },
            OutputTextAnnotation::FilePath { file_id, index } => Self::FilePath { file_id, index },
        }
    }
}

/// Durable replacement history returned by server-side compaction.
///
/// This output must contain conversation items only, without copying the active
/// system instructions or tool catalog into history. The agent reapplies that
/// runtime configuration separately after compaction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CompactOutput {
    pub(crate) output: Vec<Value>,
    pub(crate) usage: TokenUsage,
}

impl CompactOutput {
    /// Validates one provider-native compacted context.
    pub fn from_output(output: Vec<Value>, usage: TokenUsage) -> Result<Self> {
        ensure_output_size(&output)?;
        validate_usage(&usage)?;
        if output.is_empty() {
            return Err(Error::Provider(
                "compaction returned an empty context".into(),
            ));
        }
        Ok(Self { output, usage })
    }

    /// Returns the compacted provider-neutral context.
    #[must_use]
    pub fn output(&self) -> &[Value] {
        &self.output
    }

    /// Returns the validated token usage.
    #[must_use]
    pub fn usage(&self) -> &TokenUsage {
        &self.usage
    }
}

/// Human-readable settings exposed to frontends.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub model: String,
    pub reasoning_effort: Option<String>,
}

/// One selectable runtime model route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    pub route: String,
    pub group: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub context_window: Option<i64>,
    pub supports_image_input: bool,
}

/// A model provider Adapter used by the agent loop.
pub trait Model: Send + Sync {
    /// Returns stable display metadata without exposing credentials.
    fn info(&self) -> ModelInfo {
        ModelInfo::default()
    }

    /// Reports whether this provider accepts native image input.
    fn supports_image_input(&self) -> bool {
        false
    }

    /// Produces one streamed response.
    fn respond<'a>(
        &'a self,
        request: ModelRequest<'a>,
        events: ModelEventSink,
    ) -> BoxFuture<'a, Result<ModelOutput>>;

    /// Reports whether this provider exposes a native compaction endpoint.
    fn compaction_endpoint(&self) -> bool {
        false
    }

    /// Calls the native history-compaction endpoint when advertised.
    ///
    /// Implementations must preserve the history-only contract of
    /// [`CompactRequest`] and [`CompactOutput`]; runtime instructions and tools
    /// are reapplied by the agent after compaction.
    fn compact<'a>(&'a self, _request: CompactRequest<'a>) -> BoxFuture<'a, Result<CompactOutput>> {
        Box::pin(async {
            Err(Error::Provider(
                "model provider has no compaction endpoint".into(),
            ))
        })
    }
}

/// Selects a model Adapter by a stable provider ID.
pub struct ModelRouter {
    default: String,
    routes: Vec<ModelRoute>,
}

struct ModelRoute {
    choice: ModelChoice,
    provider: Arc<dyn Model>,
}

impl ModelRouter {
    /// Creates a router with its first provider.
    pub fn new(id: impl Into<String>, provider: Arc<dyn Model>) -> Self {
        let id = id.into();
        let choice = inferred_choice(&id, provider.as_ref());
        Self {
            default: id,
            routes: vec![ModelRoute { choice, provider }],
        }
    }

    /// Registers another provider.
    pub fn register(&mut self, id: impl Into<String>, provider: Arc<dyn Model>) -> Result<()> {
        let id = id.into();
        if self.routes.iter().any(|route| route.choice.route == id) {
            return Err(Error::Duplicate(format!("model provider `{id}`")));
        }
        self.routes.push(ModelRoute {
            choice: inferred_choice(&id, provider.as_ref()),
            provider,
        });
        Ok(())
    }

    /// Returns the selectable routes in frontend display order.
    #[must_use]
    pub fn choices(
        &self,
    ) -> impl DoubleEndedIterator<Item = &ModelChoice> + ExactSizeIterator + Clone {
        self.routes.iter().map(|route| &route.choice)
    }

    /// Resolves one route and optional reasoning effort through the model catalog.
    pub fn resolve_choice(
        &self,
        route: &str,
        reasoning_effort: Option<&str>,
    ) -> Result<&ModelChoice> {
        let choice = self
            .choices()
            .find(|choice| choice.route == route)
            .ok_or_else(|| Error::Unknown(format!("model route `{route}`")))?;
        let Some(reasoning_effort) = reasoning_effort else {
            return Ok(choice);
        };
        self.choices()
            .find(|candidate| {
                candidate.group == choice.group
                    && candidate.reasoning_effort.as_deref() == Some(reasoning_effort)
            })
            .ok_or_else(|| {
                Error::Unknown(format!(
                    "reasoning effort `{reasoning_effort}` for model route `{route}`"
                ))
            })
    }

    /// Replaces display metadata for one registered route.
    pub fn configure_choice(&mut self, mut choice: ModelChoice) -> Result<()> {
        if choice.group.trim().is_empty() || choice.model.trim().is_empty() {
            return Err(Error::Config(
                "model choice group and model cannot be empty".into(),
            ));
        }
        if choice.context_window.is_some_and(|window| window <= 0) {
            return Err(Error::Config(
                "model choice context window must be positive".into(),
            ));
        }
        let current = self
            .routes
            .iter_mut()
            .find(|current| current.choice.route == choice.route)
            .ok_or_else(|| Error::Unknown(format!("model route `{}`", choice.route)))?;
        choice.supports_image_input = current.provider.supports_image_input();
        current.choice = choice;
        Ok(())
    }

    /// Returns the default provider ID.
    #[must_use]
    pub fn default_provider(&self) -> &str {
        &self.default
    }

    /// Streams one response through the selected provider.
    pub async fn respond(
        &self,
        provider: &str,
        request: ModelRequest<'_>,
        events: ModelEventSink,
    ) -> Result<ModelOutput> {
        self.provider(provider)?.respond(request, events).await
    }

    /// Reports whether one route has a native compaction endpoint.
    pub fn compaction_endpoint(&self, provider: &str) -> Result<bool> {
        Ok(self.provider(provider)?.compaction_endpoint())
    }

    /// Reports whether one route accepts native image input.
    pub fn supports_image_input(&self, provider: &str) -> Result<bool> {
        Ok(self.provider(provider)?.supports_image_input())
    }

    /// Compacts context through the selected provider.
    pub async fn compact(
        &self,
        provider: &str,
        request: CompactRequest<'_>,
    ) -> Result<CompactOutput> {
        self.provider(provider)?.compact(request).await
    }

    fn provider(&self, id: &str) -> Result<&dyn Model> {
        self.routes
            .iter()
            .find(|route| route.choice.route == id)
            .map(|route| route.provider.as_ref())
            .ok_or_else(|| Error::Unknown(format!("model provider `{id}`")))
    }
}

fn inferred_choice(route: &str, provider: &dyn Model) -> ModelChoice {
    let mut info = provider.info();
    if info.model.is_empty() {
        info.model = route.to_string();
    }
    ModelChoice {
        route: route.to_string(),
        group: route.to_string(),
        model: info.model,
        reasoning_effort: info.reasoning_effort,
        context_window: None,
        supports_image_input: provider.supports_image_input(),
    }
}

pub(crate) fn image_input<'a>(
    part: &'a Value,
    provider: &str,
) -> Result<Option<(&'a str, &'a str)>> {
    if part.get("type").and_then(Value::as_str) != Some("input_image") {
        return Ok(None);
    }
    let media_type = part
        .get("media_type")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::Provider(format!("{provider} image input omitted media_type").into())
        })?;
    let data = part
        .get("data")
        .and_then(Value::as_str)
        .filter(|data| !data.is_empty())
        .ok_or_else(|| Error::Provider(format!("{provider} image input omitted data").into()))?;
    let Some(subtype) = media_type.strip_prefix("image/") else {
        return Err(Error::Provider(
            format!("{provider} image input requires an image media type").into(),
        ));
    };
    if subtype.is_empty()
        || !subtype.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                )
        })
    {
        return Err(Error::Provider(
            format!("{provider} image input has an invalid media type").into(),
        ));
    }
    Ok(Some((media_type, data)))
}

pub(crate) fn image_data_url(media_type: &str, data: &str) -> String {
    format!("data:{media_type};base64,{data}")
}

fn validate_usage(usage: &TokenUsage) -> Result<()> {
    if [
        usage.input_tokens,
        usage.cached_input_tokens,
        usage.cache_write_input_tokens,
        usage.output_tokens,
        usage.reasoning_output_tokens,
        usage.total_tokens,
    ]
    .into_iter()
    .any(|tokens| tokens < 0)
    {
        return Err(Error::Provider(
            "model returned negative token usage".into(),
        ));
    }
    Ok(())
}

pub(super) fn usage_i64(
    usage: Option<&Value>,
    pointer: &str,
    provider: &str,
) -> Result<Option<i64>> {
    let Some(usage) = usage else {
        return Ok(None);
    };
    if !usage.is_object() {
        return Err(Error::Provider(
            format!("{provider} usage was not an object").into(),
        ));
    }
    let Some(value) = usage.pointer(pointer) else {
        return Ok(None);
    };
    value.as_i64().map(Some).ok_or_else(|| {
        Error::Provider(format!("{provider} usage field `{pointer}` was not an integer").into())
    })
}

fn decode_tool_call(item: &Value, call_ids: &mut BTreeSet<String>) -> Result<ToolCall> {
    let call_id = required_output_string(item, "call_id", MAX_TOOL_CALL_ID_BYTES)?;
    if !call_ids.insert(call_id.to_string()) {
        return Err(Error::Provider(
            format!("model returned duplicate tool-call ID `{call_id}`").into(),
        ));
    }
    let name = required_output_string(item, "name", MAX_TOOL_NAME_BYTES)?;
    let arguments = required_output_string(item, "arguments", MAX_TOOL_ARGUMENT_BYTES)?;
    let arguments: Value = serde_json::from_str(arguments)?;
    if !arguments.is_object() {
        return Err(Error::Provider(
            format!("tool call `{call_id}` arguments must be a JSON object").into(),
        ));
    }
    Ok(ToolCall {
        call_id: call_id.to_string(),
        name: name.to_string(),
        arguments,
    })
}

fn required_output_string<'a>(item: &'a Value, field: &str, limit: usize) -> Result<&'a str> {
    let value = item
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Provider(format!("function call omitted {field}").into()))?;
    if value.len() > limit {
        return Err(Error::Provider(
            format!("function call {field} exceeded size limit").into(),
        ));
    }
    Ok(value)
}

fn ensure_output_size(output: &[Value]) -> Result<()> {
    let mut writer = SizeWriter::new(MAX_MODEL_OUTPUT_BYTES);
    match serde_json::to_writer(&mut writer, output) {
        Ok(()) => Ok(()),
        Err(_) if writer.exceeded => {
            Err(Error::Provider("model output exceeded size limit".into()))
        }
        Err(error) => Err(error.into()),
    }
}

struct SizeWriter {
    bytes: usize,
    limit: usize,
    exceeded: bool,
}

impl SizeWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: 0,
            limit,
            exceeded: false,
        }
    }
}

impl Write for SizeWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.bytes.saturating_add(buffer.len()) > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("size limit exceeded"));
        }
        self.bytes += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Creates a Responses API user-message item.
#[must_use]
pub fn user_message(text: &str) -> Value {
    serde_json::json!({
        "role": "user",
        "content": [{"type": "input_text", "text": text}]
    })
}

/// Creates a durable user message carrying opaque uploaded-file references.
#[must_use]
pub fn user_message_with_attachments(text: &str, attachments: &[SessionFileReference]) -> Value {
    let mut message = user_message(text);
    if !attachments.is_empty() {
        message[ATTACHMENTS_FIELD] =
            serde_json::to_value(attachments).unwrap_or_else(|_| Value::Array(Vec::new()));
    }
    message
}

pub(crate) fn internal_user_message(kind: &str, text: &str) -> Value {
    let mut message = user_message(text);
    message[INTERNAL_MESSAGE_FIELD] = Value::String(kind.into());
    message
}

/// Creates a Responses API function-call-output item.
#[must_use]
pub fn tool_output(call_id: &str, output: &str, is_error: bool) -> Value {
    let mut value = serde_json::json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": output
    });
    value[TOOL_ERROR_FIELD] = Value::Bool(is_error);
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DefaultCapabilities;

    impl Model for DefaultCapabilities {
        fn respond<'a>(
            &'a self,
            _request: ModelRequest<'a>,
            _events: ModelEventSink,
        ) -> BoxFuture<'a, Result<ModelOutput>> {
            Box::pin(async { Err(Error::Provider("response was not expected".into())) })
        }
    }

    #[test]
    fn image_input_requires_explicit_provider_support() {
        let model: Arc<dyn Model> = Arc::new(DefaultCapabilities);
        let router = ModelRouter::new("text-only", Arc::clone(&model));

        assert!(!model.supports_image_input());
        assert!(!router.supports_image_input("text-only").expect("route"));
        assert!(
            !router
                .choices()
                .next()
                .expect("choice")
                .supports_image_input
        );
    }

    #[test]
    fn normalized_output_derives_text_and_validates_tool_calls() {
        let output = ModelOutput::from_output(
            vec![
                serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "Done."}]
                }),
                serde_json::json!({
                    "type": "function_call",
                    "call_id": "call-1",
                    "name": "read",
                    "arguments": "{\"path\":\"README.md\"}"
                }),
            ],
            true,
            TokenUsage::default(),
        )
        .expect("normalized output");

        assert_eq!(output.text(), "Done.");
        assert_eq!(
            output.tool_calls(),
            vec![ToolCall {
                call_id: "call-1".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": "README.md"}),
            }]
        );
    }

    #[test]
    fn normalized_output_preserves_complete_typed_step_content() {
        let output = ModelOutput::from_output(
            vec![
                serde_json::json!({
                    "type": "reasoning",
                    (REPLAY_REASONING_FIELD): "Inspect the state."
                }),
                serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "phase": "commentary",
                    "content": [
                        {"type": "output_text", "text": "Checking "},
                        {"type": "output_text", "text": "now."}
                    ]
                }),
                serde_json::json!({
                    "type": "function_call",
                    "call_id": "call-1",
                    "name": "read",
                    "arguments": "{}"
                }),
                serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "Done."}]
                }),
            ],
            true,
            TokenUsage::default(),
        )
        .expect("normalized output");

        assert_eq!(
            output.content(),
            vec![
                ModelStepContent {
                    output_index: 0,
                    part_index: 0,
                    phase: ModelStepContentPhase::Reasoning,
                    text: "Inspect the state.".into(),
                    annotations: Vec::new(),
                },
                ModelStepContent {
                    output_index: 1,
                    part_index: 0,
                    phase: ModelStepContentPhase::Commentary,
                    text: "Checking ".into(),
                    annotations: Vec::new(),
                },
                ModelStepContent {
                    output_index: 1,
                    part_index: 1,
                    phase: ModelStepContentPhase::Commentary,
                    text: "now.".into(),
                    annotations: Vec::new(),
                },
                ModelStepContent {
                    output_index: 3,
                    part_index: 0,
                    phase: ModelStepContentPhase::FinalAnswer,
                    text: "Done.".into(),
                    annotations: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn normalized_output_rejects_unmodeled_annotation_fields() {
        let error = ModelOutput::from_output(
            vec![serde_json::json!({
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "Source",
                    "annotations": [{
                        "type": "url_citation",
                        "url": "https://example.com",
                        "title": "Example",
                        "start_index": 0,
                        "end_index": 6,
                        "unmodeled": true
                    }]
                }]
            })],
            true,
            TokenUsage::default(),
        )
        .expect_err("unmodeled annotation fields must fail");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn normalized_output_rejects_duplicate_tool_call_ids() {
        let call = serde_json::json!({
            "type": "function_call",
            "call_id": "same",
            "name": "read",
            "arguments": "{}"
        });

        let error = ModelOutput::from_output(vec![call.clone(), call], true, TokenUsage::default())
            .expect_err("duplicate IDs must fail");

        assert!(error.to_string().contains("duplicate tool-call ID"));
    }

    #[test]
    fn normalized_output_rejects_bounded_and_invalid_values() {
        let mut writer = SizeWriter::new(1);
        assert!(writer.write_all(b"12").is_err());

        let calls = (0..=MAX_TOOL_CALLS)
            .map(|index| {
                serde_json::json!({
                    "type": "function_call",
                    "call_id": format!("call-{index}"),
                    "name": "read",
                    "arguments": "{}"
                })
            })
            .collect();
        assert!(
            ModelOutput::from_output(calls, false, TokenUsage::default())
                .expect_err("tool-call limit must fail")
                .to_string()
                .contains("tool calls")
        );

        assert!(
            ModelOutput::from_output(
                vec![user_message("response")],
                true,
                TokenUsage {
                    input_tokens: -1,
                    ..TokenUsage::default()
                },
            )
            .expect_err("negative usage must fail")
            .to_string()
            .contains("negative token usage")
        );
    }

    #[test]
    fn usage_fields_reject_out_of_range_integers() {
        assert!(
            usage_i64(
                Some(&serde_json::json!({"input_tokens": u64::MAX})),
                "/input_tokens",
                "test",
            )
            .is_err()
        );
    }
}
