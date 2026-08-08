//! Per-chat agent ownership, event sequencing, replay, and authenticated operations.

mod catalog;
mod files;
mod git;
mod providers;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use chrono::Utc;
use horus::agent::{AgentConfig, AgentSender};
use horus::backend::checkpoint::{
    ActiveExecution, Checkpoint, CheckpointStore, ExecutionOutcome, ExecutionRecord,
    ExecutionStats, SessionPageRequest, SessionSummary, TranscriptPageRequest,
    sqlite::SqliteCheckpoint,
};
use horus::backend::model::provider::provider;
use horus::middleware::FrontendExtensions;
use horus::middleware::attachments::AttachmentStore;
use horus::middleware::scratchpad::ScratchpadStore;
use horus::protocol::{
    Event, EventMsg, FrontendBlock, FrontendBlockFormat, FrontendEvent, Op, ReviewDecision,
    Submission, TokenUsage, replay_events,
};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use uuid::Uuid;

use crate::assembly::{
    BuiltAgent, assemble, configured_model_choices, configured_model_providers,
    configured_provider_for_route, provider_statuses,
};
use crate::config::{ChatSpec, ConfigStore, CredentialStore, GatewayConfig, usage_delta};
use crate::cron::{ActiveCronRun, BeginRun, CronStore};
use crate::sandbox::GatewaySandbox;
use crate::wire::{
    AgentComposition, ArtifactKind, ArtifactRecord, CronRunStatus, GitDiffScope, ProfileSnapshot,
    ProviderConfig, ReadyPayload, RenderedEvent, RenderedPreview, RunStats, RunSummary,
    ServerFrame, ServerMessage, SessionActivity, SessionActivityState, SessionOutcome,
    SessionReadyPayload, SessionRecord, SessionRunGroup, SessionWidget, VersionedAgentConfig,
    WorkspaceFileScope,
};
use crate::{Error, Result};

use self::catalog::{
    SessionCatalogMetadata, load_session_metadata, save_session_metadata, session_catalog,
    validate_session_title,
};
use self::files::{WorkspaceRead, list as list_workspace_files, read as read_workspace_file};
use self::git::{
    diff as workspace_git_diff, status as git_status, switch_branch as switch_workspace_branch,
};

const COMMAND_CAPACITY: usize = 128;
const BROADCAST_CAPACITY: usize = 512;
const REPLAY_CAPACITY: usize = 1024;
const ARTIFACT_CAPACITY: usize = 256;
const SESSION_PAGE_SIZE: usize = 100;
const RECENT_RUN_LIMIT: usize = 30;
pub(crate) const MAX_ACTIVE_SESSIONS: usize = 32;

type SessionActivities = Arc<StdMutex<HashMap<String, SessionActivity>>>;
type SessionWidgets = BTreeMap<(String, String), horus::protocol::FrontendWidget>;

#[derive(Clone)]
pub(crate) struct HostHandle {
    inner: Arc<HostInner>,
}

struct HostInner {
    session_id: Arc<str>,
    commands: mpsc::Sender<HostCommand>,
    events: broadcast::Sender<ServerFrame>,
    accepts_file_attachments: Arc<AtomicBool>,
}

/// Machine-wide chat registry. A session has at most one resident agent owner.
#[derive(Clone)]
pub(crate) struct GatewayHost {
    state: Arc<Mutex<GatewayState>>,
    events: broadcast::Sender<ServerFrame>,
}

struct GatewayState {
    store: ConfigStore,
    config: Arc<StdMutex<GatewayConfig>>,
    credentials: Arc<CredentialStore>,
    cron: Arc<CronStore>,
    checkpoints: Arc<dyn CheckpointStore>,
    scratchpad: ScratchpadStore,
    attachments: AttachmentStore,
    // ponytail: one lock is enough for at most 32 tiny catalog writes.
    catalog_lock: Arc<Mutex<()>>,
    activities: SessionActivities,
    provider_login: Arc<StdMutex<Option<String>>>,
    sessions: HashMap<String, HostHandle>,
}

pub(crate) struct HostSnapshot {
    pub(crate) ready: SessionReadyPayload,
    pub(crate) replay: Vec<ServerFrame>,
}

pub(crate) struct SessionHistoryPage {
    pub(crate) events: Vec<RenderedEvent>,
    pub(crate) next_before_sequence: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct Rejection {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) fatal: bool,
}

struct HostState {
    store: ConfigStore,
    gateway: Arc<StdMutex<GatewayConfig>>,
    spec: ChatSpec,
    credentials: Arc<CredentialStore>,
    cron: Arc<CronStore>,
    checkpoints: Arc<dyn CheckpointStore>,
    scratchpad: ScratchpadStore,
    attachments: AttachmentStore,
    accepts_file_attachments: Arc<AtomicBool>,
    catalog_lock: Arc<Mutex<()>>,
    activities: SessionActivities,
    running: RunningAgent,
    usage_baseline: TokenUsage,
    pending_turns: usize,
    approval_active: bool,
    turn_error: Option<String>,
    restart_after_turn: bool,
    suppress_history_broadcast: bool,
    pending_startup: Vec<ServerFrame>,
    active_cron: Option<ActiveCron>,
    replay_epoch: String,
    sequence: u64,
    replay: VecDeque<ServerFrame>,
    replay_truncated: bool,
    artifacts: VecDeque<ArtifactRecord>,
    widgets: SessionWidgets,
    commands: mpsc::Receiver<HostCommand>,
    events: broadcast::Sender<ServerFrame>,
    gateway_events: broadcast::Sender<ServerFrame>,
    idle_waiters: Vec<oneshot::Sender<()>>,
}

struct RunningAgent {
    session_id: String,
    sender: AgentSender,
    events: mpsc::Receiver<Event>,
    frontend: FrontendExtensions,
    session: horus::protocol::SessionConfiguredEvent,
    gateway_sandbox: Arc<GatewaySandbox>,
    subagent_template: Option<Arc<OnceLock<AgentConfig>>>,
    tool_count: usize,
    next_before_sequence: Option<u64>,
}

struct ActiveCron {
    run: ActiveCronRun,
    submission_id: String,
    turn_id: Option<String>,
    failure: Option<String>,
}

enum HostCommand {
    Snapshot {
        last_sequence: Option<u64>,
        replay_epoch: Option<String>,
        reply: oneshot::Sender<std::result::Result<HostSnapshot, Rejection>>,
    },
    HistoryPage {
        before_sequence: Option<u64>,
        max_batches: usize,
        reply: oneshot::Sender<std::result::Result<SessionHistoryPage, Rejection>>,
    },
    RenameSession {
        session_id: String,
        title: String,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    SetSessionPinned {
        session_id: String,
        pinned: bool,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    DeleteSession {
        session_id: String,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    Submit {
        submission: Submission,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    StartCronSetup {
        task: Option<String>,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    Configure {
        expected_revision: u64,
        config: AgentComposition,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    GitDiff {
        scope: GitDiffScope,
        reply: oneshot::Sender<std::result::Result<String, Rejection>>,
    },
    WorkspaceFiles {
        scope: WorkspaceFileScope,
        reply:
            oneshot::Sender<std::result::Result<Vec<crate::wire::WorkspaceFileRecord>, Rejection>>,
    },
    ReadWorkspaceFile {
        path: String,
        offset: u64,
        max_bytes: usize,
        reply: oneshot::Sender<std::result::Result<WorkspaceRead, Rejection>>,
    },
    SwitchGitBranch {
        branch: String,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    RefreshProvider {
        provider: String,
        base_url: Option<String>,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    Artifacts {
        reply: oneshot::Sender<Vec<ArtifactRecord>>,
    },
    RunCron {
        run: ActiveCronRun,
        input: String,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    WaitIdle {
        reply: oneshot::Sender<()>,
    },
    StopIfIdle {
        reply: oneshot::Sender<bool>,
    },
}

enum Next {
    Command(Option<HostCommand>),
    Event(Option<Event>),
}

impl GatewayHost {
    pub(crate) fn start(
        store: ConfigStore,
        config: GatewayConfig,
        credentials: Arc<CredentialStore>,
        cron: Arc<CronStore>,
    ) -> Result<Self> {
        let checkpoints: Arc<dyn CheckpointStore> =
            Arc::new(SqliteCheckpoint::new(store.checkpoints_path())?);
        let scratchpad = ScratchpadStore::new(Arc::clone(&checkpoints));
        let attachments = AttachmentStore::new(store.state_dir());
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        Ok(Self {
            state: Arc::new(Mutex::new(GatewayState {
                store,
                config: Arc::new(StdMutex::new(config)),
                credentials,
                cron,
                checkpoints,
                scratchpad,
                attachments,
                catalog_lock: Arc::new(Mutex::new(())),
                activities: Arc::new(StdMutex::new(HashMap::new())),
                provider_login: Arc::new(StdMutex::new(None)),
                sessions: HashMap::new(),
            })),
            events,
        })
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<ServerFrame> {
        self.events.subscribe()
    }

    pub(crate) async fn attachment_store(&self) -> AttachmentStore {
        self.state.lock().await.attachments.clone()
    }

    pub(crate) async fn ready(&self) -> std::result::Result<ReadyPayload, Rejection> {
        let state = self.state.lock().await;
        gateway_ready(&state).await
    }

    pub(crate) async fn sessions(&self) -> std::result::Result<Vec<SessionRecord>, Rejection> {
        let state = self.state.lock().await;
        session_catalog(&state.checkpoints, &state.activities)
            .await
            .map_err(internal)
    }

    pub(crate) async fn create_session(
        &self,
        workspace: &Path,
    ) -> std::result::Result<HostHandle, Rejection> {
        let mut state = self.state.lock().await;
        state.ensure_capacity().await?;
        let (default_agent, tls) = {
            let config = state
                .config
                .lock()
                .map_err(|_| internal("gateway configuration lock is poisoned"))?;
            (
                config
                    .default_agent
                    .clone()
                    .unwrap_or_else(setup_agent_config),
                config.tls.clone(),
            )
        };
        let spec = ChatSpec::new(
            workspace,
            default_agent,
            state.store.state_dir(),
            tls.as_ref(),
        )
        .map_err(invalid_workspace)?;
        let session_id = Uuid::new_v4().to_string();
        let host = HostHandle::start(
            state.store.clone(),
            Arc::clone(&state.config),
            spec,
            Arc::clone(&state.credentials),
            Arc::clone(&state.cron),
            Arc::clone(&state.checkpoints),
            state.scratchpad.clone(),
            state.attachments.clone(),
            Arc::clone(&state.catalog_lock),
            Arc::clone(&state.activities),
            self.events.clone(),
            session_id.clone(),
            "horus-gateway",
        )
        .await
        .map_err(internal)?;
        state.sessions.insert(session_id, host.clone());
        drop(state);
        self.broadcast_sessions().await?;
        Ok(host)
    }

    pub(crate) async fn open_session(
        &self,
        session_id: &str,
    ) -> std::result::Result<HostHandle, Rejection> {
        let mut state = self.state.lock().await;
        if let Some(host) = state.sessions.get(session_id) {
            return Ok(host.clone());
        }
        state.ensure_capacity().await?;
        let checkpoint = state
            .checkpoints
            .load(session_id)
            .await
            .map_err(internal)?
            .ok_or_else(unknown_session)?;
        let tls = state
            .config
            .lock()
            .map_err(|_| internal("gateway configuration lock is poisoned"))?
            .tls
            .clone();
        let spec =
            ChatSpec::from_metadata(&checkpoint.metadata, state.store.state_dir(), tls.as_ref())
                .map_err(invalid_config)?;
        let workspace = spec.workspace_info();
        let workspace_label = workspace.path.display().to_string();
        if checkpoint.session_context.workspace_id.as_deref() != Some(workspace.id.as_str())
            || checkpoint.session_context.workspace_label.as_deref()
                != Some(workspace_label.as_str())
        {
            return Err(invalid_session_workspace());
        }
        let host = HostHandle::start(
            state.store.clone(),
            Arc::clone(&state.config),
            spec,
            Arc::clone(&state.credentials),
            Arc::clone(&state.cron),
            Arc::clone(&state.checkpoints),
            state.scratchpad.clone(),
            state.attachments.clone(),
            Arc::clone(&state.catalog_lock),
            Arc::clone(&state.activities),
            self.events.clone(),
            session_id.into(),
            "horus-gateway",
        )
        .await
        .map_err(internal)?;
        state.sessions.insert(session_id.into(), host.clone());
        Ok(host)
    }

    pub(crate) async fn run_cron(
        &self,
        source_session_id: String,
        task_id: String,
    ) -> std::result::Result<(), Rejection> {
        let mut state = self.state.lock().await;
        let task = state
            .cron
            .task(&source_session_id, &task_id)
            .map_err(invalid_cron)?;
        let (_, input) = state.cron.task_input(&task.id).map_err(invalid_cron)?;
        if let Err(rejection) = state.ensure_capacity().await {
            state
                .cron
                .skip_run(&task.id, "the gateway active-chat limit was reached")
                .map_err(internal)?;
            return Err(rejection);
        }
        let checkpoint = state
            .checkpoints
            .load(&source_session_id)
            .await
            .map_err(internal)?
            .ok_or_else(unknown_session)?;
        let tls = state
            .config
            .lock()
            .map_err(|_| internal("gateway configuration lock is poisoned"))?
            .tls
            .clone();
        let spec =
            ChatSpec::from_metadata(&checkpoint.metadata, state.store.state_dir(), tls.as_ref())
                .map_err(invalid_config)?;
        let workspace = spec.workspace_info();
        let workspace_label = workspace.path.display().to_string();
        if checkpoint.session_context.workspace_id.as_deref() != Some(workspace.id.as_str())
            || checkpoint.session_context.workspace_label.as_deref()
                != Some(workspace_label.as_str())
        {
            return Err(invalid_session_workspace());
        }
        let source_sequence = checkpoint.sequence;
        let session_id = Uuid::new_v4().to_string();
        let label = format!("cron · {}", task.id.get(..8).unwrap_or(&task.id));
        let run = match state.cron.begin_run(&task.id).map_err(invalid_cron)? {
            BeginRun::Started(run) => run,
            BeginRun::Skipped => {
                return Err(Rejection {
                    code: "cron_overlap",
                    message: format!("cron task {} is already running", task.id),
                    fatal: false,
                });
            }
        };
        let checkpoint = cron_execution_checkpoint(&checkpoint, &session_id, &label);
        if let Err(error) = state
            .checkpoints
            .fork(&source_session_id, source_sequence, &checkpoint)
            .await
        {
            let message = error.to_string();
            state
                .cron
                .finish_run(run, CronRunStatus::Failed, Some(message.clone()))
                .map_err(internal)?;
            return Err(internal(message));
        }
        if let Err(error) = state.cron.attach_execution_session(&run, &session_id) {
            let message = error.to_string();
            state
                .cron
                .finish_run(run, CronRunStatus::Failed, Some(message.clone()))
                .map_err(internal)?;
            hide_checkpoint(&state.checkpoints, &session_id)
                .await
                .map_err(internal)?;
            return Err(invalid_cron(message));
        }
        let host = match HostHandle::start(
            state.store.clone(),
            Arc::clone(&state.config),
            spec,
            Arc::clone(&state.credentials),
            Arc::clone(&state.cron),
            Arc::clone(&state.checkpoints),
            state.scratchpad.clone(),
            state.attachments.clone(),
            Arc::clone(&state.catalog_lock),
            Arc::clone(&state.activities),
            self.events.clone(),
            session_id.clone(),
            &label,
        )
        .await
        {
            Ok(host) => host,
            Err(error) => {
                let message = error.to_string();
                state
                    .cron
                    .finish_run(run, CronRunStatus::Failed, Some(message.clone()))
                    .map_err(internal)?;
                hide_checkpoint(&state.checkpoints, &session_id)
                    .await
                    .map_err(internal)?;
                return Err(internal(message));
            }
        };
        let cron = Arc::clone(&state.cron);
        let checkpoints = Arc::clone(&state.checkpoints);
        state.sessions.insert(session_id.clone(), host.clone());
        drop(state);
        match host.run_cron(run, input, &cron).await {
            Ok(()) => {
                let gateway = self.clone();
                tokio::spawn(async move {
                    host.wait_idle().await;
                    gateway.state.lock().await.sessions.remove(&session_id);
                });
                Ok(())
            }
            Err(rejection) => {
                let _ = host.stop_if_idle().await;
                self.state.lock().await.sessions.remove(&session_id);
                hide_checkpoint(&checkpoints, &session_id)
                    .await
                    .map_err(internal)?;
                Err(rejection)
            }
        }
    }

    pub(crate) async fn profile(&self) -> std::result::Result<ProfileSnapshot, Rejection> {
        let (mut profile, checkpoints) = {
            let state = self.state.lock().await;
            let profile = state
                .config
                .lock()
                .map_err(|_| internal("gateway configuration lock is poisoned"))?
                .profile();
            (profile, Arc::clone(&state.checkpoints))
        };
        let sessions = gateway_session_summaries(&checkpoints)
            .await
            .map_err(internal)?;
        profile.run_stats = gateway_run_stats(&sessions).map_err(internal)?;
        let recent_runs = checkpoints
            .recent_executions(RECENT_RUN_LIMIT)
            .await
            .map_err(internal)?;
        if !recent_runs.is_empty() {
            let metadata = load_session_metadata(&checkpoints)
                .await
                .map_err(internal)?;
            profile.recent_run_groups = recent_run_groups(recent_runs, &sessions, &metadata);
        }
        Ok(profile)
    }

    async fn broadcast_sessions(&self) -> std::result::Result<(), Rejection> {
        let state = self.state.lock().await;
        let sessions = session_catalog(&state.checkpoints, &state.activities)
            .await
            .map_err(internal)?;
        drop(state);
        let _ = self.events.send(ServerFrame::new(ServerMessage::Sessions {
            request_id: None,
            sessions,
        }));
        Ok(())
    }
}

impl GatewayState {
    async fn ensure_capacity(&mut self) -> std::result::Result<(), Rejection> {
        if self.sessions.len() < MAX_ACTIVE_SESSIONS {
            return Ok(());
        }
        let candidates = self
            .sessions
            .iter()
            .filter(|(_, host)| host.is_unreferenced())
            .map(|(id, host)| (id.clone(), host.clone()))
            .collect::<Vec<_>>();
        for (id, host) in candidates {
            if host.stop_if_idle().await {
                self.sessions.remove(&id);
                if self.sessions.len() < MAX_ACTIVE_SESSIONS {
                    return Ok(());
                }
            }
        }
        Err(Rejection {
            code: "session_limit",
            message: format!(
                "this gateway already has {MAX_ACTIVE_SESSIONS} connected or running chats"
            ),
            fatal: false,
        })
    }
}

impl HostHandle {
    #[expect(
        clippy::too_many_arguments,
        reason = "one chat actor receives each owned gateway dependency explicitly"
    )]
    pub(crate) async fn start(
        store: ConfigStore,
        gateway: Arc<StdMutex<GatewayConfig>>,
        spec: ChatSpec,
        credentials: Arc<CredentialStore>,
        cron: Arc<CronStore>,
        checkpoints: Arc<dyn CheckpointStore>,
        scratchpad: ScratchpadStore,
        attachments: AttachmentStore,
        catalog_lock: Arc<Mutex<()>>,
        activities: SessionActivities,
        gateway_events: broadcast::Sender<ServerFrame>,
        session_id: String,
        origin_label: &str,
    ) -> Result<Self> {
        let gateway_config = gateway
            .lock()
            .map_err(|_| Error::Config("gateway configuration lock is poisoned".into()))?
            .clone();
        let running = start_agent(
            &gateway_config,
            &spec,
            &store,
            Arc::clone(&credentials),
            Arc::clone(&cron),
            Arc::clone(&checkpoints),
            scratchpad.clone(),
            attachments.clone(),
            session_id.clone(),
            origin_label,
            false,
        )
        .await?;
        let accepts_file_attachments = Arc::new(AtomicBool::new(runtime_accepts_attachments(
            &running.frontend,
        )));
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        activities
            .lock()
            .map_err(|_| Error::Config("session activity lock is poisoned".into()))?
            .entry(session_id.clone())
            .or_default();
        let state = HostState {
            store,
            gateway,
            spec,
            credentials,
            cron,
            checkpoints,
            scratchpad,
            attachments,
            accepts_file_attachments: Arc::clone(&accepts_file_attachments),
            catalog_lock,
            activities,
            running,
            usage_baseline: TokenUsage::default(),
            pending_turns: 0,
            approval_active: false,
            turn_error: None,
            restart_after_turn: false,
            suppress_history_broadcast: false,
            pending_startup: Vec::new(),
            active_cron: None,
            replay_epoch: Uuid::new_v4().to_string(),
            sequence: 0,
            replay: VecDeque::with_capacity(REPLAY_CAPACITY),
            replay_truncated: false,
            artifacts: VecDeque::with_capacity(ARTIFACT_CAPACITY),
            widgets: BTreeMap::new(),
            commands: receiver,
            events: events.clone(),
            gateway_events,
            idle_waiters: Vec::new(),
        };
        tokio::spawn(state.run());
        Ok(Self {
            inner: Arc::new(HostInner {
                session_id: session_id.into(),
                commands,
                events,
                accepts_file_attachments,
            }),
        })
    }

    #[must_use]
    pub(crate) fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<ServerFrame> {
        self.inner.events.subscribe()
    }

    #[must_use]
    pub(crate) fn accepts_file_attachments(&self) -> bool {
        self.inner.accepts_file_attachments.load(Ordering::Relaxed)
    }

    pub(crate) async fn snapshot(
        &self,
        last_sequence: Option<u64>,
        replay_epoch: Option<String>,
    ) -> std::result::Result<HostSnapshot, Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::Snapshot {
            last_sequence,
            replay_epoch,
            reply,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn history_page(
        &self,
        before_sequence: Option<u64>,
        max_batches: usize,
    ) -> std::result::Result<SessionHistoryPage, Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::HistoryPage {
            before_sequence,
            max_batches,
            reply,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn rename_session(
        &self,
        session_id: String,
        title: String,
    ) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::RenameSession {
            session_id,
            title,
            reply,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn set_session_pinned(
        &self,
        session_id: String,
        pinned: bool,
    ) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::SetSessionPinned {
            session_id,
            pinned,
            reply,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn delete_session(
        &self,
        session_id: String,
    ) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::DeleteSession { session_id, reply })
            .await?;
        receive(receiver).await
    }

    pub(crate) async fn submit(
        &self,
        submission: Submission,
    ) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::Submit { submission, reply }).await?;
        receive(receiver).await
    }

    pub(crate) async fn start_cron_setup(
        &self,
        task: Option<String>,
    ) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::StartCronSetup { task, reply })
            .await?;
        receive(receiver).await
    }

    pub(crate) async fn configure(
        &self,
        expected_revision: u64,
        config: AgentComposition,
    ) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::Configure {
            expected_revision,
            config,
            reply,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn git_diff(
        &self,
        scope: GitDiffScope,
    ) -> std::result::Result<String, Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::GitDiff { scope, reply }).await?;
        receiver.await.map_err(|_| stopped())?
    }

    pub(crate) async fn workspace_files(
        &self,
        scope: WorkspaceFileScope,
    ) -> std::result::Result<Vec<crate::wire::WorkspaceFileRecord>, Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::WorkspaceFiles { scope, reply })
            .await?;
        receiver.await.map_err(|_| stopped())?
    }

    pub(crate) async fn read_workspace_file(
        &self,
        path: String,
        offset: u64,
        max_bytes: usize,
    ) -> std::result::Result<WorkspaceRead, Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::ReadWorkspaceFile {
            path,
            offset,
            max_bytes,
            reply,
        })
        .await?;
        receiver.await.map_err(|_| stopped())?
    }

    pub(crate) async fn switch_git_branch(
        &self,
        branch: String,
    ) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::SwitchGitBranch { branch, reply })
            .await?;
        receive(receiver).await
    }

    async fn refresh_provider(
        &self,
        provider: String,
        base_url: Option<String>,
    ) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::RefreshProvider {
            provider,
            base_url,
            reply,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn artifacts(&self) -> std::result::Result<Vec<ArtifactRecord>, Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::Artifacts { reply }).await?;
        receiver.await.map_err(|_| stopped())
    }

    pub(crate) async fn run_cron(
        &self,
        run: ActiveCronRun,
        input: String,
        cron: &CronStore,
    ) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        if let Err(error) = self
            .inner
            .commands
            .send(HostCommand::RunCron { run, input, reply })
            .await
        {
            let HostCommand::RunCron { run, .. } = error.0 else {
                unreachable!("only a cron command was sent")
            };
            cron.finish_run(
                run,
                CronRunStatus::Failed,
                Some("the agent stopped before the scheduled run began".into()),
            )
            .map_err(internal)?;
            return Err(stopped());
        }
        receive(receiver).await
    }

    async fn wait_idle(&self) {
        let (reply, receiver) = oneshot::channel();
        if self.send(HostCommand::WaitIdle { reply }).await.is_ok() {
            let _ = receiver.await;
        }
    }

    fn is_unreferenced(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }

    async fn stop_if_idle(&self) -> bool {
        let (reply, receiver) = oneshot::channel();
        if self.send(HostCommand::StopIfIdle { reply }).await.is_err() {
            return true;
        }
        receiver.await.unwrap_or(true)
    }

    async fn send(&self, command: HostCommand) -> std::result::Result<(), Rejection> {
        self.inner
            .commands
            .send(command)
            .await
            .map_err(|_| stopped())
    }
}

impl HostState {
    async fn run(mut self) {
        loop {
            let next = tokio::select! {
                command = self.commands.recv() => Next::Command(command),
                event = self.running.events.recv() => Next::Event(event),
            };
            match next {
                Next::Command(Some(command)) => {
                    if !self.handle(command).await {
                        break;
                    }
                }
                Next::Command(None) => break,
                Next::Event(Some(event)) => {
                    if let Err(error) = self.forward_event(event).await {
                        self.broadcast(ServerMessage::Error {
                            code: "host_error".into(),
                            message: error.to_string(),
                            fatal: false,
                        });
                    }
                }
                Next::Event(None) => {
                    self.broadcast(ServerMessage::Error {
                        code: "agent_stopped".into(),
                        message: "the agent stopped".into(),
                        fatal: true,
                    });
                    if let Err(error) = self.fail_activity("the agent stopped").await {
                        self.broadcast(ServerMessage::Error {
                            code: "session_activity".into(),
                            message: error.to_string(),
                            fatal: false,
                        });
                    }
                    break;
                }
            }
        }
        if let Err(error) = fail_active_cron(
            &self.cron,
            &mut self.active_cron,
            "the agent stopped before the scheduled run completed",
        ) {
            self.broadcast(ServerMessage::Error {
                code: "cron_state_error".into(),
                message: error.to_string(),
                fatal: false,
            });
        }
        for waiter in self.idle_waiters.drain(..) {
            let _ = waiter.send(());
        }
        self.cron.cancel_setup(&self.running.session_id);
        shutdown_agent(self.running).await;
    }

    async fn handle(&mut self, command: HostCommand) -> bool {
        match command {
            HostCommand::Snapshot {
                last_sequence,
                replay_epoch,
                reply,
            } => {
                let _ = reply.send(
                    self.snapshot_value(last_sequence, replay_epoch.as_deref())
                        .await,
                );
            }
            HostCommand::HistoryPage {
                before_sequence,
                max_batches,
                reply,
            } => {
                let _ = reply.send(self.history_page_value(before_sequence, max_batches).await);
            }
            HostCommand::RenameSession {
                session_id,
                title,
                reply,
            } => {
                let result = self.rename_session(&session_id, &title).await;
                let _ = reply.send(result);
            }
            HostCommand::SetSessionPinned {
                session_id,
                pinned,
                reply,
            } => {
                let result = self.set_session_pinned(&session_id, pinned).await;
                let _ = reply.send(result);
            }
            HostCommand::DeleteSession { session_id, reply } => {
                let result = self.delete_session(&session_id).await;
                let _ = reply.send(result);
            }
            HostCommand::Submit { submission, reply } => {
                let resumes_approval = matches!(
                    &submission.op,
                    Op::ExecApproval {
                        decision: ReviewDecision::Approved
                            | ReviewDecision::ApprovedForSession
                            | ReviewDecision::Denied { .. },
                        ..
                    }
                );
                let result = match &submission.op {
                    Op::SetModel { route } => self.set_model(route).await,
                    _ => self.submit(submission, false),
                };
                if result.is_ok()
                    && resumes_approval
                    && let Err(error) = self.resume_activity().await
                {
                    self.broadcast(ServerMessage::Error {
                        code: "session_activity".into(),
                        message: error.to_string(),
                        fatal: false,
                    });
                }
                let _ = reply.send(result);
            }
            HostCommand::StartCronSetup { task, reply } => {
                let result = self.start_cron_setup(task);
                let _ = reply.send(result);
            }
            HostCommand::Configure {
                expected_revision,
                config,
                reply,
            } => {
                let result = self.configure(expected_revision, config).await;
                let _ = reply.send(result);
            }
            HostCommand::GitDiff { scope, reply } => {
                let _ = reply.send(
                    workspace_git_diff(&self.running.gateway_sandbox, &self.spec.workspace, scope)
                        .await,
                );
            }
            HostCommand::WorkspaceFiles { scope, reply } => {
                let _ = reply.send(
                    list_workspace_files(
                        &self.running.gateway_sandbox,
                        &self.spec.workspace,
                        scope,
                    )
                    .await,
                );
            }
            HostCommand::ReadWorkspaceFile {
                path,
                offset,
                max_bytes,
                reply,
            } => {
                let _ = reply.send(
                    read_workspace_file(&self.running.gateway_sandbox, &path, offset, max_bytes)
                        .await,
                );
            }
            HostCommand::SwitchGitBranch { branch, reply } => {
                let result = self.switch_git_branch(&branch).await;
                let _ = reply.send(result);
            }
            HostCommand::RefreshProvider {
                provider,
                base_url,
                reply,
            } => {
                let result = self.refresh_provider(&provider, base_url.as_deref()).await;
                let _ = reply.send(result);
            }
            HostCommand::Artifacts { reply } => {
                let _ = reply.send(self.artifacts.iter().cloned().collect());
            }
            HostCommand::RunCron { run, input, reply } => {
                let result = self.run_cron(run, input);
                let _ = reply.send(result);
            }
            HostCommand::WaitIdle { reply } => {
                if self.is_idle() {
                    let _ = reply.send(());
                } else {
                    self.idle_waiters.push(reply);
                }
            }
            HostCommand::StopIfIdle { reply } => {
                let idle = self.is_idle();
                let _ = reply.send(idle);
                return !idle;
            }
        }
        true
    }

    async fn snapshot_value(
        &self,
        last_sequence: Option<u64>,
        replay_epoch: Option<&str>,
    ) -> std::result::Result<HostSnapshot, Rejection> {
        let replay = self.replay_after(last_sequence, replay_epoch)?;
        Ok(HostSnapshot {
            ready: self.ready().await.map_err(internal)?,
            replay,
        })
    }

    async fn history_page_value(
        &self,
        before_sequence: Option<u64>,
        max_batches: usize,
    ) -> std::result::Result<SessionHistoryPage, Rejection> {
        let page = self
            .checkpoints
            .transcript_page(
                &self.running.session_id,
                TranscriptPageRequest {
                    before_sequence,
                    max_batches,
                },
            )
            .await
            .map_err(internal)?;
        let next_before_sequence = page.next_before_sequence;
        let mut items = page.into_positioned_items_chronological();
        let Some(oldest_sequence) = items.first().map(|(target, _)| target.checkpoint_sequence)
        else {
            return Ok(SessionHistoryPage {
                events: Vec::new(),
                next_before_sequence,
            });
        };
        let prefix = self
            .checkpoints
            .transcript_page(
                &self.running.session_id,
                TranscriptPageRequest {
                    before_sequence: Some(oldest_sequence),
                    max_batches: 1,
                },
            )
            .await
            .map_err(internal)?
            .into_positioned_items_chronological();
        let prefix_events = replay_events(&prefix, &self.running.session_id).len();
        let mut context = prefix;
        context.append(&mut items);
        let events = replay_events(&context, &self.running.session_id)
            .into_iter()
            .skip(prefix_events)
            .map(|event| RenderedEvent {
                blocks: self.running.frontend.render(&event),
                event,
            })
            .collect();
        Ok(SessionHistoryPage {
            events,
            next_before_sequence,
        })
    }

    fn replay_after(
        &self,
        last_sequence: Option<u64>,
        replay_epoch: Option<&str>,
    ) -> std::result::Result<Vec<ServerFrame>, Rejection> {
        let Some(last_sequence) = last_sequence else {
            return Ok(self.replay.iter().cloned().collect());
        };
        if replay_epoch != Some(self.replay_epoch.as_str()) || last_sequence > self.sequence {
            return Err(Rejection {
                code: "replay_unavailable",
                message: "the gateway restarted; reload the active session".into(),
                fatal: false,
            });
        }
        let oldest = self.replay.front().and_then(event_sequence);
        if oldest.is_some_and(|oldest| last_sequence.saturating_add(1) < oldest) {
            return Err(Rejection {
                code: "replay_unavailable",
                message: "the reconnect window expired; reload the active session".into(),
                fatal: false,
            });
        }
        Ok(self
            .replay
            .iter()
            .filter(|frame| event_sequence(frame).is_some_and(|sequence| sequence > last_sequence))
            .cloned()
            .collect())
    }

    async fn rename_session(
        &mut self,
        session_id: &str,
        title: &str,
    ) -> std::result::Result<(), Rejection> {
        self.require_session(session_id).await?;
        let title = validate_session_title(title)?;
        let _catalog = self.catalog_lock.lock().await;
        let mut metadata = load_session_metadata(&self.checkpoints)
            .await
            .map_err(internal)?;
        metadata.entry(session_id.into()).or_default().title = Some(title.into());
        save_session_metadata(&self.checkpoints, &metadata)
            .await
            .map_err(internal)?;
        self.broadcast_sessions().await
    }

    async fn set_session_pinned(
        &mut self,
        session_id: &str,
        pinned: bool,
    ) -> std::result::Result<(), Rejection> {
        self.require_session(session_id).await?;
        let _catalog = self.catalog_lock.lock().await;
        let mut metadata = load_session_metadata(&self.checkpoints)
            .await
            .map_err(internal)?;
        metadata.entry(session_id.into()).or_default().pinned = pinned;
        save_session_metadata(&self.checkpoints, &metadata)
            .await
            .map_err(internal)?;
        self.broadcast_sessions().await
    }

    async fn delete_session(&mut self, session_id: &str) -> std::result::Result<(), Rejection> {
        if session_id == self.running.session_id {
            self.require_idle()?;
        }
        self.require_session(session_id).await?;
        let _catalog = self.catalog_lock.lock().await;
        let mut metadata = load_session_metadata(&self.checkpoints)
            .await
            .map_err(internal)?;
        metadata.entry(session_id.into()).or_default().hidden = true;
        save_session_metadata(&self.checkpoints, &metadata)
            .await
            .map_err(internal)?;
        self.broadcast_sessions().await
    }

    async fn require_session(&self, session_id: &str) -> std::result::Result<(), Rejection> {
        if self
            .checkpoints
            .load(session_id)
            .await
            .map_err(internal)?
            .is_none()
        {
            return Err(Rejection {
                code: "unknown_session",
                message: "the requested session does not exist".into(),
                fatal: false,
            });
        }
        Ok(())
    }

    fn run_cron(
        &mut self,
        run: ActiveCronRun,
        input: String,
    ) -> std::result::Result<(), Rejection> {
        if let Err(rejection) = self.require_idle() {
            self.cron
                .finish_run(
                    run,
                    CronRunStatus::Failed,
                    Some("the agent was busy when this invocation became due".into()),
                )
                .map_err(internal)?;
            return Err(rejection);
        }
        let submission_id = Uuid::new_v4().to_string();
        self.active_cron = Some(ActiveCron {
            run,
            submission_id: submission_id.clone(),
            turn_id: None,
            failure: None,
        });
        let submission = Submission {
            id: submission_id,
            op: Op::UserInput {
                text: input,
                attachments: Vec::new(),
            },
        };
        if let Err(rejection) = self.submit(submission, true) {
            let active = self.active_cron.take().expect("active cron was just set");
            self.cron
                .finish_run(
                    active.run,
                    CronRunStatus::Failed,
                    Some(rejection.message.clone()),
                )
                .map_err(internal)?;
            return Err(rejection);
        }
        Ok(())
    }

    fn submit(
        &mut self,
        submission: Submission,
        scheduled: bool,
    ) -> std::result::Result<(), Rejection> {
        let starts_turn = matches!(submission.op, Op::UserInput { .. });
        let resolves_approval = matches!(submission.op, Op::ExecApproval { .. });
        if starts_turn && self.active_cron.is_some() && !scheduled {
            return Err(Rejection {
                code: "agent_busy",
                message: "wait for the scheduled run to finish".into(),
                fatal: false,
            });
        }
        self.running
            .sender
            .send(submission)
            .map_err(|error| Rejection {
                code: match error {
                    horus::Error::Busy(_) => "agent_busy",
                    horus::Error::Stopped(_) => "agent_stopped",
                    _ => "invalid_submission",
                },
                message: error.to_string(),
                fatal: matches!(error, horus::Error::Stopped(_)),
            })?;
        self.pending_turns += usize::from(starts_turn);
        if resolves_approval {
            self.approval_active = false;
        }
        Ok(())
    }

    fn start_cron_setup(&mut self, task: Option<String>) -> std::result::Result<(), Rejection> {
        self.require_idle()?;
        let input = self
            .cron
            .begin_setup(&self.running.session_id, task.as_deref())
            .map_err(invalid_cron)?;
        let submission = Submission {
            id: Uuid::new_v4().to_string(),
            op: Op::UserInput {
                text: input,
                attachments: Vec::new(),
            },
        };
        if let Err(rejection) = self.submit(submission, false) {
            self.cron.cancel_setup(&self.running.session_id);
            return Err(rejection);
        }
        Ok(())
    }

    async fn configure(
        &mut self,
        expected_revision: u64,
        composition: AgentComposition,
    ) -> std::result::Result<(), Rejection> {
        self.require_idle()?;
        if expected_revision != self.spec.agent.revision {
            return Err(Rejection {
                code: "revision_conflict",
                message: format!("configuration revision is now {}", self.spec.agent.revision),
                fatal: false,
            });
        }
        let gateway = self
            .gateway
            .lock()
            .map_err(|_| internal("gateway configuration lock is poisoned"))?
            .clone();
        let models =
            configured_model_choices(&gateway, &self.store, &self.credentials).map_err(internal)?;
        crate::middleware_manifest::validate_choices(&composition.middleware, &models)
            .map_err(invalid_config)?;
        let next = self
            .spec
            .replacing_agent(
                expected_revision,
                composition,
                &gateway,
                self.store.state_dir(),
                gateway.tls.as_ref(),
            )
            .map_err(invalid_config)?;
        let session_id = self.running.session_id.clone();
        let replacement = start_agent(
            &gateway,
            &next,
            &self.store,
            Arc::clone(&self.credentials),
            Arc::clone(&self.cron),
            Arc::clone(&self.checkpoints),
            self.scratchpad.clone(),
            self.attachments.clone(),
            session_id,
            "horus-gateway",
            true,
        )
        .await
        .map_err(internal)?;
        let suppress_history_broadcast = reset_replay_for_restart(
            &mut self.replay,
            &self.running.session_id,
            &replacement.session_id,
        );
        self.replay_truncated = false;
        let previous = std::mem::replace(&mut self.running, replacement);
        self.accepts_file_attachments.store(
            runtime_accepts_attachments(&self.running.frontend),
            Ordering::Relaxed,
        );
        self.suppress_history_broadcast = suppress_history_broadcast;
        self.spec = next;
        shutdown_agent(previous).await;
        if suppress_history_broadcast {
            self.record_replacement_startup().map_err(internal)?;
        }
        self.broadcast_changed().await?;
        Ok(())
    }

    async fn set_model(&mut self, route: &str) -> std::result::Result<(), Rejection> {
        self.require_idle()?;
        let gateway = self
            .gateway
            .lock()
            .map_err(|_| internal("gateway configuration lock is poisoned"))?
            .clone();
        let mut provider =
            configured_provider_for_route(&gateway, &self.store, &self.credentials, route)
                .map_err(invalid_config)?;
        if provider.provider == self.spec.agent.config.provider.provider {
            provider.base_url = self.spec.agent.config.provider.base_url.clone();
            provider.web_search = self.spec.agent.config.provider.web_search;
        }
        if self.running.session.model.route == route && self.spec.agent.config.provider == provider
        {
            return Ok(());
        }
        let mut composition = self.spec.agent.config.clone();
        composition.provider = provider;
        self.configure(self.spec.agent.revision, composition).await
    }

    async fn refresh_provider(
        &mut self,
        provider_id: &str,
        base_url: Option<&str>,
    ) -> std::result::Result<(), Rejection> {
        if !provider_credential_matches(&self.spec.agent.config.provider, provider_id, base_url)
            .map_err(invalid_config)?
        {
            return Ok(());
        }
        if self.pending_turns > 0 || self.approval_active {
            self.restart_after_turn = true;
            return Ok(());
        }
        self.restart("horus-gateway").await?;
        self.broadcast_changed().await
    }

    async fn restart(&mut self, origin_label: &str) -> std::result::Result<(), Rejection> {
        let gateway = self
            .gateway
            .lock()
            .map_err(|_| internal("gateway configuration lock is poisoned"))?
            .clone();
        let replacement = start_agent(
            &gateway,
            &self.spec,
            &self.store,
            Arc::clone(&self.credentials),
            Arc::clone(&self.cron),
            Arc::clone(&self.checkpoints),
            self.scratchpad.clone(),
            self.attachments.clone(),
            self.running.session_id.clone(),
            origin_label,
            false,
        )
        .await
        .map_err(internal)?;
        let suppress_history_broadcast = reset_replay_for_restart(
            &mut self.replay,
            &self.running.session_id,
            &replacement.session_id,
        );
        self.replay_truncated = false;
        let previous = std::mem::replace(&mut self.running, replacement);
        self.accepts_file_attachments.store(
            runtime_accepts_attachments(&self.running.frontend),
            Ordering::Relaxed,
        );
        self.suppress_history_broadcast = suppress_history_broadcast;
        self.widgets.clear();
        shutdown_agent(previous).await;
        if suppress_history_broadcast {
            self.record_replacement_startup().map_err(internal)?;
        }
        self.pending_turns = 0;
        self.approval_active = false;
        self.turn_error = None;
        Ok(())
    }

    fn record_replacement_startup(&mut self) -> Result<()> {
        loop {
            match self.running.events.try_recv() {
                Ok(event) => {
                    let is_history = self.suppress_history_broadcast
                        && matches!(&event.msg, EventMsg::SessionHistory(_));
                    if is_history {
                        self.suppress_history_broadcast = false;
                    }
                    let frame = self.record_event(event, true)?;
                    if !is_history {
                        self.pending_startup.push(frame);
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    return Err(Error::Config(
                        "replacement agent stopped during startup".into(),
                    ));
                }
            }
        }
        self.suppress_history_broadcast = false;
        Ok(())
    }

    async fn forward_event(&mut self, event: Event) -> Result<()> {
        let was_active = self.pending_turns > 0;
        let next_activity = self.activity_for_event(&event.msg)?;
        let suppress_broadcast =
            self.suppress_history_broadcast && matches!(&event.msg, EventMsg::SessionHistory(_));
        if suppress_broadcast {
            self.suppress_history_broadcast = false;
        }
        match &event.msg {
            EventMsg::ExecApprovalRequest(_) => self.approval_active = true,
            EventMsg::TurnComplete(_) => {
                self.pending_turns = self.pending_turns.saturating_sub(1);
                self.approval_active = false;
            }
            EventMsg::TurnAborted(_) => {
                self.pending_turns = self.pending_turns.saturating_sub(1);
                self.approval_active = false;
                self.cron.cancel_setup(&self.running.session_id);
            }
            _ => {}
        }
        let cron_completion = self.observe_cron_event(&event)?;
        self.record_event(event, suppress_broadcast)?;
        if let Some(activity) = next_activity {
            self.set_activity(activity)?;
            self.broadcast_sessions()
                .await
                .map_err(|rejection| Error::Config(rejection.message))?;
        }

        if self.pending_turns == 0 && was_active {
            if let Some((active, status, message)) = cron_completion {
                self.cron.finish_run(active.run, status, message)?;
            } else if self.restart_after_turn {
                self.restart_after_turn = false;
                self.restart("horus-gateway")
                    .await
                    .map_err(|rejection| Error::Config(rejection.message))?;
                self.broadcast_changed()
                    .await
                    .map_err(|rejection| Error::Config(rejection.message))?;
            }
            if !self.approval_active && self.active_cron.is_none() {
                for waiter in self.idle_waiters.drain(..) {
                    let _ = waiter.send(());
                }
            }
        }
        Ok(())
    }

    fn record_event(&mut self, event: Event, suppress_broadcast: bool) -> Result<ServerFrame> {
        if let Some(delta) = live_usage_delta(&mut self.usage_baseline, &event)? {
            let mut gateway = self
                .gateway
                .lock()
                .map_err(|_| Error::Config("gateway configuration lock is poisoned".into()))?;
            if gateway.observe_usage(&delta)? {
                self.store.save(&gateway)?;
            }
        }
        update_widgets(&mut self.widgets, &event.msg);
        let blocks = self.running.frontend.render(&event.msg);
        self.record_artifacts(&blocks);
        let history = render_history(&self.running.frontend, &event.msg);
        if let Some(history) = &history {
            for rendered in history {
                self.record_artifacts(&rendered.blocks);
            }
        }
        let preview = render_preview(&self.running.frontend, &event.msg);
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| Error::Config("event sequence overflow".into()))?;
        let frame = ServerFrame::new(ServerMessage::AgentEvent {
            session_id: self.running.session_id.clone(),
            sequence: self.sequence,
            event,
            blocks,
            history,
            preview,
        });
        self.replay_truncated |= record_and_publish(
            &mut self.replay,
            &self.events,
            frame.clone(),
            suppress_broadcast,
        );
        Ok(frame)
    }

    fn observe_cron_event(
        &mut self,
        event: &Event,
    ) -> Result<Option<(ActiveCron, CronRunStatus, Option<String>)>> {
        let Some(active) = self.active_cron.as_mut() else {
            return Ok(None);
        };
        let completion = match &event.msg {
            EventMsg::TurnStarted(turn)
                if event.submission_id.as_deref() == Some(active.submission_id.as_str()) =>
            {
                active.turn_id = Some(turn.turn_id.clone());
                None
            }
            EventMsg::Error(error) => {
                active.failure.get_or_insert_with(|| error.message.clone());
                None
            }
            EventMsg::ExecApprovalRequest(request)
                if active.turn_id.as_deref() == Some(request.turn_id.as_str()) =>
            {
                active.failure.get_or_insert_with(|| {
                    "headless cron run requested interactive tool approval".into()
                });
                self.running.sender.send(Submission {
                    id: Uuid::new_v4().to_string(),
                    op: Op::ExecApproval {
                        id: request.id.clone(),
                        decision: ReviewDecision::Abort,
                    },
                })?;
                self.approval_active = false;
                None
            }
            EventMsg::TurnComplete(turn)
                if active.turn_id.as_deref() == Some(turn.turn_id.as_str()) =>
            {
                Some(match active.failure.clone() {
                    Some(message) => (CronRunStatus::Failed, Some(message)),
                    None => (CronRunStatus::Succeeded, None),
                })
            }
            EventMsg::TurnAborted(turn)
                if active.turn_id.as_deref() == Some(turn.turn_id.as_str()) =>
            {
                Some((
                    CronRunStatus::Failed,
                    Some(
                        active
                            .failure
                            .clone()
                            .unwrap_or_else(|| turn.reason.clone()),
                    ),
                ))
            }
            _ => None,
        };
        Ok(completion.map(|(status, message)| {
            let active = self
                .active_cron
                .take()
                .expect("completion requires an active cron run");
            (active, status, message)
        }))
    }

    fn record_artifacts(&mut self, blocks: &[FrontendBlock]) {
        for block in blocks
            .iter()
            .filter(|block| block.format == FrontendBlockFormat::UnifiedDiff)
        {
            upsert_artifact(&mut self.artifacts, &self.running.session_id, block);
        }
    }

    async fn ready(&self) -> Result<SessionReadyPayload> {
        let checkpoint = self
            .checkpoints
            .load(&self.running.session_id)
            .await?
            .ok_or_else(|| Error::Config("the running session has no checkpoint".into()))?;
        let mut run_stats = completed_run_stats(&checkpoint.execution_stats);
        run_stats.active = checkpoint
            .active_execution
            .as_ref()
            .map(|active| active_run_summary(&checkpoint.session_id, active));
        Ok(SessionReadyPayload {
            replay_epoch: self.replay_epoch.clone(),
            latest_sequence: self.sequence,
            next_before_sequence: next_history_cursor(
                self.running.next_before_sequence,
                checkpoint.sequence,
                self.replay_truncated,
            ),
            workspace: self.spec.workspace_info(),
            git: git_status(&self.running.gateway_sandbox).await,
            session: self.running.session.clone(),
            contributions: self.running.frontend.contributions().to_vec(),
            widgets: self
                .widgets
                .iter()
                .map(|((capability, _), item)| SessionWidget {
                    capability: capability.clone(),
                    item: item.clone(),
                })
                .collect(),
            tool_count: self.running.tool_count,
            run_stats,
            config: self.spec.agent.clone(),
        })
    }

    async fn switch_git_branch(&mut self, branch: &str) -> std::result::Result<(), Rejection> {
        self.require_idle()?;
        switch_workspace_branch(&self.running.gateway_sandbox, branch).await?;
        self.broadcast_changed().await
    }

    async fn broadcast_changed(&mut self) -> std::result::Result<(), Rejection> {
        let payload = self.ready().await.map_err(internal)?;
        let ready = ServerFrame::new(ServerMessage::SessionChanged { payload });
        let pending = std::mem::take(&mut self.pending_startup);
        publish_ready_and_pending(&self.events, ready, pending);
        Ok(())
    }

    async fn broadcast_sessions(&self) -> std::result::Result<(), Rejection> {
        let sessions = session_catalog(&self.checkpoints, &self.activities)
            .await
            .map_err(internal)?;
        let _ = self
            .gateway_events
            .send(ServerFrame::new(ServerMessage::Sessions {
                request_id: None,
                sessions,
            }));
        Ok(())
    }

    fn activity_for_event(&mut self, event: &EventMsg) -> Result<Option<SessionActivity>> {
        let current = self.activity()?;
        let next = match event {
            EventMsg::TurnStarted(turn) => {
                self.turn_error = None;
                Some(SessionActivity {
                    state: SessionActivityState::Running,
                    turn_id: Some(turn.turn_id.clone()),
                    started_at: Some(Utc::now().timestamp()),
                    ..SessionActivity::default()
                })
            }
            EventMsg::ExecApprovalRequest(request) => Some(SessionActivity {
                state: SessionActivityState::AwaitingApproval,
                turn_id: Some(request.turn_id.clone()),
                started_at: current.started_at.or_else(|| Some(Utc::now().timestamp())),
                ..SessionActivity::default()
            }),
            EventMsg::Error(error) if current.state == SessionActivityState::Idle => {
                self.turn_error = None;
                Some(SessionActivity {
                    last_outcome: Some(SessionOutcome::Failed),
                    message: Some(error.message.clone()),
                    ..SessionActivity::default()
                })
            }
            EventMsg::Error(error) => {
                self.turn_error = Some(error.message.clone());
                None
            }
            EventMsg::TurnComplete(_) => {
                let message = self.turn_error.take();
                Some(SessionActivity {
                    last_outcome: Some(if message.is_some() {
                        SessionOutcome::Failed
                    } else {
                        SessionOutcome::Completed
                    }),
                    message,
                    ..SessionActivity::default()
                })
            }
            EventMsg::TurnAborted(turn) => {
                let error = self.turn_error.take();
                Some(SessionActivity {
                    last_outcome: Some(if error.is_some() {
                        SessionOutcome::Failed
                    } else {
                        SessionOutcome::Aborted
                    }),
                    message: Some(error.unwrap_or_else(|| turn.reason.clone())),
                    ..SessionActivity::default()
                })
            }
            _ => None,
        };
        Ok(next)
    }

    async fn resume_activity(&self) -> Result<()> {
        let current = self.activity()?;
        if current.state != SessionActivityState::AwaitingApproval {
            return Ok(());
        }
        self.set_activity(SessionActivity {
            state: SessionActivityState::Running,
            turn_id: current.turn_id,
            started_at: current.started_at,
            ..SessionActivity::default()
        })?;
        self.broadcast_sessions()
            .await
            .map_err(|rejection| Error::Config(rejection.message))
    }

    async fn fail_activity(&self, message: &str) -> Result<()> {
        if self.activity()?.state == SessionActivityState::Idle {
            return Ok(());
        }
        self.set_activity(SessionActivity {
            last_outcome: Some(SessionOutcome::Failed),
            message: Some(message.into()),
            ..SessionActivity::default()
        })?;
        self.broadcast_sessions()
            .await
            .map_err(|rejection| Error::Config(rejection.message))
    }

    fn activity(&self) -> Result<SessionActivity> {
        let activities = self
            .activities
            .lock()
            .map_err(|_| Error::Config("session activity lock is poisoned".into()))?;
        Ok(activities
            .get(&self.running.session_id)
            .cloned()
            .unwrap_or_default())
    }

    fn set_activity(&self, activity: SessionActivity) -> Result<()> {
        self.activities
            .lock()
            .map_err(|_| Error::Config("session activity lock is poisoned".into()))?
            .insert(self.running.session_id.clone(), activity);
        Ok(())
    }

    fn broadcast(&self, message: ServerMessage) {
        let _ = self.events.send(ServerFrame::new(message));
    }

    fn require_idle(&self) -> std::result::Result<(), Rejection> {
        if !self.is_idle() {
            Err(Rejection {
                code: "agent_busy",
                message: "finish or interrupt the active turn before changing gateway state".into(),
                fatal: false,
            })
        } else {
            Ok(())
        }
    }

    fn is_idle(&self) -> bool {
        self.pending_turns == 0 && !self.approval_active && self.active_cron.is_none()
    }
}

fn live_usage_delta(baseline: &mut TokenUsage, event: &Event) -> Result<Option<TokenUsage>> {
    let EventMsg::TokenCount(count) = &event.msg else {
        return Ok(None);
    };
    let Some(info) = &count.info else {
        return Ok(None);
    };
    let delta = usage_delta(&info.total_token_usage, baseline)?;
    baseline.clone_from(&info.total_token_usage);
    if event.submission_id.is_none() {
        return Ok(None);
    }
    Ok(delta)
}

fn provider_credential_matches(
    selection: &ProviderConfig,
    provider_id: &str,
    base_url: Option<&str>,
) -> Result<bool> {
    if selection.provider != provider_id {
        return Ok(false);
    }
    let definition = provider(provider_id)?;
    let selected_base_url = definition
        .configurable_base_url()
        .then(|| {
            selection
                .base_url
                .as_deref()
                .or_else(|| definition.default_base_url())
        })
        .flatten();
    Ok(selected_base_url == base_url)
}

fn fail_active_cron(
    cron: &CronStore,
    active: &mut Option<ActiveCron>,
    message: &str,
) -> Result<()> {
    let Some(active) = active.take() else {
        return Ok(());
    };
    cron.finish_run(active.run, CronRunStatus::Failed, Some(message.to_string()))
        .map(|_| ())
}

async fn gateway_session_summaries(
    checkpoints: &Arc<dyn CheckpointStore>,
) -> Result<Vec<SessionSummary>> {
    let mut cursor = None;
    let mut sessions = Vec::new();
    loop {
        let page = checkpoints
            .list_sessions_page(SessionPageRequest {
                cursor,
                limit: SESSION_PAGE_SIZE,
            })
            .await?;
        sessions.extend(page.sessions);
        let Some(next) = page.next_cursor else {
            return Ok(sessions);
        };
        cursor = Some(next);
    }
}

fn gateway_run_stats(sessions: &[SessionSummary]) -> Result<RunStats> {
    let mut totals = RunStats::default();
    for session in sessions {
        add_execution_stats(&mut totals, &session.execution_stats)?;
    }
    Ok(totals)
}

fn add_execution_stats(total: &mut RunStats, stats: &ExecutionStats) -> Result<()> {
    let (
        Some(run_count),
        Some(failed_run_count),
        Some(aborted_run_count),
        Some(model_calls),
        Some(tool_calls),
        Some(failed_tool_calls),
        Some(elapsed_ms),
    ) = (
        total.run_count.checked_add(stats.run_count),
        total.failed_run_count.checked_add(stats.failed_run_count),
        total.aborted_run_count.checked_add(stats.aborted_run_count),
        total.model_calls.checked_add(stats.model_calls),
        total.tool_calls.checked_add(stats.tool_calls),
        total.failed_tool_calls.checked_add(stats.failed_tool_calls),
        total.elapsed_ms.checked_add(stats.elapsed_ms),
    )
    else {
        return Err(Error::Config(
            "gateway execution statistics exceed the supported range".into(),
        ));
    };
    let mut usage = total.usage.clone();
    if usage.checked_add(&stats.usage).is_none() {
        return Err(Error::Config(
            "gateway execution statistics exceed the supported range".into(),
        ));
    }
    *total = RunStats {
        run_count,
        failed_run_count,
        aborted_run_count,
        model_calls,
        tool_calls,
        failed_tool_calls,
        elapsed_ms,
        usage,
        active: None,
    };
    Ok(())
}

fn completed_run_stats(stats: &ExecutionStats) -> RunStats {
    RunStats {
        run_count: stats.run_count,
        failed_run_count: stats.failed_run_count,
        aborted_run_count: stats.aborted_run_count,
        model_calls: stats.model_calls,
        tool_calls: stats.tool_calls,
        failed_tool_calls: stats.failed_tool_calls,
        elapsed_ms: stats.elapsed_ms,
        usage: stats.usage.clone(),
        active: None,
    }
}

fn run_summary(record: ExecutionRecord) -> RunSummary {
    RunSummary {
        session_id: record.session_id,
        submission_id: record.submission_id,
        turn_id: record.turn_id,
        started_at_ms: record.started_at_ms,
        finished_at_ms: Some(record.finished_at_ms),
        elapsed_ms: record.elapsed_ms,
        outcome: Some(session_outcome(record.outcome)),
        model_calls: record.model_calls,
        tool_calls: record.tool_calls,
        failed_tool_calls: record.failed_tool_calls,
        usage: record.usage,
    }
}

fn recent_run_groups(
    records: Vec<ExecutionRecord>,
    sessions: &[SessionSummary],
    metadata: &SessionCatalogMetadata,
) -> Vec<SessionRunGroup> {
    let sessions_by_id = sessions
        .iter()
        .map(|session| (session.session_id.as_str(), session))
        .collect::<HashMap<_, _>>();
    let mut groups: Vec<SessionRunGroup> = Vec::new();
    for record in records {
        let Some(root) = visible_session(&record.session_id, &sessions_by_id) else {
            continue;
        };
        if metadata
            .get(&root.session_id)
            .is_some_and(|item| item.hidden)
        {
            continue;
        }
        let run = run_summary(record);
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.session_id == root.session_id)
        {
            group.runs.push(run);
        } else {
            groups.push(SessionRunGroup {
                session_id: root.session_id.clone(),
                title: session_run_group_title(root, metadata),
                runs: vec![run],
            });
        }
    }
    groups
}

fn visible_session<'a>(
    session_id: &str,
    sessions: &'a HashMap<&str, &'a SessionSummary>,
) -> Option<&'a SessionSummary> {
    let mut session = *sessions.get(session_id)?;
    for _ in 0..sessions.len() {
        if session.catalog_visible {
            return Some(session);
        }
        session = *sessions.get(session.parent_session_id.as_deref()?)?;
    }
    None
}

fn session_run_group_title(session: &SessionSummary, metadata: &SessionCatalogMetadata) -> String {
    metadata
        .get(&session.session_id)
        .and_then(|item| item.title.as_deref())
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .or_else(|| {
            session
                .first_user_message
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
        })
        .unwrap_or("Untitled")
        .to_owned()
}

fn active_run_summary(session_id: &str, active: &ActiveExecution) -> RunSummary {
    let elapsed_ms = Utc::now()
        .timestamp_millis()
        .checked_sub(active.started_at_ms)
        .and_then(|elapsed| u64::try_from(elapsed).ok())
        .unwrap_or_default();
    RunSummary {
        session_id: session_id.into(),
        submission_id: active.submission_id.clone(),
        turn_id: active.turn_id.clone(),
        started_at_ms: active.started_at_ms,
        finished_at_ms: None,
        elapsed_ms,
        outcome: None,
        model_calls: active.model_calls,
        tool_calls: active.tool_calls,
        failed_tool_calls: active.failed_tool_calls,
        usage: active.usage.clone(),
    }
}

const fn session_outcome(outcome: ExecutionOutcome) -> SessionOutcome {
    match outcome {
        ExecutionOutcome::Completed => SessionOutcome::Completed,
        ExecutionOutcome::Aborted => SessionOutcome::Aborted,
        ExecutionOutcome::Failed => SessionOutcome::Failed,
    }
}

async fn gateway_ready(state: &GatewayState) -> std::result::Result<ReadyPayload, Rejection> {
    let config = state
        .config
        .lock()
        .map_err(|_| internal("gateway configuration lock is poisoned"))?
        .clone();
    let models =
        configured_model_choices(&config, &state.store, &state.credentials).map_err(internal)?;
    let model_providers =
        configured_model_providers(&config, &state.store, &state.credentials).map_err(internal)?;
    let middleware_features = crate::middleware_manifest::features(&models);
    Ok(ReadyPayload {
        machine_name: local_machine_name().map_err(internal)?,
        sessions: session_catalog(&state.checkpoints, &state.activities)
            .await
            .map_err(internal)?,
        providers: provider_statuses(&config, &state.store, &state.credentials)
            .map_err(internal)?,
        models,
        model_providers,
        default_config: config.default_agent,
        middleware_features,
        max_active_sessions: MAX_ACTIVE_SESSIONS,
    })
}

fn local_machine_name() -> Result<String> {
    let name = nix::unistd::gethostname()
        .map_err(|error| Error::Config(format!("failed to read the machine hostname: {error}")))?
        .into_string()
        .map_err(|_| Error::Config("the machine hostname is not valid UTF-8".into()))?;
    let name = name.trim();
    if name.is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
        return Err(Error::Config("the machine hostname is invalid".into()));
    }
    Ok(name.to_owned())
}

fn setup_agent_config() -> VersionedAgentConfig {
    VersionedAgentConfig {
        revision: 1,
        config: AgentComposition::default(),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "agent assembly keeps chat and gateway dependencies explicit"
)]
async fn start_agent(
    gateway: &GatewayConfig,
    spec: &ChatSpec,
    store: &ConfigStore,
    credentials: Arc<CredentialStore>,
    cron: Arc<CronStore>,
    checkpoints: Arc<dyn CheckpointStore>,
    scratchpad: ScratchpadStore,
    attachments: AttachmentStore,
    session_id: String,
    origin_label: &str,
    override_saved_model_route: bool,
) -> Result<RunningAgent> {
    let BuiltAgent {
        agent,
        gateway_sandbox,
        subagent_template,
    } = assemble(
        gateway,
        spec,
        store,
        credentials,
        cron,
        checkpoints,
        scratchpad,
        attachments,
        Some(session_id),
        origin_label,
        override_saved_model_route,
    )
    .await?;
    let session = agent.session().clone();
    let frontend = agent.frontend().clone();
    let tool_count = agent.tool_count();
    let next_before_sequence = agent.next_before_sequence();
    let session_id = session.session_id.clone();
    let (sender, events) = agent.into_parts();
    Ok(RunningAgent {
        session_id,
        sender,
        events,
        frontend,
        session,
        gateway_sandbox,
        subagent_template,
        tool_count,
        next_before_sequence,
    })
}

fn runtime_accepts_attachments(frontend: &FrontendExtensions) -> bool {
    frontend
        .contributions()
        .iter()
        .any(|contribution| contribution.accepts_file_attachments)
}

async fn shutdown_agent(agent: RunningAgent) {
    let RunningAgent {
        sender,
        mut events,
        subagent_template,
        ..
    } = agent;
    drop(sender);
    while events.recv().await.is_some() {}
    drop(subagent_template);
}

fn cron_execution_checkpoint(
    source: &Checkpoint,
    session_id: &str,
    origin_label: &str,
) -> Checkpoint {
    let mut checkpoint = Checkpoint::empty(session_id);
    checkpoint
        .session_context
        .clone_from(&source.session_context);
    checkpoint.session_context.origin_label = Some(origin_label.into());
    checkpoint.metadata.clone_from(&source.metadata);
    checkpoint.model_route.clone_from(&source.model_route);
    checkpoint
}

async fn hide_checkpoint(checkpoints: &Arc<dyn CheckpointStore>, session_id: &str) -> Result<()> {
    let Some(mut checkpoint) = checkpoints.load(session_id).await? else {
        return Ok(());
    };
    checkpoint.catalog_visible = false;
    checkpoint.sequence = checkpoint
        .sequence
        .checked_add(1)
        .ok_or_else(|| Error::Config("checkpoint sequence overflow".into()))?;
    checkpoints.save(&checkpoint, &[], None).await?;
    Ok(())
}

fn render_preview(frontend: &FrontendExtensions, event: &EventMsg) -> Option<RenderedPreview> {
    let EventMsg::Frontend(FrontendEvent::Preview { title, events }) = event else {
        return None;
    };
    Some(RenderedPreview {
        title: title.clone(),
        events: flatten_preview(events)
            .into_iter()
            .map(|event| RenderedEvent {
                blocks: frontend.render(&event),
                event,
            })
            .collect(),
    })
}

fn render_history(frontend: &FrontendExtensions, event: &EventMsg) -> Option<Vec<RenderedEvent>> {
    render_history_with(event, |event| frontend.render(event))
}

fn render_history_with(
    event: &EventMsg,
    render: impl Fn(&EventMsg) -> Vec<FrontendBlock>,
) -> Option<Vec<RenderedEvent>> {
    let EventMsg::SessionHistory(history) = event else {
        return None;
    };
    Some(
        flatten_history(&history.events)
            .into_iter()
            .map(|event| RenderedEvent {
                blocks: render(&event),
                event,
            })
            .collect(),
    )
}

fn flatten_history(events: &[EventMsg]) -> Vec<EventMsg> {
    let mut flattened = Vec::new();
    for event in events {
        match event {
            EventMsg::SessionHistory(history) => flattened.extend(flatten_history(&history.events)),
            event => flattened.push(event.clone()),
        }
    }
    flattened
}

fn flatten_preview(events: &[EventMsg]) -> Vec<EventMsg> {
    let mut flattened = Vec::new();
    for event in events {
        match event {
            EventMsg::SessionHistory(history) => flattened.extend(flatten_preview(&history.events)),
            EventMsg::Frontend(
                FrontendEvent::Widget { .. }
                | FrontendEvent::RemoveWidget { .. }
                | FrontendEvent::Picker { .. }
                | FrontendEvent::Preview { .. },
            ) => {}
            event => flattened.push(event.clone()),
        }
    }
    flattened
}

fn event_sequence(frame: &ServerFrame) -> Option<u64> {
    match frame.message {
        ServerMessage::AgentEvent { sequence, .. } => Some(sequence),
        _ => None,
    }
}

fn reset_replay_for_restart(
    replay: &mut VecDeque<ServerFrame>,
    previous_session: &str,
    next_session: &str,
) -> bool {
    replay.clear();
    previous_session == next_session
}

fn next_history_cursor(
    initial: Option<u64>,
    latest_checkpoint_sequence: u64,
    replay_truncated: bool,
) -> Option<u64> {
    if replay_truncated && latest_checkpoint_sequence > 0 {
        Some(latest_checkpoint_sequence.saturating_add(1))
    } else {
        initial
    }
}

fn update_widgets(widgets: &mut SessionWidgets, event: &EventMsg) {
    match event {
        EventMsg::Frontend(FrontendEvent::Widget { capability, item }) => {
            widgets.insert((capability.clone(), item.id.clone()), item.clone());
        }
        EventMsg::Frontend(FrontendEvent::RemoveWidget { capability, id }) => {
            widgets.remove(&(capability.clone(), id.clone()));
        }
        _ => {}
    }
}

fn upsert_artifact(
    artifacts: &mut VecDeque<ArtifactRecord>,
    session_id: &str,
    block: &FrontendBlock,
) {
    let id = block
        .id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    if let Some(index) = artifacts.iter().position(|artifact| artifact.id == id) {
        artifacts.remove(index);
    } else if artifacts.len() == ARTIFACT_CAPACITY {
        artifacts.pop_front();
    }
    artifacts.push_back(ArtifactRecord {
        id,
        session_id: session_id.into(),
        kind: ArtifactKind::CodeDiff,
        title: block.group.clone().unwrap_or_else(|| "Code diff".into()),
        block: block.clone(),
    });
}

fn record_and_publish(
    replay: &mut VecDeque<ServerFrame>,
    events: &broadcast::Sender<ServerFrame>,
    frame: ServerFrame,
    suppress_broadcast: bool,
) -> bool {
    let mut truncated = false;
    if !matches!(
        &frame.message,
        ServerMessage::AgentEvent {
            event: Event {
                msg: EventMsg::SessionResumeRequested(_),
                ..
            },
            ..
        }
    ) {
        if replay.len() == REPLAY_CAPACITY {
            replay.pop_front();
            truncated = true;
        }
        replay.push_back(frame.clone());
    }
    if !suppress_broadcast {
        let _ = events.send(frame);
    }
    truncated
}

fn publish_ready_and_pending(
    events: &broadcast::Sender<ServerFrame>,
    ready: ServerFrame,
    pending: Vec<ServerFrame>,
) {
    let _ = events.send(ready);
    for frame in pending {
        let _ = events.send(frame);
    }
}

async fn receive<T>(
    receiver: oneshot::Receiver<std::result::Result<T, Rejection>>,
) -> std::result::Result<T, Rejection> {
    receiver.await.map_err(|_| stopped())?
}

fn stopped() -> Rejection {
    Rejection {
        code: "gateway_stopped",
        message: "the gateway host stopped".into(),
        fatal: true,
    }
}

fn internal(error: impl std::fmt::Display) -> Rejection {
    Rejection {
        code: "gateway_error",
        message: error.to_string(),
        fatal: false,
    }
}

fn invalid_config(error: impl std::fmt::Display) -> Rejection {
    Rejection {
        code: "invalid_config",
        message: error.to_string(),
        fatal: false,
    }
}

fn invalid_workspace(error: impl std::fmt::Display) -> Rejection {
    Rejection {
        code: "invalid_workspace",
        message: error.to_string(),
        fatal: false,
    }
}

fn invalid_session_workspace() -> Rejection {
    Rejection {
        code: "invalid_session_workspace",
        message: "the requested session belongs to another workspace".into(),
        fatal: false,
    }
}

fn unknown_session() -> Rejection {
    Rejection {
        code: "unknown_session",
        message: "the requested chat does not exist".into(),
        fatal: false,
    }
}

fn invalid_cron(error: impl std::fmt::Display) -> Rejection {
    Rejection {
        code: "invalid_cron",
        message: error.to_string(),
        fatal: false,
    }
}

#[cfg(test)]
mod tests {
    use horus::backend::checkpoint::{Checkpoint, TranscriptPageRequest};
    use horus::backend::model::user_message;
    use horus::protocol::{SessionContext, TokenCountEvent, TokenUsageInfo};

    use super::*;

    #[test]
    fn startup_usage_seeds_the_live_delta_without_being_counted() {
        let usage = |tokens| TokenUsage {
            input_tokens: tokens,
            total_tokens: tokens,
            ..TokenUsage::default()
        };
        let event = |submission_id: Option<&str>, tokens| Event {
            submission_id: submission_id.map(str::to_owned),
            msg: EventMsg::TokenCount(TokenCountEvent {
                info: Some(TokenUsageInfo {
                    total_token_usage: usage(tokens),
                    last_token_usage: usage(tokens),
                    model_context_window: None,
                }),
                rate_limits: None,
            }),
        };
        let mut baseline = TokenUsage::default();

        let startup = live_usage_delta(&mut baseline, &event(None, 100)).expect("startup usage");
        let live =
            live_usage_delta(&mut baseline, &event(Some("submission"), 130)).expect("live usage");

        assert_eq!(
            (startup, live, baseline),
            (None, Some(usage(30)), usage(130))
        );
    }

    #[test]
    fn widget_snapshot_is_namespaced_updated_and_removed() {
        let widget = |text: &str| horus::protocol::FrontendWidget {
            id: "status".into(),
            slot: horus::protocol::FrontendSlot::Header,
            text: text.into(),
            tone: horus::protocol::FrontendTone::Neutral,
            symbol: None,
            icon_only: false,
            progress: None,
            content: None,
            action: None,
        };
        let mut widgets = SessionWidgets::new();
        update_widgets(
            &mut widgets,
            &EventMsg::Frontend(FrontendEvent::Widget {
                capability: "tasks".into(),
                item: widget("one"),
            }),
        );
        update_widgets(
            &mut widgets,
            &EventMsg::Frontend(FrontendEvent::Widget {
                capability: "subagents".into(),
                item: widget("two"),
            }),
        );
        update_widgets(
            &mut widgets,
            &EventMsg::Frontend(FrontendEvent::Widget {
                capability: "tasks".into(),
                item: widget("updated"),
            }),
        );
        update_widgets(
            &mut widgets,
            &EventMsg::Frontend(FrontendEvent::RemoveWidget {
                capability: "subagents".into(),
                id: "status".into(),
            }),
        );

        assert_eq!(
            widgets
                .into_iter()
                .map(|((capability, id), item)| (capability, id, item.text))
                .collect::<Vec<_>>(),
            vec![("tasks".into(), "status".into(), "updated".into())]
        );
    }

    #[test]
    fn completed_execution_stats_project_without_an_active_run() {
        let stats = ExecutionStats {
            run_count: 3,
            failed_run_count: 1,
            aborted_run_count: 1,
            model_calls: 5,
            tool_calls: 8,
            failed_tool_calls: 2,
            elapsed_ms: 900,
            usage: TokenUsage {
                total_tokens: 42,
                ..TokenUsage::default()
            },
        };

        let projected = completed_run_stats(&stats);

        assert_eq!(
            (
                projected.run_count,
                projected.failed_run_count,
                projected.aborted_run_count,
                projected.model_calls,
                projected.tool_calls,
                projected.failed_tool_calls,
                projected.elapsed_ms,
                projected.usage.total_tokens,
                projected.active,
            ),
            (3, 1, 1, 5, 8, 2, 900, 42, None)
        );
    }

    #[test]
    fn recent_runs_group_under_the_nearest_visible_session_in_source_order() {
        let sessions = vec![
            session_summary("root", None, true, Some("Root preview")),
            session_summary("nested-agent", Some("agent"), false, None),
            session_summary("agent", Some("root"), false, None),
            session_summary("fork", Some("root"), true, None),
            session_summary("fork-agent", Some("fork"), false, None),
        ];
        let records = vec![
            execution_record("nested-agent", "nested", 5),
            execution_record("fork-agent", "fork-agent", 4),
            execution_record("root", "root", 3),
            execution_record("fork", "fork", 2),
        ];
        let mut metadata = SessionCatalogMetadata::new();
        metadata.insert(
            "root".into(),
            catalog::SessionMetadata {
                title: Some("Renamed root".into()),
                ..catalog::SessionMetadata::default()
            },
        );

        let groups = recent_run_groups(records, &sessions, &metadata);
        let projection = groups
            .iter()
            .map(|group| {
                (
                    group.session_id.as_str(),
                    group.title.as_str(),
                    group
                        .runs
                        .iter()
                        .map(|run| (run.session_id.as_str(), run.turn_id.as_str()))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            projection,
            vec![
                (
                    "root",
                    "Renamed root",
                    vec![("nested-agent", "nested"), ("root", "root")]
                ),
                (
                    "fork",
                    "Untitled",
                    vec![("fork-agent", "fork-agent"), ("fork", "fork")]
                )
            ]
        );
    }

    #[test]
    fn recent_runs_omit_metadata_hidden_roots() {
        let sessions = vec![
            session_summary("hidden", None, true, None),
            session_summary("hidden-agent", Some("hidden"), false, None),
            session_summary("shown", None, true, Some("  Shown thread  ")),
        ];
        let records = vec![
            execution_record("hidden-agent", "hidden", 2),
            execution_record("shown", "shown", 1),
        ];
        let mut metadata = SessionCatalogMetadata::new();
        metadata.insert(
            "hidden".into(),
            catalog::SessionMetadata {
                hidden: true,
                ..catalog::SessionMetadata::default()
            },
        );

        let groups = recent_run_groups(records, &sessions, &metadata);

        assert!(matches!(
            groups.as_slice(),
            [SessionRunGroup { session_id, title, runs }]
                if session_id == "shown" && title == "Shown thread" && runs[0].turn_id == "shown"
        ));
    }

    fn session_summary(
        session_id: &str,
        parent_session_id: Option<&str>,
        catalog_visible: bool,
        first_user_message: Option<&str>,
    ) -> SessionSummary {
        SessionSummary {
            session_id: session_id.into(),
            session_context: SessionContext::default(),
            parent_session_id: parent_session_id.map(str::to_owned),
            parent_sequence: parent_session_id.map(|_| 0),
            sequence: 0,
            catalog_visible,
            first_user_message: first_user_message.map(str::to_owned),
            execution_stats: ExecutionStats::default(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn execution_record(session_id: &str, turn_id: &str, started_at_ms: i64) -> ExecutionRecord {
        ExecutionRecord {
            session_id: session_id.into(),
            submission_id: format!("submission-{turn_id}"),
            turn_id: turn_id.into(),
            started_at_ms,
            finished_at_ms: started_at_ms,
            elapsed_ms: 0,
            outcome: ExecutionOutcome::Completed,
            model_calls: 0,
            tool_calls: 0,
            failed_tool_calls: 0,
            usage: TokenUsage::default(),
        }
    }

    #[tokio::test]
    async fn durable_history_pages_and_initial_replay_share_a_cursor() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) =
            ConfigStore::initialize(root.path().join("state"), listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credentials"));
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway = GatewayHost::start(store, config, credentials, cron).expect("gateway");
        let host = gateway
            .create_session(&workspace)
            .await
            .expect("create session");
        let checkpoints = Arc::clone(&gateway.state.lock().await.checkpoints);
        let mut checkpoint = checkpoints
            .load(host.session_id())
            .await
            .expect("load checkpoint")
            .expect("checkpoint");
        let user = user_message("first");
        checkpoint.sequence += 1;
        checkpoint.context.push(user.clone());
        checkpoints
            .save(&checkpoint, &[user], None)
            .await
            .expect("save user message");
        let assistant = serde_json::json!({"role": "assistant", "content": "second"});
        checkpoint.sequence += 1;
        checkpoint.context.push(assistant.clone());
        checkpoints
            .save(&checkpoint, &[assistant], None)
            .await
            .expect("save assistant message");

        let newest = host.history_page(None, 1).await.expect("newest page");
        let oldest = host
            .history_page(newest.next_before_sequence, 1)
            .await
            .expect("oldest page");

        assert!(matches!(
            (&oldest.events[..], &newest.events[..]),
            ([RenderedEvent { event: EventMsg::UserMessage(user), .. }],
             [RenderedEvent { event: EventMsg::AgentMessage(agent), .. }])
                if user.message == "first" && agent.message == "second"
        ));

        for sequence in 3..=101 {
            let item = user_message(&format!("message {sequence}"));
            checkpoint.sequence = sequence;
            checkpoint.context.push(item.clone());
            checkpoints
                .save(&checkpoint, &[item], None)
                .await
                .expect("save history item");
        }
        let expected = checkpoints
            .transcript_page(
                host.session_id(),
                TranscriptPageRequest {
                    before_sequence: None,
                    max_batches: 100,
                },
            )
            .await
            .expect("initial transcript page")
            .next_before_sequence;
        let session_id = host.session_id().to_owned();
        assert!(host.stop_if_idle().await);
        gateway.state.lock().await.sessions.remove(&session_id);
        drop(host);

        let reopened = gateway
            .open_session(&session_id)
            .await
            .expect("reopen session");
        let snapshot = reopened
            .snapshot(None, None)
            .await
            .expect("session snapshot");

        assert_eq!(
            (expected, snapshot.ready.next_before_sequence),
            (Some(2), Some(2))
        );
    }

    #[test]
    fn cron_execution_inherits_the_chat_recipe_without_transcript_state() {
        let mut source = Checkpoint::empty("source");
        source.context.push(serde_json::json!({"role": "user"}));
        source.first_user_message = Some("source message".into());
        source.model_route = Some("kimi::kimi-k2.5::high".into());
        source.metadata.insert(
            "horus_gateway.chat".into(),
            serde_json::json!({"version": 1}),
        );
        source.session_context.workspace_id = Some("workspace".into());

        let execution = cron_execution_checkpoint(&source, "execution", "cron · task");

        assert_eq!(execution.model_route, source.model_route);
        assert_eq!(execution.metadata, source.metadata);
        assert_eq!(
            execution.session_context,
            horus::protocol::SessionContext {
                workspace_id: Some("workspace".into()),
                origin_label: Some("cron · task".into()),
                ..horus::protocol::SessionContext::default()
            }
        );
        assert!(execution.context.is_empty());
        assert!(execution.first_user_message.is_none());
        assert_eq!(execution.sequence, 0);
    }

    #[test]
    fn stopped_agent_finishes_its_active_cron_run() {
        let state = tempfile::tempdir().expect("state");
        let cron = CronStore::open(state.path()).expect("cron");
        let task = cron
            .add_for_test("source", "do work", "17 3 * * *")
            .expect("task");
        let run = match cron.begin_run(&task.id).expect("begin run") {
            BeginRun::Started(run) => run,
            BeginRun::Skipped => panic!("new task must start"),
        };
        let mut active = Some(ActiveCron {
            run,
            submission_id: "submission".into(),
            turn_id: None,
            failure: None,
        });

        fail_active_cron(&cron, &mut active, "agent stopped").expect("finish run");
        let history = cron.history("source", Some(&task.id)).expect("history");

        assert!(active.is_none());
        assert_eq!(history[0].status, CronRunStatus::Failed);
        assert_eq!(history[0].message.as_deref(), Some("agent stopped"));
    }

    #[tokio::test]
    async fn overlapping_cron_does_not_create_a_visible_execution_chat() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let state_dir = root.path().join("state");
        std::fs::create_dir(&workspace).expect("workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) = ConfigStore::initialize(state_dir, listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credentials"));
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway =
            GatewayHost::start(store, config, credentials, Arc::clone(&cron)).expect("gateway");
        let source = gateway
            .create_session(&workspace)
            .await
            .expect("source chat");
        let task = cron
            .add_for_test(source.session_id(), "do work", "* * * * *")
            .expect("task");
        let held = match cron.begin_run(&task.id).expect("claim run") {
            BeginRun::Started(run) => run,
            BeginRun::Skipped => panic!("first run must start"),
        };
        let before = gateway.sessions().await.expect("sessions before");

        let error = gateway
            .run_cron(source.session_id().into(), task.id)
            .await
            .expect_err("overlap must fail");
        let after = gateway.sessions().await.expect("sessions after");
        cron.finish_run(held, CronRunStatus::Succeeded, None)
            .expect("finish held run");

        assert_eq!(error.code, "cron_overlap");
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn chats_keep_independent_workspace_and_agent_configuration() {
        let root = tempfile::tempdir().expect("root");
        let first = root.path().join("first");
        let second = root.path().join("second");
        let state = root.path().join("state");
        std::fs::create_dir(&first).expect("first workspace");
        std::fs::create_dir(&second).expect("second workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) = ConfigStore::initialize(state, listen, None).expect("config");
        let config = config
            .registering_provider(AgentComposition::default().provider, Vec::new(), Vec::new())
            .expect("register provider");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credential store"));
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway = GatewayHost::start(store, config, credentials, cron).expect("gateway");
        let first_host = gateway.create_session(&first).await.expect("first chat");
        let second_host = gateway.create_session(&second).await.expect("second chat");
        let first_before = first_host
            .snapshot(None, None)
            .await
            .expect("first snapshot")
            .ready;
        let second_before = second_host
            .snapshot(None, None)
            .await
            .expect("second snapshot")
            .ready;
        let rejection = match first_host
            .snapshot(Some(0), Some(second_before.replay_epoch.clone()))
            .await
        {
            Ok(_) => panic!("a cursor from another host epoch must be rejected"),
            Err(rejection) => rejection,
        };
        assert_eq!(rejection.code, "replay_unavailable");
        let mut composition = first_before.config.config.clone();
        composition.middleware.set_enabled("tools", false);

        first_host
            .configure(first_before.config.revision, composition)
            .await
            .expect("configure first chat");
        let first_after = first_host
            .snapshot(None, None)
            .await
            .expect("first updated")
            .ready;
        let second_after = second_host
            .snapshot(None, None)
            .await
            .expect("second unchanged")
            .ready;

        assert_ne!(first_after.workspace, second_after.workspace);
        assert!(!first_after.config.config.middleware.enabled("tools"));
        assert!(
            first_after
                .contributions
                .iter()
                .any(|contribution| contribution.capability == "sessions"),
            "the /resume picker is gateway-standard, not an optional agent feature"
        );
        assert_eq!(second_after.config, second_before.config);

        let first_id = first_host.session_id().to_owned();
        let second_id = second_host.session_id().to_owned();
        let (first_renamed, second_renamed) = tokio::join!(
            first_host.rename_session(first_id.clone(), "first".into()),
            second_host.rename_session(second_id.clone(), "second".into())
        );
        first_renamed.expect("rename first chat");
        second_renamed.expect("rename second chat");
        let checkpoints = Arc::clone(&gateway.state.lock().await.checkpoints);
        let metadata = load_session_metadata(&checkpoints)
            .await
            .expect("catalog metadata");
        assert_eq!(metadata[&first_id].title.as_deref(), Some("first"));
        assert_eq!(metadata[&second_id].title.as_deref(), Some("second"));
    }

    #[tokio::test]
    async fn model_selection_updates_only_the_chat_and_new_chats_keep_the_gateway_default() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        std::fs::create_dir(&workspace).expect("workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) = ConfigStore::initialize(state, listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credential store"));
        credentials
            .set("openai_socket", "test-secret", None)
            .expect("OpenAI credential");
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway = GatewayHost::start(store, config, credentials, cron).expect("gateway");
        let mut gateway_updates = gateway.subscribe();
        let ready = gateway
            .register_provider(
                ProviderConfig {
                    provider: "openai_socket".into(),
                    model: "gpt-5.6-sol".into(),
                    base_url: None,
                    reasoning_effort: Some("medium".into()),
                    web_search: horus::backend::model::provider::HostedWebSearch::Off,
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .expect("register OpenAI");
        let broadcast = gateway_updates
            .try_recv()
            .expect("gateway-wide catalog update");
        assert!(matches!(
            broadcast.message,
            ServerMessage::Ready { payload } if payload.models == ready.models
        ));
        let alternate = ready
            .models
            .iter()
            .find(|choice| {
                choice.model == "gpt-5.6-terra"
                    && choice.reasoning_effort.as_deref() == Some("high")
            })
            .expect("alternate OpenAI model")
            .route
            .clone();
        let selected = gateway
            .create_session(&workspace)
            .await
            .expect("selected chat");
        let mut selected_config = selected
            .snapshot(None, None)
            .await
            .expect("selected snapshot")
            .ready
            .config
            .config;
        selected_config.provider.web_search =
            horus::backend::model::provider::HostedWebSearch::Live;
        selected
            .configure(1, selected_config)
            .await
            .expect("configure selected chat search");

        selected
            .submit(Submission {
                id: "set-model".into(),
                op: Op::SetModel {
                    route: alternate.clone(),
                },
            })
            .await
            .expect("select alternate model");
        let selected_ready = selected
            .snapshot(None, None)
            .await
            .expect("selected snapshot")
            .ready;
        let fresh = gateway
            .create_session(&workspace)
            .await
            .expect("fresh chat");
        let fresh_ready = fresh
            .snapshot(None, None)
            .await
            .expect("fresh snapshot")
            .ready;

        assert_eq!(selected_ready.session.model.route, alternate);
        assert_eq!(selected_ready.config.config.provider.model, "gpt-5.6-terra");
        assert_eq!(
            selected_ready.config.config.provider.web_search,
            horus::backend::model::provider::HostedWebSearch::Live
        );
        assert_eq!(fresh_ready.config.config.provider.model, "gpt-5.6-sol");
        assert_eq!(
            fresh_ready.config.config.provider.web_search,
            horus::backend::model::provider::HostedWebSearch::Off
        );
        assert_ne!(selected.session_id(), fresh.session_id());
    }

    #[tokio::test]
    async fn capacity_reclaims_an_unreferenced_idle_chat() {
        let root = tempfile::tempdir().expect("root");
        let state_dir = root.path().join("state");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) = ConfigStore::initialize(state_dir, listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credentials"));
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway = GatewayHost::start(store, config, credentials, cron).expect("gateway");
        let mut state = gateway.state.lock().await;
        for index in 0..MAX_ACTIVE_SESSIONS {
            let (commands, mut receiver) = mpsc::channel(1);
            tokio::spawn(async move {
                if let Some(HostCommand::StopIfIdle { reply }) = receiver.recv().await {
                    let _ = reply.send(true);
                }
            });
            let (events, _) = broadcast::channel(1);
            let id = format!("chat-{index}");
            state.sessions.insert(
                id.clone(),
                HostHandle {
                    inner: Arc::new(HostInner {
                        session_id: id.into(),
                        commands,
                        events,
                        accepts_file_attachments: Arc::new(AtomicBool::new(false)),
                    }),
                },
            );
        }

        state.ensure_capacity().await.expect("reclaim capacity");

        assert_eq!(state.sessions.len(), MAX_ACTIVE_SESSIONS - 1);
    }

    #[test]
    fn truncated_replay_pages_from_the_latest_durable_batch() {
        assert_eq!(next_history_cursor(Some(4), 10, false), Some(4));
        assert_eq!(next_history_cursor(Some(4), 10, true), Some(11));
        assert_eq!(next_history_cursor(None, 0, true), None);

        let frame = ServerFrame::new(ServerMessage::Error {
            code: "test".into(),
            message: String::new(),
            fatal: false,
        });
        let mut replay = VecDeque::from(vec![frame.clone(); REPLAY_CAPACITY]);
        let (events, _) = broadcast::channel(1);
        assert!(record_and_publish(&mut replay, &events, frame, true));
        assert_eq!(replay.len(), REPLAY_CAPACITY);
    }

    #[test]
    fn every_restart_resets_replay_and_only_same_session_history_is_suppressed_live() {
        let mut replay = VecDeque::from([ServerFrame::new(ServerMessage::Error {
            code: "old".into(),
            message: "old session".into(),
            fatal: false,
        })]);

        let suppress = reset_replay_for_restart(&mut replay, "session-a", "session-a");

        assert!(replay.is_empty());
        assert!(suppress);
        assert!(!reset_replay_for_restart(
            &mut replay,
            "session-a",
            "session-b"
        ));

        let (events, mut receiver) = broadcast::channel(4);
        let history = ServerFrame::new(ServerMessage::Error {
            code: "history".into(),
            message: "recorded only".into(),
            fatal: false,
        });
        record_and_publish(&mut replay, &events, history.clone(), true);

        assert_eq!(replay.back(), Some(&history));
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn resume_requests_are_broadcast_without_entering_replay() {
        let (events, mut receiver) = broadcast::channel(1);
        let mut replay = VecDeque::new();
        let frame = ServerFrame::new(ServerMessage::AgentEvent {
            session_id: "source".into(),
            sequence: 1,
            event: Event {
                submission_id: Some("resume".into()),
                msg: EventMsg::SessionResumeRequested(
                    horus::protocol::SessionResumeRequestedEvent {
                        session_id: "target".into(),
                        context: Default::default(),
                    },
                ),
            },
            blocks: Vec::new(),
            history: None,
            preview: None,
        });

        record_and_publish(&mut replay, &events, frame.clone(), false);

        assert!(replay.is_empty());
        assert_eq!(receiver.try_recv().expect("live resume request"), frame);
    }

    #[test]
    fn replacement_startup_is_published_only_after_ready() {
        let (events, mut receiver) = broadcast::channel(4);
        let ready = ServerFrame::new(ServerMessage::Error {
            code: "ready".into(),
            message: String::new(),
            fatal: false,
        });
        let startup = ServerFrame::new(ServerMessage::Error {
            code: "startup".into(),
            message: String::new(),
            fatal: false,
        });

        publish_ready_and_pending(&events, ready, vec![startup]);

        assert!(matches!(
            receiver.try_recv().expect("ready frame").message,
            ServerMessage::Error { code, .. } if code == "ready"
        ));
        assert!(matches!(
            receiver.try_recv().expect("startup frame").message,
            ServerMessage::Error { code, .. } if code == "startup"
        ));
    }

    #[test]
    fn session_history_carries_rendered_blocks_and_child_actions() {
        let action = Op::CapabilityCommand {
            capability: "subagents".into(),
            command: "subagents".into(),
            arguments: String::new(),
            input: None,
            target: None,
        };
        let event = EventMsg::SessionHistory(horus::protocol::SessionHistoryEvent {
            events: vec![
                EventMsg::UserMessage(horus::protocol::UserMessageEvent {
                    message: "inspect".into(),
                    attachments: Vec::new(),
                    message_target: None,
                }),
                EventMsg::SessionHistory(horus::protocol::SessionHistoryEvent {
                    events: vec![EventMsg::Frontend(FrontendEvent::Widget {
                        capability: "subagents".into(),
                        item: horus::protocol::FrontendWidget {
                            id: "subagents".into(),
                            slot: horus::protocol::FrontendSlot::Header,
                            text: "subagents".into(),
                            tone: horus::protocol::FrontendTone::Neutral,
                            symbol: Some(horus::protocol::FrontendSymbol::Agent),
                            icon_only: true,
                            progress: None,
                            content: None,
                            action: Some(action.clone()),
                        },
                    })],
                }),
            ],
        });

        let rendered = render_history_with(&event, |_| {
            vec![FrontendBlock {
                id: Some("rendered".into()),
                group: None,
                append: false,
                pending: false,
                text: "rendered child".into(),
                format: FrontendBlockFormat::PlainText,
                tone: horus::protocol::FrontendTone::Neutral,
            }]
        })
        .expect("rendered history");

        assert_eq!(rendered.len(), 2);
        assert_eq!(rendered[0].blocks[0].text, "rendered child");
        assert!(matches!(
            &rendered[1].event,
            EventMsg::Frontend(FrontendEvent::Widget { item, .. })
                if item.action.as_ref() == Some(&action)
        ));
    }

    #[test]
    fn artifact_catalog_uses_block_identity_and_upserts_updates() {
        let mut artifacts = VecDeque::new();
        let mut block = FrontendBlock {
            id: Some("tools/turn-a/call-a".into()),
            group: Some("tools/turn-a".into()),
            append: false,
            pending: false,
            text: "first diff".into(),
            format: FrontendBlockFormat::UnifiedDiff,
            tone: horus::protocol::FrontendTone::Success,
        };
        upsert_artifact(&mut artifacts, "session-a", &block);
        block.text = "updated diff".into();

        upsert_artifact(&mut artifacts, "session-a", &block);

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].id, "tools/turn-a/call-a");
        assert_eq!(artifacts[0].block.text, "updated diff");
    }
}
