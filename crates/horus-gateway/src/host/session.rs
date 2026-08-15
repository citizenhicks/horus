mod events;
mod runtime;

use super::*;

pub(super) type SessionWidgets = Vec<((String, String), horus::protocol::FrontendWidget)>;

#[derive(Clone)]
pub(crate) struct HostHandle {
    pub(super) inner: Arc<HostInner>,
}

pub(super) struct HostInner {
    pub(super) session_id: Arc<str>,
    pub(super) commands: mpsc::Sender<HostCommand>,
    pub(super) events: broadcast::Sender<ServerFrame>,
    pub(super) accepts_file_attachments: Arc<AtomicBool>,
    pub(super) alive: Arc<AtomicBool>,
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
    pub(super) replay: VecDeque<ServerFrame>,
    pub(super) replay_bytes: usize,
    pub(super) next_before_sequence: Option<u64>,
    pub(super) artifacts: VecDeque<ArtifactRecord>,
    pub(super) widgets: SessionWidgets,
    commands: mpsc::Receiver<HostCommand>,
    events: broadcast::Sender<ServerFrame>,
    gateway_events: broadcast::Sender<ServerFrame>,
    idle_waiters: Vec<oneshot::Sender<()>>,
}

pub(super) struct LoadedReplay {
    pub(super) latest_sequence: u64,
    pub(super) replay: VecDeque<ServerFrame>,
    pub(super) replay_bytes: usize,
    pub(super) next_before_sequence: Option<u64>,
    pub(super) artifacts: VecDeque<ArtifactRecord>,
    pub(super) widgets: SessionWidgets,
}

struct RunningAgent {
    session_id: String,
    sender: Option<AgentSender>,
    events: mpsc::Receiver<JournalEvent>,
    model_router: Arc<ModelRouter>,
    frontend: FrontendExtensions,
    session: horus::protocol::SessionConfiguredEvent,
    gateway_sandbox: Arc<GatewaySandbox>,
    subagent_template: Option<Arc<OnceLock<AgentConfig>>>,
    tool_count: usize,
}

pub(super) struct ActiveCron {
    pub(super) run: ActiveCronRun,
    pub(super) submission_id: String,
    pub(super) turn_id: Option<String>,
    pub(super) failure: Option<String>,
}

pub(super) enum HostCommand {
    Snapshot {
        last_sequence: Option<u64>,
        reply: oneshot::Sender<std::result::Result<HostSnapshot, Rejection>>,
    },
    HistoryPage {
        before_sequence: Option<u64>,
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
        reply: oneshot::Sender<std::result::Result<WorkspaceFiles, Rejection>>,
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
pub(super) enum JournalSequence {
    AlreadyLoaded,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JournalDelivery {
    Live,
    LoadedStartup,
    ReplacementStartup,
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
            None,
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

    pub(super) fn is_alive(&self) -> bool {
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
    ) -> std::result::Result<SessionHistoryPage, Rejection> {
        let (reply, receiver) = oneshot::channel();
        self.send(HostCommand::HistoryPage {
            before_sequence,
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
    ) -> std::result::Result<WorkspaceFiles, Rejection> {
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

    pub(super) async fn refresh_provider(
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

    pub(super) async fn wait_idle(&self) {
        let (reply, receiver) = oneshot::channel();
        if self.send(HostCommand::WaitIdle { reply }).await.is_ok() {
            let _ = receiver.await;
        }
    }

    pub(super) fn is_unreferenced(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }

    pub(super) async fn stop_if_idle(&self) -> bool {
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

pub(super) fn provider_credential_matches(
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

pub(super) fn fail_active_cron(
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

pub(super) fn setup_agent_config() -> VersionedAgentConfig {
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
    reusable_model_router: Option<Arc<ModelRouter>>,
) -> Result<RunningAgent> {
    let BuiltAgent {
        agent,
        model_router,
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
        reusable_model_router,
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
        model_router,
        frontend,
        session,
        gateway_sandbox,
        subagent_template,
        tool_count,
    })
}

pub(super) fn reusable_model_router(
    old_spec: &ChatSpec,
    next_spec: &ChatSpec,
    router: &Arc<ModelRouter>,
) -> Option<Arc<ModelRouter>> {
    provider_config_unchanged(old_spec, next_spec).then(|| Arc::clone(router))
}

pub(super) fn provider_config_unchanged(old_spec: &ChatSpec, next_spec: &ChatSpec) -> bool {
    old_spec.agent.config.provider == next_spec.agent.config.provider
}

pub(super) fn runtime_accepts_attachments(frontend: &FrontendExtensions) -> bool {
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

pub(super) fn cron_execution_checkpoint(
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

pub(super) async fn hide_checkpoint(
    checkpoints: &Arc<dyn CheckpointStore>,
    session_id: &str,
) -> Result<()> {
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
