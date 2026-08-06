//! Versioned, bounded JSON frames shared by gateway clients and the server.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use futures_util::{Sink, SinkExt as _, Stream, StreamExt as _};
use horus::backend::checkpoint::SessionSummary;
use horus::backend::model::ModelChoice;
use horus::backend::model::provider::HostedWebSearch;
use horus::backend::sandbox::ApprovalPolicy;
use horus::protocol::{
    Event, EventMsg, FrontendBlock, FrontendContribution, FrontendSettingValue, MiddlewareFeature,
    SessionConfiguredEvent, Submission, TokenUsage,
};
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio_tungstenite::tungstenite::error::Error as WebSocketError;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::{Error, Result};

/// Current gateway protocol version.
pub const PROTOCOL_VERSION: u16 = 9;
/// Maximum encoded JSON payload accepted in one frame.
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;

/// Cancellation-safe reader for length-prefixed gateway frames.
pub struct FrameReader<R> {
    reader: R,
    buffer: Vec<u8>,
}

impl<R> FrameReader<R> {
    /// Wraps one transport reader and retains partial frames between reads.
    pub const fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
        }
    }
}

/// One client-to-gateway frame.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientFrame {
    pub version: u16,
    #[serde(flatten)]
    pub message: ClientMessage,
}

impl<'de> Deserialize<'de> for ClientFrame {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let (version, message) = deserialize_frame(deserializer)?;
        let message = serde_json::from_value(message).map_err(D::Error::custom)?;
        Ok(Self { version, message })
    }
}

impl ClientFrame {
    /// Wraps a message in the current protocol version.
    #[must_use]
    pub const fn new(message: ClientMessage) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            message,
        }
    }
}

/// Authenticated operations accepted by the gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClientMessage {
    Pair {
        code: String,
        client_label: String,
        client_kind: ClientKind,
    },
    Authenticate {
        token: String,
        client_kind: ClientKind,
    },
    ListClients {
        request_id: String,
    },
    UnpairClient {
        request_id: String,
        client_id: String,
    },
    ListSessions {
        request_id: String,
    },
    CreateSession {
        request_id: String,
        workspace: PathBuf,
    },
    OpenSession {
        request_id: String,
        session_id: String,
        last_sequence: Option<u64>,
        replay_epoch: Option<String>,
    },
    RenameSession {
        request_id: String,
        session_id: String,
        title: String,
    },
    SetSessionPinned {
        request_id: String,
        session_id: String,
        pinned: bool,
    },
    DeleteSession {
        request_id: String,
        session_id: String,
    },
    Submit {
        session_id: String,
        submission: Submission,
    },
    ConfigureSession {
        request_id: String,
        session_id: String,
        expected_revision: u64,
        config: AgentComposition,
    },
    ConfigureDefaultAgent {
        request_id: String,
        expected_revision: u64,
        config: AgentComposition,
    },
    GetGitDiff {
        request_id: String,
        session_id: String,
    },
    SwitchGitBranch {
        request_id: String,
        session_id: String,
        branch: String,
    },
    ListDirectories {
        request_id: String,
        path: PathBuf,
        include_files: bool,
    },
    SetProviderCredential {
        request_id: String,
        provider: String,
        api_key: String,
    },
    SetProviderEndpointCredential {
        request_id: String,
        provider: String,
        base_url: String,
        api_key: String,
    },
    RegisterProvider {
        request_id: String,
        config: ProviderConfig,
    },
    CreatePairingCode {
        request_id: String,
    },
    StartProviderLogin {
        request_id: String,
        provider: String,
    },
    GetProfile {
        request_id: String,
    },
    ListArtifacts {
        request_id: String,
        session_id: String,
    },
    StartCronSetup {
        request_id: String,
        session_id: String,
        task: Option<String>,
    },
    ListCron {
        request_id: String,
        session_id: String,
    },
    RescheduleCron {
        request_id: String,
        session_id: String,
        id: String,
        schedule: String,
    },
    DeleteCron {
        request_id: String,
        session_id: String,
        id: String,
    },
    RunCron {
        request_id: String,
        session_id: String,
        id: String,
    },
    ListCronHistory {
        request_id: String,
        session_id: String,
        id: Option<String>,
    },
}

/// One gateway-to-client frame.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServerFrame {
    pub version: u16,
    #[serde(flatten)]
    pub message: ServerMessage,
}

impl<'de> Deserialize<'de> for ServerFrame {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let (version, message) = deserialize_frame(deserializer)?;
        let message = serde_json::from_value(message).map_err(D::Error::custom)?;
        Ok(Self { version, message })
    }
}

impl ServerFrame {
    /// Wraps a message in the current protocol version.
    #[must_use]
    pub const fn new(message: ServerMessage) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            message,
        }
    }
}

/// Results and broadcasts emitted by the gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ServerMessage {
    Paired {
        client_id: String,
        token: String,
    },
    Authenticated,
    Ready {
        payload: ReadyPayload,
    },
    SessionOpened {
        request_id: String,
        payload: SessionReadyPayload,
    },
    SessionReplayComplete {
        request_id: String,
        session_id: String,
    },
    SessionChanged {
        payload: SessionReadyPayload,
    },
    GatewayConfigured {
        request_id: String,
        payload: ReadyPayload,
    },
    Accepted {
        request_id: String,
    },
    Rejected {
        request_id: String,
        code: String,
        message: String,
        fatal: bool,
    },
    AgentEvent {
        session_id: String,
        sequence: u64,
        event: Event,
        blocks: Vec<FrontendBlock>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        history: Option<Vec<RenderedEvent>>,
        preview: Option<RenderedPreview>,
    },
    Sessions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        sessions: Vec<SessionRecord>,
    },
    Clients {
        request_id: String,
        current_client_id: String,
        clients: Vec<ClientStatus>,
    },
    ProviderCredentialStatus {
        request_id: String,
        provider: String,
        configured: bool,
    },
    PairingCode {
        request_id: String,
        code: String,
        expires_at: i64,
    },
    ProviderLoginStarted {
        request_id: String,
        login_id: String,
        provider: String,
        verification_url: String,
        user_code: String,
    },
    ProviderLoginFinished {
        request_id: String,
        login_id: String,
        provider: String,
    },
    Profile {
        request_id: String,
        profile: ProfileSnapshot,
    },
    Artifacts {
        request_id: String,
        session_id: String,
        artifacts: Vec<ArtifactRecord>,
    },
    GitDiff {
        request_id: String,
        session_id: String,
        diff: String,
    },
    Directories {
        request_id: String,
        listing: DirectoryListing,
    },
    CronTasks {
        request_id: String,
        session_id: String,
        tasks: Vec<CronTask>,
    },
    CronHistory {
        request_id: String,
        session_id: String,
        runs: Vec<CronRun>,
    },
    Error {
        code: String,
        message: String,
        fatal: bool,
    },
}

/// Gateway-wide frontend-safe state sent after authentication.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadyPayload {
    pub sessions: Vec<SessionRecord>,
    pub providers: Vec<ProviderStatus>,
    pub default_config: Option<VersionedAgentConfig>,
    pub models: Vec<ModelChoice>,
    pub middleware_features: Vec<MiddlewareFeature>,
    pub max_active_sessions: usize,
}

/// Frontend-safe state for one opened session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionReadyPayload {
    pub replay_epoch: String,
    pub latest_sequence: u64,
    pub workspace: WorkspaceInfo,
    pub git: Option<GitStatus>,
    pub session: SessionConfiguredEvent,
    pub contributions: Vec<FrontendContribution>,
    pub tool_count: usize,
    pub config: VersionedAgentConfig,
}

/// One visible session with gateway-owned catalog presentation metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub title: Option<String>,
    pub pinned: bool,
    pub activity: SessionActivity,
}

/// Gateway-observed lifecycle state for one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionActivity {
    pub state: SessionActivityState,
    pub turn_id: Option<String>,
    pub started_at: Option<i64>,
    pub last_outcome: Option<SessionOutcome>,
    pub message: Option<String>,
}

impl Default for SessionActivity {
    fn default() -> Self {
        Self {
            state: SessionActivityState::Idle,
            turn_id: None,
            started_at: None,
            last_outcome: None,
            message: None,
        }
    }
}

/// Current work state advertised in the session catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActivityState {
    Idle,
    Running,
    AwaitingApproval,
}

/// Most recent terminal outcome advertised in the session catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOutcome {
    Completed,
    Aborted,
    Failed,
}

/// Canonical workspace identity and path for one chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: String,
    pub path: PathBuf,
}

/// Local branch state for a Git-backed workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatus {
    pub current_branch: String,
    pub branches: Vec<String>,
}

/// One bounded folder listing from the gateway host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryListing {
    pub path: PathBuf,
    pub parent: Option<PathBuf>,
    pub entries: Vec<DirectoryEntry>,
}

/// A selectable child folder on the gateway host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_directory: bool,
}

/// A frontend-safe agent composition guarded by an optimistic revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionedAgentConfig {
    pub revision: u64,
    pub config: AgentComposition,
}

/// Runtime settings an authenticated client may read and replace atomically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentComposition {
    pub provider: ProviderConfig,
    pub middleware: MiddlewareConfig,
    pub approval: ApprovalPolicy,
    pub system_prompt: String,
}

/// Provider and model settings. Credentials are resolved only on the gateway host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub web_search: HostedWebSearch,
}

/// Credential availability exposed without returning credential material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub provider: String,
    pub label: String,
    pub symbol: String,
    pub description: String,
    pub configured: bool,
    pub selection: Option<ProviderConfig>,
    pub auth: ProviderAuthKind,
    pub default_base_url: Option<String>,
    pub default_api_key_env: Option<String>,
    pub models: Vec<ProviderModel>,
    pub web_search: Vec<HostedWebSearch>,
}

/// Frontend type attached to one authenticated connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Cli,
    Macos,
    Ios,
    Ipados,
    GatewayDashboard,
}

/// One paired client and its current connection state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientStatus {
    pub client_id: String,
    pub label: String,
    pub kinds: Vec<ClientKind>,
    pub connections: usize,
}

impl ProviderStatus {
    #[must_use]
    pub fn configurable_base_url(&self) -> bool {
        self.default_base_url.is_some()
    }

    #[must_use]
    pub fn default_model(&self) -> Option<&ProviderModel> {
        self.models.first()
    }
}

/// One model advertised by a provider manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModel {
    pub id: String,
    pub label: String,
    pub description: String,
    pub context_window: i64,
    pub reasoning: Vec<ReasoningChoice>,
    pub default_reasoning: Option<String>,
}

/// One reasoning effort advertised for a provider model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningChoice {
    pub id: String,
    pub label: String,
    pub description: String,
}

/// Frontend-safe provider authentication mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthKind {
    ApiKey,
    DeviceCode,
}

/// Enabled optional middleware IDs and their schema-backed settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareConfig {
    pub(crate) enabled: BTreeSet<String>,
    pub settings: BTreeMap<String, BTreeMap<String, FrontendSettingValue>>,
}

impl MiddlewareConfig {
    /// Returns whether one advertised optional middleware is enabled.
    #[must_use]
    pub fn enabled(&self, id: &str) -> bool {
        self.enabled.contains(id)
    }

    /// Updates one advertised optional middleware before gateway validation.
    pub fn set_enabled(&mut self, id: impl Into<String>, enabled: bool) {
        let id = id.into();
        if enabled {
            self.enabled.insert(id);
        } else {
            self.enabled.remove(&id);
        }
    }

    /// Returns one advertised middleware setting.
    #[must_use]
    pub fn setting(&self, middleware: &str, setting: &str) -> Option<&FrontendSettingValue> {
        self.settings.get(middleware)?.get(setting)
    }

    /// Sets or clears one advertised middleware setting before gateway validation.
    pub fn set_setting(
        &mut self,
        middleware: impl Into<String>,
        setting: impl Into<String>,
        value: Option<FrontendSettingValue>,
    ) {
        let middleware = middleware.into();
        let setting = setting.into();
        if let Some(value) = value {
            self.settings
                .entry(middleware)
                .or_default()
                .insert(setting, value);
        } else if let Some(settings) = self.settings.get_mut(&middleware) {
            settings.remove(&setting);
            if settings.is_empty() {
                self.settings.remove(&middleware);
            }
        }
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = &str> {
        self.enabled.iter().map(String::as_str)
    }
}

/// Capability-rendered preview whose inner events remain provider-neutral.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderedPreview {
    pub title: String,
    pub events: Vec<RenderedEvent>,
}

/// One preview event and its capability-rendered blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderedEvent {
    pub event: EventMsg,
    pub blocks: Vec<FrontendBlock>,
}

/// Gateway-owned profile and aggregate usage information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSnapshot {
    pub user_name: Option<String>,
    pub daily_usage: Vec<DailyUsage>,
}

/// Usage accrued during one Unix day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyUsage {
    pub unix_day: u64,
    pub usage: TokenUsage,
}

/// A reusable artifact emitted for one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub id: String,
    pub session_id: String,
    pub kind: ArtifactKind,
    pub title: String,
    pub block: FrontendBlock,
}

/// Frontend-neutral artifact category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    CodeDiff,
}

/// One persisted scheduled task owned by its source session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronTask {
    pub id: String,
    pub session_id: String,
    pub task: PathBuf,
    pub schedule: String,
}

/// One completed or active invocation of a scheduled task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronRun {
    pub id: String,
    pub task_id: String,
    pub source_session_id: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub status: CronRunStatus,
    pub session_id: Option<String>,
    pub message: Option<String>,
}

/// Durable state of one cron invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronRunStatus {
    Running,
    Succeeded,
    Failed,
    Skipped,
}

fn deserialize_frame<'de, D>(deserializer: D) -> std::result::Result<(u16, Value), D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Value::Object(mut object) = Value::deserialize(deserializer)? else {
        return Err(D::Error::custom("gateway frame must be a JSON object"));
    };
    let version = object
        .remove("version")
        .ok_or_else(|| D::Error::missing_field("version"))?;
    let version = serde_json::from_value(version).map_err(D::Error::custom)?;
    Ok((version, Value::Object(object)))
}

/// Reads one length-prefixed JSON value, returning `None` only for a clean EOF.
pub async fn read_frame<T>(reader: &mut FrameReader<impl AsyncRead + Unpin>) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    loop {
        if reader.buffer.len() >= 4 {
            let prefix = reader.buffer[..4]
                .try_into()
                .map_err(|_| Error::Protocol("frame length is unsupported".into()))?;
            let length = usize::try_from(u32::from_be_bytes(prefix))
                .map_err(|_| Error::Protocol("frame length is unsupported".into()))?;
            if length == 0 || length > MAX_FRAME_BYTES {
                return Err(Error::Protocol(format!(
                    "frame length must be 1–{MAX_FRAME_BYTES} bytes"
                )));
            }
            let frame_end = 4 + length;
            if reader.buffer.len() >= frame_end {
                let frame = serde_json::from_slice(&reader.buffer[4..frame_end])?;
                reader.buffer.drain(..frame_end);
                return Ok(Some(frame));
            }
        }
        let mut chunk = [0_u8; 8 * 1024];
        let read = reader.reader.read(&mut chunk).await?;
        if read == 0 {
            if reader.buffer.is_empty() {
                return Ok(None);
            }
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
        }
        reader.buffer.extend_from_slice(&chunk[..read]);
    }
}

/// Writes one bounded length-prefixed JSON value.
pub async fn write_frame<T>(writer: &mut (impl AsyncWrite + Unpin), value: &T) -> Result<()>
where
    T: Serialize,
{
    let payload = serde_json::to_vec(value)?;
    if payload.is_empty() || payload.len() > MAX_FRAME_BYTES {
        return Err(Error::Protocol(format!(
            "encoded frame must be 1–{MAX_FRAME_BYTES} bytes"
        )));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| Error::Protocol("encoded frame length is unsupported".into()))?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

pub(crate) async fn websocket_to_framed(
    mut incoming: impl Stream<Item = std::result::Result<Message, WebSocketError>> + Unpin,
    mut writer: impl AsyncWrite + Unpin,
) -> Result<()> {
    while let Some(message) = incoming.next().await {
        match message.map_err(websocket_error)? {
            Message::Binary(payload) if (1..=MAX_FRAME_BYTES).contains(&payload.len()) => {
                let length = u32::try_from(payload.len())
                    .map_err(|_| Error::Protocol("WebSocket message is too large".into()))?;
                writer.write_all(&length.to_be_bytes()).await?;
                writer.write_all(&payload).await?;
            }
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Close(_) => return Ok(()),
            Message::Binary(payload) => {
                return Err(Error::Protocol(format!(
                    "WebSocket message length must be 1–{MAX_FRAME_BYTES} bytes, got {}",
                    payload.len()
                )));
            }
            Message::Text(_) | Message::Frame(_) => {
                return Err(Error::Protocol(
                    "WebSocket messages must be binary JSON frames".into(),
                ));
            }
        }
    }
    Ok(())
}

pub(crate) async fn framed_to_websocket(
    mut reader: impl AsyncRead + Unpin,
    mut outgoing: impl Sink<Message, Error = WebSocketError> + Unpin,
) -> Result<()> {
    loop {
        let mut prefix = [0_u8; 4];
        if reader.read(&mut prefix[..1]).await? == 0 {
            return outgoing.close().await.map_err(websocket_error);
        }
        reader.read_exact(&mut prefix[1..]).await?;
        let length = usize::try_from(u32::from_be_bytes(prefix))
            .map_err(|_| Error::Protocol("frame length is unsupported".into()))?;
        if length == 0 || length > MAX_FRAME_BYTES {
            return Err(Error::Protocol(format!(
                "frame length must be 1–{MAX_FRAME_BYTES} bytes"
            )));
        }
        let mut payload = vec![0_u8; length];
        reader.read_exact(&mut payload).await?;
        outgoing
            .send(Message::Binary(payload.into()))
            .await
            .map_err(websocket_error)?;
    }
}

pub(crate) fn websocket_error(error: WebSocketError) -> Error {
    Error::Protocol(format!("WebSocket transport failed: {error}"))
}

/// Rejects frames from incompatible clients before interpreting their message.
pub fn validate_version(version: u16) -> Result<()> {
    if version != PROTOCOL_VERSION {
        return Err(Error::Protocol(format!(
            "unsupported protocol version {version}; expected {PROTOCOL_VERSION}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncWriteExt as _, duplex};

    use super::*;

    #[tokio::test]
    async fn framed_json_round_trip_preserves_the_versioned_message() {
        let expected = ClientFrame::new(ClientMessage::Authenticate {
            token: "secret".into(),
            client_kind: ClientKind::Cli,
        });
        let (mut writer, reader) = duplex(1024);
        let mut reader = FrameReader::new(reader);

        write_frame(&mut writer, &expected)
            .await
            .expect("write frame");
        let actual: ClientFrame = read_frame(&mut reader)
            .await
            .expect("read frame")
            .expect("frame");

        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn websocket_bridge_rejects_text_messages() {
        let incoming = futures_util::stream::iter([Ok(Message::Text("{}".into()))]);
        let (writer, _reader) = duplex(64);

        let error = websocket_to_framed(incoming, writer)
            .await
            .expect_err("text message must fail");

        assert!(error.to_string().contains("must be binary"));
    }

    #[test]
    fn connection_handshakes_have_no_session_replay_cursor() {
        let frames = [
            ClientMessage::Authenticate {
                token: "secret".into(),
                client_kind: ClientKind::Cli,
            },
            ClientMessage::Pair {
                code: "pairing-code".into(),
                client_label: "client".into(),
                client_kind: ClientKind::Macos,
            },
        ];

        let has_cursor = frames
            .into_iter()
            .map(ClientFrame::new)
            .map(|frame| serde_json::to_value(frame).expect("encode handshake"))
            .any(|value| value.get("last_sequence").is_some());

        assert!(!has_cursor);
    }

    #[tokio::test]
    async fn framed_reader_retains_a_partial_prefix_when_cancelled() {
        let first = ClientFrame::new(ClientMessage::ListCron {
            request_id: "request-a".into(),
            session_id: "session-a".into(),
        });
        let second = ClientFrame::new(ClientMessage::GetProfile {
            request_id: "request-b".into(),
        });
        let encode = |frame: &ClientFrame| {
            let payload = serde_json::to_vec(frame).expect("encode frame");
            let mut encoded = u32::try_from(payload.len())
                .expect("frame length")
                .to_be_bytes()
                .to_vec();
            encoded.extend_from_slice(&payload);
            encoded
        };
        let mut encoded = encode(&first);
        encoded.extend_from_slice(&encode(&second));
        let (mut writer, reader) = duplex(4096);
        let mut reader = FrameReader::new(reader);
        writer
            .write_all(&encoded[..1])
            .await
            .expect("write partial prefix");

        {
            let pending = read_frame::<ClientFrame>(&mut reader);
            tokio::pin!(pending);
            tokio::select! {
                biased;
                result = &mut pending => panic!("partial frame completed: {result:?}"),
                () = tokio::task::yield_now() => {}
            }
        }
        writer.write_all(&encoded[1..]).await.expect("finish frame");
        let actual_first = read_frame::<ClientFrame>(&mut reader)
            .await
            .expect("read resumed frame")
            .expect("frame");
        let actual_second = read_frame::<ClientFrame>(&mut reader)
            .await
            .expect("read buffered frame")
            .expect("frame");

        assert_eq!([actual_first, actual_second], [first, second]);
    }

    #[test]
    fn client_frame_round_trip_handles_a_nested_operation_tag() {
        let expected = ClientFrame::new(ClientMessage::Submit {
            session_id: "session-a".into(),
            submission: Submission {
                id: "submission-a".into(),
                op: horus::protocol::Op::CapabilityCommand {
                    capability: "subagents".into(),
                    command: "subagents".into(),
                    arguments: String::new(),
                    target: None,
                },
            },
        });

        let encoded = serde_json::to_vec(&expected).expect("encode nested submission");
        let actual: ClientFrame =
            serde_json::from_slice(&encoded).expect("decode nested submission");

        assert_eq!(actual, expected);
    }

    #[test]
    fn protocol_v9_rejects_an_untargeted_legacy_capability_shape() {
        let frame = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "type": "submit",
            "session_id": "session-a",
            "submission": {
                "id": "submission-a",
                "op": {
                    "type": "capability_command",
                    "capability": "sessions",
                    "command": "fork",
                    "arguments": ""
                }
            }
        });

        let error = serde_json::from_value::<ClientFrame>(frame)
            .expect_err("v9 capability commands require an explicit target field");

        assert!(error.to_string().contains("missing field `target`"));
    }

    #[test]
    fn session_creation_uses_a_gateway_host_workspace() {
        let frame = ClientFrame::new(ClientMessage::CreateSession {
            request_id: "request-a".into(),
            workspace: PathBuf::from("/srv/horus/project"),
        });

        let encoded = serde_json::to_value(frame).expect("encode session creation");

        assert_eq!(
            encoded,
            serde_json::json!({
                "version": PROTOCOL_VERSION,
                "type": "create_session",
                "request_id": "request-a",
                "workspace": "/srv/horus/project"
            })
        );
    }

    #[test]
    fn provider_registration_is_gateway_scoped() {
        let frame = ClientFrame::new(ClientMessage::RegisterProvider {
            request_id: "request-provider".into(),
            config: ProviderConfig {
                provider: "kimi".into(),
                model: "kimi-k3".into(),
                base_url: None,
                reasoning_effort: Some("max".into()),
                web_search: HostedWebSearch::Off,
            },
        });

        let encoded = serde_json::to_value(frame).expect("encode provider registration");

        assert_eq!(encoded["type"], "register_provider");
        assert_eq!(encoded["config"]["provider"], "kimi");
        assert!(encoded.get("session_id").is_none());
    }

    #[test]
    fn provider_config_rejects_api_key_environment_overrides() {
        let encoded = serde_json::json!({
            "provider": "kimi",
            "model": "kimi-k3",
            "api_key_env": "CUSTOM_KIMI_API_KEY"
        });

        let error = serde_json::from_value::<ProviderConfig>(encoded)
            .expect_err("provider environment overrides must be rejected");

        assert!(error.to_string().contains("unknown field `api_key_env`"));
    }

    #[test]
    fn opening_a_session_owns_its_replay_cursor() {
        let frame = ClientFrame::new(ClientMessage::OpenSession {
            request_id: "request-open".into(),
            session_id: "session-a".into(),
            last_sequence: Some(7),
            replay_epoch: Some("epoch-a".into()),
        });

        let encoded = serde_json::to_value(frame).expect("encode session open");

        assert_eq!(encoded["last_sequence"], 7);
        assert_eq!(encoded["replay_epoch"], "epoch-a");
    }

    #[test]
    fn cron_setup_is_an_explicit_correlated_request() {
        let frame = ClientFrame::new(ClientMessage::StartCronSetup {
            request_id: "request-cron".into(),
            session_id: "session-a".into(),
            task: Some("Review open pull requests".into()),
        });

        let encoded = serde_json::to_value(frame).expect("encode cron setup");

        assert_eq!(encoded["type"], "start_cron_setup");
        assert_eq!(encoded["request_id"], "request-cron");
        assert_eq!(encoded["session_id"], "session-a");
        assert_eq!(encoded["task"], "Review open pull requests");
    }

    #[test]
    fn cron_management_is_session_scoped() {
        let frames = [
            ClientMessage::ListCron {
                request_id: "list".into(),
                session_id: "session-a".into(),
            },
            ClientMessage::RescheduleCron {
                request_id: "reschedule".into(),
                session_id: "session-a".into(),
                id: "cron-a".into(),
                schedule: "0 9 * * *".into(),
            },
            ClientMessage::DeleteCron {
                request_id: "delete".into(),
                session_id: "session-a".into(),
                id: "cron-a".into(),
            },
            ClientMessage::RunCron {
                request_id: "run".into(),
                session_id: "session-a".into(),
                id: "cron-a".into(),
            },
            ClientMessage::ListCronHistory {
                request_id: "history".into(),
                session_id: "session-a".into(),
                id: None,
            },
        ];

        let session_ids = frames
            .into_iter()
            .map(ClientFrame::new)
            .map(|frame| serde_json::to_value(frame).expect("encode cron operation"))
            .map(|value| value["session_id"].clone())
            .collect::<Vec<_>>();

        assert_eq!(session_ids, vec![Value::String("session-a".into()); 5]);
    }

    #[test]
    fn session_actions_have_flat_authenticated_frames() {
        let rename = serde_json::to_value(ClientFrame::new(ClientMessage::RenameSession {
            request_id: "request-a".into(),
            session_id: "session-a".into(),
            title: "Renamed chat".into(),
        }))
        .expect("encode rename");
        let pin = serde_json::to_value(ClientFrame::new(ClientMessage::SetSessionPinned {
            request_id: "request-c".into(),
            session_id: "session-a".into(),
            pinned: true,
        }))
        .expect("encode pin");
        let delete = serde_json::to_value(ClientFrame::new(ClientMessage::DeleteSession {
            request_id: "request-d".into(),
            session_id: "session-a".into(),
        }))
        .expect("encode delete");

        assert_eq!(rename["type"], "rename_session");
        assert_eq!(rename["title"], "Renamed chat");
        assert_eq!(pin["type"], "set_session_pinned");
        assert_eq!(pin["pinned"], true);
        assert_eq!(delete["type"], "delete_session");
    }

    #[test]
    fn git_diff_query_has_a_correlated_unified_diff_response() {
        let request = serde_json::to_value(ClientFrame::new(ClientMessage::GetGitDiff {
            request_id: "request-diff".into(),
            session_id: "session-a".into(),
        }))
        .expect("encode Git diff request");
        let response = serde_json::to_value(ServerFrame::new(ServerMessage::GitDiff {
            request_id: "request-diff".into(),
            session_id: "session-a".into(),
            diff: "diff --git a/file b/file\n".into(),
        }))
        .expect("encode Git diff response");

        assert_eq!(
            (request["type"].as_str(), response["type"].as_str()),
            (Some("get_git_diff"), Some("git_diff"))
        );
        assert_eq!(response["request_id"], "request-diff");
        assert_eq!(response["session_id"], "session-a");
        assert_eq!(response["diff"], "diff --git a/file b/file\n");
    }

    #[test]
    fn git_branch_switch_is_an_explicit_session_request() {
        let request = serde_json::to_value(ClientFrame::new(ClientMessage::SwitchGitBranch {
            request_id: "request-branch".into(),
            session_id: "session-a".into(),
            branch: "feature/ui".into(),
        }))
        .expect("encode Git branch switch");

        assert_eq!(
            request,
            serde_json::json!({
                "version": PROTOCOL_VERSION,
                "type": "switch_git_branch",
                "request_id": "request-branch",
                "session_id": "session-a",
                "branch": "feature/ui"
            })
        );
    }

    #[test]
    fn agent_events_identify_their_session() {
        let frame = ServerFrame::new(ServerMessage::AgentEvent {
            session_id: "session-a".into(),
            sequence: 1,
            event: Event {
                submission_id: None,
                msg: EventMsg::ContextCompacted,
            },
            blocks: Vec::new(),
            history: None,
            preview: None,
        });

        let encoded = serde_json::to_value(frame).expect("encode agent event");

        assert_eq!(encoded["session_id"], "session-a");
    }

    #[test]
    fn session_record_flattens_checkpoint_fields_with_catalog_metadata() {
        let record = SessionRecord {
            summary: SessionSummary {
                session_id: "session-a".into(),
                session_context: horus::protocol::SessionContext::default(),
                parent_session_id: None,
                parent_sequence: None,
                sequence: 3,
                catalog_visible: true,
                first_user_message: Some("hello".into()),
                created_at: 1,
                updated_at: 2,
            },
            title: Some("Greeting".into()),
            pinned: true,
            activity: SessionActivity {
                state: SessionActivityState::Running,
                turn_id: Some("turn-a".into()),
                started_at: Some(2),
                last_outcome: None,
                message: None,
            },
        };

        let encoded = serde_json::to_value(record).expect("encode session record");

        assert_eq!(encoded["session_id"], "session-a");
        assert_eq!(encoded["title"], "Greeting");
        assert_eq!(encoded["pinned"], true);
        assert_eq!(encoded["activity"]["state"], "running");
        assert_eq!(encoded["activity"]["turn_id"], "turn-a");
        assert!(encoded.get("summary").is_none());
    }

    #[test]
    fn session_record_requires_activity() {
        let encoded = serde_json::json!({
            "session_id": "session-a",
            "session_context": {},
            "parent_session_id": null,
            "parent_sequence": null,
            "sequence": 3,
            "catalog_visible": true,
            "first_user_message": null,
            "created_at": 1,
            "updated_at": 2,
            "title": null,
            "pinned": false
        });

        assert!(serde_json::from_value::<SessionRecord>(encoded).is_err());
    }

    #[test]
    fn directory_listing_request_uses_a_gateway_host_path() {
        let frame = ClientFrame::new(ClientMessage::ListDirectories {
            request_id: "request-b".into(),
            path: PathBuf::from("/srv/horus"),
            include_files: true,
        });

        let encoded = serde_json::to_value(frame).expect("encode directory listing");

        assert_eq!(
            encoded,
            serde_json::json!({
                "version": PROTOCOL_VERSION,
                "type": "list_directories",
                "request_id": "request-b",
                "path": "/srv/horus",
                "include_files": true
            })
        );
    }

    #[test]
    fn gateway_ready_contains_no_selected_session() {
        let frame = ServerFrame::new(ServerMessage::Ready {
            payload: ReadyPayload {
                sessions: Vec::new(),
                providers: Vec::new(),
                default_config: Some(VersionedAgentConfig {
                    revision: 1,
                    config: AgentComposition::default(),
                }),
                models: Vec::new(),
                middleware_features: Vec::new(),
                max_active_sessions: 32,
            },
        });

        let encoded = serde_json::to_value(frame).expect("encode gateway ready");

        assert_eq!(
            (
                encoded["payload"]["max_active_sessions"].as_u64(),
                encoded["payload"].get("session"),
                encoded["payload"].get("workspace"),
            ),
            (Some(32), None, None)
        );
    }

    #[test]
    fn server_frame_decodes_session_opened_with_a_widget_action_tag() {
        let encoded = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "type": "session_opened",
            "request_id": "request-open",
            "payload": {
                "replay_epoch": "epoch-a",
                "latest_sequence": 4,
                "workspace": { "id": "workspace-a", "path": "/workspace" },
                "git": null,
                "session": {
                    "session_id": "session-a",
                    "context": {},
                    "model": {
                        "route": "default",
                        "model": "model-a",
                        "reasoning_effort": null,
                        "model_context_window": null
                    }
                },
                "contributions": [{
                    "capability": "subagents",
                    "commands": [],
                    "widgets": [{
                        "id": "subagents",
                        "slot": "header",
                        "text": "subagents",
                        "tone": "neutral",
                        "symbol": null,
                        "icon_only": false,
                        "progress": null,
                        "content": null,
                        "action": {
                            "type": "capability_command",
                            "capability": "subagents",
                            "command": "subagents",
                            "arguments": "",
                            "target": null
                        }
                    }],
                    "references": [],
                    "active_input": null
                }],
                "tool_count": 3,
                "config": {
                    "revision": 1,
                    "config": {
                        "provider": {
                            "provider": "openai_codex",
                            "model": "model-a",
                            "reasoning_effort": null,
                            "web_search": "off"
                        },
                        "middleware": {
                            "enabled": [
                                "compaction", "context_offloading", "skills", "steering",
                                "subagents", "tools"
                            ],
                            "settings": {
                                "context_offloading": {"stale_after_tokens": 50000}
                            }
                        },
                        "approval": "on",
                        "system_prompt": "test"
                    }
                }
            }
        });

        let frame: ServerFrame =
            serde_json::from_value(encoded).expect("decode nested session ready");
        let ServerMessage::SessionOpened {
            request_id,
            payload,
        } = frame.message
        else {
            panic!("expected session-opened frame");
        };

        assert_eq!(
            (
                request_id.as_str(),
                payload.session.session_id.as_str(),
                payload.contributions[0].widgets[0].action.is_some(),
            ),
            ("request-open", "session-a", true)
        );
    }

    #[test]
    fn replay_completion_is_correlated_to_the_open_request() {
        let frame = ServerFrame::new(ServerMessage::SessionReplayComplete {
            request_id: "request-open".into(),
            session_id: "session-a".into(),
        });

        let encoded = serde_json::to_value(frame).expect("encode replay completion");

        assert_eq!(encoded["type"], "session_replay_complete");
        assert_eq!(encoded["request_id"], "request-open");
        assert_eq!(encoded["session_id"], "session-a");
    }

    #[tokio::test]
    async fn read_frame_rejects_an_oversized_declared_payload() {
        let (mut writer, reader) = duplex(8);
        let mut reader = FrameReader::new(reader);
        let oversized = u32::try_from(MAX_FRAME_BYTES + 1).expect("frame limit fits u32");
        writer
            .write_all(&oversized.to_be_bytes())
            .await
            .expect("write prefix");

        let error = read_frame::<ClientFrame>(&mut reader)
            .await
            .expect_err("oversized frame must fail");

        assert!(matches!(error, Error::Protocol(_)), "{error}");
    }

    #[test]
    fn validate_version_rejects_a_future_protocol() {
        let error = validate_version(PROTOCOL_VERSION + 1).expect_err("future version must fail");

        assert!(matches!(error, Error::Protocol(_)), "{error}");
    }
}
