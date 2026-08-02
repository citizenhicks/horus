//! Single-agent ownership, event sequencing, replay, and authenticated operations.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use horus::agent::{AgentConfig, AgentSender};
use horus::backend::checkpoint::{CheckpointStore, SessionPageRequest, sqlite::SqliteCheckpoint};
use horus::backend::model::provider::{ProviderAuth, provider};
use horus::backend::sandbox::CommandOutput;
use horus::middleware::FrontendExtensions;
use horus::protocol::{
    Event, EventMsg, FrontendBlock, FrontendBlockFormat, FrontendEvent, Op, ReviewDecision,
    Submission,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

use crate::assembly::{BuiltAgent, assemble, provider_statuses};
use crate::config::{ConfigStore, CredentialStore, GatewayConfig};
use crate::cron::{ActiveCronRun, BeginRun, CronStore};
use crate::sandbox::{GatewaySandbox, MAX_COMMAND_OUTPUT_BYTES};
use crate::wire::{
    AgentComposition, ArtifactKind, ArtifactRecord, CronRunStatus, GitStatus, ProfileSnapshot,
    ReadyPayload, RenderedEvent, RenderedPreview, ServerFrame, ServerMessage, SessionRecord,
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

#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionMetadata {
    title: Option<String>,
    pinned: bool,
    hidden: bool,
}

type SessionCatalogMetadata = BTreeMap<String, SessionMetadata>;

#[derive(Clone)]
pub(crate) struct HostHandle {
    commands: mpsc::Sender<HostCommand>,
    events: broadcast::Sender<ServerFrame>,
}

pub(crate) struct HostSnapshot {
    pub(crate) ready: ReadyPayload,
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
    config: GatewayConfig,
    credentials: Arc<CredentialStore>,
    cron: Arc<CronStore>,
    checkpoints: Arc<dyn CheckpointStore>,
    running: RunningAgent,
    pending_turns: usize,
    approval_active: bool,
    restart_after_turn: bool,
    provider_login: Option<String>,
    suppress_history_broadcast: bool,
    pending_startup: Vec<ServerFrame>,
    active_cron: Option<ActiveCron>,
    sequence: u64,
    replay: VecDeque<ServerFrame>,
    artifacts: VecDeque<ArtifactRecord>,
    commands: mpsc::Receiver<HostCommand>,
    command_sender: mpsc::WeakSender<HostCommand>,
    events: broadcast::Sender<ServerFrame>,
}

struct RunningAgent {
    session_id: String,
    sender: AgentSender,
    events: mpsc::Receiver<Event>,
    frontend: FrontendExtensions,
    session: horus::protocol::SessionConfiguredEvent,
    model_choices: Vec<horus::backend::model::ModelChoice>,
    gateway_sandbox: Arc<GatewaySandbox>,
    subagent_template: Option<Arc<OnceLock<AgentConfig>>>,
}

struct ActiveCron {
    run: ActiveCronRun,
    submission_id: String,
    turn_id: Option<String>,
    return_session_id: String,
    failure: Option<String>,
}

enum HostCommand {
    Snapshot {
        last_sequence: Option<u64>,
        reply: oneshot::Sender<std::result::Result<HostSnapshot, Rejection>>,
    },
    OpenSession {
        session_id: Option<String>,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
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
    Configure {
        expected_revision: u64,
        config: AgentComposition,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    SetWorkspace {
        path: PathBuf,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    GitDiff {
        reply: oneshot::Sender<std::result::Result<String, Rejection>>,
    },
    SetCredential {
        provider: String,
        api_key: String,
        base_url: Option<String>,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    StartProviderLogin {
        request_id: String,
        provider: String,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
    ProviderLoginCompleted {
        request_id: String,
        login_id: String,
        provider: String,
        result: std::result::Result<(), String>,
    },
    Profile {
        reply: oneshot::Sender<ProfileSnapshot>,
    },
    Artifacts {
        reply: oneshot::Sender<Vec<ArtifactRecord>>,
    },
    RunCron {
        id: String,
        reply: oneshot::Sender<std::result::Result<(), Rejection>>,
    },
}

enum Next {
    Command(Option<HostCommand>),
    Event(Option<Event>),
}

impl HostHandle {
    pub(crate) async fn start(
        store: ConfigStore,
        config: GatewayConfig,
        credentials: Arc<CredentialStore>,
        cron: Arc<CronStore>,
    ) -> Result<Self> {
        let checkpoints: Arc<dyn CheckpointStore> =
            Arc::new(SqliteCheckpoint::new(store.checkpoints_path())?);
        let running = start_agent(
            &config,
            &store,
            Arc::clone(&credentials),
            Arc::clone(&checkpoints),
            None,
            "horus-gateway",
        )
        .await?;
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        let state = HostState {
            store,
            config,
            credentials,
            cron,
            checkpoints,
            running,
            pending_turns: 0,
            approval_active: false,
            restart_after_turn: false,
            provider_login: None,
            suppress_history_broadcast: false,
            pending_startup: Vec::new(),
            active_cron: None,
            sequence: 0,
            replay: VecDeque::with_capacity(REPLAY_CAPACITY),
            artifacts: VecDeque::with_capacity(ARTIFACT_CAPACITY),
            commands: receiver,
            command_sender: commands.downgrade(),
            events: events.clone(),
        };
        tokio::spawn(state.run());
        Ok(Self { commands, events })
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<ServerFrame> {
        self.events.subscribe()
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

    pub(crate) async fn open_session(
        &self,
        session_id: Option<String>,
    ) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::OpenSession { session_id, reply })
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

    pub(crate) async fn set_workspace(&self, path: PathBuf) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::SetWorkspace { path, reply }).await?;
        receive(receiver).await
    }

    pub(crate) async fn git_diff(&self) -> std::result::Result<String, Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::GitDiff { reply }).await?;
        receiver.await.map_err(|_| stopped())?
    }

    pub(crate) async fn set_credential(
        &self,
        provider: String,
        api_key: String,
        base_url: Option<String>,
    ) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::SetCredential {
            provider,
            api_key,
            base_url,
            reply,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn start_provider_login(
        &self,
        request_id: String,
        provider: String,
    ) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::StartProviderLogin {
            request_id,
            provider,
            reply,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn profile(&self) -> std::result::Result<ProfileSnapshot, Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::Profile { reply }).await?;
        receiver.await.map_err(|_| stopped())
    }

    pub(crate) async fn artifacts(&self) -> std::result::Result<Vec<ArtifactRecord>, Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::Artifacts { reply }).await?;
        receiver.await.map_err(|_| stopped())
    }

    pub(crate) async fn run_cron(&self, id: String) -> std::result::Result<(), Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::RunCron { id, reply }).await?;
        receive(receiver).await
    }

    async fn send(&self, command: HostCommand) -> std::result::Result<(), Rejection> {
        self.commands.send(command).await.map_err(|_| stopped())
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
                Next::Command(Some(command)) => self.handle(command).await,
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
        shutdown_agent(self.running).await;
    }

    async fn handle(&mut self, command: HostCommand) {
        match command {
            HostCommand::Snapshot {
                last_sequence,
                reply,
            } => {
                let _ = reply.send(self.snapshot_value(last_sequence).await);
            }
            HostCommand::OpenSession { session_id, reply } => {
                let result = self.open_session(session_id).await;
                let _ = reply.send(result);
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
            HostCommand::Configure {
                expected_revision,
                config,
                reply,
            } => {
                let result = self.configure(expected_revision, config).await;
                let _ = reply.send(result);
            }
            HostCommand::SetWorkspace { path, reply } => {
                let result = self.set_workspace(path).await;
                let _ = reply.send(result);
            }
            HostCommand::GitDiff { reply } => {
                let _ = reply.send(
                    workspace_git_diff(&self.running.gateway_sandbox, &self.config.workspace).await,
                );
            }
            HostCommand::SetCredential {
                provider,
                api_key,
                base_url,
                reply,
            } => {
                let result = self
                    .set_credential(&provider, &api_key, base_url.as_deref())
                    .await;
                let _ = reply.send(result);
            }
            HostCommand::StartProviderLogin {
                request_id,
                provider,
                reply,
            } => {
                let result = self.start_provider_login(request_id, provider).await;
                let _ = reply.send(result);
            }
            HostCommand::ProviderLoginCompleted {
                request_id,
                login_id,
                provider,
                result,
            } => {
                self.finish_provider_login(request_id, login_id, provider, result)
                    .await;
            }
            HostCommand::Profile { reply } => {
                let _ = reply.send(self.config.profile());
            }
            HostCommand::Artifacts { reply } => {
                let _ = reply.send(self.artifacts.iter().cloned().collect());
            }
            HostCommand::RunCron { id, reply } => {
                let result = self.run_cron(&id).await;
                let _ = reply.send(result);
            }
        }
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

    async fn open_session(
        &mut self,
        session_id: Option<String>,
    ) -> std::result::Result<(), Rejection> {
        self.require_idle()?;
        if let Some(session_id) = session_id.as_deref() {
            let checkpoint = self
                .checkpoints
                .load(session_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| Rejection {
                    code: "unknown_session",
                    message: "the requested session does not exist".into(),
                    fatal: false,
                })?;
            let current_workspace = self.config.workspace_info();
            if checkpoint.session_context.workspace_id.as_deref()
                != Some(current_workspace.id.as_str())
            {
                return Err(invalid_session_workspace());
            }
        }
        self.restart(session_id, "horus-gateway").await?;
        self.broadcast_ready().await?;
        Ok(())
    }

    async fn rename_session(
        &mut self,
        session_id: &str,
        title: &str,
    ) -> std::result::Result<(), Rejection> {
        self.require_session(session_id).await?;
        let title = validate_session_title(title)?;
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
            return Err(Rejection {
                code: "active_session",
                message: "open another chat before deleting this one".into(),
                fatal: false,
            });
        }
        self.require_session(session_id).await?;
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

    async fn run_cron(&mut self, id: &str) -> std::result::Result<(), Rejection> {
        let task = self.cron.task(id).map_err(invalid_cron)?;
        if let Err(rejection) = self.require_idle() {
            self.cron
                .skip_run(
                    &task.id,
                    "the agent was busy when this invocation became due",
                )
                .map_err(internal)?;
            return Err(rejection);
        }
        let run = match self.cron.begin_run(&task.id).map_err(invalid_cron)? {
            BeginRun::Started(run) => run,
            BeginRun::Skipped => {
                return Err(Rejection {
                    code: "cron_overlap",
                    message: format!("cron task {} is already running", task.id),
                    fatal: false,
                });
            }
        };
        let (_, input) = match self.cron.task_input(&task.id) {
            Ok(task) => task,
            Err(error) => {
                let message = error.to_string();
                self.cron
                    .finish_run(run, CronRunStatus::Failed, Some(message.clone()))
                    .map_err(internal)?;
                return Err(invalid_cron(message));
            }
        };
        let return_session_id = self.running.session_id.clone();
        let label = format!("cron · {}", task.id.get(..8).unwrap_or(&task.id));
        if let Err(rejection) = self.restart(None, &label).await {
            self.cron
                .finish_run(run, CronRunStatus::Failed, Some(rejection.message.clone()))
                .map_err(internal)?;
            return Err(rejection);
        }
        if let Err(error) = self.cron.attach_session(&run, &self.running.session_id) {
            let message = error.to_string();
            self.cron
                .finish_run(run, CronRunStatus::Failed, Some(message.clone()))
                .map_err(internal)?;
            self.restore_after_failed_cron_start(return_session_id)
                .await;
            return Err(internal(message));
        }
        let submission_id = Uuid::new_v4().to_string();
        self.active_cron = Some(ActiveCron {
            run,
            submission_id: submission_id.clone(),
            turn_id: None,
            return_session_id,
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
            self.restore_after_failed_cron_start(active.return_session_id)
                .await;
            return Err(rejection);
        }
        self.broadcast_ready().await?;
        Ok(())
    }

    async fn restore_after_failed_cron_start(&mut self, session_id: String) {
        if self
            .restart(Some(session_id), "horus-gateway")
            .await
            .is_ok()
        {
            let _ = self.broadcast_ready().await;
        }
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

    async fn configure(
        &mut self,
        expected_revision: u64,
        composition: AgentComposition,
    ) -> std::result::Result<(), Rejection> {
        self.require_idle()?;
        if expected_revision != self.config.agent.revision {
            return Err(Rejection {
                code: "revision_conflict",
                message: format!(
                    "configuration revision is now {}",
                    self.config.agent.revision
                ),
                fatal: false,
            });
        }
        let next = self
            .config
            .replacing_agent(expected_revision, composition)
            .map_err(invalid_config)?;
        let session_id = self.running.session_id.clone();
        let replacement = start_agent(
            &next,
            &self.store,
            Arc::clone(&self.credentials),
            Arc::clone(&self.checkpoints),
            Some(session_id),
            "horus-gateway",
        )
        .await
        .map_err(internal)?;
        self.store.save(&next).map_err(internal)?;
        let suppress_history_broadcast = reset_replay_for_restart(
            &mut self.replay,
            &self.running.session_id,
            &replacement.session_id,
        );
        let previous = std::mem::replace(&mut self.running, replacement);
        self.suppress_history_broadcast = suppress_history_broadcast;
        self.config = next;
        shutdown_agent(previous).await;
        if suppress_history_broadcast {
            self.record_replacement_startup().map_err(internal)?;
        }
        self.broadcast(ServerMessage::ConfigChanged {
            snapshot: self.config.agent.clone(),
        });
        self.broadcast_ready().await?;
        Ok(())
    }

    async fn set_workspace(&mut self, path: PathBuf) -> std::result::Result<(), Rejection> {
        self.require_idle()?;
        let next = self
            .store
            .replacing_workspace(&self.config, &path)
            .map_err(invalid_workspace)?;
        if next.workspace == self.config.workspace {
            self.broadcast_ready().await?;
            return Ok(());
        }
        self.switch_workspace(next, None).await?;
        self.broadcast_ready().await?;
        Ok(())
    }

    async fn switch_workspace(
        &mut self,
        next: GatewayConfig,
        session_id: Option<String>,
    ) -> std::result::Result<(), Rejection> {
        if !self.cron.list().map_err(internal)?.is_empty() {
            return Err(Rejection {
                code: "workspace_has_cron",
                message: "delete all cron tasks before changing the workspace".into(),
                fatal: false,
            });
        }
        let replacement = start_agent(
            &next,
            &self.store,
            Arc::clone(&self.credentials),
            Arc::clone(&self.checkpoints),
            session_id,
            "horus-gateway",
        )
        .await
        .map_err(internal)?;
        if let Err(error) = self.store.save(&next) {
            shutdown_agent(replacement).await;
            return Err(internal(error));
        }
        if let Err(error) = self.cron.set_workspace(&next.workspace) {
            let message = match self.store.save(&self.config) {
                Ok(()) => error.to_string(),
                Err(rollback) => format!(
                    "{error}; restoring the previous gateway configuration failed: {rollback}"
                ),
            };
            shutdown_agent(replacement).await;
            return Err(invalid_workspace(message));
        }
        reset_replay_for_restart(
            &mut self.replay,
            &self.running.session_id,
            &replacement.session_id,
        );
        let previous = std::mem::replace(&mut self.running, replacement);
        self.config = next;
        self.suppress_history_broadcast = false;
        self.pending_startup.clear();
        self.artifacts.clear();
        shutdown_agent(previous).await;
        Ok(())
    }

    async fn set_credential(
        &mut self,
        provider_id: &str,
        api_key: &str,
        base_url: Option<&str>,
    ) -> std::result::Result<(), Rejection> {
        self.require_idle()?;
        let definition = provider(provider_id).map_err(invalid_config)?;
        let base_url = base_url.or_else(|| {
            if !definition.configurable_base_url() {
                return None;
            }
            if self.config.agent.config.provider.provider == provider_id {
                self.config.agent.config.provider.base_url.as_deref()
            } else {
                None
            }
            .or_else(|| definition.default_base_url())
        });
        definition
            .validate_base_url(base_url)
            .map_err(invalid_config)?;
        let active = &self.config.agent.config.provider;
        let active_base_url = if definition.configurable_base_url() {
            active
                .base_url
                .as_deref()
                .or_else(|| definition.default_base_url())
        } else {
            None
        };
        let restart = active.provider == provider_id && active_base_url == base_url;
        self.credentials
            .set(provider_id, api_key, base_url)
            .map_err(invalid_config)?;
        if restart {
            let session_id = Some(self.running.session_id.clone());
            self.restart(session_id, "horus-gateway").await?;
        }
        self.broadcast_ready().await?;
        Ok(())
    }

    async fn start_provider_login(
        &mut self,
        request_id: String,
        provider_id: String,
    ) -> std::result::Result<(), Rejection> {
        ensure_provider_login_available(self.provider_login.as_deref())?;
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
        let login = auth.start_device().await.map_err(internal)?;
        let login_id = Uuid::new_v4().to_string();
        self.provider_login = Some(login_id.clone());
        self.broadcast(ServerMessage::ProviderLoginStarted {
            request_id: request_id.clone(),
            login_id: login_id.clone(),
            provider: provider_id.clone(),
            verification_url: login.verification_url().into(),
            user_code: login.user_code().into(),
        });
        let path = self.store.provider_auth_path();
        let commands = self.command_sender.clone();
        tokio::spawn(async move {
            let result = login
                .complete(path)
                .await
                .map_err(|error| error.to_string());
            if let Some(commands) = commands.upgrade() {
                let _ = commands
                    .send(HostCommand::ProviderLoginCompleted {
                        request_id,
                        login_id,
                        provider: provider_id,
                        result,
                    })
                    .await;
            }
        });
        Ok(())
    }

    async fn finish_provider_login(
        &mut self,
        request_id: String,
        login_id: String,
        provider: String,
        result: std::result::Result<(), String>,
    ) {
        if self.provider_login.as_deref() != Some(login_id.as_str()) {
            return;
        }
        self.provider_login = None;
        if let Err(message) = result {
            self.broadcast(ServerMessage::Rejected {
                request_id,
                code: "provider_login_failed".into(),
                message,
                fatal: false,
            });
            return;
        }
        self.broadcast(ServerMessage::ProviderLoginFinished {
            request_id,
            login_id,
            provider: provider.clone(),
        });
        if self.config.agent.config.provider.provider != provider {
            let _ = self.broadcast_ready().await;
            return;
        }
        if self.pending_turns > 0 || self.approval_active {
            self.restart_after_turn = true;
            return;
        }
        if let Err(rejection) = self
            .restart(Some(self.running.session_id.clone()), "horus-gateway")
            .await
        {
            self.broadcast(ServerMessage::Error {
                code: rejection.code.into(),
                message: rejection.message,
                fatal: rejection.fatal,
            });
            return;
        }
        let _ = self.broadcast_ready().await;
    }

    async fn restart(
        &mut self,
        session_id: Option<String>,
        origin_label: &str,
    ) -> std::result::Result<(), Rejection> {
        let replacement = start_agent(
            &self.config,
            &self.store,
            Arc::clone(&self.credentials),
            Arc::clone(&self.checkpoints),
            session_id,
            origin_label,
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
            EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_) => {
                self.pending_turns = self.pending_turns.saturating_sub(1);
                self.approval_active = false;
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
                self.restart_after_turn = false;
                self.restart(Some(active.return_session_id), "horus-gateway")
                    .await
                    .map_err(|rejection| Error::Config(rejection.message))?;
                self.broadcast_ready()
                    .await
                    .map_err(|rejection| Error::Config(rejection.message))?;
            } else if self.restart_after_turn {
                self.restart_after_turn = false;
                self.restart(Some(self.running.session_id.clone()), "horus-gateway")
                    .await
                    .map_err(|rejection| Error::Config(rejection.message))?;
                self.broadcast_ready()
                    .await
                    .map_err(|rejection| Error::Config(rejection.message))?;
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
            && self.config.observe_usage(
                &self.running.session_id,
                &info.total_token_usage,
                was_active,
            )?
        {
            self.store.save(&self.config)?;
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

    async fn ready(&self) -> Result<ReadyPayload> {
        let workspace = self.config.workspace_info();
        let sessions = session_catalog(&self.checkpoints, &workspace.id).await?;
        Ok(ReadyPayload {
            latest_sequence: self.sequence,
            workspace,
            git: git_status(&self.running.gateway_sandbox).await,
            session: self.running.session.clone(),
            sessions,
            model_choices: self.running.model_choices.clone(),
            contributions: self.running.frontend.contributions().to_vec(),
            config: self.config.agent.clone(),
            providers: provider_statuses(&self.store, &self.credentials)?,
        })
    }

    async fn broadcast_ready(&mut self) -> std::result::Result<(), Rejection> {
        let payload = self.ready().await.map_err(internal)?;
        let ready = ServerFrame::new(ServerMessage::Ready { payload });
        let pending = std::mem::take(&mut self.pending_startup);
        publish_ready_and_pending(&self.events, ready, pending);
        Ok(())
    }

    async fn broadcast_sessions(&self) -> std::result::Result<(), Rejection> {
        let workspace = self.config.workspace_info();
        let sessions = session_catalog(&self.checkpoints, &workspace.id)
            .await
            .map_err(internal)?;
        self.broadcast(ServerMessage::Sessions { sessions });
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
}

async fn start_agent(
    config: &GatewayConfig,
    store: &ConfigStore,
    credentials: Arc<CredentialStore>,
    checkpoints: Arc<dyn CheckpointStore>,
    session_id: Option<String>,
    origin_label: &str,
) -> Result<RunningAgent> {
    let BuiltAgent {
        agent,
        gateway_sandbox,
        subagent_template,
    } = assemble(
        config,
        store,
        credentials,
        checkpoints,
        session_id,
        origin_label,
    )
    .await?;
    let session = agent.session().clone();
    let model_choices = agent.model_choices().to_vec();
    let frontend = agent.frontend().clone();
    let session_id = session.session_id.clone();
    let (sender, events) = agent.into_parts();
    Ok(RunningAgent {
        session_id,
        sender,
        events,
        frontend,
        session,
        model_choices,
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

async fn session_catalog(
    checkpoints: &Arc<dyn CheckpointStore>,
    workspace_id: &str,
) -> Result<Vec<SessionRecord>> {
    // ponytail: scan one global page; add store-side workspace filtering if foreign sessions
    // can obscure older sessions from this workspace.
    let page = checkpoints
        .list_sessions_page(SessionPageRequest {
            cursor: None,
            limit: SESSION_PAGE_SIZE,
        })
        .await?;
    let mut sessions = page
        .sessions
        .into_iter()
        .filter(|session| {
            session.catalog_visible
                && (session.sequence > 0 || session.parent_session_id.is_some())
                && session.session_context.workspace_id.as_deref() == Some(workspace_id)
        })
        .collect::<Vec<_>>();
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
    use std::time::Duration;

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

    async fn save_chat(
        checkpoints: &SqliteCheckpoint,
        session_id: &str,
        workspace: crate::wire::WorkspaceInfo,
    ) {
        let mut checkpoint = Checkpoint::empty(session_id);
        checkpoint.session_context.workspace_id = Some(workspace.id);
        checkpoint.session_context.workspace_label = Some(workspace.label);
        checkpoint.first_user_message = Some(format!("chat {session_id}"));
        checkpoint.sequence = 1;
        checkpoints.save(&checkpoint, &[]).await.expect("save chat");
    }

    async fn next_sessions(events: &mut broadcast::Receiver<ServerFrame>) -> Vec<SessionRecord> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let ServerMessage::Sessions { sessions } =
                    events.recv().await.expect("gateway broadcast").message
                {
                    return sessions;
                }
            }
        })
        .await
        .expect("sessions timeout")
    }

    #[tokio::test]
    async fn credential_endpoints_are_validated_and_persisted() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        std::fs::create_dir(&workspace).expect("workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) =
            ConfigStore::initialize(state, workspace, listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credential store"));
        let cron = Arc::new(CronStore::open(store.state_dir(), &config.workspace).expect("cron"));
        let host = HostHandle::start(store, config, Arc::clone(&credentials), cron)
            .await
            .expect("host");
        let custom_endpoint = "https://example.com/v1";

        host.set_credential(
            "responses".into(),
            "custom-secret".into(),
            Some(custom_endpoint.into()),
        )
        .await
        .expect("store custom credential");
        let error = host
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
    async fn workspace_change_rebuilds_persists_and_broadcasts_ready() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let replacement = root.path().join("replacement");
        let state = root.path().join("state");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(&replacement).expect("replacement workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) =
            ConfigStore::initialize(state.clone(), workspace, listen, None).expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credential store"));
        let cron = Arc::new(CronStore::open(store.state_dir(), &config.workspace).expect("cron"));
        let host = HostHandle::start(store, config, credentials, cron)
            .await
            .expect("host");
        let previous_session = host
            .snapshot(None)
            .await
            .expect("initial snapshot")
            .ready
            .session
            .session_id;
        let mut events = host.subscribe();

        host.set_workspace(replacement.clone())
            .await
            .expect("change workspace");
        let frame = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("ready timeout")
            .expect("ready broadcast");
        let ServerMessage::Ready { payload } = frame.message else {
            panic!("workspace change must broadcast ready");
        };
        let (_, persisted) = ConfigStore::open(state).expect("persisted config");
        let replacement = std::fs::canonicalize(replacement).expect("canonical replacement");

        assert_eq!(payload.workspace, persisted.workspace_info());
        assert_eq!(persisted.workspace, replacement);
        assert_ne!(payload.session.session_id, previous_session);
        assert_eq!(
            payload.session.context.workspace_id.as_deref(),
            Some(payload.workspace.id.as_str())
        );
    }

    #[tokio::test]
    async fn opening_a_foreign_workspace_chat_is_rejected_and_catalog_stays_local() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let foreign_workspace = root.path().join("foreign");
        let state = root.path().join("state");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(&foreign_workspace).expect("foreign workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) =
            ConfigStore::initialize(state.clone(), workspace, listen, None).expect("config");
        let foreign = GatewayConfig::new(
            listen,
            std::fs::canonicalize(&foreign_workspace).expect("canonical foreign workspace"),
            None,
        )
        .expect("foreign config")
        .workspace_info();
        let checkpoints = SqliteCheckpoint::new(store.checkpoints_path()).expect("checkpoints");
        save_chat(&checkpoints, "local", config.workspace_info()).await;
        save_chat(&checkpoints, "foreign", foreign.clone()).await;
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credential store"));
        let cron = Arc::new(CronStore::open(store.state_dir(), &config.workspace).expect("cron"));
        let host = HostHandle::start(store, config, credentials, cron)
            .await
            .expect("host");

        let error = host
            .open_session(Some("foreign".into()))
            .await
            .expect_err("foreign chat must be rejected");
        let payload = host.snapshot(None).await.expect("snapshot").ready;
        let (_, persisted) = ConfigStore::open(state).expect("persisted config");

        assert_eq!(error.code, "invalid_session_workspace");
        assert_ne!(payload.workspace, foreign);
        assert_ne!(payload.session.session_id, "foreign");
        assert_eq!(payload.sessions.len(), 1);
        assert_eq!(payload.sessions[0].summary.session_id, "local");
        assert_eq!(persisted.workspace_info(), payload.workspace);
    }

    #[tokio::test]
    async fn session_catalog_includes_fresh_forks_but_not_empty_roots() {
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

        let mut sessions = session_catalog(&checkpoints, "workspace")
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

        let sessions = session_catalog(&checkpoints, "workspace")
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
    async fn session_actions_persist_metadata_and_broadcast_the_visible_catalog() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        std::fs::create_dir(&workspace).expect("workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) =
            ConfigStore::initialize(state, workspace, listen, None).expect("config");
        let checkpoints = SqliteCheckpoint::new(store.checkpoints_path()).expect("checkpoints");
        save_chat(&checkpoints, "target", config.workspace_info()).await;
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credential store"));
        let cron = Arc::new(CronStore::open(store.state_dir(), &config.workspace).expect("cron"));
        let host = HostHandle::start(store, config, credentials, cron)
            .await
            .expect("host");
        let mut events = host.subscribe();

        host.rename_session("target".into(), "  Renamed  ".into())
            .await
            .expect("rename chat");
        let renamed = next_sessions(&mut events).await;
        host.set_session_pinned("target".into(), true)
            .await
            .expect("pin chat");
        let pinned = next_sessions(&mut events).await;
        host.delete_session("target".into())
            .await
            .expect("hide chat");
        let hidden = next_sessions(&mut events).await;
        let metadata = load_session_metadata(&(Arc::new(checkpoints) as Arc<dyn CheckpointStore>))
            .await
            .expect("load metadata");

        assert_eq!(renamed[0].title.as_deref(), Some("Renamed"));
        assert!(pinned[0].pinned);
        assert!(hidden.is_empty());
        assert!(metadata["target"].hidden);
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
        let rejection = ensure_provider_login_available(Some("login-a"))
            .expect_err("a second provider login must be rejected");

        assert_eq!(rejection.code, "provider_login_in_progress");
        assert!(ensure_provider_login_available(None).is_ok());
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
