//! Per-chat agent ownership, event sequencing, replay, and authenticated operations.

mod catalog;
mod files;
mod git;
mod providers;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use chrono::Utc;
use horus::agent::{AgentConfig, AgentSender};
use horus::backend::checkpoint::{
    ActiveExecution, Checkpoint, CheckpointStore, EventPageRequest, ExecutionOutcome,
    ExecutionRecord, ExecutionStats, JournalEvent, SessionPageRequest, SessionSummary,
    sqlite::SqliteCheckpoint,
};
use horus::backend::model::provider::provider;
use horus::middleware::FrontendExtensions;
use horus::middleware::scratchpad::ScratchpadStore;
use horus::middleware::session_files::SessionFileStore;
use horus::protocol::{
    Event, EventMsg, FrontendBlock, FrontendBlockFormat, FrontendBlockRole, FrontendBlockState,
    FrontendBlockUpdate, FrontendEvent, ModelStepOutcome, Op, RenderedBlock, ReviewDecision,
    SessionFileReference, Submission,
};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use uuid::Uuid;

use crate::assembly::{
    BuiltAgent, assemble, configured_model_choices, configured_model_providers,
    configured_provider_for_route, provider_statuses,
};
use crate::config::{ChatSpec, ConfigStore, CredentialStore, GatewayConfig};
use crate::cron::{ActiveCronRun, BeginRun, CronStore};
use crate::sandbox::GatewaySandbox;
use crate::wire::{
    AgentComposition, ArtifactKind, ArtifactRecord, CronRunStatus, GitDiffScope, MAX_FRAME_BYTES,
    ProfileSnapshot, ProviderConfig, ReadyPayload, RecordedEvent, RenderedEvent, RenderedPreview,
    RunStats, RunSummary, ServerFrame, ServerMessage, SessionActivity, SessionActivityState,
    SessionOutcome, SessionReadyPayload, SessionRecord, SessionRunGroup, SessionWidget,
    VersionedAgentConfig, WorkspaceFileScope,
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
const REPLAY_LOAD_PAGE_SIZE: usize = 8;
const MAX_REPLAY_BYTES: usize = MAX_FRAME_BYTES;
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
    alive: Arc<AtomicBool>,
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
    session_files: SessionFileStore,
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
    pub(crate) records: Vec<RecordedEvent>,
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
    session_files: SessionFileStore,
    accepts_file_attachments: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    catalog_lock: Arc<Mutex<()>>,
    activities: SessionActivities,
    running: RunningAgent,
    pending_turns: usize,
    approval_active: bool,
    turn_error: Option<String>,
    restart_after_turn: bool,
    pending_startup: Vec<ServerFrame>,
    active_cron: Option<ActiveCron>,
    sequence: u64,
    replay: VecDeque<ServerFrame>,
    replay_bytes: usize,
    next_before_sequence: Option<u64>,
    artifacts: VecDeque<ArtifactRecord>,
    widgets: SessionWidgets,
    commands: mpsc::Receiver<HostCommand>,
    events: broadcast::Sender<ServerFrame>,
    gateway_events: broadcast::Sender<ServerFrame>,
    idle_waiters: Vec<oneshot::Sender<()>>,
}

struct LoadedReplay {
    latest_sequence: u64,
    replay: VecDeque<ServerFrame>,
    replay_bytes: usize,
    next_before_sequence: Option<u64>,
    artifacts: VecDeque<ArtifactRecord>,
    widgets: SessionWidgets,
}

struct RunningAgent {
    session_id: String,
    sender: Option<AgentSender>,
    events: mpsc::Receiver<JournalEvent>,
    frontend: FrontendExtensions,
    session: horus::protocol::SessionConfiguredEvent,
    gateway_sandbox: Arc<GatewaySandbox>,
    subagent_template: Option<Arc<OnceLock<AgentConfig>>>,
    tool_count: usize,
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
        reply: oneshot::Sender<std::result::Result<HostSnapshot, Rejection>>,
    },
    HistoryPage {
        before_sequence: Option<u64>,
        max_events: usize,
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
        reply: oneshot::Sender<std::result::Result<Vec<ArtifactRecord>, Rejection>>,
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
    Event(Option<JournalEvent>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JournalSequence {
    AlreadyLoaded,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JournalDelivery {
    Live,
    LoadedStartup,
    ReplacementStartup,
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
        let session_files = SessionFileStore::new(store.state_dir());
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        Ok(Self {
            state: Arc::new(Mutex::new(GatewayState {
                store,
                config: Arc::new(StdMutex::new(config)),
                credentials,
                cron,
                checkpoints,
                scratchpad,
                session_files,
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

    pub(crate) async fn session_file_store(&self) -> SessionFileStore {
        self.state.lock().await.session_files.clone()
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
            state.session_files.clone(),
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
        if let Some(host) = state.sessions.get(session_id)
            && host.is_alive()
        {
            return Ok(host.clone());
        }
        state.sessions.remove(session_id);
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
            state.session_files.clone(),
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

    pub(crate) async fn delete_session(
        &self,
        session_id: &str,
    ) -> std::result::Result<(), Rejection> {
        let mut state = self.state.lock().await;
        let summaries = gateway_session_summaries(&state.checkpoints)
            .await
            .map_err(internal)?;
        let session_ids = session_tree_ids(session_id, &summaries).ok_or_else(unknown_session)?;
        for id in &session_ids {
            state.cron.require_session_idle(id).map_err(internal)?;
        }
        for id in &session_ids {
            let Some(host) = state.sessions.get(id).cloned() else {
                continue;
            };
            if !host.stop_if_idle().await {
                return Err(Rejection {
                    code: "agent_busy",
                    message: "finish or interrupt the active turn before deleting this chat".into(),
                    fatal: false,
                });
            }
            state.sessions.remove(id);
        }
        for id in &session_ids {
            state.cron.delete_session(id).map_err(internal)?;
            state
                .session_files
                .delete_session(id)
                .await
                .map_err(internal)?;
        }
        let catalog_lock = Arc::clone(&state.catalog_lock);
        let _catalog = catalog_lock.lock().await;
        let mut metadata = load_session_metadata(&state.checkpoints)
            .await
            .map_err(internal)?;
        for id in &session_ids {
            metadata.remove(id);
        }
        save_session_metadata(&state.checkpoints, &metadata)
            .await
            .map_err(internal)?;
        if !state
            .checkpoints
            .delete_session(session_id)
            .await
            .map_err(internal)?
        {
            return Err(unknown_session());
        }
        state
            .activities
            .lock()
            .map_err(|_| internal("session activity lock is poisoned"))?
            .retain(|id, _| !session_ids.iter().any(|deleted| deleted == id));
        drop(_catalog);
        drop(state);
        self.broadcast_sessions().await
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
            state.session_files.clone(),
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
        session_files: SessionFileStore,
        catalog_lock: Arc<Mutex<()>>,
        activities: SessionActivities,
        gateway_events: broadcast::Sender<ServerFrame>,
        session_id: String,
        origin_label: &str,
    ) -> Result<Self> {
        let running = start_agent(
            Arc::clone(&gateway),
            &spec,
            &store,
            Arc::clone(&credentials),
            Arc::clone(&cron),
            Arc::clone(&checkpoints),
            scratchpad.clone(),
            session_files.clone(),
            session_id.clone(),
            origin_label,
            false,
        )
        .await?;
        let accepts_file_attachments = Arc::new(AtomicBool::new(runtime_accepts_attachments(
            &running.frontend,
        )));
        let alive = Arc::new(AtomicBool::new(true));
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        let loaded = load_replay(checkpoints.as_ref(), &session_id, &running.frontend).await?;
        activities
            .lock()
            .map_err(|_| Error::Config("session activity lock is poisoned".into()))?
            .entry(session_id.clone())
            .or_default();
        let mut state = HostState {
            store,
            gateway,
            spec,
            credentials,
            cron,
            checkpoints,
            scratchpad,
            session_files,
            accepts_file_attachments: Arc::clone(&accepts_file_attachments),
            alive: Arc::clone(&alive),
            catalog_lock,
            activities,
            running,
            pending_turns: 0,
            approval_active: false,
            turn_error: None,
            restart_after_turn: false,
            pending_startup: Vec::new(),
            active_cron: None,
            sequence: loaded.latest_sequence,
            replay: loaded.replay,
            replay_bytes: loaded.replay_bytes,
            next_before_sequence: loaded.next_before_sequence,
            artifacts: loaded.artifacts,
            widgets: loaded.widgets,
            commands: receiver,
            events: events.clone(),
            gateway_events,
            idle_waiters: Vec::new(),
        };
        state.reconcile_loaded_startup().await?;
        tokio::spawn(state.run());
        Ok(Self {
            inner: Arc::new(HostInner {
                session_id: session_id.into(),
                commands,
                events,
                accepts_file_attachments,
                alive,
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

    fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::Acquire)
    }

    pub(crate) async fn snapshot(
        &self,
        last_sequence: Option<u64>,
    ) -> std::result::Result<HostSnapshot, Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::Snapshot {
            last_sequence,
            reply,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn history_page(
        &self,
        before_sequence: Option<u64>,
        max_events: usize,
    ) -> std::result::Result<SessionHistoryPage, Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::HistoryPage {
            before_sequence,
            max_events,
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
        receive(receiver).await
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
    async fn reconcile_loaded_startup(&mut self) -> Result<()> {
        self.reconcile_startup_through(self.sequence, JournalDelivery::LoadedStartup)
            .await
    }

    async fn reconcile_replacement_startup(&mut self) -> Result<()> {
        let high_water = self
            .checkpoints
            .event_page(
                &self.running.session_id,
                EventPageRequest {
                    before_sequence: None,
                    limit: 1,
                },
            )
            .await?
            .latest_sequence;
        self.reconcile_startup_through(high_water, JournalDelivery::ReplacementStartup)
            .await
    }

    async fn reconcile_startup_through(
        &mut self,
        high_water: u64,
        delivery: JournalDelivery,
    ) -> Result<()> {
        if high_water == 0 {
            return Ok(());
        }
        loop {
            let record = self.running.events.recv().await.ok_or_else(|| {
                Error::Config("agent stopped before the startup high-water was delivered".into())
            })?;
            let sequence = record.sequence;
            if let Some(frame) = self.project_and_publish(record, delivery)?
                && delivery == JournalDelivery::ReplacementStartup
            {
                self.pending_startup.push(frame);
            }
            if sequence >= high_water {
                break;
            }
        }
        loop {
            match self.running.events.try_recv() {
                Ok(record) => {
                    if let Some(frame) = self.project_and_publish(record, delivery)?
                        && delivery == JournalDelivery::ReplacementStartup
                    {
                        self.pending_startup.push(frame);
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    return Err(Error::Config(
                        "agent stopped while startup events were reconciled".into(),
                    ));
                }
            }
        }
        Ok(())
    }

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
                        let message = error.to_string();
                        self.broadcast(ServerMessage::Error {
                            code: "host_error".into(),
                            message: message.clone(),
                            fatal: true,
                        });
                        if let Err(activity_error) = self.fail_activity(&message).await {
                            self.broadcast(ServerMessage::Error {
                                code: "session_activity".into(),
                                message: activity_error.to_string(),
                                fatal: false,
                            });
                        }
                        break;
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
        self.alive.store(false, Ordering::Release);
    }

    async fn handle(&mut self, command: HostCommand) -> bool {
        match command {
            HostCommand::Snapshot {
                last_sequence,
                reply,
            } => {
                let _ = reply.send(self.snapshot_value(last_sequence).await);
            }
            HostCommand::HistoryPage {
                before_sequence,
                max_events,
                reply,
            } => {
                let _ = reply.send(self.history_page_value(before_sequence, max_events).await);
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
                let _ = reply.send(self.list_artifacts().await);
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
    ) -> std::result::Result<HostSnapshot, Rejection> {
        let replay = self.replay_after(last_sequence)?;
        Ok(HostSnapshot {
            ready: self.ready().await.map_err(internal)?,
            replay,
        })
    }

    async fn history_page_value(
        &self,
        before_sequence: Option<u64>,
        max_events: usize,
    ) -> std::result::Result<SessionHistoryPage, Rejection> {
        let page = self
            .checkpoints
            .event_page(
                &self.running.session_id,
                EventPageRequest {
                    before_sequence,
                    limit: max_events,
                },
            )
            .await
            .map_err(internal)?;
        let next_before_sequence = page.next_before_sequence;
        let records = page
            .into_chronological()
            .into_iter()
            .map(|event| project_record(&self.running.frontend, event))
            .collect();
        Ok(SessionHistoryPage {
            records,
            next_before_sequence,
        })
    }

    fn replay_after(
        &self,
        last_sequence: Option<u64>,
    ) -> std::result::Result<Vec<ServerFrame>, Rejection> {
        let Some(last_sequence) = last_sequence else {
            return Ok(self.replay.iter().cloned().collect());
        };
        if last_sequence > self.sequence {
            return Err(Rejection {
                code: "replay_unavailable",
                message: "the reconnect cursor is ahead of the durable session".into(),
                fatal: false,
            });
        }
        let oldest = self.replay.front().and_then(event_sequence);
        if last_sequence < self.sequence
            && oldest.is_none_or(|oldest| last_sequence.saturating_add(1) < oldest)
        {
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
            .as_ref()
            .ok_or_else(stopped)?
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
        if !self.spec.agent.config.middleware.enabled("cron") {
            return Err(Rejection {
                code: "capability_disabled",
                message: "scheduling is disabled for this chat".into(),
                fatal: false,
            });
        }
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
        self.stop_and_drain_running().await.map_err(internal)?;
        let replacement = start_agent(
            Arc::clone(&self.gateway),
            &next,
            &self.store,
            Arc::clone(&self.credentials),
            Arc::clone(&self.cron),
            Arc::clone(&self.checkpoints),
            self.scratchpad.clone(),
            self.session_files.clone(),
            session_id,
            "horus-gateway",
            true,
        )
        .await
        .map_err(internal)?;
        let previous = std::mem::replace(&mut self.running, replacement);
        self.accepts_file_attachments.store(
            runtime_accepts_attachments(&self.running.frontend),
            Ordering::Relaxed,
        );
        self.spec = next;
        drop(previous);
        self.reconcile_replacement_startup()
            .await
            .map_err(internal)?;
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
        let session_id = self.running.session_id.clone();
        self.stop_and_drain_running().await.map_err(internal)?;
        let replacement = start_agent(
            Arc::clone(&self.gateway),
            &self.spec,
            &self.store,
            Arc::clone(&self.credentials),
            Arc::clone(&self.cron),
            Arc::clone(&self.checkpoints),
            self.scratchpad.clone(),
            self.session_files.clone(),
            session_id,
            origin_label,
            false,
        )
        .await
        .map_err(internal)?;
        let previous = std::mem::replace(&mut self.running, replacement);
        self.accepts_file_attachments.store(
            runtime_accepts_attachments(&self.running.frontend),
            Ordering::Relaxed,
        );
        self.widgets.clear();
        drop(previous);
        self.reconcile_replacement_startup()
            .await
            .map_err(internal)?;
        self.pending_turns = 0;
        self.approval_active = false;
        self.turn_error = None;
        Ok(())
    }

    async fn stop_and_drain_running(&mut self) -> Result<()> {
        drop(self.running.sender.take());
        while let Some(record) = self.running.events.recv().await {
            self.apply_event(record).await?;
        }
        self.running.subagent_template.take();
        Ok(())
    }

    async fn forward_event(&mut self, record: JournalEvent) -> Result<()> {
        if self.apply_event(record).await? {
            self.restart_after_turn = false;
            self.restart("horus-gateway")
                .await
                .map_err(|rejection| Error::Config(rejection.message))?;
            self.broadcast_changed()
                .await
                .map_err(|rejection| Error::Config(rejection.message))?;
        }
        Ok(())
    }

    async fn apply_event(&mut self, record: JournalEvent) -> Result<bool> {
        let was_active = self.pending_turns > 0;
        let event = record.event.clone();
        if self
            .project_and_publish(record, JournalDelivery::Live)?
            .is_none()
        {
            return Ok(false);
        }
        let next_activity = self.activity_for_event(&event.msg)?;
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
        if let Some(activity) = next_activity {
            self.set_activity(activity)?;
            self.broadcast_sessions()
                .await
                .map_err(|rejection| Error::Config(rejection.message))?;
        }

        let became_idle = self.pending_turns == 0 && was_active;
        let mut restart = false;
        if became_idle {
            if let Some((active, status, message)) = cron_completion {
                self.cron.finish_run(active.run, status, message)?;
            } else {
                restart = self.restart_after_turn;
            }
            if !self.approval_active && self.active_cron.is_none() {
                for waiter in self.idle_waiters.drain(..) {
                    let _ = waiter.send(());
                }
            }
        }
        Ok(restart)
    }

    fn project_and_publish(
        &mut self,
        journal: JournalEvent,
        delivery: JournalDelivery,
    ) -> Result<Option<ServerFrame>> {
        validate_gateway_event(&journal.event.msg)?;
        let sequence_kind = classify_journal_sequence(self.sequence, journal.sequence, delivery)?;
        let sequence = journal.sequence;
        let frame = ServerFrame::new(ServerMessage::AgentEvent {
            session_id: self.running.session_id.clone(),
            record: project_record(&self.running.frontend, journal),
        });
        validate_event_frame(&frame)?;
        if let ServerMessage::AgentEvent { record, .. } = &frame.message {
            update_widgets(&mut self.widgets, &record.event.msg);
            self.record_artifacts(&record.blocks);
            if let EventMsg::ModelStepCompleted(step) = &record.event.msg
                && matches!(&step.outcome, ModelStepOutcome::Completed { .. })
            {
                compact_replay_deltas(
                    &mut self.replay,
                    &mut self.replay_bytes,
                    &step.model_step_id,
                )?;
            }
        }
        if sequence_kind == JournalSequence::AlreadyLoaded {
            return Ok(None);
        }
        let truncated = record_and_publish(
            &mut self.replay,
            &mut self.replay_bytes,
            &self.events,
            frame.clone(),
            delivery != JournalDelivery::Live,
        )?;
        if truncated {
            self.next_before_sequence = self.replay.front().and_then(event_sequence);
        }
        self.sequence = sequence;
        Ok(Some(frame))
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
                self.running
                    .sender
                    .as_ref()
                    .ok_or_else(|| {
                        Error::Horus(horus::Error::Stopped(
                            "agent command channel is closed".into(),
                        ))
                    })?
                    .send(Submission {
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

    fn record_artifacts(&mut self, blocks: &[RenderedBlock]) {
        for block in blocks {
            upsert_artifact(&mut self.artifacts, &self.running.session_id, block);
        }
    }

    async fn list_artifacts(&self) -> std::result::Result<Vec<ArtifactRecord>, Rejection> {
        let stored_files = self
            .session_files
            .list_artifacts(&self.running.session_id)
            .await
            .map_err(internal)?;
        Ok(merge_stored_file_artifacts(
            &self.artifacts,
            &self.running.session_id,
            stored_files,
        ))
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
            latest_sequence: self.sequence,
            next_before_sequence: self.next_before_sequence,
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

fn session_tree_ids(root_session_id: &str, sessions: &[SessionSummary]) -> Option<Vec<String>> {
    sessions
        .iter()
        .any(|session| session.session_id == root_session_id)
        .then_some(())?;
    let mut seen = HashSet::from([root_session_id.to_owned()]);
    let mut ordered = vec![root_session_id.to_owned()];
    loop {
        let mut changed = false;
        for session in sessions {
            if seen.contains(&session.session_id)
                || !session
                    .parent_session_id
                    .as_ref()
                    .is_some_and(|parent| seen.contains(parent))
            {
                continue;
            }
            seen.insert(session.session_id.clone());
            ordered.push(session.session_id.clone());
            changed = true;
        }
        if !changed {
            ordered.reverse();
            return Some(ordered);
        }
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
    gateway: Arc<StdMutex<GatewayConfig>>,
    spec: &ChatSpec,
    store: &ConfigStore,
    credentials: Arc<CredentialStore>,
    cron: Arc<CronStore>,
    checkpoints: Arc<dyn CheckpointStore>,
    scratchpad: ScratchpadStore,
    session_files: SessionFileStore,
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
        session_files,
        Some(session_id),
        origin_label,
        override_saved_model_route,
    )
    .await?;
    let session = agent.session().clone();
    let frontend = agent.frontend().clone();
    let tool_count = agent.tool_count();
    let session_id = session.session_id.clone();
    let (sender, events) = agent.into_recorded_parts();
    Ok(RunningAgent {
        session_id,
        sender: Some(sender),
        events,
        frontend,
        session,
        gateway_sandbox,
        subagent_template,
        tool_count,
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

async fn load_replay(
    checkpoints: &dyn CheckpointStore,
    session_id: &str,
    frontend: &FrontendExtensions,
) -> Result<LoadedReplay> {
    let mut latest_sequence = 0;
    let mut before_sequence = None;
    let mut scanned = 0;
    let mut newest_first = VecDeque::with_capacity(REPLAY_CAPACITY);
    let mut replay_bytes = 0_usize;
    let mut has_earlier = false;
    'pages: loop {
        let remaining = REPLAY_CAPACITY.saturating_sub(scanned);
        if remaining == 0 {
            has_earlier = true;
            break;
        }
        let page = checkpoints
            .event_page(
                session_id,
                EventPageRequest {
                    before_sequence,
                    limit: remaining.min(REPLAY_LOAD_PAGE_SIZE),
                },
            )
            .await?;
        if scanned == 0 {
            latest_sequence = page.latest_sequence;
        }
        let next_before_sequence = page.next_before_sequence;
        for journal in page.events {
            scanned += 1;
            latest_sequence = latest_sequence.max(journal.sequence);
            let frame = ServerFrame::new(ServerMessage::AgentEvent {
                session_id: session_id.into(),
                record: project_record(frontend, journal),
            });
            if !replayable(&frame) {
                continue;
            }
            let frame_bytes = validate_event_frame(&frame)?;
            if replay_bytes.saturating_add(frame_bytes) > MAX_REPLAY_BYTES {
                has_earlier = true;
                break 'pages;
            }
            replay_bytes = replay_bytes.saturating_add(frame_bytes);
            newest_first.push_back(frame);
        }
        let Some(cursor) = next_before_sequence else {
            break;
        };
        before_sequence = Some(cursor);
    }
    let replay = newest_first.into_iter().rev().collect::<VecDeque<_>>();
    let next_before_sequence = if has_earlier {
        replay
            .front()
            .and_then(event_sequence)
            .or_else(|| latest_sequence.checked_add(1))
    } else {
        None
    };
    let mut artifacts = VecDeque::with_capacity(ARTIFACT_CAPACITY);
    let mut widgets = BTreeMap::new();
    for frame in &replay {
        let ServerMessage::AgentEvent { record, .. } = &frame.message else {
            continue;
        };
        update_widgets(&mut widgets, &record.event.msg);
        for block in &record.blocks {
            upsert_artifact(&mut artifacts, session_id, block);
        }
    }
    Ok(LoadedReplay {
        latest_sequence,
        replay,
        replay_bytes,
        next_before_sequence,
        artifacts,
        widgets,
    })
}

fn render_preview(frontend: &FrontendExtensions, event: &EventMsg) -> Option<RenderedPreview> {
    let EventMsg::Frontend(FrontendEvent::Preview {
        id,
        title,
        subtitle,
        page_id,
        update,
        events,
        next,
    }) = event
    else {
        return None;
    };
    Some(RenderedPreview {
        id: id.clone(),
        title: title.clone(),
        subtitle: subtitle.clone(),
        page_id: page_id.clone(),
        update: *update,
        events: flatten_preview(events)
            .into_iter()
            .map(|event| RenderedEvent {
                blocks: frontend.render(&event),
                event,
            })
            .collect(),
        next: next.clone(),
    })
}

fn project_record(frontend: &FrontendExtensions, mut journal: JournalEvent) -> RecordedEvent {
    let (blocks, preview) = project_event(frontend, &journal.event.msg);
    if preview.is_some() {
        clear_projected_preview_events(&mut journal.event.msg);
    }
    RecordedEvent {
        sequence: journal.sequence,
        recorded_at_ms: journal.recorded_at_ms,
        event: journal.event,
        stream_metrics: journal.stream_metrics,
        blocks,
        preview,
    }
}

fn clear_projected_preview_events(event: &mut EventMsg) {
    if let EventMsg::Frontend(FrontendEvent::Preview { events, .. }) = event {
        events.clear();
    }
}

fn project_event(
    frontend: &FrontendExtensions,
    event: &EventMsg,
) -> (Vec<RenderedBlock>, Option<RenderedPreview>) {
    (frontend.render(event), render_preview(frontend, event))
}

fn classify_journal_sequence(
    current: u64,
    incoming: u64,
    delivery: JournalDelivery,
) -> Result<JournalSequence> {
    if incoming <= current && delivery == JournalDelivery::LoadedStartup {
        return Ok(JournalSequence::AlreadyLoaded);
    }
    let expected = current
        .checked_add(1)
        .ok_or_else(|| Error::Config("event sequence overflow".into()))?;
    if incoming != expected {
        return Err(Error::Horus(horus::Error::Checkpoint(format!(
            "event journal delivery sequence is {incoming}, expected {expected}"
        ))));
    }
    Ok(JournalSequence::Next)
}

fn validate_gateway_event(event: &EventMsg) -> Result<()> {
    if matches!(event, EventMsg::SessionHistory(_)) {
        return Err(Error::Protocol(
            "gateway agents must emit canonical events instead of nested session history".into(),
        ));
    }
    Ok(())
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
        ServerMessage::AgentEvent { ref record, .. } => Some(record.sequence),
        _ => None,
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
    rendered: &RenderedBlock,
) {
    let block = &rendered.block;
    let (kind, title) = if let Some(file) = block.files.first() {
        (ArtifactKind::File, file.name.clone())
    } else if block.format == FrontendBlockFormat::UnifiedDiff {
        (ArtifactKind::CodeDiff, block.title.clone())
    } else {
        return;
    };
    let source_id = block
        .id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let id = scoped_block_id(&rendered.capability, &source_id);
    if let Some(index) = artifacts.iter().position(|artifact| artifact.id == id) {
        artifacts.remove(index);
    } else if artifacts.len() == ARTIFACT_CAPACITY {
        artifacts.pop_front();
    }
    artifacts.push_back(ArtifactRecord {
        id,
        session_id: session_id.into(),
        kind,
        title,
        block: block.clone(),
    });
}

fn scoped_block_id(capability: &str, source_id: &str) -> String {
    format!("block:{}:{capability}{source_id}", capability.len())
}

fn merge_stored_file_artifacts(
    live: &VecDeque<ArtifactRecord>,
    session_id: &str,
    stored_files: Vec<SessionFileReference>,
) -> Vec<ArtifactRecord> {
    let stored_ids = stored_files
        .iter()
        .map(|file| file.id.as_str())
        .collect::<HashSet<_>>();
    let mut seen_files = HashSet::new();
    let mut artifacts = Vec::with_capacity(live.len().saturating_add(stored_files.len()));
    for artifact in live {
        if artifact.kind == ArtifactKind::CodeDiff {
            artifacts.push(artifact.clone());
            continue;
        }
        let Some(file) = artifact.block.files.first() else {
            continue;
        };
        if stored_ids.contains(file.id.as_str()) && seen_files.insert(file.id.clone()) {
            artifacts.push(artifact.clone());
        }
    }
    for file in stored_files {
        if seen_files.insert(file.id.clone()) {
            artifacts.push(stored_file_artifact(session_id, file));
        }
    }
    artifacts
}

fn stored_file_artifact(session_id: &str, file: SessionFileReference) -> ArtifactRecord {
    let id = format!("artifacts/file/{}", file.id);
    let title = file.name.clone();
    ArtifactRecord {
        id: id.clone(),
        session_id: session_id.into(),
        kind: ArtifactKind::File,
        title: title.clone(),
        block: FrontendBlock {
            id: Some(id),
            group: None,
            update: FrontendBlockUpdate::Replace,
            state: FrontendBlockState::Complete,
            role: FrontendBlockRole::Artifact,
            title: format!("Sent {title}"),
            text: String::new(),
            symbol: None,
            files: vec![file],
            format: FrontendBlockFormat::PlainText,
            tone: horus::protocol::FrontendTone::Success,
        },
    }
}

fn record_and_publish(
    replay: &mut VecDeque<ServerFrame>,
    replay_bytes: &mut usize,
    events: &broadcast::Sender<ServerFrame>,
    frame: ServerFrame,
    suppress_broadcast: bool,
) -> Result<bool> {
    let frame_bytes = validate_event_frame(&frame)?;
    let mut truncated = false;
    if replayable(&frame) {
        while replay.len() >= REPLAY_CAPACITY
            || replay_bytes.saturating_add(frame_bytes) > MAX_REPLAY_BYTES
        {
            let Some(discarded) = replay.pop_front() else {
                break;
            };
            *replay_bytes = replay_bytes.saturating_sub(serde_json::to_vec(&discarded)?.len());
            truncated = true;
        }
        *replay_bytes = replay_bytes.saturating_add(frame_bytes);
        replay.push_back(frame.clone());
    }
    if !suppress_broadcast {
        let _ = events.send(frame);
    }
    Ok(truncated)
}

fn compact_replay_deltas(
    replay: &mut VecDeque<ServerFrame>,
    replay_bytes: &mut usize,
    model_step_id: &str,
) -> Result<()> {
    replay.retain(|frame| {
        !matches!(
            &frame.message,
            ServerMessage::AgentEvent {
                record: RecordedEvent {
                    event: Event {
                        msg: EventMsg::AgentMessageContentDelta(delta),
                        ..
                    },
                    ..
                },
                ..
            } if delta.model_step_id == model_step_id
        ) && !matches!(
            &frame.message,
            ServerMessage::AgentEvent {
                record: RecordedEvent {
                    event: Event {
                        msg: EventMsg::AgentReasoningContentDelta(delta),
                        ..
                    },
                    ..
                },
                ..
            } if delta.model_step_id == model_step_id
        )
    });
    *replay_bytes = replay.iter().try_fold(0_usize, |total, frame| {
        Ok::<_, Error>(total.saturating_add(serde_json::to_vec(frame)?.len()))
    })?;
    Ok(())
}

fn replayable(frame: &ServerFrame) -> bool {
    !matches!(
        &frame.message,
        ServerMessage::AgentEvent {
            record: RecordedEvent {
                event: Event {
                    msg: EventMsg::SessionResumeRequested(_)
                        | EventMsg::Frontend(
                            FrontendEvent::Preview { .. }
                                | FrontendEvent::Picker { .. }
                                | FrontendEvent::Widget { .. }
                                | FrontendEvent::RemoveWidget { .. }
                        ),
                    ..
                },
                ..
            },
            ..
        }
    )
}

fn validate_event_frame(frame: &ServerFrame) -> Result<usize> {
    let frame_bytes = serde_json::to_vec(frame)?.len();
    if frame_bytes > MAX_FRAME_BYTES {
        return Err(Error::Protocol(format!(
            "agent event exceeds the {MAX_FRAME_BYTES}-byte gateway frame limit"
        )));
    }
    Ok(frame_bytes)
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
    use horus::backend::checkpoint::Checkpoint;
    use horus::protocol::{SessionContext, TokenUsage};

    use super::*;

    #[test]
    fn gateway_rejects_nested_session_history() {
        assert!(
            validate_gateway_event(&EventMsg::SessionHistory(
                horus::protocol::SessionHistoryEvent { events: Vec::new() }
            ))
            .is_err()
        );
        assert!(validate_gateway_event(&EventMsg::ContextCompacted).is_ok());
    }

    #[test]
    fn projected_preview_drops_the_raw_nested_event_duplicate() {
        let mut event = EventMsg::Frontend(FrontendEvent::Preview {
            id: "/root/reviewer".into(),
            title: "reviewer".into(),
            subtitle: "Full context".into(),
            page_id: "/root/reviewer:latest".into(),
            update: horus::protocol::FrontendPreviewUpdate::Replace,
            events: vec![EventMsg::ContextCompacted],
            next: None,
        });

        clear_projected_preview_events(&mut event);

        assert!(matches!(
            event,
            EventMsg::Frontend(FrontendEvent::Preview {
                id,
                events,
                ..
            }) if id == "/root/reviewer" && events.is_empty()
        ));
    }

    #[test]
    fn journal_delivery_accepts_loaded_records_and_rejects_gaps() {
        assert_eq!(
            classify_journal_sequence(5, 3, JournalDelivery::LoadedStartup).expect("loaded record"),
            JournalSequence::AlreadyLoaded
        );
        assert_eq!(
            classify_journal_sequence(5, 5, JournalDelivery::LoadedStartup)
                .expect("loaded high-water"),
            JournalSequence::AlreadyLoaded
        );
        assert_eq!(
            classify_journal_sequence(5, 6, JournalDelivery::Live).expect("next record"),
            JournalSequence::Next
        );
        assert!(
            classify_journal_sequence(5, 7, JournalDelivery::Live)
                .expect_err("sequence gap")
                .to_string()
                .contains("expected 6")
        );
        assert!(
            classify_journal_sequence(5, 5, JournalDelivery::Live)
                .expect_err("stale live record")
                .to_string()
                .contains("expected 6")
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
    async fn durable_event_journal_restores_replay_and_history_cursor() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) =
            ConfigStore::initialize(root.path().join("state"), listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credentials"));
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway =
            GatewayHost::start(store, config, credentials, Arc::clone(&cron)).expect("gateway");
        let host = gateway
            .create_session(&workspace)
            .await
            .expect("create session");
        let checkpoints = Arc::clone(&gateway.state.lock().await.checkpoints);
        let session_id = host.session_id().to_owned();
        assert!(host.stop_if_idle().await);
        gateway.state.lock().await.sessions.remove(&session_id);
        drop(host);
        let mut latest_sequence = 0;
        for index in 0..=REPLAY_CAPACITY {
            let event = Event {
                submission_id: None,
                msg: EventMsg::Warning(horus::protocol::WarningEvent {
                    message: format!("event {index}"),
                }),
            };
            latest_sequence = checkpoints
                .append_event(
                    &session_id,
                    i64::try_from(index).expect("timestamp"),
                    &event,
                )
                .await
                .expect("append journal event")
                .sequence;
        }
        let durable_highwater = checkpoints
            .append_event(
                &session_id,
                i64::try_from(REPLAY_CAPACITY + 1).expect("timestamp"),
                &Event {
                    submission_id: None,
                    msg: EventMsg::Frontend(FrontendEvent::Preview {
                        id: "transient".into(),
                        title: "Transient".into(),
                        subtitle: String::new(),
                        page_id: "transient:latest".into(),
                        update: horus::protocol::FrontendPreviewUpdate::Replace,
                        events: Vec::new(),
                        next: None,
                    }),
                },
            )
            .await
            .expect("advance journal high-water")
            .sequence;

        let reopened = gateway
            .open_session(&session_id)
            .await
            .expect("reopen session");
        let snapshot = reopened.snapshot(None).await.expect("session snapshot");
        let newest = reopened
            .history_page(latest_sequence.checked_add(1), 1)
            .await
            .expect("newest seeded event");

        assert!(snapshot.ready.latest_sequence >= durable_highwater);
        assert_eq!(snapshot.replay.len(), REPLAY_CAPACITY);
        assert!(snapshot.ready.next_before_sequence.is_some());
        assert!(matches!(
            &newest.records[..],
            [RecordedEvent {
                event: Event { msg: EventMsg::Warning(warning), .. },
                ..
            }] if warning.message == format!("event {REPLAY_CAPACITY}")
        ));
        assert!(
            reopened
                .snapshot(Some(0))
                .await
                .is_err_and(|rejection| rejection.code == "replay_unavailable")
        );
    }

    #[tokio::test]
    async fn initial_snapshot_restores_transient_widgets_without_replaying_them() {
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

        let snapshot = host.snapshot(None).await.expect("session snapshot");

        assert!(!snapshot.ready.widgets.is_empty());
        assert!(snapshot.replay.iter().all(|frame| {
            !matches!(
                &frame.message,
                ServerMessage::AgentEvent {
                    record: RecordedEvent {
                        event: Event {
                            msg: EventMsg::Frontend(
                                FrontendEvent::Widget { .. } | FrontendEvent::RemoveWidget { .. }
                            ),
                            ..
                        },
                        ..
                    },
                    ..
                }
            )
        }));
    }

    #[tokio::test]
    async fn replacement_ready_precedes_every_reconciled_startup_event() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) =
            ConfigStore::initialize(root.path().join("state"), listen, None).expect("config");
        let config = config
            .registering_provider(AgentComposition::default().provider, Vec::new(), Vec::new())
            .expect("register provider");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credentials"));
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway = GatewayHost::start(store, config, credentials, cron).expect("gateway");
        let host = gateway
            .create_session(&workspace)
            .await
            .expect("create session");
        let before = host.snapshot(None).await.expect("initial snapshot").ready;
        let mut composition = before.config.config.clone();
        composition.middleware.set_enabled("cron", false);
        let mut updates = host.subscribe();

        host.configure(before.config.revision, composition)
            .await
            .expect("replace agent");

        let changed = updates.try_recv().expect("session changed");
        let ServerMessage::SessionChanged { payload } = changed.message else {
            panic!("replacement must publish ready before startup events");
        };
        let startup = std::iter::from_fn(|| updates.try_recv().ok()).collect::<Vec<_>>();
        assert!(!payload.widgets.is_empty());
        assert!(startup.iter().any(|frame| {
            matches!(
                &frame.message,
                ServerMessage::AgentEvent {
                    record: RecordedEvent {
                        event: Event {
                            msg: EventMsg::Frontend(FrontendEvent::Widget { .. }),
                            ..
                        },
                        ..
                    },
                    ..
                }
            )
        }));
        assert!(startup.iter().all(|frame| {
            event_sequence(frame).is_none_or(|sequence| {
                sequence > before.latest_sequence && sequence <= payload.latest_sequence
            })
        }));

        host.submit(Submission {
            id: "post-replacement".into(),
            op: Op::Interrupt {
                turn_id: "not-active".into(),
            },
        })
        .await
        .expect("submit after replacement");
        let sequences = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            let mut sequences = Vec::new();
            loop {
                let frame = updates.recv().await.expect("post-replacement event");
                let ServerMessage::AgentEvent { record, .. } = frame.message else {
                    continue;
                };
                sequences.push(record.sequence);
                if record.event.submission_id.as_deref() == Some("post-replacement") {
                    return sequences;
                }
            }
        })
        .await
        .expect("post-replacement delivery");
        assert_eq!(
            sequences.first().copied(),
            payload.latest_sequence.checked_add(1)
        );
        assert!(
            sequences
                .windows(2)
                .all(|pair| pair[1] == pair[0].saturating_add(1))
        );
    }

    #[tokio::test]
    async fn delete_session_stops_the_host_and_removes_its_durable_tree() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) =
            ConfigStore::initialize(root.path().join("state"), listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credentials"));
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway =
            GatewayHost::start(store, config, credentials, Arc::clone(&cron)).expect("gateway");
        let deleted = gateway
            .create_session(&workspace)
            .await
            .expect("create deleted session");
        let retained = gateway
            .create_session(&workspace)
            .await
            .expect("create retained session");
        let deleted_id = deleted.session_id().to_owned();
        let retained_id = retained.session_id().to_owned();
        let (checkpoints, session_files) = {
            let state = gateway.state.lock().await;
            (Arc::clone(&state.checkpoints), state.session_files.clone())
        };
        let parent = checkpoints
            .load(&deleted_id)
            .await
            .expect("load parent")
            .expect("parent checkpoint");
        checkpoints
            .fork(
                &deleted_id,
                parent.sequence,
                &Checkpoint::empty("deleted-child"),
            )
            .await
            .expect("fork child");
        for session_id in [&deleted_id, "deleted-child"] {
            session_files
                .publish_artifact(
                    session_id,
                    "result.txt".into(),
                    "text/plain".into(),
                    b"result",
                )
                .await
                .expect("publish artifact");
        }
        deleted
            .rename_session(deleted_id.clone(), "Deleted".into())
            .await
            .expect("title deleted session");
        retained
            .rename_session(retained_id.clone(), "Retained".into())
            .await
            .expect("title retained session");
        let task = cron
            .add_for_test(&deleted_id, "scheduled task", "0 9 * * *")
            .expect("schedule task");
        let run = match cron.begin_run(&task.id).expect("begin run") {
            BeginRun::Started(run) => run,
            BeginRun::Skipped => panic!("new run must start"),
        };
        cron.finish_run(run, CronRunStatus::Succeeded, None)
            .expect("finish run");

        gateway
            .delete_session(&deleted_id)
            .await
            .expect("delete session");

        assert!(
            checkpoints
                .load(&deleted_id)
                .await
                .expect("load deleted")
                .is_none()
        );
        assert!(
            checkpoints
                .load("deleted-child")
                .await
                .expect("load deleted child")
                .is_none()
        );
        assert!(
            session_files
                .list_artifacts(&deleted_id)
                .await
                .expect("deleted artifacts")
                .is_empty()
        );
        let metadata = load_session_metadata(&checkpoints)
            .await
            .expect("catalog metadata");
        assert!(!metadata.contains_key(&deleted_id));
        assert_eq!(metadata[&retained_id].title.as_deref(), Some("Retained"));
        assert!(
            cron.list(&deleted_id)
                .expect("deleted schedules")
                .is_empty()
        );
        assert!(
            cron.history(&deleted_id, None)
                .expect("deleted schedule history")
                .is_empty()
        );
        assert!(!task.task.exists());
        assert!(deleted.snapshot(None).await.is_err());
        assert_eq!(
            gateway
                .sessions()
                .await
                .expect("remaining sessions")
                .into_iter()
                .map(|session| session.summary.session_id)
                .collect::<Vec<_>>(),
            [retained_id]
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
        let gateway =
            GatewayHost::start(store, config, credentials, Arc::clone(&cron)).expect("gateway");
        let first_host = gateway.create_session(&first).await.expect("first chat");
        let second_host = gateway.create_session(&second).await.expect("second chat");
        let scheduled = cron
            .add_for_test(first_host.session_id(), "keep scheduled work", "0 9 * * *")
            .expect("scheduled task");
        let first_before = first_host
            .snapshot(None)
            .await
            .expect("first snapshot")
            .ready;
        let second_before = second_host
            .snapshot(None)
            .await
            .expect("second snapshot")
            .ready;
        let mut composition = first_before.config.config.clone();
        composition.middleware.set_enabled("cron", false);

        first_host
            .configure(first_before.config.revision, composition)
            .await
            .expect("configure first chat");
        let first_after = first_host
            .snapshot(None)
            .await
            .expect("first updated")
            .ready;
        let second_after = second_host
            .snapshot(None)
            .await
            .expect("second unchanged")
            .ready;

        assert_ne!(first_after.workspace, second_after.workspace);
        assert!(!first_after.config.config.middleware.enabled("cron"));
        assert_eq!(first_after.tool_count + 1, first_before.tool_count);
        assert_eq!(
            first_host
                .start_cron_setup(None)
                .await
                .expect_err("disabled scheduler must reject setup")
                .code,
            "capability_disabled"
        );
        assert_eq!(
            cron.list(first_host.session_id())
                .expect("existing schedules")
                .first()
                .map(|task| task.id.as_str()),
            Some(scheduled.id.as_str())
        );
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
            .snapshot(None)
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
            .snapshot(None)
            .await
            .expect("selected snapshot")
            .ready;
        let fresh = gateway
            .create_session(&workspace)
            .await
            .expect("fresh chat");
        let fresh_ready = fresh.snapshot(None).await.expect("fresh snapshot").ready;

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
    async fn opening_a_stopped_cached_chat_creates_a_fresh_actor() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let state_dir = root.path().join("state");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) = ConfigStore::initialize(state_dir, listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credentials"));
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway = GatewayHost::start(store, config, credentials, cron).expect("gateway");
        let original = gateway
            .create_session(&workspace)
            .await
            .expect("create chat");
        let session_id = original.session_id().to_string();

        assert!(original.stop_if_idle().await);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while original.is_alive() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actor stopped");
        let reopened = gateway
            .open_session(&session_id)
            .await
            .expect("reopen chat");

        assert!(reopened.is_alive());
        assert!(!Arc::ptr_eq(&original.inner, &reopened.inner));
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
                        alive: Arc::new(AtomicBool::new(true)),
                    }),
                },
            );
        }

        state.ensure_capacity().await.expect("reclaim capacity");

        assert_eq!(state.sessions.len(), MAX_ACTIVE_SESSIONS - 1);
    }

    #[test]
    fn replay_is_bounded_by_event_count() {
        let frame = ServerFrame::new(ServerMessage::Error {
            code: "test".into(),
            message: String::new(),
            fatal: false,
        });
        let mut replay = VecDeque::from(vec![frame.clone(); REPLAY_CAPACITY]);
        let mut replay_bytes =
            serde_json::to_vec(&frame).expect("encode frame").len() * replay.len();
        let (events, _) = broadcast::channel(1);
        assert!(
            record_and_publish(&mut replay, &mut replay_bytes, &events, frame, true)
                .expect("record event")
        );
        assert_eq!(replay.len(), REPLAY_CAPACITY);
    }

    #[test]
    fn replay_is_bounded_by_encoded_bytes() {
        let (events, _) = broadcast::channel(1);
        let mut replay = VecDeque::new();
        let mut replay_bytes = 0;
        let large_message = "x".repeat(MAX_REPLAY_BYTES / 2);
        let first = ServerFrame::new(ServerMessage::Error {
            code: "first".into(),
            message: large_message.clone(),
            fatal: false,
        });
        let second = ServerFrame::new(ServerMessage::Error {
            code: "second".into(),
            message: large_message,
            fatal: false,
        });

        assert!(
            !record_and_publish(&mut replay, &mut replay_bytes, &events, first, true)
                .expect("record first frame")
        );
        assert!(
            record_and_publish(&mut replay, &mut replay_bytes, &events, second, true)
                .expect("record second frame")
        );

        assert_eq!(replay.len(), 1);
        assert!(replay_bytes <= MAX_REPLAY_BYTES);
    }

    #[test]
    fn suppressed_frames_enter_replay_without_broadcasting() {
        let mut replay = VecDeque::new();
        let mut replay_bytes = 0;
        let (events, mut receiver) = broadcast::channel(4);
        let history = ServerFrame::new(ServerMessage::Error {
            code: "history".into(),
            message: "recorded only".into(),
            fatal: false,
        });
        record_and_publish(
            &mut replay,
            &mut replay_bytes,
            &events,
            history.clone(),
            true,
        )
        .expect("record history");

        assert_eq!(replay.back(), Some(&history));
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn transient_controls_are_broadcast_without_entering_replay() {
        let (events, mut receiver) = broadcast::channel(5);
        let mut replay = VecDeque::new();
        let mut replay_bytes = 0;
        let messages = [
            EventMsg::SessionResumeRequested(horus::protocol::SessionResumeRequestedEvent {
                session_id: "target".into(),
                context: Default::default(),
            }),
            EventMsg::Frontend(FrontendEvent::Preview {
                id: "preview".into(),
                title: "Preview".into(),
                subtitle: String::new(),
                page_id: "preview:latest".into(),
                update: horus::protocol::FrontendPreviewUpdate::Replace,
                events: Vec::new(),
                next: None,
            }),
            EventMsg::Frontend(FrontendEvent::Picker {
                title: "Choose".into(),
                options: Vec::new(),
            }),
            EventMsg::Frontend(FrontendEvent::Widget {
                capability: "test".into(),
                item: horus::protocol::FrontendWidget {
                    id: "status".into(),
                    slot: horus::protocol::FrontendSlot::Header,
                    text: "Current".into(),
                    tone: horus::protocol::FrontendTone::Neutral,
                    symbol: None,
                    icon_only: false,
                    progress: None,
                    content: None,
                    action: None,
                },
            }),
            EventMsg::Frontend(FrontendEvent::RemoveWidget {
                capability: "test".into(),
                id: "status".into(),
            }),
        ];
        for (index, msg) in messages.into_iter().enumerate() {
            let frame = ServerFrame::new(ServerMessage::AgentEvent {
                session_id: "source".into(),
                record: RecordedEvent {
                    sequence: u64::try_from(index + 1).expect("sequence"),
                    recorded_at_ms: 1,
                    event: Event {
                        submission_id: Some("transient".into()),
                        msg,
                    },
                    stream_metrics: Vec::new(),
                    blocks: Vec::new(),
                    preview: None,
                },
            });
            record_and_publish(
                &mut replay,
                &mut replay_bytes,
                &events,
                frame.clone(),
                false,
            )
            .expect("broadcast transient control");
            assert_eq!(receiver.try_recv().expect("live transient control"), frame);
        }

        assert!(replay.is_empty());
        assert_eq!(replay_bytes, 0);
    }

    #[test]
    fn completed_step_compacts_only_its_progressive_replay_frames() {
        let frame = |sequence, msg| {
            ServerFrame::new(ServerMessage::AgentEvent {
                session_id: "session".into(),
                record: RecordedEvent {
                    sequence,
                    recorded_at_ms: 1,
                    event: Event {
                        submission_id: Some("submission".into()),
                        msg,
                    },
                    stream_metrics: Vec::new(),
                    blocks: Vec::new(),
                    preview: None,
                },
            })
        };
        let mut replay = VecDeque::from([
            frame(
                1,
                EventMsg::AgentMessageContentDelta(
                    horus::protocol::AgentMessageContentDeltaEvent {
                        session_id: "session".into(),
                        turn_id: "turn".into(),
                        model_step_id: "completed".into(),
                        delta: "answer".into(),
                        phase: horus::protocol::AgentMessagePhase::FinalAnswer,
                    },
                ),
            ),
            frame(
                2,
                EventMsg::AgentReasoningContentDelta(
                    horus::protocol::AgentReasoningContentDeltaEvent {
                        session_id: "session".into(),
                        turn_id: "turn".into(),
                        model_step_id: "completed".into(),
                        delta: "reasoning".into(),
                    },
                ),
            ),
            frame(
                3,
                EventMsg::AgentMessageContentDelta(
                    horus::protocol::AgentMessageContentDeltaEvent {
                        session_id: "session".into(),
                        turn_id: "turn".into(),
                        model_step_id: "active".into(),
                        delta: "partial".into(),
                        phase: horus::protocol::AgentMessagePhase::FinalAnswer,
                    },
                ),
            ),
        ]);
        let mut replay_bytes = replay
            .iter()
            .map(|frame| serde_json::to_vec(frame).expect("encode frame").len())
            .sum();

        compact_replay_deltas(&mut replay, &mut replay_bytes, "completed")
            .expect("compact completed step");

        assert_eq!(replay.len(), 1);
        assert_eq!(replay.front().and_then(event_sequence), Some(3));
        assert_eq!(
            replay_bytes,
            serde_json::to_vec(replay.front().expect("remaining frame"))
                .expect("encode remaining frame")
                .len()
        );
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
    fn artifact_catalog_uses_block_identity_and_upserts_updates() {
        let mut artifacts = VecDeque::new();
        let mut block = FrontendBlock {
            id: Some("tools/turn-a/call-a".into()),
            group: Some("tools/turn-a".into()),
            update: FrontendBlockUpdate::Replace,
            state: FrontendBlockState::Complete,
            role: FrontendBlockRole::Artifact,
            title: "Code diff".into(),
            text: "first diff".into(),
            symbol: None,
            format: FrontendBlockFormat::UnifiedDiff,
            tone: horus::protocol::FrontendTone::Success,
            files: Vec::new(),
        };
        upsert_artifact(
            &mut artifacts,
            "session-a",
            &RenderedBlock {
                capability: "tools".into(),
                block: block.clone(),
            },
        );
        block.text = "updated diff".into();

        upsert_artifact(
            &mut artifacts,
            "session-a",
            &RenderedBlock {
                capability: "tools".into(),
                block,
            },
        );

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].id, "block:5:toolstools/turn-a/call-a");
        assert_eq!(artifacts[0].block.text, "updated diff");
    }

    #[test]
    fn artifact_catalog_scopes_equal_block_ids_by_capability() {
        let mut artifacts = VecDeque::new();
        for capability in ["tools", "review"] {
            upsert_artifact(
                &mut artifacts,
                "session-a",
                &RenderedBlock {
                    capability: capability.into(),
                    block: FrontendBlock {
                        id: Some("result".into()),
                        group: None,
                        update: FrontendBlockUpdate::Replace,
                        state: FrontendBlockState::Complete,
                        role: FrontendBlockRole::Artifact,
                        title: capability.into(),
                        text: "diff".into(),
                        symbol: None,
                        format: FrontendBlockFormat::UnifiedDiff,
                        tone: horus::protocol::FrontendTone::Success,
                        files: Vec::new(),
                    },
                },
            );
        }

        assert_eq!(
            artifacts
                .iter()
                .map(|artifact| artifact.id.as_str())
                .collect::<Vec<_>>(),
            ["block:5:toolsresult", "block:6:reviewresult"]
        );
    }

    #[test]
    fn artifact_catalog_uses_session_file_metadata() {
        let mut artifacts = VecDeque::new();
        let block = FrontendBlock {
            id: Some("artifacts/turn-a/call-a".into()),
            group: Some("artifacts/turn-a".into()),
            update: FrontendBlockUpdate::Replace,
            state: FrontendBlockState::Complete,
            role: FrontendBlockRole::Artifact,
            title: "Sent report.xlsx".into(),
            text: String::new(),
            symbol: None,
            format: FrontendBlockFormat::PlainText,
            tone: horus::protocol::FrontendTone::Success,
            files: vec![horus::protocol::SessionFileReference {
                id: "file-a".into(),
                name: "report.xlsx".into(),
                size: 42,
                media_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                    .into(),
            }],
        };

        upsert_artifact(
            &mut artifacts,
            "session-a",
            &RenderedBlock {
                capability: "artifacts".into(),
                block,
            },
        );

        assert_eq!(
            artifacts
                .front()
                .map(|artifact| (artifact.kind, artifact.title.as_str())),
            Some((ArtifactKind::File, "report.xlsx"))
        );
    }

    #[test]
    fn stored_files_restore_the_artifact_catalog_without_live_replay() {
        let file = horus::protocol::SessionFileReference {
            id: "file-a".into(),
            name: "report.xlsx".into(),
            size: 42,
            media_type: "application/octet-stream".into(),
        };

        let artifacts =
            merge_stored_file_artifacts(&VecDeque::new(), "session-a", vec![file.clone()]);

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].session_id, "session-a");
        assert_eq!(artifacts[0].kind, ArtifactKind::File);
        assert_eq!(artifacts[0].title, "report.xlsx");
        assert_eq!(artifacts[0].block.files, [file]);
    }
}
