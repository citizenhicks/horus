//! Per-chat agent ownership, event sequencing, replay, and authenticated operations.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Component, Path};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use horus::agent::{AgentConfig, AgentSender};
use horus::backend::checkpoint::{
    Checkpoint, CheckpointStore, SessionPageRequest, sqlite::SqliteCheckpoint,
};
use horus::backend::model::provider::{ProviderAuth, provider};
use horus::backend::sandbox::CommandOutput;
use horus::middleware::FrontendExtensions;
use horus::protocol::{
    Event, EventMsg, FrontendBlock, FrontendBlockFormat, FrontendEvent, Op, ReviewDecision,
    Submission,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use uuid::Uuid;

use crate::assembly::{
    BuiltAgent, assemble, configured_model_choices, configured_provider_for_route,
    credential_is_configured, provider_statuses,
};
use crate::config::{ChatSpec, ConfigStore, CredentialStore, GatewayConfig};
use crate::cron::{ActiveCronRun, BeginRun, CronStore};
use crate::sandbox::{GatewaySandbox, MAX_COMMAND_OUTPUT_BYTES};
use crate::wire::{
    AgentComposition, ArtifactKind, ArtifactRecord, CronRunStatus, GitStatus, ProfileSnapshot,
    ProviderConfig, ReadyPayload, RenderedEvent, RenderedPreview, ServerFrame, ServerMessage,
    SessionReadyPayload, SessionRecord, VersionedAgentConfig,
};
use crate::{Error, Result};

const COMMAND_CAPACITY: usize = 128;
const BROADCAST_CAPACITY: usize = 512;
const REPLAY_CAPACITY: usize = 1024;
const ARTIFACT_CAPACITY: usize = 256;
const SESSION_PAGE_SIZE: usize = 100;
const SESSION_CATALOG_SCOPE: &str = "gateway";
const SESSION_CATALOG_KEY: &str = "session_catalog";
const MAX_SESSION_TITLE_BYTES: usize = 256;
const MAX_SESSION_PREVIEW_BYTES: usize = 512;
const MAX_GIT_DIFF_BYTES: usize = MAX_COMMAND_OUTPUT_BYTES;
const GIT_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const MAX_ACTIVE_SESSIONS: usize = 32;

#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionMetadata {
    title: Option<String>,
    pinned: bool,
    hidden: bool,
}

type SessionCatalogMetadata = BTreeMap<String, SessionMetadata>;

#[derive(Clone)]
pub(crate) struct HostHandle {
    inner: Arc<HostInner>,
}

struct HostInner {
    session_id: Arc<str>,
    commands: mpsc::Sender<HostCommand>,
    events: broadcast::Sender<ServerFrame>,
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
    // ponytail: one lock is enough for at most 32 tiny catalog writes.
    catalog_lock: Arc<Mutex<()>>,
    provider_login: Arc<StdMutex<Option<String>>>,
    sessions: HashMap<String, HostHandle>,
}

pub(crate) struct HostSnapshot {
    pub(crate) ready: SessionReadyPayload,
    pub(crate) replay: Vec<ServerFrame>,
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
    catalog_lock: Arc<Mutex<()>>,
    running: RunningAgent,
    pending_turns: usize,
    approval_active: bool,
    restart_after_turn: bool,
    suppress_history_broadcast: bool,
    pending_startup: Vec<ServerFrame>,
    active_cron: Option<ActiveCron>,
    sequence: u64,
    replay: VecDeque<ServerFrame>,
    artifacts: VecDeque<ArtifactRecord>,
    commands: mpsc::Receiver<HostCommand>,
    events: broadcast::Sender<ServerFrame>,
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
    SetModel {
        route: String,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    GitDiff {
        reply: oneshot::Sender<std::result::Result<String, Rejection>>,
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
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        Ok(Self {
            state: Arc::new(Mutex::new(GatewayState {
                store,
                config: Arc::new(StdMutex::new(config)),
                credentials,
                cron,
                checkpoints,
                catalog_lock: Arc::new(Mutex::new(())),
                provider_login: Arc::new(StdMutex::new(None)),
                sessions: HashMap::new(),
            })),
            events,
        })
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<ServerFrame> {
        self.events.subscribe()
    }

    pub(crate) async fn ready(&self) -> std::result::Result<ReadyPayload, Rejection> {
        let state = self.state.lock().await;
        gateway_ready(&state).await
    }

    pub(crate) async fn set_credential(
        &self,
        provider_id: String,
        api_key: String,
        base_url: Option<String>,
    ) -> std::result::Result<(), Rejection> {
        let base_url = {
            let state = self.state.lock().await;
            let definition = provider(&provider_id).map_err(invalid_config)?;
            let base_url = if definition.configurable_base_url() {
                base_url.or_else(|| definition.default_base_url().map(str::to_owned))
            } else {
                base_url
            };
            definition
                .validate_base_url(base_url.as_deref())
                .map_err(invalid_config)?;
            state
                .credentials
                .set(&provider_id, &api_key, base_url.as_deref())
                .map_err(invalid_config)?;
            base_url
        };
        self.refresh_provider_sessions(&provider_id, base_url.as_deref())
            .await
    }

    pub(crate) async fn start_provider_login(
        &self,
        request_id: String,
        provider_id: String,
    ) -> std::result::Result<(), Rejection> {
        let definition = provider(&provider_id).map_err(invalid_config)?;
        let ProviderAuth::Browser(auth) = definition.auth() else {
            return Err(Rejection {
                code: "invalid_provider_auth",
                message: "the selected provider uses an API key".into(),
                fatal: false,
            });
        };
        if !auth.supports_device_login() {
            return Err(Rejection {
                code: "device_login_unavailable",
                message: "the selected provider does not support device-code login".into(),
                fatal: false,
            });
        }
        let (login_guard, path) = {
            let state = self.state.lock().await;
            (
                Arc::clone(&state.provider_login),
                state.store.provider_auth_path(),
            )
        };
        let login_id = Uuid::new_v4().to_string();
        reserve_provider_login(&login_guard, &login_id)?;
        let login = match auth.start_device().await {
            Ok(login) => login,
            Err(error) => {
                release_provider_login(&login_guard, &login_id)?;
                return Err(internal(error));
            }
        };
        self.broadcast(ServerMessage::ProviderLoginStarted {
            request_id: request_id.clone(),
            login_id: login_id.clone(),
            provider: provider_id.clone(),
            verification_url: login.verification_url().into(),
            user_code: login.user_code().into(),
        });
        let gateway = self.clone();
        tokio::spawn(async move {
            let result = login
                .complete(path)
                .await
                .map_err(|error| error.to_string());
            gateway
                .finish_provider_login(request_id, login_id, provider_id, result)
                .await;
        });
        Ok(())
    }

    async fn finish_provider_login(
        &self,
        request_id: String,
        login_id: String,
        provider: String,
        result: std::result::Result<(), String>,
    ) {
        let login_guard = Arc::clone(&self.state.lock().await.provider_login);
        match release_provider_login(&login_guard, &login_id) {
            Ok(true) => {}
            Ok(false) => return,
            Err(rejection) => {
                self.broadcast(ServerMessage::Error {
                    code: rejection.code.into(),
                    message: rejection.message,
                    fatal: rejection.fatal,
                });
                return;
            }
        }
        if let Err(message) = result {
            self.broadcast(ServerMessage::Rejected {
                request_id,
                code: "provider_login_failed".into(),
                message,
                fatal: false,
            });
            return;
        }
        let refresh = self.refresh_provider_sessions(&provider, None).await;
        self.broadcast(ServerMessage::ProviderLoginFinished {
            request_id,
            login_id,
            provider,
        });
        if let Err(rejection) = refresh {
            self.broadcast(ServerMessage::Error {
                code: rejection.code.into(),
                message: rejection.message,
                fatal: rejection.fatal,
            });
        }
    }

    async fn refresh_provider_sessions(
        &self,
        provider: &str,
        base_url: Option<&str>,
    ) -> std::result::Result<(), Rejection> {
        let sessions = self
            .state
            .lock()
            .await
            .sessions
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut failure = None;
        for host in sessions {
            if let Err(rejection) = host
                .refresh_provider(provider.into(), base_url.map(str::to_owned))
                .await
            {
                failure.get_or_insert(rejection);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn broadcast(&self, message: ServerMessage) {
        let _ = self.events.send(ServerFrame::new(message));
    }

    pub(crate) async fn register_provider(
        &self,
        selection: ProviderConfig,
    ) -> std::result::Result<ReadyPayload, Rejection> {
        let state = self.state.lock().await;
        if !credential_is_configured(&selection, &state.store, &state.credentials)
            .map_err(invalid_config)?
        {
            return Err(invalid_config(Error::Config(format!(
                "provider `{}` is not configured on this gateway",
                selection.provider
            ))));
        }
        {
            let mut current = state
                .config
                .lock()
                .map_err(|_| internal("gateway configuration lock is poisoned"))?;
            let next = current
                .registering_provider(selection)
                .map_err(invalid_config)?;
            state.store.save(&next).map_err(internal)?;
            *current = next;
        }
        let payload = gateway_ready(&state).await?;
        let frame = ServerFrame::new(ServerMessage::Ready {
            payload: payload.clone(),
        });
        let _ = self.events.send(frame);
        Ok(payload)
    }

    pub(crate) async fn sessions(&self) -> std::result::Result<Vec<SessionRecord>, Rejection> {
        let state = self.state.lock().await;
        session_catalog(&state.checkpoints).await.map_err(internal)
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
            Arc::clone(&state.catalog_lock),
            session_id.clone(),
            "horus-gateway",
        )
        .await
        .map_err(internal)?;
        state.sessions.insert(session_id, host.clone());
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
            Arc::clone(&state.catalog_lock),
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
            Arc::clone(&state.catalog_lock),
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
        let state = self.state.lock().await;
        let profile = state
            .config
            .lock()
            .map_err(|_| internal("gateway configuration lock is poisoned"))?
            .profile();
        Ok(profile)
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
        catalog_lock: Arc<Mutex<()>>,
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
            session_id.clone(),
            origin_label,
            false,
        )
        .await?;
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        let state = HostState {
            store,
            gateway,
            spec,
            credentials,
            cron,
            checkpoints,
            catalog_lock,
            running,
            pending_turns: 0,
            approval_active: false,
            restart_after_turn: false,
            suppress_history_broadcast: false,
            pending_startup: Vec::new(),
            active_cron: None,
            sequence: 0,
            replay: VecDeque::with_capacity(REPLAY_CAPACITY),
            artifacts: VecDeque::with_capacity(ARTIFACT_CAPACITY),
            commands: receiver,
            events: events.clone(),
            idle_waiters: Vec::new(),
        };
        tokio::spawn(state.run());
        Ok(Self {
            inner: Arc::new(HostInner {
                session_id: session_id.into(),
                commands,
                events,
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

    pub(crate) async fn set_model(&self, route: String) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::SetModel { route, reply }).await?;
        receive(receiver).await
    }

    pub(crate) async fn git_diff(&self) -> std::result::Result<String, Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::GitDiff { reply }).await?;
        receiver.await.map_err(|_| stopped())?
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
                reply,
            } => {
                let _ = reply.send(self.snapshot_value(last_sequence).await);
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
                let result = self.submit(submission, false);
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
            HostCommand::SetModel { route, reply } => {
                let result = self.set_model(&route).await;
                let _ = reply.send(result);
            }
            HostCommand::GitDiff { reply } => {
                let _ = reply.send(
                    workspace_git_diff(&self.running.gateway_sandbox, &self.spec.workspace).await,
                );
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
    ) -> std::result::Result<HostSnapshot, Rejection> {
        let replay = self.replay_after(last_sequence)?;
        Ok(HostSnapshot {
            ready: self.ready().await.map_err(internal)?,
            replay,
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
            op: Op::UserInput { text: input },
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
            op: Op::UserInput { text: input },
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
        let next = self
            .spec
            .replacing_agent(
                expected_revision,
                composition,
                self.store.state_dir(),
                self.gateway
                    .lock()
                    .map_err(|_| internal("gateway configuration lock is poisoned"))?
                    .tls
                    .as_ref(),
            )
            .map_err(invalid_config)?;
        let session_id = self.running.session_id.clone();
        let gateway = self
            .gateway
            .lock()
            .map_err(|_| internal("gateway configuration lock is poisoned"))?
            .clone();
        let replacement = start_agent(
            &gateway,
            &next,
            &self.store,
            Arc::clone(&self.credentials),
            Arc::clone(&self.cron),
            Arc::clone(&self.checkpoints),
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
        let previous = std::mem::replace(&mut self.running, replacement);
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
        let provider =
            configured_provider_for_route(&gateway, &self.store, &self.credentials, route)
                .map_err(invalid_config)?;
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
        let previous = std::mem::replace(&mut self.running, replacement);
        self.suppress_history_broadcast = suppress_history_broadcast;
        shutdown_agent(previous).await;
        if suppress_history_broadcast {
            self.record_replacement_startup().map_err(internal)?;
        }
        self.pending_turns = 0;
        self.approval_active = false;
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
                    let frame = self.record_event(event, false, true)?;
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
        self.record_event(event, was_active, suppress_broadcast)?;

        if self.pending_turns == 0 && was_active {
            self.broadcast_sessions()
                .await
                .map_err(|rejection| Error::Config(rejection.message))?;
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

    fn record_event(
        &mut self,
        event: Event,
        was_active: bool,
        suppress_broadcast: bool,
    ) -> Result<ServerFrame> {
        if let EventMsg::TokenCount(count) = &event.msg
            && let Some(info) = &count.info
        {
            let mut gateway = self
                .gateway
                .lock()
                .map_err(|_| Error::Config("gateway configuration lock is poisoned".into()))?;
            if gateway.observe_usage(
                &self.running.session_id,
                &info.total_token_usage,
                was_active,
            )? {
                self.store.save(&gateway)?;
            }
        }
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
        record_and_publish(
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
        Ok(SessionReadyPayload {
            latest_sequence: self.sequence,
            workspace: self.spec.workspace_info(),
            git: git_status(&self.running.gateway_sandbox).await,
            session: self.running.session.clone(),
            contributions: self.running.frontend.contributions().to_vec(),
            config: self.spec.agent.clone(),
        })
    }

    async fn broadcast_changed(&mut self) -> std::result::Result<(), Rejection> {
        let payload = self.ready().await.map_err(internal)?;
        let ready = ServerFrame::new(ServerMessage::SessionChanged { payload });
        let pending = std::mem::take(&mut self.pending_startup);
        publish_ready_and_pending(&self.events, ready, pending);
        Ok(())
    }

    async fn broadcast_sessions(&self) -> std::result::Result<(), Rejection> {
        let sessions = session_catalog(&self.checkpoints).await.map_err(internal)?;
        self.broadcast(ServerMessage::Sessions {
            request_id: None,
            sessions,
        });
        Ok(())
    }

    fn broadcast(&self, message: ServerMessage) {
        let _ = self.events.send(ServerFrame::new(message));
    }

    fn require_idle(&self) -> std::result::Result<(), Rejection> {
        if self.pending_turns > 0 || self.approval_active {
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

async fn gateway_ready(state: &GatewayState) -> std::result::Result<ReadyPayload, Rejection> {
    let config = state
        .config
        .lock()
        .map_err(|_| internal("gateway configuration lock is poisoned"))?
        .clone();
    Ok(ReadyPayload {
        sessions: session_catalog(&state.checkpoints)
            .await
            .map_err(internal)?,
        providers: provider_statuses(&state.store, &state.credentials).map_err(internal)?,
        models: configured_model_choices(&config, &state.store, &state.credentials)
            .map_err(internal)?,
        default_config: config.default_agent,
        middleware_features: crate::middleware_manifest::features(),
        max_active_sessions: MAX_ACTIVE_SESSIONS,
    })
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
        Some(session_id),
        origin_label,
        override_saved_model_route,
    )
    .await?;
    let session = agent.session().clone();
    let frontend = agent.frontend().clone();
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
    })
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
    checkpoints.save(&checkpoint, &[]).await?;
    Ok(())
}

async fn session_catalog(checkpoints: &Arc<dyn CheckpointStore>) -> Result<Vec<SessionRecord>> {
    let mut cursor = None;
    let mut sessions = Vec::new();
    while sessions.len() < SESSION_PAGE_SIZE {
        let page = checkpoints
            .list_sessions_page(SessionPageRequest {
                cursor,
                limit: SESSION_PAGE_SIZE,
            })
            .await?;
        sessions.extend(
            page.sessions
                .into_iter()
                .filter(|session| session.catalog_visible),
        );
        let Some(next) = page.next_cursor else {
            break;
        };
        cursor = Some(next);
    }
    sessions.truncate(SESSION_PAGE_SIZE);
    for session in &mut sessions {
        if let Some(message) = &mut session.first_user_message
            && message.len() > MAX_SESSION_PREVIEW_BYTES
        {
            let mut end = MAX_SESSION_PREVIEW_BYTES;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
    }
    let metadata = load_session_metadata(checkpoints).await?;
    let mut sessions = sessions
        .into_iter()
        .filter_map(|summary| {
            let metadata = metadata.get(&summary.session_id);
            (!metadata.is_some_and(|metadata| metadata.hidden)).then(|| SessionRecord {
                title: metadata.and_then(|metadata| metadata.title.clone()),
                pinned: metadata.is_some_and(|metadata| metadata.pinned),
                summary,
            })
        })
        .collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        right
            .pinned
            .cmp(&left.pinned)
            .then_with(|| right.summary.updated_at.cmp(&left.summary.updated_at))
            .then_with(|| right.summary.sequence.cmp(&left.summary.sequence))
            .then_with(|| left.summary.session_id.cmp(&right.summary.session_id))
    });
    Ok(sessions)
}

async fn load_session_metadata(
    checkpoints: &Arc<dyn CheckpointStore>,
) -> Result<SessionCatalogMetadata> {
    let Some(value) = checkpoints
        .load_state(SESSION_CATALOG_SCOPE, SESSION_CATALOG_KEY)
        .await?
    else {
        return Ok(SessionCatalogMetadata::default());
    };
    Ok(serde_json::from_value(value)?)
}

async fn save_session_metadata(
    checkpoints: &Arc<dyn CheckpointStore>,
    metadata: &SessionCatalogMetadata,
) -> Result<()> {
    checkpoints
        .save_state(
            SESSION_CATALOG_SCOPE,
            SESSION_CATALOG_KEY,
            &serde_json::to_value(metadata)?,
        )
        .await?;
    Ok(())
}

fn validate_session_title(title: &str) -> std::result::Result<&str, Rejection> {
    let title = title.trim();
    if title.is_empty() || title.len() > MAX_SESSION_TITLE_BYTES {
        return Err(Rejection {
            code: "invalid_session_title",
            message: format!("chat title must be 1–{MAX_SESSION_TITLE_BYTES} UTF-8 bytes"),
            fatal: false,
        });
    }
    Ok(title)
}

async fn git_status(sandbox: &GatewaySandbox) -> Option<GitStatus> {
    let current = tokio::time::timeout(
        GIT_TIMEOUT,
        sandbox.execute_git(&["branch", "--show-current"]),
    )
    .await
    .ok()?
    .ok()?;
    if current.exit_code != 0
        || current.stdout.len() > MAX_GIT_DIFF_BYTES
        || current.stderr.len() > MAX_GIT_DIFF_BYTES
    {
        return None;
    }
    Some(GitStatus {
        current_branch: current.stdout.trim().into(),
    })
}

async fn workspace_git_diff(
    sandbox: &GatewaySandbox,
    workspace: &Path,
) -> std::result::Result<String, Rejection> {
    tokio::time::timeout(GIT_TIMEOUT, workspace_git_diff_inner(sandbox, workspace))
        .await
        .map_err(|_| git_timeout())?
}

async fn workspace_git_diff_inner(
    sandbox: &GatewaySandbox,
    workspace: &Path,
) -> std::result::Result<String, Rejection> {
    let repository = git_output(sandbox, &["rev-parse", "--is-inside-work-tree"]).await?;
    if repository.exit_code != 0 {
        if repository.stderr.contains("not a git repository") {
            return Ok(String::new());
        }
        return Err(git_failure(
            "checking the Git workspace failed",
            &repository.stderr,
        ));
    }
    if repository.stdout != "true\n" {
        return Ok(String::new());
    }

    let head = git_output(sandbox, &["rev-parse", "--verify", "--quiet", "HEAD"]).await?;
    let mut diff = if head.exit_code == 0 {
        successful_git_output(
            git_output(
                sandbox,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-color",
                    "--no-textconv",
                    "HEAD",
                    "--",
                ],
            )
            .await?,
            "git diff failed",
        )?
    } else {
        let staged = successful_git_output(
            git_output(
                sandbox,
                &[
                    "diff",
                    "--cached",
                    "--no-ext-diff",
                    "--no-color",
                    "--no-textconv",
                    "--",
                ],
            )
            .await?,
            "staged git diff failed",
        )?;
        let unstaged = successful_git_output(
            git_output(
                sandbox,
                &["diff", "--no-ext-diff", "--no-color", "--no-textconv", "--"],
            )
            .await?,
            "unstaged git diff failed",
        )?;
        let mut diff = Vec::new();
        append_diff(&mut diff, &staged)?;
        append_diff(&mut diff, &unstaged)?;
        diff
    };
    if diff.len() > MAX_GIT_DIFF_BYTES {
        return Err(git_diff_too_large());
    }

    let untracked = successful_git_output(
        git_output(
            sandbox,
            &["ls-files", "--others", "--exclude-standard", "-z", "--"],
        )
        .await?,
        "listing untracked files failed",
    )?;
    if untracked.len() > MAX_GIT_DIFF_BYTES {
        return Err(git_diff_too_large());
    }
    for path in untracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).map_err(|_| git_invalid_path())?;
        let relative = Path::new(path);
        if !safe_git_path(relative) {
            return Err(git_invalid_path());
        }
        let metadata = match tokio::fs::symlink_metadata(workspace.join(relative)).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(git_error(error)),
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        let patch = untracked_git_diff(sandbox, path).await?;
        if !is_binary_diff(&patch) {
            append_diff(&mut diff, &patch)?;
        }
    }

    Ok(String::from_utf8_lossy(&diff).into_owned())
}

async fn git_output(
    sandbox: &GatewaySandbox,
    args: &[&str],
) -> std::result::Result<CommandOutput, Rejection> {
    let output = sandbox.execute_git(args).await.map_err(git_error)?;
    if output.stdout.len() > MAX_GIT_DIFF_BYTES || output.stderr.len() > MAX_GIT_DIFF_BYTES {
        return Err(git_diff_too_large());
    }
    Ok(output)
}

async fn untracked_git_diff(
    sandbox: &GatewaySandbox,
    path: &str,
) -> std::result::Result<Vec<u8>, Rejection> {
    let output = git_output(
        sandbox,
        &[
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--no-textconv",
            "--no-index",
            "--",
            "/dev/null",
            path,
        ],
    )
    .await?;
    if matches!(output.exit_code, 0 | 1) {
        Ok(output.stdout.into_bytes())
    } else {
        Err(git_failure("untracked git diff failed", &output.stderr))
    }
}

fn successful_git_output(
    output: CommandOutput,
    failure: &str,
) -> std::result::Result<Vec<u8>, Rejection> {
    if output.exit_code == 0 {
        Ok(output.stdout.into_bytes())
    } else {
        Err(git_failure(failure, &output.stderr))
    }
}

fn append_diff(target: &mut Vec<u8>, patch: &[u8]) -> std::result::Result<(), Rejection> {
    if patch.is_empty() {
        return Ok(());
    }
    let separator = usize::from(!target.is_empty() && !target.ends_with(b"\n"));
    if target
        .len()
        .saturating_add(separator)
        .saturating_add(patch.len())
        > MAX_GIT_DIFF_BYTES
    {
        return Err(git_diff_too_large());
    }
    if separator == 1 {
        target.push(b'\n');
    }
    target.extend_from_slice(patch);
    Ok(())
}

fn safe_git_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn is_binary_diff(diff: &[u8]) -> bool {
    diff.split(|byte| *byte == b'\n')
        .any(|line| line.starts_with(b"Binary files "))
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
) {
    if replay.len() == REPLAY_CAPACITY {
        replay.pop_front();
    }
    replay.push_back(frame.clone());
    if !suppress_broadcast {
        let _ = events.send(frame);
    }
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

fn ensure_provider_login_available(
    active_login: Option<&str>,
) -> std::result::Result<(), Rejection> {
    if active_login.is_some() {
        return Err(Rejection {
            code: "provider_login_in_progress",
            message: "finish the active provider login before starting another".into(),
            fatal: false,
        });
    }
    Ok(())
}

fn reserve_provider_login(
    active_login: &StdMutex<Option<String>>,
    login_id: &str,
) -> std::result::Result<(), Rejection> {
    let mut active_login = active_login
        .lock()
        .map_err(|_| internal("provider login lock is poisoned"))?;
    ensure_provider_login_available(active_login.as_deref())?;
    *active_login = Some(login_id.into());
    Ok(())
}

fn release_provider_login(
    active_login: &StdMutex<Option<String>>,
    login_id: &str,
) -> std::result::Result<bool, Rejection> {
    let mut active_login = active_login
        .lock()
        .map_err(|_| internal("provider login lock is poisoned"))?;
    if active_login.as_deref() != Some(login_id) {
        return Ok(false);
    }
    *active_login = None;
    Ok(true)
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

fn git_error(error: impl std::fmt::Display) -> Rejection {
    Rejection {
        code: "git_error",
        message: error.to_string(),
        fatal: false,
    }
}

fn git_failure(prefix: &str, stderr: &str) -> Rejection {
    let detail = stderr.trim();
    Rejection {
        code: "git_error",
        message: if detail.is_empty() {
            prefix.into()
        } else {
            format!("{prefix}: {detail}")
        },
        fatal: false,
    }
}

fn git_timeout() -> Rejection {
    Rejection {
        code: "git_timeout",
        message: "Git inspection exceeded 5 seconds".into(),
        fatal: false,
    }
}

fn git_diff_too_large() -> Rejection {
    Rejection {
        code: "git_diff_too_large",
        message: format!("workspace Git diff exceeds {MAX_GIT_DIFF_BYTES} bytes"),
        fatal: false,
    }
}

fn git_invalid_path() -> Rejection {
    Rejection {
        code: "git_error",
        message: "Git returned an invalid untracked path".into(),
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

    use super::*;

    fn run_git(workspace: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .env("LC_ALL", "C")
            .current_dir(workspace)
            .output()
            .expect("run Git");
        assert!(
            output.status.success(),
            "Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn test_git_sandbox(workspace: &Path) -> (tempfile::TempDir, GatewaySandbox) {
        let state = tempfile::tempdir().expect("state");
        let sandbox =
            GatewaySandbox::new(workspace, state.path(), None, GIT_TIMEOUT).expect("Git sandbox");
        (state, sandbox)
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
    async fn credential_endpoints_are_validated_and_persisted() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        std::fs::create_dir(&workspace).expect("workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) = ConfigStore::initialize(state, listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credential store"));
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway =
            GatewayHost::start(store, config, Arc::clone(&credentials), cron).expect("gateway");
        gateway.create_session(&workspace).await.expect("chat");
        let custom_endpoint = "https://example.com/v1";

        gateway
            .set_credential(
                "responses".into(),
                "custom-secret".into(),
                Some(custom_endpoint.into()),
            )
            .await
            .expect("store custom credential");
        let error = gateway
            .set_credential(
                "kimi".into(),
                "fixed-secret".into(),
                Some(custom_endpoint.into()),
            )
            .await
            .expect_err("fixed provider endpoint must be rejected");

        assert_eq!(
            credentials
                .get("responses", Some(custom_endpoint))
                .expect("custom credential"),
            Some("custom-secret".into())
        );
        assert_eq!(error.code, "invalid_config");
        assert!(error.message.contains("fixed API endpoint"));
        assert_eq!(
            credentials.get("kimi", None).expect("fixed credential"),
            None
        );
    }

    #[tokio::test]
    async fn credential_update_refreshes_every_matching_resident_chat() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        std::fs::create_dir(&workspace).expect("workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) = ConfigStore::initialize(state, listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credential store"));
        credentials
            .set("kimi", "old-secret", None)
            .expect("initial Kimi credential");
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway = GatewayHost::start(store, config, credentials, cron).expect("gateway");
        gateway
            .register_provider(ProviderConfig {
                provider: "kimi".into(),
                model: "kimi-k3".into(),
                base_url: None,
                reasoning_effort: Some("max".into()),
                web_search: horus::backend::model::provider::HostedWebSearch::Off,
            })
            .await
            .expect("register Kimi");
        let first = gateway
            .create_session(&workspace)
            .await
            .expect("first chat");
        let second = gateway
            .create_session(&workspace)
            .await
            .expect("second chat");
        let mut first_events = first.subscribe();
        let mut second_events = second.subscribe();

        gateway
            .set_credential("kimi".into(), "new-secret".into(), None)
            .await
            .expect("replace Kimi credential");

        for events in [&mut first_events, &mut second_events] {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if matches!(
                        events.recv().await.expect("chat event").message,
                        ServerMessage::SessionChanged { .. }
                    ) {
                        break;
                    }
                }
            })
            .await
            .expect("matching chat refresh");
        }
    }

    #[test]
    fn credential_refresh_matches_only_the_selected_custom_endpoint() {
        let selection = ProviderConfig {
            provider: "responses".into(),
            model: "custom-model".into(),
            base_url: Some("https://first.example/v1".into()),
            reasoning_effort: None,
            web_search: horus::backend::model::provider::HostedWebSearch::Off,
        };

        assert!(
            provider_credential_matches(&selection, "responses", Some("https://first.example/v1"))
                .expect("matching endpoint")
                && !provider_credential_matches(
                    &selection,
                    "responses",
                    Some("https://second.example/v1")
                )
                .expect("different endpoint")
        );
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
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credential store"));
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway = GatewayHost::start(store, config, credentials, cron).expect("gateway");
        let first_host = gateway.create_session(&first).await.expect("first chat");
        let second_host = gateway.create_session(&second).await.expect("second chat");
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
        composition.middleware.set_enabled("tools", false);

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
            .set("kimi", "test-secret", None)
            .expect("Kimi credential");
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let gateway = GatewayHost::start(store, config, credentials, cron).expect("gateway");
        let mut gateway_updates = gateway.subscribe();
        let ready = gateway
            .register_provider(ProviderConfig {
                provider: "kimi".into(),
                model: "kimi-k3".into(),
                base_url: None,
                reasoning_effort: Some("max".into()),
                web_search: horus::backend::model::provider::HostedWebSearch::Off,
            })
            .await
            .expect("register Kimi");
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
            .find(|choice| choice.model == "kimi-k2.7-code")
            .expect("alternate Kimi model")
            .route
            .clone();
        let selected = gateway
            .create_session(&workspace)
            .await
            .expect("selected chat");

        selected
            .set_model(alternate.clone())
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
        assert_eq!(
            selected_ready.config.config.provider.model,
            "kimi-k2.7-code"
        );
        assert_eq!(fresh_ready.config.config.provider.model, "kimi-k3");
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
                    }),
                },
            );
        }

        state.ensure_capacity().await.expect("reclaim capacity");

        assert_eq!(state.sessions.len(), MAX_ACTIVE_SESSIONS - 1);
    }

    #[tokio::test]
    async fn session_catalog_includes_empty_roots_and_fresh_forks() {
        let workspace = tempfile::tempdir().expect("workspace");
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(
            SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
                .expect("checkpoints"),
        );
        let mut parent = Checkpoint::empty("parent");
        parent.session_context.workspace_id = Some("workspace".into());
        parent.sequence = 1;
        checkpoints.save(&parent, &[]).await.expect("save parent");
        let mut empty_root = Checkpoint::empty("empty-root");
        empty_root.session_context.workspace_id = Some("workspace".into());
        checkpoints
            .save(&empty_root, &[])
            .await
            .expect("save empty root");
        let mut child = Checkpoint::empty("child");
        child.session_context.workspace_id = Some("workspace".into());
        checkpoints
            .fork("parent", parent.sequence, &child)
            .await
            .expect("fork parent");

        let mut sessions = session_catalog(&checkpoints)
            .await
            .expect("session catalog")
            .into_iter()
            .map(|record| (record.summary.session_id, record.summary.parent_session_id))
            .collect::<Vec<_>>();
        sessions.sort();

        assert_eq!(
            sessions,
            vec![
                ("child".into(), Some("parent".into())),
                ("empty-root".into(), None),
                ("parent".into(), None)
            ]
        );
    }

    #[tokio::test]
    async fn session_catalog_is_bounded_and_truncates_utf8_previews() {
        let workspace = tempfile::tempdir().expect("workspace");
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(
            SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
                .expect("checkpoints"),
        );
        for index in 0..=SESSION_PAGE_SIZE {
            let mut checkpoint = Checkpoint::empty(format!("{index:03}"));
            checkpoint.session_context.workspace_id = Some("workspace".into());
            checkpoint.sequence = 1;
            checkpoint.first_user_message = Some(if index == SESSION_PAGE_SIZE {
                "€".repeat(MAX_SESSION_PREVIEW_BYTES / '€'.len_utf8() + 1)
            } else {
                format!("chat {index}")
            });
            checkpoints.save(&checkpoint, &[]).await.expect("save chat");
        }

        let sessions = session_catalog(&checkpoints)
            .await
            .expect("session catalog");
        let preview = sessions
            .iter()
            .find(|session| session.summary.session_id == "100")
            .and_then(|session| session.summary.first_user_message.as_deref())
            .expect("UTF-8 preview");

        assert_eq!(sessions.len(), SESSION_PAGE_SIZE);
        assert!(
            sessions
                .iter()
                .all(|session| session.summary.session_id != "000")
        );
        assert_eq!(
            preview,
            "€".repeat(MAX_SESSION_PREVIEW_BYTES / '€'.len_utf8())
        );
    }

    #[tokio::test]
    async fn workspace_diff_includes_staged_unstaged_and_untracked_text() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (_state, sandbox) = test_git_sandbox(workspace.path());
        run_git(workspace.path(), &["init", "--quiet"]);
        run_git(
            workspace.path(),
            &["config", "user.email", "horus@example.invalid"],
        );
        run_git(workspace.path(), &["config", "user.name", "Horus Test"]);
        run_git(workspace.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::write(workspace.path().join("staged.txt"), "before\n").expect("staged file");
        std::fs::write(workspace.path().join("unstaged.txt"), "before\n").expect("unstaged file");
        std::fs::write(workspace.path().join(".gitignore"), "ignored.txt\n").expect("ignore file");
        run_git(workspace.path(), &["add", "--", "."]);
        run_git(workspace.path(), &["commit", "--quiet", "-m", "initial"]);

        std::fs::write(workspace.path().join("staged.txt"), "staged change\n")
            .expect("change staged file");
        run_git(workspace.path(), &["add", "--", "staged.txt"]);
        std::fs::write(workspace.path().join("unstaged.txt"), "unstaged change\n")
            .expect("change unstaged file");
        std::fs::write(workspace.path().join("new.txt"), "untracked content\n")
            .expect("untracked file");
        std::fs::write(workspace.path().join("ignored.txt"), "ignored\n").expect("ignored file");
        std::fs::write(workspace.path().join("binary.bin"), [0, 1, 2]).expect("binary file");

        let diff = workspace_git_diff(&sandbox, workspace.path())
            .await
            .expect("workspace diff");

        assert!(
            diff.contains("diff --git a/staged.txt b/staged.txt")
                && diff.contains("+staged change")
                && diff.contains("diff --git a/unstaged.txt b/unstaged.txt")
                && diff.contains("+unstaged change")
                && diff.contains("diff --git a/new.txt b/new.txt")
                && diff.contains("--- /dev/null")
                && diff.contains("+untracked content")
                && !diff.contains("ignored.txt")
                && !diff.contains("binary.bin"),
            "unexpected diff:\n{diff}"
        );
    }

    #[tokio::test]
    async fn workspace_diff_is_empty_outside_a_git_repository() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (_state, sandbox) = test_git_sandbox(workspace.path());

        let diff = workspace_git_diff(&sandbox, workspace.path())
            .await
            .expect("non-Git workspace");

        assert!(diff.is_empty());
    }

    #[tokio::test]
    async fn workspace_diff_rejects_oversized_output() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (_state, sandbox) = test_git_sandbox(workspace.path());
        run_git(workspace.path(), &["init", "--quiet"]);
        std::fs::write(
            workspace.path().join("large.txt"),
            "x".repeat(MAX_GIT_DIFF_BYTES),
        )
        .expect("large untracked file");

        let error = workspace_git_diff(&sandbox, workspace.path())
            .await
            .expect_err("oversized diff");

        assert_eq!(error.code, "git_diff_too_large");
    }

    #[test]
    fn session_titles_are_trimmed_and_bounded() {
        assert_eq!(
            validate_session_title("  hello  ").expect("valid title"),
            "hello"
        );
        assert_eq!(
            validate_session_title(" ").expect_err("blank title").code,
            "invalid_session_title"
        );
        assert!(validate_session_title(&"x".repeat(MAX_SESSION_TITLE_BYTES + 1)).is_err());
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
    fn active_provider_login_reserves_the_only_polling_slot() {
        let active = StdMutex::new(None);
        reserve_provider_login(&active, "login-a").expect("reserve first login");
        let rejection = reserve_provider_login(&active, "login-b")
            .expect_err("a second provider login must be rejected");

        assert_eq!(rejection.code, "provider_login_in_progress");
        release_provider_login(&active, "another-login").expect("ignore stale completion");
        assert!(reserve_provider_login(&active, "login-b").is_err());
        release_provider_login(&active, "login-a").expect("finish first login");
        reserve_provider_login(&active, "login-b").expect("reserve next login");
    }

    #[test]
    fn session_history_carries_rendered_blocks_and_child_actions() {
        let action = Op::CapabilityCommand {
            capability: "subagents".into(),
            command: "subagents".into(),
            arguments: String::new(),
        };
        let event = EventMsg::SessionHistory(horus::protocol::SessionHistoryEvent {
            events: vec![
                EventMsg::UserMessage(horus::protocol::UserMessageEvent {
                    message: "inspect".into(),
                }),
                EventMsg::SessionHistory(horus::protocol::SessionHistoryEvent {
                    events: vec![EventMsg::Frontend(FrontendEvent::Widget {
                        capability: "subagents".into(),
                        item: horus::protocol::FrontendWidget {
                            id: "subagents".into(),
                            slot: horus::protocol::FrontendSlot::Header,
                            text: "subagents".into(),
                            tone: horus::protocol::FrontendTone::Neutral,
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
