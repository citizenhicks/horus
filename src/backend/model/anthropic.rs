//! Native Anthropic Messages API provider.

use reqwest::Client;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use super::Model;
use super::ModelEventSink;
use super::ModelInfo;
use super::ModelOutput;
use super::ModelRequest;
use super::REPLAY_REASONING_FIELD;
use super::TOOL_ERROR_FIELD;
use super::ToolDefinition;
use super::provider::HostedWebSearch;
use super::provider::ModelPreset;
use super::provider::ProviderAuth;
use super::provider::ProviderBuildConfig;
use super::provider::ProviderDefinition;
use super::provider::ReasoningPreset;
use super::transport::MAX_SSE_FRAME_BYTES;
use super::transport::frame_data;
use super::transport::push_sse_chunk;
use super::transport::status_error;
use super::transport::streaming_client;
use super::transport::take_sse_frame;
use super::usage_i64;
use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::protocol::FrontendSymbol;
use crate::protocol::ModelEvent;
use crate::protocol::TokenUsage;
use crate::protocol::WebSearchAction;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const MAX_OUTPUT_TOKENS: u64 = 64_000;
const MAX_CONTENT_BLOCKS: usize = 1_024;
const RAW_CONTENT: &str = "_anthropic_content";

const REASONING: &[ReasoningPreset] = &[
    ReasoningPreset {
        id: "low",
        label: "Low",
        description: "Prefer speed and lower cost",
    },
    ReasoningPreset {
        id: "medium",
        label: "Medium",
        description: "Balance reasoning and latency",
    },
    ReasoningPreset {
        id: "high",
        label: "High",
        description: "Anthropic's default reasoning effort",
    },
    ReasoningPreset {
        id: "xhigh",
        label: "Extra high",
        description: "Extended effort for long-horizon work",
    },
    ReasoningPreset {
        id: "max",
        label: "Maximum",
        description: "Use maximum available reasoning",
    },
];

const MODELS: &[ModelPreset] = &[
    ModelPreset {
        id: "claude-sonnet-5",
        label: "Claude Sonnet 5",
        description: "Fast frontier model for coding and agents",
        context_window: 1_000_000,
        reasoning: REASONING,
        default_reasoning: Some("high"),
    },
    ModelPreset {
        id: "claude-opus-4-8",
        label: "Claude Opus 4.8",
        description: "Highest-capability Anthropic model",
        context_window: 1_000_000,
        reasoning: REASONING,
        default_reasoning: Some("high"),
    },
    ModelPreset {
        id: "claude-haiku-4-5",
        label: "Claude Haiku 4.5",
        description: "Fast, economical Anthropic model",
        context_window: 200_000,
        reasoning: &[],
        default_reasoning: None,
    },
];

const SEARCH: &[HostedWebSearch] = &[HostedWebSearch::Off, HostedWebSearch::Live];

/// Anthropic's native Messages API provider.
pub struct Anthropic {
    client: Client,
    api_key: String,
    model: String,
    reasoning_effort: Option<String>,
    web_search: bool,
}

impl Anthropic {
    /// Creates a provider for Anthropic's fixed Messages API endpoint.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        Self::with_client(api_key, model, streaming_client()?)
    }

    fn with_client(
        api_key: impl Into<String>,
        model: impl Into<String>,
        client: Client,
    ) -> Result<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(Error::Config("ANTHROPIC_API_KEY is empty".into()));
        }
        let model = model.into();
        if model.trim().is_empty() {
            return Err(Error::Config("Anthropic model is empty".into()));
        }
        Ok(Self {
            client,
            api_key,
            model,
            reasoning_effort: None,
            web_search: false,
        })
    }

    /// Enables adaptive thinking at one supported Anthropic effort level.
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Result<Self> {
        let effort = effort.into();
        if !matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max") {
            return Err(Error::Config(format!(
                "unsupported Anthropic reasoning effort `{effort}`"
            )));
        }
        self.reasoning_effort = Some(effort);
        Ok(self)
    }

    /// Enables Anthropic-hosted web search.
    #[must_use]
    pub fn with_web_search(mut self) -> Self {
        self.web_search = true;
        self
    }

    async fn send_response(
        &self,
        request: ModelRequest<'_>,
        events: ModelEventSink,
    ) -> Result<ModelOutput> {
        let body = self.request_body(
            request.instructions,
            request.input,
            request.tools,
            request.allow_hosted_tools,
        )?;
        let mut response = self.post(&body).await?;
        let mut bytes = Vec::new();
        let mut stream = StreamState::default();
        let mut stream_bytes = 0;
        while let Some(chunk) = response.chunk().await? {
            push_sse_chunk(&mut bytes, &mut stream_bytes, &chunk, "Anthropic")?;
            while let Some(frame) = take_sse_frame(&mut bytes)? {
                let Some(data) = frame_data(&frame) else {
                    continue;
                };
                stream.apply(serde_json::from_str(&data)?, &events)?;
            }
            if bytes.len() > MAX_SSE_FRAME_BYTES {
                return Err(Error::Provider(
                    "Anthropic SSE frame exceeded size limit".into(),
                ));
            }
        }
        if !stream.stopped {
            return Err(Error::Provider(
                "Anthropic stream ended before message_stop".into(),
            ));
        }
        stream.finish()
    }

    fn request_body(
        &self,
        instructions: &str,
        input: &[Value],
        tools: &[ToolDefinition],
        allow_hosted_tools: bool,
    ) -> Result<Value> {
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": MAX_OUTPUT_TOKENS,
            "system": instructions,
            "messages": translate_messages(input)?,
            "tools": wire_tools(tools, self.web_search && allow_hosted_tools),
            "cache_control": {"type": "ephemeral"},
            "stream": true
        });
        self.apply_reasoning(&mut body);
        Ok(body)
    }

    fn apply_reasoning(&self, body: &mut Value) {
        if let Some(effort) = &self.reasoning_effort {
            body["thinking"] = serde_json::json!({"type": "adaptive"});
            body["output_config"] = serde_json::json!({"effort": effort});
        }
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response> {
        let response = self
            .client
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(body)
            .send()
            .await?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(status_error(response, "Anthropic").await)
        }
    }
}

impl Model for Anthropic {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
        }
    }

    fn respond<'a>(
        &'a self,
        request: ModelRequest<'a>,
        events: ModelEventSink,
    ) -> BoxFuture<'a, Result<ModelOutput>> {
        Box::pin(self.send_response(request, events))
    }
}

#[derive(Default)]
struct StreamState {
    blocks: BTreeMap<usize, Value>,
    partial_json: BTreeMap<usize, String>,
    web_queries: BTreeMap<String, Option<String>>,
    usage: Usage,
    stop_reason: Option<String>,
    stopped: bool,
}

impl StreamState {
    fn apply(&mut self, event: Value, events: &ModelEventSink) -> Result<()> {
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => self.usage.update(event.pointer("/message/usage"))?,
            Some("content_block_start") => self.start_block(&event, events)?,
            Some("content_block_delta") => self.delta_block(&event, events)?,
            Some("content_block_stop") => self.stop_block(&event)?,
            Some("message_delta") => {
                self.usage.update(event.get("usage"))?;
                self.stop_reason = event
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
            }
            Some("message_stop") => self.stopped = true,
            Some("error") => {
                let message = event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Anthropic stream error");
                return Err(Error::Provider(message.to_string().into()));
            }
            Some("ping") | None | Some(_) => {}
        }
        Ok(())
    }

    fn start_block(&mut self, event: &Value, events: &ModelEventSink) -> Result<()> {
        let index = event_index(event)?;
        if self.blocks.contains_key(&index) {
            return Err(Error::Provider(
                format!("Anthropic repeated content block index {index}").into(),
            ));
        }
        if self.blocks.len() >= MAX_CONTENT_BLOCKS {
            return Err(Error::Provider(
                format!("Anthropic returned more than {MAX_CONTENT_BLOCKS} content blocks").into(),
            ));
        }
        let block = event
            .get("content_block")
            .cloned()
            .ok_or_else(|| Error::Provider("Anthropic content block omitted value".into()))?;
        if block.get("type").and_then(Value::as_str) == Some("server_tool_use")
            && block.get("name").and_then(Value::as_str) == Some("web_search")
        {
            let id = required_string(&block, "id")?.to_string();
            let query = block
                .pointer("/input/query")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            self.web_queries.insert(id.clone(), query);
            events(ModelEvent::WebSearchStarted { call_id: id })?;
        }
        if block.get("type").and_then(Value::as_str) == Some("web_search_tool_result") {
            let call_id = required_string(&block, "tool_use_id")?.to_string();
            let query = self.web_queries.get(&call_id).cloned().flatten();
            events(ModelEvent::WebSearchCompleted {
                call_id,
                action: WebSearchAction::Search { query },
            })?;
        }
        self.blocks.insert(index, block);
        Ok(())
    }

    fn delta_block(&mut self, event: &Value, events: &ModelEventSink) -> Result<()> {
        let index = event_index(event)?;
        let delta = event
            .get("delta")
            .ok_or_else(|| Error::Provider("Anthropic content delta omitted value".into()))?;
        let block = self
            .blocks
            .get_mut(&index)
            .ok_or_else(|| Error::Provider("Anthropic delta referenced unknown block".into()))?;
        match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => {
                let text = required_string(delta, "text")?;
                append_string(block, "text", text);
                events(ModelEvent::TextDelta(text.to_string()))?;
            }
            Some("thinking_delta") => {
                let thinking = required_string(delta, "thinking")?;
                append_string(block, "thinking", thinking);
                events(ModelEvent::ReasoningDelta(thinking.to_string()))?;
            }
            Some("signature_delta") => {
                block["signature"] =
                    Value::String(required_string(delta, "signature")?.to_string());
            }
            Some("input_json_delta") => self
                .partial_json
                .entry(index)
                .or_default()
                .push_str(required_string(delta, "partial_json")?),
            Some("citations_delta") => {
                if let Some(citation) = delta.get("citation") {
                    let citations = block
                        .as_object_mut()
                        .ok_or_else(|| {
                            Error::Provider("Anthropic content block was not an object".into())
                        })?
                        .entry("citations")
                        .or_insert_with(|| Value::Array(Vec::new()));
                    citations
                        .as_array_mut()
                        .ok_or_else(|| {
                            Error::Provider("Anthropic citations were not an array".into())
                        })?
                        .push(citation.clone());
                }
            }
            None | Some(_) => {}
        }
        Ok(())
    }

    fn stop_block(&mut self, event: &Value) -> Result<()> {
        let index = event_index(event)?;
        let Some(partial) = self.partial_json.remove(&index) else {
            return Ok(());
        };
        let input: Value = serde_json::from_str(&partial)?;
        let block = self
            .blocks
            .get_mut(&index)
            .ok_or_else(|| Error::Provider("Anthropic stop referenced unknown block".into()))?;
        block["input"] = input;
        if block.get("type").and_then(Value::as_str) == Some("server_tool_use")
            && block.get("name").and_then(Value::as_str) == Some("web_search")
        {
            let id = required_string(block, "id")?.to_string();
            let query = block
                .pointer("/input/query")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            self.web_queries.insert(id, query);
        }
        Ok(())
    }

    fn finish(self) -> Result<ModelOutput> {
        let content = self.blocks.into_values().collect::<Vec<_>>();
        let calls = content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
            .map(|block| {
                let arguments = block
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                Ok(serde_json::json!({
                    "type": "function_call",
                    "call_id": required_string(block, "id")?,
                    "name": required_string(block, "name")?,
                    "arguments": serde_json::to_string(&arguments)?
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let visible = content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .map(|block| {
                serde_json::json!({
                    "type": "output_text",
                    "text": block.get("text").and_then(Value::as_str).unwrap_or_default()
                })
            })
            .collect::<Vec<_>>();
        let mut message = serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": visible
        });
        let reasoning = content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("thinking"))
            .filter_map(|block| block.get("thinking").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if !reasoning.is_empty() {
            message[REPLAY_REASONING_FIELD] = Value::String(reasoning);
        }
        message[RAW_CONTENT] = Value::Array(content);
        let mut output = vec![message];
        output.extend(calls);
        ModelOutput::from_output(
            output,
            self.stop_reason.as_deref() != Some("pause_turn"),
            self.usage.finish()?,
        )
    }
}

#[derive(Default)]
struct Usage {
    input: i64,
    cache_read: i64,
    cache_write: i64,
    output: i64,
    thinking: i64,
}

impl Usage {
    fn update(&mut self, usage: Option<&Value>) -> Result<()> {
        let Some(usage) = usage else {
            return Ok(());
        };
        update_i64(&mut self.input, usage, "/input_tokens")?;
        update_i64(&mut self.cache_read, usage, "/cache_read_input_tokens")?;
        update_i64(&mut self.cache_write, usage, "/cache_creation_input_tokens")?;
        update_i64(&mut self.output, usage, "/output_tokens")?;
        update_i64(
            &mut self.thinking,
            usage,
            "/output_tokens_details/thinking_tokens",
        )?;
        Ok(())
    }

    fn finish(self) -> Result<TokenUsage> {
        let input_tokens = self
            .input
            .checked_add(self.cache_read)
            .and_then(|tokens| tokens.checked_add(self.cache_write))
            .ok_or_else(|| Error::Provider("Anthropic token usage overflowed".into()))?;
        let total_tokens = input_tokens
            .checked_add(self.output)
            .ok_or_else(|| Error::Provider("Anthropic token usage overflowed".into()))?;
        Ok(TokenUsage {
            input_tokens,
            cached_input_tokens: self.cache_read,
            cache_write_input_tokens: self.cache_write,
            output_tokens: self.output,
            reasoning_output_tokens: self.thinking,
            total_tokens,
        })
    }
}

fn translate_messages(input: &[Value]) -> Result<Vec<Value>> {
    let mut messages = Vec::new();
    let mut preserved_tools = BTreeSet::new();
    for item in input {
        let kind = item
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| item.get("role").is_some().then_some("message"));
        match kind {
            Some("message") => {
                let role = item
                    .get("role")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::Provider("history message omitted role".into()))?;
                if let Some(content) = item.get(RAW_CONTENT).and_then(Value::as_array) {
                    preserved_tools.extend(
                        content
                            .iter()
                            .filter(|block| {
                                block.get("type").and_then(Value::as_str) == Some("tool_use")
                            })
                            .filter_map(|block| block.get("id").and_then(Value::as_str))
                            .map(ToString::to_string),
                    );
                    push_message(&mut messages, role, content.clone());
                } else {
                    let blocks = item
                        .get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|part| {
                            matches!(
                                part.get("type").and_then(Value::as_str),
                                Some("input_text" | "output_text")
                            )
                        })
                        .map(|part| {
                            serde_json::json!({
                                "type": "text",
                                "text": part.get("text").and_then(Value::as_str).unwrap_or_default()
                            })
                        })
                        .collect::<Vec<_>>();
                    push_message(&mut messages, role, blocks);
                }
            }
            Some("function_call") => {
                let call_id = required_string(item, "call_id")?;
                if !preserved_tools.contains(call_id) {
                    push_message(
                        &mut messages,
                        "assistant",
                        vec![serde_json::json!({
                            "type": "tool_use",
                            "id": call_id,
                            "name": required_string(item, "name")?,
                            "input": serde_json::from_str::<Value>(required_string(item, "arguments")?)?
                        })],
                    );
                }
            }
            Some("function_call_output") => push_message(
                &mut messages,
                "user",
                vec![serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": required_string(item, "call_id")?,
                    "content": string_field(item, "output")?,
                    "is_error": item.get(TOOL_ERROR_FIELD).and_then(Value::as_bool).unwrap_or(false)
                })],
            ),
            None | Some(_) => {}
        }
    }
    if messages.is_empty() {
        return Err(Error::Provider("Anthropic request has no messages".into()));
    }
    Ok(messages)
}

fn push_message(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        content.extend(blocks);
        return;
    }
    messages.push(serde_json::json!({"role": role, "content": blocks}));
}

fn wire_tools(tools: &[ToolDefinition], web_search: bool) -> Vec<Value> {
    let mut output = tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.parameters
            })
        })
        .collect::<Vec<_>>();
    if web_search {
        output.push(serde_json::json!({
            "type": "web_search_20260318",
            "name": "web_search"
        }));
    }
    output
}

fn event_index(event: &Value) -> Result<usize> {
    event
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| Error::Provider("Anthropic event omitted block index".into()))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    let value = string_field(value, field)?;
    if value.is_empty() {
        return Err(Error::Provider(
            format!("Anthropic value omitted {field}").into(),
        ));
    }
    Ok(value)
}

fn string_field<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Provider(format!("Anthropic value omitted {field}").into()))
}

fn append_string(value: &mut Value, field: &str, addition: &str) {
    if let Some(Value::String(current)) = value.get_mut(field) {
        current.push_str(addition);
    } else {
        value[field] = Value::String(addition.to_string());
    }
}

fn update_i64(target: &mut i64, value: &Value, path: &str) -> Result<()> {
    if let Some(value) = usage_i64(Some(value), path, "Anthropic")? {
        *target = value;
    }
    Ok(())
}

pub(super) const fn provider() -> ProviderDefinition {
    ProviderDefinition::new(
        "anthropic",
        "Anthropic",
        FrontendSymbol::Claude,
        "Native Messages API with adaptive thinking",
        ProviderAuth::ApiKey("ANTHROPIC_API_KEY"),
        MODELS,
        SEARCH,
        build_provider,
    )
}

fn build_provider(config: ProviderBuildConfig) -> Result<Arc<dyn Model>> {
    let api_key = config.credential.into_api_key("anthropic")?;
    let provider = Anthropic::with_client(api_key, config.model, config.http)?;
    let provider = match config.reasoning_effort {
        Some(effort) => provider.with_reasoning_effort(effort)?,
        None => provider,
    };
    let provider = if config.web_search == HostedWebSearch::Live {
        provider.with_web_search()
    } else {
        provider
    };
    Ok(Arc::new(provider))
}

#[cfg(test)]
#[path = "anthropic_tests.rs"]
mod tests;
