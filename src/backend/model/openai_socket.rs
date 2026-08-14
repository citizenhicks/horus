//! First-party OpenAI Responses WebSocket transport.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::io;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::SinkExt;
use futures_util::StreamExt;
use futures_util::future::join_all;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio::time::timeout;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::Error as WebSocketError;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use super::CompactOutput;
use super::CompactRequest;
use super::Model;
use super::ModelEventSink;
use super::ModelInfo;
use super::ModelOutput;
use super::ModelPricing;
use super::ModelRequest;
use super::PromptCacheCapability;
use super::openai::OpenAi;
use super::openai::attach_stream_output;
use super::openai::collect_stream_output;
use super::openai::decode_response;
use super::openai::emit_reasoning_event;
use super::openai::emit_text_event;
use super::openai::emit_web_event;
use super::openai::response_error;
use super::openai::wire_input_with_cache;
use super::openai::wire_tools;
use super::openai_auth::ApiKeyAuthorization;
use super::openai_auth::OpenAiAuthorization;
use super::openai_auth::ResolvedAuthorization;
use super::provider::HostedWebSearch;
use super::provider::ModelPreset;
use super::provider::ProviderAuth;
use super::provider::ProviderBuildConfig;
use super::provider::ProviderDefinition;
use super::transport::account_stream_bytes;
use crate::BoxFuture;
use crate::Error;
use crate::ProviderError;
use crate::Result;
use crate::protocol::FrontendSymbol;

mod manifest {
    include!(concat!(
        env!("OUT_DIR"),
        "/src_backend_model_openai_socket_manifest.rs"
    ));
}

const OPENAI_HTTP_URL: &str = "https://api.openai.com/v1";
const OPENAI_SOCKET_URL: &str = "wss://api.openai.com/v1/responses";
const MAX_SOCKET_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_SOCKET_SESSIONS: usize = 128;
const MAX_STREAM_EVENTS: usize = 65_536;
const SOCKET_COMMAND_CAPACITY: usize = 8;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const COMPACTION_STREAM_RETRY_LIMIT: usize = 2;
const COMPACTION_RETRY_BASE_DELAY: Duration = Duration::from_millis(200);

type RawSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// OpenAI's persistent Responses WebSocket transport.
pub struct OpenAiSocket {
    auth: Arc<dyn OpenAiAuthorization>,
    socket_url: String,
    model: String,
    reasoning_effort: Option<String>,
    hosted_tools: Vec<Value>,
    sessions: Mutex<BTreeMap<String, Arc<Mutex<SocketState>>>>,
    http: OpenAi,
}

struct SocketState {
    connection: Option<OpenAiWsConnection>,
    continuation: Option<Continuation>,
    use_http: bool,
    last_used_at: Instant,
}

struct Continuation {
    response_id: String,
    known_items: usize,
    fingerprint: u64,
    envelope_fingerprint: u64,
}

enum Exchange {
    Completed(Value),
    PreviousMissing { output_delivered: bool },
    Retry { retry_after: Option<String> },
    ConnectionLimit { retry_after: Option<String> },
    Reconnect,
}

struct OpenAiWsConnection {
    commands: mpsc::Sender<SocketCommand>,
    messages: mpsc::UnboundedReceiver<SocketEvent>,
    closed: Arc<AtomicBool>,
    pump: tokio::task::AbortHandle,
}

enum SocketCommand {
    Send {
        message: Message,
        result: oneshot::Sender<std::result::Result<(), ()>>,
    },
    Finish,
    Close {
        result: oneshot::Sender<()>,
    },
}

enum SocketEvent {
    Message(Message),
    ProtocolError(&'static str),
    Closed,
}

impl OpenAiSocket {
    /// Creates the first-party Responses transport with HTTP fallback.
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
        socket_url: impl Into<String>,
        model: impl Into<String>,
        client: reqwest::Client,
    ) -> Result<Self> {
        let model = model.into();
        let http = OpenAi::with_authorization(Arc::clone(&auth), http_url, model.clone(), client)?
            .with_explicit_prompt_cache();
        Ok(Self {
            auth,
            socket_url: socket_url.into(),
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
        self.http = self
            .http
            .with_reasoning_effort(effort.clone())?
            .with_reasoning_summary();
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
        // A connection and its continuation cursor form one ordered session exchange.
        let mut state = session.lock().await;
        state.last_used_at = Instant::now();
        if state.use_http {
            drop(state);
            return self.send_http_response(request, events).await;
        }

        let mut rebuilt_context = false;
        loop {
            let mut connection = match state.connection.take() {
                Some(connection) if connection.is_usable() => connection,
                stale => {
                    if let Some(connection) = stale {
                        connection.close().await;
                    }
                    state.continuation = None;
                    match connect(self.auth.as_ref(), &self.socket_url, request.session_id).await {
                        Ok(connection) => connection,
                        Err(Error::Provider(error)) if error.status() == Some(426) => {
                            state.use_http = true;
                            state.continuation = None;
                            drop(state);
                            return self.send_http_response(request, events).await;
                        }
                        Err(Error::Provider(error)) if error.is_stream_interrupted() => {
                            return Err(websocket_failure(
                                &mut state,
                                error.retry_after().map(str::to_owned),
                            ));
                        }
                        Err(error) => return Err(error),
                    }
                }
            };
            let envelope_fingerprint = envelope_fingerprint(
                &self.model,
                &request,
                self.reasoning_effort.as_deref(),
                &self.hosted_tools,
            )?;
            let (previous_response_id, input) = response_input(
                &mut state,
                request.input,
                request.allow_continuation,
                envelope_fingerprint,
            )?;
            let used_previous_response = previous_response_id.is_some();
            let body = response_body(
                &self.model,
                &request,
                input,
                previous_response_id.as_deref(),
                self.reasoning_effort.as_deref(),
                &self.hosted_tools,
            )?;
            match exchange(&mut connection, &body, &events).await? {
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
                            envelope_fingerprint,
                        })
                    } else {
                        None
                    };
                    state.last_used_at = Instant::now();
                    state.connection = Some(connection);
                    return Ok(output);
                }
                Exchange::PreviousMissing {
                    output_delivered: false,
                } if used_previous_response && !rebuilt_context => {
                    state.continuation = None;
                    state.connection = Some(connection);
                    rebuilt_context = true;
                }
                Exchange::PreviousMissing { .. } => {
                    state.continuation = None;
                    state.connection = Some(connection);
                    return Err(websocket_failure(&mut state, None));
                }
                Exchange::Retry { retry_after } => {
                    state.continuation = None;
                    state.connection = Some(connection);
                    return Err(websocket_failure(&mut state, retry_after));
                }
                Exchange::ConnectionLimit { retry_after } => {
                    state.continuation = None;
                    connection.close().await;
                    let error = websocket_failure(&mut state, retry_after);
                    drop(state);
                    self.close_idle_connections(request.session_id).await;
                    return Err(error);
                }
                Exchange::Reconnect => {
                    state.continuation = None;
                    drop(connection);
                    return Err(websocket_failure(&mut state, None));
                }
            }
        }
    }

    async fn send_http_response(
        &self,
        request: ModelRequest<'_>,
        events: ModelEventSink,
    ) -> Result<ModelOutput> {
        self.http
            .respond(request, events)
            .await
            .map_err(|error| match error {
                Error::Http(_) => Error::Provider("HTTPS fallback transport failed".into()),
                error => error,
            })
    }

    async fn compact_response(&self, request: CompactRequest<'_>) -> Result<CompactOutput> {
        let mut input = request.input.to_vec();
        input.push(serde_json::json!({"type": "compaction_trigger"}));
        let mut retries = 0;
        let output = loop {
            match self
                .send_response(
                    ModelRequest {
                        session_id: request.session_id,
                        prompt_cache: request.prompt_cache,
                        instructions: request.instructions,
                        input: &input,
                        tools: request.tools,
                        allow_hosted_tools: true,
                        allow_continuation: true,
                    },
                    Arc::new(|_| Ok(())),
                )
                .await
            {
                Ok(output) => break output,
                Err(Error::Provider(error))
                    if error.is_stream_interrupted() && retries < COMPACTION_STREAM_RETRY_LIMIT =>
                {
                    let delay = compaction_retry_delay(&error, retries);
                    retries += 1;
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        };
        let compaction = output
            .output()
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("compaction"))
            .cloned()
            .collect::<Vec<_>>();
        if compaction.len() != 1 {
            return Err(Error::Provider(
                format!(
                    "Responses compaction expected exactly one compaction item, got {}",
                    compaction.len()
                )
                .into(),
            ));
        }
        CompactOutput::from_output(compaction, output.usage().clone())
    }

    async fn session(&self, session_id: &str) -> Result<Arc<Mutex<SocketState>>> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(session_id) {
            return Ok(Arc::clone(session));
        }

        let mut close = Vec::new();
        let websocket_sessions = sessions
            .values()
            .filter(|session| session.try_lock().map_or(true, |state| !state.use_http))
            .count();
        if websocket_sessions >= MAX_SOCKET_SESSIONS {
            let idle = sessions
                .iter()
                .filter(|(_, session)| Arc::strong_count(session) == 1)
                .filter_map(|(id, session)| {
                    let state = session.try_lock().ok()?;
                    (!state.use_http).then_some((id.clone(), state.last_used_at))
                })
                .min_by_key(|(_, last_used_at)| *last_used_at)
                .map(|(id, _)| id);
            if let Some(idle) = idle {
                if let Some(session) = sessions.remove(&idle)
                    && let Ok(mut state) = session.try_lock()
                    && let Some(connection) = state.connection.take()
                {
                    close.push(connection);
                }
            } else {
                return Err(Error::Provider(
                    format!("all {MAX_SOCKET_SESSIONS} WebSocket sessions are currently active")
                        .into(),
                ));
            }
        }
        let session = Arc::new(Mutex::new(SocketState {
            connection: None,
            continuation: None,
            use_http: false,
            last_used_at: Instant::now(),
        }));
        sessions.insert(session_id.to_string(), Arc::clone(&session));
        drop(sessions);
        close_connections(close).await;
        Ok(session)
    }

    async fn close_idle_connections(&self, current_session_id: &str) {
        let mut close = Vec::new();
        let sessions = self.sessions.lock().await;
        for (session_id, session) in sessions.iter() {
            if session_id == current_session_id || Arc::strong_count(session) != 1 {
                continue;
            }
            let Ok(mut state) = session.try_lock() else {
                continue;
            };
            state.continuation = None;
            if let Some(connection) = state.connection.take() {
                close.push(connection);
            }
        }
        drop(sessions);
        close_connections(close).await;
    }
}

fn compaction_retry_delay(error: &crate::ProviderError, retry: usize) -> Duration {
    let backoff = COMPACTION_RETRY_BASE_DELAY.saturating_mul(1_u32 << retry.min(4));
    error
        .retry_after()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .map_or(backoff, |retry_after| retry_after.max(backoff))
}

async fn close_connections(connections: Vec<OpenAiWsConnection>) {
    join_all(connections.into_iter().map(OpenAiWsConnection::close)).await;
}

fn websocket_failure(state: &mut SocketState, retry_after: Option<String>) -> Error {
    state.last_used_at = Instant::now();
    Error::Provider(ProviderError::stream_interrupted(retry_after))
}

fn response_input<'a>(
    state: &mut SocketState,
    input: &'a [Value],
    allow_continuation: bool,
    envelope_fingerprint: u64,
) -> Result<(Option<String>, &'a [Value])> {
    if allow_continuation {
        continuation_input(state, input, envelope_fingerprint)
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

    fn prompt_cache_capability(&self) -> PromptCacheCapability {
        PromptCacheCapability::Explicit
    }

    fn pricing(&self) -> Option<ModelPricing> {
        self.http.pricing()
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
        Box::pin(self.compact_response(request))
    }
}

impl OpenAiWsConnection {
    fn new(socket: RawSocket) -> Self {
        let (commands, command_receiver) = mpsc::channel(SOCKET_COMMAND_CAPACITY);
        let (message_sender, messages) = mpsc::unbounded_channel();
        let closed = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(socket_pump(
            socket,
            command_receiver,
            message_sender,
            Arc::clone(&closed),
        ));
        Self {
            commands,
            messages,
            closed,
            pump: task.abort_handle(),
        }
    }

    fn is_usable(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
    }

    async fn start(&mut self, message: Message) -> std::result::Result<(), ()> {
        while self.messages.try_recv().is_ok() {}
        if !self.is_usable() {
            return Err(());
        }
        let (result, response) = oneshot::channel();
        if self
            .commands
            .try_send(SocketCommand::Send { message, result })
            .is_err()
        {
            self.retire();
            return Err(());
        }
        match timeout(SOCKET_IO_TIMEOUT, response).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) | Err(_) => {
                self.retire();
                Err(())
            }
        }
    }

    fn finish(&self) {
        if self.commands.try_send(SocketCommand::Finish).is_err() {
            self.retire();
        }
    }

    async fn close(self) {
        let (result, closed) = oneshot::channel();
        if self
            .commands
            .try_send(SocketCommand::Close { result })
            .is_ok()
        {
            let _ = timeout(SOCKET_IO_TIMEOUT, closed).await;
        }
        self.retire();
    }

    fn retire(&self) {
        self.closed.store(true, Ordering::Release);
        self.pump.abort();
    }
}

impl Drop for OpenAiWsConnection {
    fn drop(&mut self) {
        self.retire();
    }
}

async fn socket_pump(
    mut socket: RawSocket,
    mut commands: mpsc::Receiver<SocketCommand>,
    messages: mpsc::UnboundedSender<SocketEvent>,
    closed: Arc<AtomicBool>,
) {
    let mut active = false;
    let mut stream_bytes = 0;
    let mut stream_events = 0;
    let mut close_result = None;
    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(SocketCommand::Send { message, result }) if !active => {
                        let sent = matches!(
                            timeout(SOCKET_IO_TIMEOUT, socket.send(message)).await,
                            Ok(Ok(()))
                        );
                        active = sent;
                        stream_bytes = 0;
                        stream_events = 0;
                        let _ = result.send(if sent { Ok(()) } else { Err(()) });
                        if !sent {
                            break;
                        }
                    }
                    Some(SocketCommand::Send { result, .. }) => {
                        let _ = result.send(Err(()));
                    }
                    Some(SocketCommand::Finish) => {
                        active = false;
                    }
                    Some(SocketCommand::Close { result }) => {
                        close_result = Some(result);
                        break;
                    }
                    None => break,
                }
            }
            message = socket.next() => {
                match message {
                    Some(Ok(Message::Ping(payload))) => {
                        if !matches!(
                            timeout(SOCKET_IO_TIMEOUT, socket.send(Message::Pong(payload))).await,
                            Ok(Ok(()))
                        ) {
                            if active {
                                let _ = messages.send(SocketEvent::Closed);
                            }
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
                    Some(Ok(message @ (Message::Text(_) | Message::Binary(_)))) if active => {
                        if message.len() > MAX_SOCKET_MESSAGE_BYTES {
                            let _ = messages.send(SocketEvent::ProtocolError(
                                "WebSocket message exceeded size limit",
                            ));
                            break;
                        }
                        stream_events += 1;
                        if stream_events > MAX_STREAM_EVENTS
                            || account_stream_bytes(
                                &mut stream_bytes,
                                message.len(),
                                "WebSocket",
                            )
                            .is_err()
                        {
                            let _ = messages.send(SocketEvent::ProtocolError(
                                "WebSocket response exceeded size limit",
                            ));
                            break;
                        }
                        if messages.send(SocketEvent::Message(message)).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(_) | Message::Binary(_))) => {}
                    Some(Err(WebSocketError::Capacity(_))) => {
                        if active {
                            let _ = messages.send(SocketEvent::ProtocolError(
                                "WebSocket message exceeded size limit",
                            ));
                        }
                        break;
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                        if active {
                            let _ = messages.send(SocketEvent::Closed);
                        }
                        break;
                    }
                }
            }
        }
    }
    closed.store(true, Ordering::Release);
    drop(messages);
    let _ = timeout(SOCKET_IO_TIMEOUT, socket.close(None)).await;
    if let Some(result) = close_result {
        let _ = result.send(());
    }
}

async fn connect(
    auth: &dyn OpenAiAuthorization,
    socket_url: &str,
    session_id: &str,
) -> Result<OpenAiWsConnection> {
    for attempt in 0..2 {
        let authorization = auth.authorize_websocket(session_id).await?;
        let rejected_token = authorization.token.clone();
        let request = connection_request(socket_url, authorization)?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_SOCKET_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_SOCKET_MESSAGE_BYTES));
        let result = timeout(
            CONNECT_TIMEOUT,
            connect_async_with_config(request, Some(config), false),
        )
        .await
        .map_err(|_| Error::Provider(ProviderError::stream_interrupted(None)))?;
        match result {
            Ok((socket, _)) => return Ok(OpenAiWsConnection::new(socket)),
            Err(error) if attempt == 0 && unauthorized(&error) => {
                if auth.recover_unauthorized(&rejected_token).await? {
                    continue;
                }
                return Err(Error::Auth("WebSocket authorization was rejected".into()));
            }
            Err(error) if unauthorized(&error) => {
                return Err(Error::Auth("WebSocket authorization was rejected".into()));
            }
            Err(error) => return Err(websocket_connect_error(error)),
        }
    }
    unreachable!("WebSocket authorization retry is bounded")
}

fn websocket_connect_error(error: WebSocketError) -> Error {
    if let WebSocketError::Http(response) = error {
        let status = response.status();
        let retry_after = response
            .headers()
            .get(tokio_tungstenite::tungstenite::http::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        return Error::Provider(ProviderError::http(
            format!("WebSocket HTTP {status}"),
            status.as_u16(),
            retry_after,
        ));
    }
    Error::Provider(ProviderError::stream_interrupted(None))
}

fn connection_request(
    socket_url: &str,
    authorization: ResolvedAuthorization,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
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

fn unauthorized(error: &tokio_tungstenite::tungstenite::Error) -> bool {
    matches!(
        error,
        tokio_tungstenite::tungstenite::Error::Http(response)
            if response.status()
                == tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED
    )
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
        "input": wire_input_with_cache(input, true, true)?,
        "tools": wire_tools(request.tools, hosted_tools, request.allow_hosted_tools),
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "include": ["reasoning.encrypted_content"],
        "store": false
    });
    if let Some(prompt_cache) = request.prompt_cache {
        body["prompt_cache_key"] = Value::String(prompt_cache.key.into());
    }
    body["prompt_cache_options"] = serde_json::json!({"mode": "explicit"});
    if let Some(response_id) = previous_response_id {
        body["previous_response_id"] = Value::String(response_id.into());
    }
    if let Some(effort) = reasoning_effort {
        body["reasoning"] = serde_json::json!({"effort": effort, "summary": "auto"});
    }
    Ok(body)
}

fn envelope_fingerprint(
    model: &str,
    request: &ModelRequest<'_>,
    reasoning_effort: Option<&str>,
    hosted_tools: &[Value],
) -> Result<u64> {
    let envelope = serde_json::json!({
        "model": model,
        "instructions": request.instructions,
        "tools": wire_tools(request.tools, hosted_tools, request.allow_hosted_tools),
        "reasoning_effort": reasoning_effort,
        "prompt_cache": request.prompt_cache.map(|cache| {
            serde_json::json!({
                "key": cache.key,
                "context_epoch": cache.context_epoch,
                "mode": "explicit"
            })
        })
    });
    fingerprint(std::iter::once(&envelope))
}

fn continuation_input<'a>(
    state: &mut SocketState,
    input: &'a [Value],
    envelope_fingerprint: u64,
) -> Result<(Option<String>, &'a [Value])> {
    let Some(continuation) = &state.continuation else {
        return Ok((None, input));
    };
    if continuation.envelope_fingerprint == envelope_fingerprint
        && continuation.known_items <= input.len()
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

async fn exchange(
    connection: &mut OpenAiWsConnection,
    body: &Value,
    events: &ModelEventSink,
) -> Result<Exchange> {
    let message = Message::text(serde_json::to_string(body)?);
    if connection.start(message).await.is_err() {
        return Ok(Exchange::Reconnect);
    }
    let result = read_exchange(&mut connection.messages, events).await;
    connection.finish();
    result
}

async fn read_exchange(
    messages: &mut mpsc::UnboundedReceiver<SocketEvent>,
    events: &ModelEventSink,
) -> Result<Exchange> {
    let mut web_searches = BTreeSet::new();
    let mut commentary = BTreeSet::new();
    let mut reasoning_part = None;
    let mut output = BTreeMap::new();
    let mut stream_bytes = 0;
    let output_delivered = Arc::new(AtomicBool::new(false));
    let tracked_delivery = Arc::clone(&output_delivered);
    let downstream = Arc::clone(events);
    let tracked_events: ModelEventSink = Arc::new(move |event| {
        downstream(event)?;
        tracked_delivery.store(true, Ordering::Release);
        Ok(())
    });
    loop {
        let message = match timeout(STREAM_IDLE_TIMEOUT, messages.recv()).await {
            Ok(Some(SocketEvent::Message(message))) => message,
            Ok(Some(SocketEvent::ProtocolError(message))) => {
                return Err(Error::Provider(message.into()));
            }
            Ok(Some(SocketEvent::Closed) | None) | Err(_) => return Ok(Exchange::Reconnect),
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
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
            Message::Close(_) => return Ok(Exchange::Reconnect),
        };
        collect_stream_output(&event, &mut output)?;
        if emit_web_event(&event, &mut web_searches, &tracked_events)? {
            continue;
        }
        if emit_reasoning_event(&event, &mut reasoning_part, &tracked_events)? {
            continue;
        }
        if emit_text_event(&event, &mut commentary, &tracked_events)? {
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
                return failed_exchange(&event, output_delivered.load(Ordering::Acquire));
            }
            _ => {}
        }
    }
}

fn failed_exchange(event: &Value, output_delivered: bool) -> Result<Exchange> {
    let code = response_error_code(event);
    let message = response_error(event);
    let retry_after = response_retry_after(event);
    match code {
        Some("previous_response_not_found") => Ok(Exchange::PreviousMissing { output_delivered }),
        Some("websocket_connection_limit_reached") => Ok(Exchange::ConnectionLimit { retry_after }),
        _ if retryable_response_error(code, &message) => Ok(Exchange::Retry { retry_after }),
        _ => Err(Error::Provider(message.into())),
    }
}

fn response_retry_after(event: &Value) -> Option<String> {
    [
        "/headers/retry-after",
        "/headers/retry_after",
        "/error/retry_after",
        "/response/error/retry_after",
    ]
    .into_iter()
    .find_map(|pointer| event.pointer(pointer))
    .and_then(|value| match value {
        Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    })
}

fn response_error_code(event: &Value) -> Option<&str> {
    event
        .pointer("/error/code")
        .or_else(|| event.pointer("/response/error/code"))
        .or_else(|| event.pointer("/error/type"))
        .or_else(|| event.pointer("/response/error/type"))
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty())
}

fn retryable_response_error(code: Option<&str>, message: &str) -> bool {
    matches!(
        code,
        Some(
            "server_error"
                | "internal_server_error"
                | "rate_limit_exceeded"
                | "service_unavailable"
                | "temporarily_unavailable"
        )
    ) || (message.starts_with("An error occurred while processing your request.")
        && message.contains("retry your request"))
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
