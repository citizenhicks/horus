//! OpenAI Responses API Adapter.

use reqwest::Client;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use super::CompactOutput;
use super::CompactRequest;
use super::Model;
use super::ModelEventSink;
use super::ModelInfo;
use super::ModelOutput;
use super::ModelRequest;
use super::ToolDefinition;
use super::image_data_url;
use super::image_input;
use super::openai_auth::ApiKeyAuthorization;
use super::openai_auth::OpenAiAuthorization;
use super::provider::HostedWebSearch;
use super::provider::ModelPreset;
use super::provider::ProviderAuth;
use super::provider::ProviderBuildConfig;
use super::provider::ProviderDefinition;
use super::provider::validate_base_url;
use super::transport::MAX_SSE_FRAME_BYTES;
use super::transport::frame_data;
use super::transport::push_sse_chunk;
use super::transport::read_limited;
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

const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_STREAM_OUTPUT_ITEMS: usize = 1_024;
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// OpenAI Responses API configuration.
pub struct OpenAi {
    client: Client,
    auth: Arc<dyn OpenAiAuthorization>,
    base_url: String,
    model: String,
    reasoning_effort: Option<String>,
    hosted_tools: Vec<Value>,
    compaction_endpoint: bool,
    image_input: bool,
}

impl OpenAi {
    /// Creates an OpenAI or Responses-compatible provider.
    pub fn new(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self> {
        Self::with_client(api_key, base_url, model, streaming_client()?)
    }

    pub(super) fn with_client(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        client: Client,
    ) -> Result<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(Error::Config("OPENAI_API_KEY is empty".into()));
        }
        Self::with_authorization(
            Arc::new(ApiKeyAuthorization::new(api_key)),
            base_url.into(),
            model.into(),
            client,
        )
    }

    pub(super) fn with_authorization(
        auth: Arc<dyn OpenAiAuthorization>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        client: Client,
    ) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let model = model.into();
        validate_base_url(&base_url)?;
        if model.trim().is_empty() {
            return Err(Error::Config("OPENAI_MODEL is empty".into()));
        }
        Ok(Self {
            client,
            auth,
            base_url,
            model,
            reasoning_effort: None,
            hosted_tools: Vec::new(),
            compaction_endpoint: false,
            image_input: true,
        })
    }

    /// Selects a Responses reasoning effort.
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Result<Self> {
        let effort = effort.into();
        if effort.trim().is_empty() {
            return Err(Error::Config("reasoning effort cannot be empty".into()));
        }
        self.reasoning_effort = Some(effort);
        Ok(self)
    }

    /// Enables provider-hosted live web search.
    #[must_use]
    pub fn with_web_search(self) -> Self {
        self.with_hosted_tool(serde_json::json!({"type": "web_search"}))
    }

    /// Enables provider-hosted cached-only web search.
    #[must_use]
    pub fn with_cached_web_search(self) -> Self {
        self.with_hosted_tool(serde_json::json!({
            "type": "web_search",
            "external_web_access": false
        }))
    }

    /// Adds one provider-specific hosted tool to Responses requests.
    #[must_use]
    pub fn with_hosted_tool(mut self, tool: Value) -> Self {
        self.hosted_tools.push(tool);
        self
    }

    /// Marks an endpoint that implements native Responses compaction.
    #[must_use]
    pub fn with_compaction_endpoint(mut self) -> Self {
        self.compaction_endpoint = true;
        self
    }

    /// Disables image input for a Responses-compatible endpoint that rejects it.
    #[must_use]
    pub(super) fn without_image_input(mut self) -> Self {
        self.image_input = false;
        self
    }

    async fn send_response(
        &self,
        request: ModelRequest<'_>,
        events: ModelEventSink,
    ) -> Result<ModelOutput> {
        let mut body = serde_json::json!({
            "model": self.model,
            "instructions": request.instructions,
            "input": wire_input(request.input, self.image_input)?,
            "tools": wire_tools(request.tools, &self.hosted_tools, request.allow_hosted_tools),
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "include": ["reasoning.encrypted_content"],
            "store": false,
            "stream": true
        });
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning"] = if self.auth.reasoning_summary() {
                serde_json::json!({"effort": effort, "summary": "auto"})
            } else {
                serde_json::json!({"effort": effort})
            };
        }
        let request = self
            .client
            .post(format!("{}/responses", self.base_url))
            .json(&body);
        let mut response = self.authorize(request, true).await?.send().await?;
        if !response.status().is_success() {
            return Err(status_error(response, "Responses").await);
        }

        let mut bytes = Vec::new();
        let mut completed = None;
        let mut commentary = BTreeSet::new();
        let mut web_searches = BTreeSet::new();
        let mut output = BTreeMap::new();
        let mut stream_bytes = 0;
        while let Some(chunk) = response.chunk().await? {
            push_sse_chunk(&mut bytes, &mut stream_bytes, &chunk, "Responses")?;
            while let Some(frame) = take_sse_frame(&mut bytes)? {
                let Some(data) = frame_data(&frame) else {
                    continue;
                };
                if data == "[DONE]" {
                    continue;
                }
                let event: Value = serde_json::from_str(&data)?;
                collect_stream_output(&event, &mut output)?;
                if emit_web_event(&event, &mut web_searches, &events)? {
                    continue;
                }
                if emit_reasoning_event(&event, &events)? {
                    continue;
                }
                if emit_text_event(&event, &mut commentary, &events)? {
                    continue;
                }
                match event.get("type").and_then(Value::as_str) {
                    Some("response.completed") => {
                        completed = event
                            .get("response")
                            .cloned()
                            .map(|response| attach_stream_output(response, &output));
                    }
                    Some("error" | "response.failed" | "response.incomplete") => {
                        return Err(Error::Provider(response_error(&event).into()));
                    }
                    _ => {}
                }
            }
            if bytes.len() > MAX_SSE_FRAME_BYTES {
                return Err(Error::Provider("SSE frame exceeded size limit".into()));
            }
        }
        let response = completed
            .ok_or_else(|| Error::Provider("stream ended before response.completed".into()))?;
        decode_response(response)
    }

    async fn compact_response(&self, request: CompactRequest<'_>) -> Result<CompactOutput> {
        if !self.compaction_endpoint {
            return Err(Error::Provider(
                "OpenAI-compatible provider has no compaction endpoint".into(),
            ));
        }
        let body = self.compact_body(request)?;
        let request = self
            .client
            .post(format!("{}/responses/compact", self.base_url))
            .json(&body);
        let response = self.authorize(request, false).await?.send().await?;
        if !response.status().is_success() {
            return Err(status_error(response, "Responses").await);
        }
        let response: Value =
            serde_json::from_slice(&read_limited(response, MAX_JSON_BYTES, "Responses").await?)?;
        CompactOutput::from_output(
            response
                .get("output")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            decode_usage(response.get("usage"))?,
        )
    }

    async fn authorize(
        &self,
        mut request: reqwest::RequestBuilder,
        streaming: bool,
    ) -> Result<reqwest::RequestBuilder> {
        let authorization = self.auth.authorize_http(streaming).await?;
        request = request.bearer_auth(authorization.token);
        for (name, value) in authorization.headers {
            request = request.header(name, value);
        }
        Ok(request)
    }

    fn compact_body(&self, request: CompactRequest<'_>) -> Result<Value> {
        Ok(serde_json::json!({
            "model": self.model,
            "instructions": request.instructions,
            "input": wire_input(request.input, self.image_input)?,
            "tools": wire_tools(request.tools, &self.hosted_tools, true),
            "parallel_tool_calls": true
        }))
    }
}

impl Model for OpenAi {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
        }
    }

    fn supports_attachment_input(&self) -> bool {
        self.image_input
    }

    fn respond<'a>(
        &'a self,
        request: ModelRequest<'a>,
        events: ModelEventSink,
    ) -> BoxFuture<'a, Result<ModelOutput>> {
        Box::pin(self.send_response(request, events))
    }

    fn compaction_endpoint(&self) -> bool {
        self.compaction_endpoint
    }

    fn compact<'a>(&'a self, request: CompactRequest<'a>) -> BoxFuture<'a, Result<CompactOutput>> {
        Box::pin(self.compact_response(request))
    }
}

pub(super) fn wire_input(input: &[Value], allow_images: bool) -> Result<Vec<Value>> {
    let mut input = input.to_vec();
    for item in &mut input {
        if let Some(fields) = item.as_object_mut() {
            fields.retain(|name, _| !name.starts_with('_'));
        }
        let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in content {
            if part.get("type").and_then(Value::as_str) != Some("input_image") {
                continue;
            }
            if !allow_images {
                return Err(Error::Provider(
                    "this model provider does not support image attachments".into(),
                ));
            }
            let Some((media_type, data)) = image_input(part, "Responses")? else {
                continue;
            };
            *part = serde_json::json!({
                "type": "input_image",
                "image_url": image_data_url(media_type, data)
            });
        }
    }
    Ok(input)
}

pub(super) fn collect_stream_output(
    event: &Value,
    output: &mut BTreeMap<u64, Value>,
) -> Result<()> {
    if event.get("type").and_then(Value::as_str) != Some("response.output_item.done") {
        return Ok(());
    }
    let item = event
        .get("item")
        .cloned()
        .ok_or_else(|| Error::Provider("completed output item omitted item".into()))?;
    let index = event
        .get("output_index")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| {
            output
                .last_key_value()
                .map_or(0, |(index, _)| index.saturating_add(1))
        });
    if output.len() >= MAX_STREAM_OUTPUT_ITEMS && !output.contains_key(&index) {
        return Err(Error::Provider(
            format!("response returned more than {MAX_STREAM_OUTPUT_ITEMS} output items").into(),
        ));
    }
    if output.insert(index, item).is_some() {
        return Err(Error::Provider(
            format!("response repeated output item index {index}").into(),
        ));
    }
    Ok(())
}

pub(super) fn attach_stream_output(mut response: Value, output: &BTreeMap<u64, Value>) -> Value {
    if !output.is_empty() {
        response["output"] = Value::Array(output.values().cloned().collect());
    }
    response
}

pub(super) fn wire_tools(
    tools: &[ToolDefinition],
    hosted_tools: &[Value],
    allow_hosted_tools: bool,
) -> Vec<Value> {
    let mut tools = tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
                "strict": false
            })
        })
        .collect::<Vec<_>>();
    if allow_hosted_tools {
        tools.extend_from_slice(hosted_tools);
    }
    tools
}

const MODELS: &[ModelPreset] = &[];
const SEARCH: &[HostedWebSearch] = &[HostedWebSearch::Off];

pub(super) const fn generic_provider() -> ProviderDefinition {
    ProviderDefinition::new(
        "responses",
        "Local and Other",
        FrontendSymbol::Storage,
        "Any local or remote OpenAI-compatible Responses endpoint",
        ProviderAuth::ApiKey("OPENAI_API_KEY"),
        MODELS,
        SEARCH,
        build_generic,
    )
    .with_base_url(DEFAULT_BASE_URL)
}

fn build_generic(config: ProviderBuildConfig) -> Result<std::sync::Arc<dyn Model>> {
    let api_key = config.credential.into_api_key("responses")?;
    let base_url = config
        .base_url
        .ok_or_else(|| Error::Config("Responses provider requires a base URL".into()))?;
    let provider = OpenAi::with_client(api_key, base_url, config.model, config.http)?;
    let provider = match config.reasoning_effort {
        Some(effort) => provider.with_reasoning_effort(effort)?,
        None => provider,
    };
    Ok(std::sync::Arc::new(provider))
}

fn web_search_item(event: &Value) -> Option<&Value> {
    event
        .get("item")
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("web_search_call"))
}

pub(super) fn emit_web_event(
    event: &Value,
    seen: &mut BTreeSet<String>,
    events: &ModelEventSink,
) -> Result<bool> {
    let Some(item) = web_search_item(event) else {
        return Ok(false);
    };
    let call_id = required_string(item, "id")?.to_string();
    let added = seen.insert(call_id.clone());
    if added {
        events(ModelEvent::WebSearchStarted {
            call_id: call_id.clone(),
        })?;
    }
    if event.get("type").and_then(Value::as_str) == Some("response.output_item.done") {
        events(ModelEvent::WebSearchCompleted {
            call_id,
            action: decode_web_action(item),
        })?;
    }
    Ok(true)
}

pub(super) fn emit_reasoning_event(event: &Value, events: &ModelEventSink) -> Result<bool> {
    if !matches!(
        event.get("type").and_then(Value::as_str),
        Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta")
    ) {
        return Ok(false);
    }
    if let Some(delta) = event.get("delta").and_then(Value::as_str)
        && !delta.is_empty()
    {
        events(ModelEvent::ReasoningDelta(delta.to_string()))?;
    }
    Ok(true)
}

pub(super) fn emit_text_event(
    event: &Value,
    commentary: &mut BTreeSet<String>,
    events: &ModelEventSink,
) -> Result<bool> {
    match event.get("type").and_then(Value::as_str) {
        Some("response.output_item.added") => {
            let Some(item) = event.get("item").filter(|item| {
                item.get("type").and_then(Value::as_str) == Some("message")
                    && item.get("phase").and_then(Value::as_str) == Some("commentary")
            }) else {
                return Ok(false);
            };
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                return Ok(false);
            };
            commentary.insert(id.to_string());
            Ok(true)
        }
        Some("response.output_item.done") => Ok(event
            .get("item")
            .and_then(|item| item.get("id"))
            .and_then(Value::as_str)
            .is_some_and(|id| commentary.remove(id))),
        Some("response.output_text.delta") => {
            let Some(delta) = event.get("delta").and_then(Value::as_str) else {
                return Ok(true);
            };
            let is_commentary = event
                .get("item_id")
                .and_then(Value::as_str)
                .is_some_and(|id| commentary.contains(id));
            if is_commentary {
                events(ModelEvent::CommentaryDelta(delta.to_string()))?;
            } else {
                events(ModelEvent::TextDelta(delta.to_string()))?;
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn decode_web_action(item: &Value) -> WebSearchAction {
    let Some(action) = item.get("action") else {
        return WebSearchAction::Other;
    };
    let string = |field| {
        action
            .get(field)
            .and_then(Value::as_str)
            .map(ToString::to_string)
    };
    match action.get("type").and_then(Value::as_str) {
        Some("search") => WebSearchAction::Search {
            query: string("query").or_else(|| {
                action
                    .get("queries")
                    .and_then(Value::as_array)
                    .and_then(|queries| queries.first())
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            }),
        },
        Some("open_page") => WebSearchAction::OpenPage { url: string("url") },
        Some("find_in_page") => WebSearchAction::FindInPage {
            url: string("url"),
            pattern: string("pattern"),
        },
        _ => WebSearchAction::Other,
    }
}

pub(super) fn decode_response(response: Value) -> Result<ModelOutput> {
    let end_turn = response
        .get("end_turn")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut output = response
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| Error::Provider("response omitted output".into()))?;
    for item in &mut output {
        if item.get("type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        let text = |field| {
            item.get(field)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let reasoning = text("summary");
        let reasoning = if reasoning.is_empty() {
            text("content")
        } else {
            reasoning
        };
        if !reasoning.is_empty() {
            item[super::REPLAY_REASONING_FIELD] = Value::String(reasoning);
        }
    }
    ModelOutput::from_output(output, end_turn, decode_usage(response.get("usage"))?)
}

fn required_string<'a>(item: &'a Value, field: &str) -> Result<&'a str> {
    item.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::Provider(format!("function call omitted {field}").into()))
}

fn decode_usage(usage: Option<&Value>) -> Result<TokenUsage> {
    let value = |pointer| -> Result<i64> {
        Ok(usage_i64(usage, pointer, "Responses")?.unwrap_or_default())
    };
    Ok(TokenUsage {
        input_tokens: value("/input_tokens")?,
        cached_input_tokens: value("/input_tokens_details/cached_tokens")?,
        cache_write_input_tokens: value("/input_tokens_details/cache_write_tokens")?,
        output_tokens: value("/output_tokens")?,
        reasoning_output_tokens: value("/output_tokens_details/reasoning_tokens")?,
        total_tokens: value("/total_tokens")?,
    })
}

#[cfg(test)]
#[path = "openai_tests.rs"]
mod tests;

pub(super) fn response_error(event: &Value) -> String {
    event
        .pointer("/response/error/message")
        .or_else(|| event.pointer("/error/message"))
        .or_else(|| event.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("response failed")
        .to_string()
}
