//! First-party OpenAI Responses WebSocket transport.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::io;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use futures_util::SinkExt;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;

use super::CompactOutput;
use super::CompactRequest;
use super::Model;
use super::ModelEventSink;
use super::ModelInfo;
use super::ModelOutput;
use super::ModelRequest;
use super::openai::OpenAi;
use super::openai::attach_stream_output;
use super::openai::collect_stream_output;
use super::openai::decode_response;
use super::openai::emit_reasoning_event;
use super::openai::emit_text_event;
use super::openai::emit_web_event;
use super::openai::response_error;
use super::openai::wire_input;
use super::openai::wire_tools;
use super::openai_auth::ApiKeyAuthorization;
use super::openai_auth::OpenAiAuthorization;
use super::provider::HostedWebSearch;
use super::provider::ModelPreset;
use super::provider::ProviderAuth;
use super::provider::ProviderBuildConfig;
use super::provider::ProviderDefinition;
use super::transport::account_stream_bytes;
use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::protocol::FrontendSymbol;

mod manifest {
    include!(concat!(
        env!("OUT_DIR"),
        "/src_backend_model_openai_socket_manifest.rs"
    ));
}
use tokio::time::timeout;

const OPENAI_HTTP_URL: &str = "https://api.openai.com/v1";
const OPENAI_SOCKET_URL: &str = "wss://api.openai.com/v1/responses";
const MAX_SOCKET_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_SOCKET_SESSIONS: usize = 128;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_TIMEOUT: Duration = Duration::from_secs(180);
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(600);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// OpenAI's persistent Responses WebSocket transport.
pub struct OpenAiSocket {
    auth: Arc<dyn OpenAiAuthorization>,
    socket_url: &'static str,
    model: String,
    reasoning_effort: Option<String>,
    hosted_tools: Vec<Value>,
    sessions: Mutex<BTreeMap<String, Arc<Mutex<SocketState>>>>,
    http: OpenAi,
}

struct SocketState {
    socket: Option<Socket>,
    continuation: Option<Continuation>,
}

struct Continuation {
    response_id: String,
    known_items: usize,
    fingerprint: u64,
}

enum Exchange {
    Completed(Value),
    PreviousMissing,
    Reconnect(String),
}

impl OpenAiSocket {
    /// Creates the first-party socket transport and HTTP compaction fallback.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        Self::with_client(api_key, model, super::transport::streaming_client()?)
    }

    fn with_client(
        api_key: impl Into<String>,
        model: impl Into<String>,
        client: reqwest::Client,
    ) -> Result<Self> {
        let api_key = api_key.into();
        let model = model.into();
        if api_key.trim().is_empty() {
            return Err(Error::Config("OPENAI_API_KEY is empty".into()));
        }
        Self::with_authorization(
            Arc::new(ApiKeyAuthorization::new(api_key)),
            OPENAI_HTTP_URL,
            OPENAI_SOCKET_URL,
            model,
            client,
        )
    }

    pub(super) fn with_authorization(
        auth: Arc<dyn OpenAiAuthorization>,
        http_url: &str,
        socket_url: &'static str,
        model: impl Into<String>,
        client: reqwest::Client,
    ) -> Result<Self> {
        let model = model.into();
        let http = OpenAi::with_authorization(Arc::clone(&auth), http_url, model.clone(), client)?
            .with_compaction_endpoint();
        Ok(Self {
            auth,
            socket_url,
            model,
            reasoning_effort: None,
            hosted_tools: Vec::new(),
            sessions: Mutex::new(BTreeMap::new()),
            http,
        })
    }

    /// Selects a Responses reasoning effort.
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Result<Self> {
        let effort = effort.into();
        let supported = manifest::MODELS
            .iter()
            .find(|model| model.id == self.model)
            .is_some_and(|model| model.reasoning.iter().any(|preset| preset.id == effort));
        if !supported {
            return Err(Error::Config(format!(
                "model `{}` does not support reasoning effort `{effort}`",
                self.model
            )));
        }
        self.reasoning_effort = Some(effort);
        Ok(self)
    }

    /// Enables provider-hosted live web search.
    #[must_use]
    pub fn with_web_search(mut self) -> Self {
        let tool = serde_json::json!({"type": "web_search"});
        self.hosted_tools.push(tool.clone());
        self.http = self.http.with_hosted_tool(tool);
        self
    }

    /// Enables provider-hosted cached-only web search.
    #[must_use]
    pub fn with_cached_web_search(mut self) -> Self {
        let tool = serde_json::json!({
            "type": "web_search",
            "external_web_access": false
        });
        self.hosted_tools.push(tool.clone());
        self.http = self.http.with_hosted_tool(tool);
        self
    }

    async fn send_response(
        &self,
        request: ModelRequest<'_>,
        events: ModelEventSink,
    ) -> Result<ModelOutput> {
        let session = self.session(request.session_id).await?;
        // A socket and its continuation cursor form one ordered session exchange.
        let mut state = session.lock().await;
        for attempt in 0..2 {
            let mut socket = match state.socket.take() {
                Some(socket) => socket,
                None => {
                    state.continuation = None;
                    connect(self.auth.as_ref(), self.socket_url, request.session_id).await?
                }
            };
            let (previous_response_id, input) =
                response_input(&mut state, request.input, request.allow_continuation)?;
            let body = response_body(
                &self.model,
                &request,
                input,
                previous_response_id.as_deref(),
                self.reasoning_effort.as_deref(),
                &self.hosted_tools,
            )?;
            let exchange = timeout(EXCHANGE_TIMEOUT, exchange(&mut socket, &body, &events))
                .await
                .map_err(|_| Error::Provider("WebSocket response timed out".into()))??;
            match exchange {
                Exchange::Completed(response) => {
                    let response_id = response
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| Error::Provider("response omitted id".into()))?
                        .to_string();
                    let output = decode_response(response)?;
                    let known_items = request.input.len() + output.output().len();
                    state.continuation = if request.allow_continuation {
                        Some(Continuation {
                            response_id,
                            known_items,
                            fingerprint: fingerprint(
                                request.input.iter().chain(output.output().iter()),
                            )?,
                        })
                    } else {
                        None
                    };
                    state.socket = Some(socket);
                    return Ok(output);
                }
                Exchange::PreviousMissing => {
                    state.continuation = None;
                    state.socket = Some(socket);
                }
                Exchange::Reconnect(_) if attempt == 0 => {
                    state.continuation = None;
                }
                Exchange::Reconnect(message) => return Err(Error::Provider(message.into())),
            }
        }
        Err(Error::Provider("WebSocket retry exhausted".into()))
    }

    async fn session(&self, session_id: &str) -> Result<Arc<Mutex<SocketState>>> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(session_id) {
            return Ok(Arc::clone(session));
        }
        if sessions.len() >= MAX_SOCKET_SESSIONS {
            let idle = sessions
                .iter()
                .find(|(_, session)| Arc::strong_count(session) == 1)
                .map(|(id, _)| id.clone())
                .ok_or_else(|| {
                    Error::Provider(
                        format!(
                            "all {MAX_SOCKET_SESSIONS} WebSocket sessions are currently active"
                        )
                        .into(),
                    )
                })?;
            sessions.remove(&idle);
        }
        let session = Arc::new(Mutex::new(SocketState {
            socket: None,
            continuation: None,
        }));
        sessions.insert(session_id.to_string(), Arc::clone(&session));
        Ok(session)
    }
}

fn response_input<'a>(
    state: &mut SocketState,
    input: &'a [Value],
    allow_continuation: bool,
) -> Result<(Option<String>, &'a [Value])> {
    if allow_continuation {
        continuation_input(state, input)
    } else {
        state.continuation = None;
        Ok((None, input))
    }
}

impl Model for OpenAiSocket {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
        }
    }

    fn supports_image_input(&self) -> bool {
        true
    }

    fn respond<'a>(
        &'a self,
        request: ModelRequest<'a>,
        events: ModelEventSink,
    ) -> BoxFuture<'a, Result<ModelOutput>> {
        Box::pin(self.send_response(request, events))
    }

    fn compaction_endpoint(&self) -> bool {
        true
    }

    fn compact<'a>(&'a self, request: CompactRequest<'a>) -> BoxFuture<'a, Result<CompactOutput>> {
        self.http.compact(request)
    }
}

async fn connect(
    auth: &dyn OpenAiAuthorization,
    socket_url: &str,
    session_id: &str,
) -> Result<Socket> {
    let request = connection_request(auth, socket_url, session_id).await?;
    timeout(CONNECT_TIMEOUT, connect_async(request))
        .await
        .map_err(|_| Error::Provider("WebSocket connection timed out".into()))?
        .map(|(socket, _)| socket)
        .map_err(socket_error)
}

pub(super) async fn connection_request(
    auth: &dyn OpenAiAuthorization,
    socket_url: &str,
    session_id: &str,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
    let authorization = auth.authorize_websocket(session_id).await?;
    let mut request = socket_url.into_client_request().map_err(socket_error)?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", authorization.token))
            .map_err(|_| Error::Auth("access token is not a valid header value".into()))?,
    );
    for (name, value) in authorization.headers {
        let name = tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| Error::Auth(format!("{name} is not a valid header name")))?;
        request.headers_mut().insert(
            name,
            HeaderValue::from_str(&value)
                .map_err(|_| Error::Auth("authorization header value is invalid".into()))?,
        );
    }
    Ok(request)
}

fn response_body(
    model: &str,
    request: &ModelRequest<'_>,
    input: &[Value],
    previous_response_id: Option<&str>,
    reasoning_effort: Option<&str>,
    hosted_tools: &[Value],
) -> Result<Value> {
    let mut body = serde_json::json!({
        "type": "response.create",
        "model": model,
        "instructions": request.instructions,
        "input": wire_input(input, true)?,
        "tools": wire_tools(request.tools, hosted_tools, request.allow_hosted_tools),
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "include": ["reasoning.encrypted_content"],
        "store": false
    });
    if let Some(response_id) = previous_response_id {
        body["previous_response_id"] = Value::String(response_id.into());
    }
    if let Some(effort) = reasoning_effort {
        body["reasoning"] = serde_json::json!({"effort": effort, "summary": "auto"});
    }
    Ok(body)
}

fn continuation_input<'a>(
    state: &mut SocketState,
    input: &'a [Value],
) -> Result<(Option<String>, &'a [Value])> {
    let Some(continuation) = &state.continuation else {
        return Ok((None, input));
    };
    if continuation.known_items <= input.len()
        && fingerprint(input[..continuation.known_items].iter())? == continuation.fingerprint
    {
        return Ok((
            Some(continuation.response_id.clone()),
            &input[continuation.known_items..],
        ));
    }
    state.continuation = None;
    Ok((None, input))
}

async fn exchange(socket: &mut Socket, body: &Value, events: &ModelEventSink) -> Result<Exchange> {
    let message = Message::text(serde_json::to_string(body)?);
    if let Err(error) = socket.send(message).await {
        return Ok(Exchange::Reconnect(error.to_string()));
    }
    let mut web_searches = BTreeSet::new();
    let mut commentary = BTreeSet::new();
    let mut output = BTreeMap::new();
    let mut emitted = false;
    let mut stream_bytes = 0;
    loop {
        let next = match timeout(READ_TIMEOUT, socket.next()).await {
            Ok(next) => next,
            Err(_) if !emitted => {
                return Ok(Exchange::Reconnect("WebSocket read timed out".into()));
            }
            Err(_) => return Err(Error::Provider("WebSocket read timed out".into())),
        };
        let message = match next {
            Some(Ok(message)) => message,
            Some(Err(error)) if !emitted => return Ok(Exchange::Reconnect(error.to_string())),
            Some(Err(error)) => return Err(socket_error(error)),
            None if !emitted => return Ok(Exchange::Reconnect("WebSocket closed".into())),
            None => return Err(Error::Provider("WebSocket closed during response".into())),
        };
        if message.len() > MAX_SOCKET_MESSAGE_BYTES {
            return Err(Error::Provider(
                "WebSocket message exceeded size limit".into(),
            ));
        }
        account_stream_bytes(&mut stream_bytes, message.len(), "WebSocket")?;
        let event = match message {
            Message::Text(text) => serde_json::from_str(text.as_ref())?,
            Message::Binary(bytes) => serde_json::from_slice(&bytes)?,
            Message::Ping(bytes) => {
                socket
                    .send(Message::Pong(bytes))
                    .await
                    .map_err(socket_error)?;
                continue;
            }
            Message::Pong(_) | Message::Frame(_) => continue,
            Message::Close(_) if !emitted => {
                return Ok(Exchange::Reconnect("WebSocket closed".into()));
            }
            Message::Close(_) => {
                return Err(Error::Provider("WebSocket closed during response".into()));
            }
        };
        collect_stream_output(&event, &mut output)?;
        if emit_web_event(&event, &mut web_searches, events)? {
            emitted = true;
            continue;
        }
        if emit_reasoning_event(&event, events)? {
            emitted = true;
            continue;
        }
        if emit_text_event(&event, &mut commentary, events)? {
            emitted = true;
            continue;
        }
        match event.get("type").and_then(Value::as_str) {
            Some("response.completed") => {
                let response = event
                    .get("response")
                    .cloned()
                    .ok_or_else(|| Error::Provider("completion omitted response".into()))?;
                return Ok(Exchange::Completed(attach_stream_output(response, &output)));
            }
            Some("error" | "response.failed" | "response.incomplete") => {
                let code = event
                    .pointer("/error/code")
                    .or_else(|| event.pointer("/response/error/code"))
                    .and_then(Value::as_str);
                return match code {
                    Some("previous_response_not_found") => Ok(Exchange::PreviousMissing),
                    Some("websocket_connection_limit_reached") if !emitted => {
                        Ok(Exchange::Reconnect(response_error(&event)))
                    }
                    _ => Err(Error::Provider(response_error(&event).into())),
                };
            }
            _ => {}
        }
    }
}

fn fingerprint<'a>(items: impl IntoIterator<Item = &'a Value>) -> Result<u64> {
    let mut hasher = DefaultHasher::new();
    for item in items {
        let mut item_hasher = DefaultHasher::new();
        serde_json::to_writer(HasherWriter(&mut item_hasher), item)?;
        hasher.write_u64(item_hasher.finish());
    }
    Ok(hasher.finish())
}

struct HasherWriter<'a>(&'a mut DefaultHasher);

impl Write for HasherWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.write(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn socket_error(error: impl std::fmt::Display) -> Error {
    Error::Provider(format!("WebSocket: {error}").into())
}

pub(super) const MODELS: &[ModelPreset] = manifest::MODELS;
pub(super) const DEFAULT_MODEL: Option<&str> = manifest::DEFAULT_MODEL;
pub(super) const SEARCH: &[HostedWebSearch] = manifest::SEARCH;

pub(super) const fn provider() -> ProviderDefinition {
    ProviderDefinition::new(
        "openai_socket",
        manifest::PROVIDER_LABEL,
        FrontendSymbol::ChatGpt,
        manifest::PROVIDER_DESCRIPTION,
        ProviderAuth::ApiKey("OPENAI_API_KEY"),
        MODELS,
        DEFAULT_MODEL,
        SEARCH,
        build_provider,
    )
    .with_image_input()
}

fn build_provider(config: ProviderBuildConfig) -> Result<Arc<dyn Model>> {
    let api_key = config.credential.into_api_key("openai_socket")?;
    let provider = OpenAiSocket::with_client(api_key, config.model, config.http)?;
    let provider = match config.reasoning_effort {
        Some(effort) => provider.with_reasoning_effort(effort)?,
        None => provider,
    };
    let provider = match config.web_search {
        HostedWebSearch::Off => provider,
        HostedWebSearch::Cached => provider.with_cached_web_search(),
        HostedWebSearch::Live => provider.with_web_search(),
    };
    Ok(Arc::new(provider))
}

#[cfg(test)]
#[path = "openai_socket_tests.rs"]
mod tests;
