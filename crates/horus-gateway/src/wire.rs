//! Versioned, bounded JSON frames shared by gateway clients and the server.

use std::path::PathBuf;

use horus::backend::checkpoint::SessionSummary;
use horus::backend::model::ModelChoice;
use horus::backend::model::provider::HostedWebSearch;
use horus::backend::sandbox::ApprovalPolicy;
use horus::protocol::{
    Event, EventMsg, FrontendBlock, FrontendContribution, SessionConfiguredEvent, Submission,
    TokenUsage,
};
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::{Error, Result};

/// Current gateway protocol version.
pub const PROTOCOL_VERSION: u16 = 2;
/// Maximum encoded JSON payload accepted in one frame.
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;

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
        last_sequence: Option<u64>,
    },
    Authenticate {
        token: String,
        last_sequence: Option<u64>,
    },
    OpenSession {
        request_id: String,
        session_id: Option<String>,
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
        submission: Submission,
    },
    ConfigureAgent {
        request_id: String,
        expected_revision: u64,
        config: AgentComposition,
    },
    SetWorkspace {
        request_id: String,
        path: PathBuf,
    },
    GetGitDiff {
        request_id: String,
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
    },
    AddCron {
        request_id: String,
        task: PathBuf,
        schedule: String,
    },
    ListCron {
        request_id: String,
    },
    RescheduleCron {
        request_id: String,
        id: String,
        schedule: String,
    },
    DeleteCron {
        request_id: String,
        id: String,
    },
    RunCron {
        request_id: String,
        id: String,
    },
    ListCronHistory {
        request_id: String,
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
#[expect(
    clippy::large_enum_variant,
    reason = "wire variants are serialized directly and boxing would add per-frame allocations"
)]
pub enum ServerMessage {
    Paired {
        client_id: String,
        token: String,
    },
    Authenticated,
    Ready {
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
        sequence: u64,
        event: Event,
        blocks: Vec<FrontendBlock>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        history: Option<Vec<RenderedEvent>>,
        preview: Option<RenderedPreview>,
    },
    Sessions {
        sessions: Vec<SessionRecord>,
    },
    ConfigChanged {
        snapshot: VersionedAgentConfig,
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
        artifacts: Vec<ArtifactRecord>,
    },
    GitDiff {
        request_id: String,
        diff: String,
    },
    Directories {
        request_id: String,
        listing: DirectoryListing,
    },
    CronTasks {
        request_id: String,
        tasks: Vec<CronTask>,
    },
    CronHistory {
        request_id: String,
        runs: Vec<CronRun>,
    },
    Error {
        code: String,
        message: String,
        fatal: bool,
    },
}

/// Private startup payload emitted to a newly spawned local CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapPayload {
    pub pairing_code: String,
}

/// Complete frontend-safe state sent after authentication or agent restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadyPayload {
    pub latest_sequence: u64,
    pub workspace: WorkspaceInfo,
    pub git: Option<GitStatus>,
    pub session: SessionConfiguredEvent,
    pub sessions: Vec<SessionRecord>,
    pub model_choices: Vec<ModelChoice>,
    pub contributions: Vec<FrontendContribution>,
    pub config: VersionedAgentConfig,
    pub providers: Vec<ProviderStatus>,
}

/// One visible session with gateway-owned catalog presentation metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub title: Option<String>,
    pub pinned: bool,
}

/// Opaque workspace identity and display label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: String,
    pub label: String,
}

/// Local branch state for a Git-backed workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatus {
    pub current_branch: String,
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
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub web_search: HostedWebSearch,
}

/// Credential availability exposed without returning credential material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub provider: String,
    pub label: String,
    pub configured: bool,
    pub auth: ProviderAuthKind,
    pub default_model: Option<String>,
    pub default_base_url: Option<String>,
    pub default_api_key_env: Option<String>,
    pub default_reasoning_effort: Option<String>,
    pub default_web_search: HostedWebSearch,
}

/// Frontend-safe provider authentication mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthKind {
    ApiKey,
    DeviceCode,
}

/// Built-in middleware switches in observable declaration order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareConfig {
    pub tools: bool,
    pub skills: bool,
    pub subagents: bool,
    pub steering: bool,
    pub compaction: bool,
    pub sessions: bool,
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
    pub workspace: WorkspaceInfo,
    pub daily_usage: Vec<DailyUsage>,
}

/// Usage accrued during one Unix day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyUsage {
    pub unix_day: u64,
    pub usage: TokenUsage,
}

/// A reusable code-diff or subagent artifact emitted by a capability.
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

/// One persisted scheduled task owned by the gateway workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronTask {
    pub id: String,
    pub task: PathBuf,
    pub schedule: String,
}

/// One completed or active invocation of a scheduled task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronRun {
    pub id: String,
    pub task_id: String,
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
pub async fn read_frame<T>(reader: &mut (impl AsyncRead + Unpin)) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    let mut prefix = [0_u8; 4];
    match reader.read_exact(&mut prefix[..1]).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    reader.read_exact(&mut prefix[1..]).await?;
    let length = usize::try_from(u32::from_be_bytes(prefix))
        .map_err(|_| Error::Protocol("frame length is unsupported".into()))?;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(Error::Protocol(format!(
            "frame length must be 1–{MAX_FRAME_BYTES} bytes"
        )));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload).await?;
    Ok(Some(serde_json::from_slice(&payload)?))
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
            last_sequence: Some(7),
        });
        let (mut writer, mut reader) = duplex(1024);

        write_frame(&mut writer, &expected)
            .await
            .expect("write frame");
        let actual: ClientFrame = read_frame(&mut reader)
            .await
            .expect("read frame")
            .expect("frame");

        assert_eq!(actual, expected);
    }

    #[test]
    fn client_frame_round_trip_handles_a_nested_operation_tag() {
        let expected = ClientFrame::new(ClientMessage::Submit {
            submission: Submission {
                id: "submission-a".into(),
                op: horus::protocol::Op::CapabilityCommand {
                    capability: "subagents".into(),
                    command: "subagents".into(),
                    arguments: String::new(),
                },
            },
        });

        let encoded = serde_json::to_vec(&expected).expect("encode nested submission");
        let actual: ClientFrame =
            serde_json::from_slice(&encoded).expect("decode nested submission");

        assert_eq!(actual, expected);
    }

    #[test]
    fn workspace_change_uses_a_gateway_host_path() {
        let frame = ClientFrame::new(ClientMessage::SetWorkspace {
            request_id: "request-a".into(),
            path: PathBuf::from("/srv/horus/project"),
        });

        let encoded = serde_json::to_value(frame).expect("encode workspace change");

        assert_eq!(
            encoded,
            serde_json::json!({
                "version": PROTOCOL_VERSION,
                "type": "set_workspace",
                "request_id": "request-a",
                "path": "/srv/horus/project"
            })
        );
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
        }))
        .expect("encode Git diff request");
        let response = serde_json::to_value(ServerFrame::new(ServerMessage::GitDiff {
            request_id: "request-diff".into(),
            diff: "diff --git a/file b/file\n".into(),
        }))
        .expect("encode Git diff response");

        assert_eq!(
            (request["type"].as_str(), response["type"].as_str()),
            (Some("get_git_diff"), Some("git_diff"))
        );
        assert_eq!(response["request_id"], "request-diff");
        assert_eq!(response["diff"], "diff --git a/file b/file\n");
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
        };

        let encoded = serde_json::to_value(record).expect("encode session record");

        assert_eq!(encoded["session_id"], "session-a");
        assert_eq!(encoded["title"], "Greeting");
        assert_eq!(encoded["pinned"], true);
        assert!(encoded.get("summary").is_none());
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
    fn server_frame_decodes_ready_with_a_widget_action_tag() {
        let encoded = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "type": "ready",
            "payload": {
                "latest_sequence": 4,
                "workspace": { "id": "workspace-a", "label": "/workspace" },
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
                "sessions": [],
                "model_choices": [],
                "contributions": [{
                    "capability": "subagents",
                    "commands": [],
                    "widgets": [{
                        "id": "subagents",
                        "slot": "header",
                        "text": "subagents",
                        "tone": "neutral",
                        "action": {
                            "type": "capability_command",
                            "capability": "subagents",
                            "command": "subagents",
                            "arguments": ""
                        }
                    }],
                    "references": [],
                    "active_input": null
                }],
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
                            "tools": true,
                            "skills": true,
                            "subagents": true,
                            "steering": true,
                            "compaction": true,
                            "sessions": true
                        },
                        "approval": "on",
                        "system_prompt": "test"
                    }
                },
                "providers": []
            }
        });

        let frame: ServerFrame = serde_json::from_value(encoded).expect("decode nested ready");
        let ServerMessage::Ready { payload } = frame.message else {
            panic!("expected ready frame");
        };

        assert!(payload.contributions[0].widgets[0].action.is_some());
    }

    #[tokio::test]
    async fn read_frame_rejects_an_oversized_declared_payload() {
        let (mut writer, mut reader) = duplex(8);
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
