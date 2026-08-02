use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::sync::Arc;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::Notify;

use crate::Error;
use crate::Result;
use crate::agent::AgentSender;
use crate::backend::checkpoint::CheckpointStore;
use crate::backend::checkpoint::TranscriptPageRequest;
use crate::middleware::RuntimeContext;
use crate::protocol::EventMsg;
use crate::protocol::FrontendBlock;
use crate::protocol::FrontendEvent;
use crate::protocol::FrontendPickerOption;
use crate::protocol::FrontendSlot;
use crate::protocol::FrontendTone;
use crate::protocol::FrontendWidget;
use crate::protocol::Op;
use crate::protocol::replay_events;

mod coordination;
mod monitor;

pub(super) use coordination::Followup;
pub(super) use coordination::Mail;
pub(super) use monitor::monitor_agent;

const STATE_KEY: &str = "subagents.v1";
const MAX_MAILBOX_ITEMS: usize = 256;
const PREVIEW_TRANSCRIPT_BATCHES: usize = 100;
pub(super) const MAX_MESSAGE_BYTES: usize = 24_000;

pub(super) struct Shared {
    roots: Mutex<BTreeMap<String, Arc<RootSlot>>>,
    changed: Notify,
    max_concurrency: usize,
    max_agents: usize,
}

struct RootSlot {
    state: Mutex<Root>,
    writer: Mutex<()>,
}

#[derive(Clone)]
struct Root {
    checkpoints: Arc<dyn CheckpointStore>,
    frontend: crate::middleware::FrontendEventSink,
    tree: Tree,
    senders: BTreeMap<String, AgentSender>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Tree {
    agents: BTreeMap<String, AgentRecord>,
    mailbox: VecDeque<Mail>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct AgentRecord {
    pub(super) parent: String,
    pub(super) session_id: String,
    pub(super) depth: u8,
    pub(super) model: String,
    active_turn_id: Option<String>,
    status: AgentStatus,
    last_message: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentStatus {
    PendingInit,
    Running,
    Interrupted,
    Completed,
    Errored,
}

impl AgentStatus {
    fn label(&self) -> &'static str {
        match self {
            Self::PendingInit => "pending_init",
            Self::Running => "running",
            Self::Interrupted => "interrupted",
            Self::Completed => "completed",
            Self::Errored => "errored",
        }
    }

    fn is_active(&self) -> bool {
        matches!(self, Self::PendingInit | Self::Running)
    }
}

impl Shared {
    pub(super) fn new(max_concurrency: usize, max_agents: usize) -> Result<Self> {
        if max_concurrency < 2 {
            return Err(Error::Config(
                "subagent max concurrency must be at least 2 (including root)".into(),
            ));
        }
        if max_agents < max_concurrency {
            return Err(Error::Config(
                "subagent max agents must be at least max concurrency".into(),
            ));
        }
        Ok(Self {
            roots: Mutex::default(),
            changed: Notify::new(),
            max_concurrency,
            max_agents,
        })
    }

    pub(super) async fn initialize(&self, context: RuntimeContext) -> Result<()> {
        let identity = super::AgentIdentity::read(&context.session_id, &context.metadata)?;
        let root_id = identity.root_session_id;
        let existing = self.roots.lock().await.get(&root_id).cloned();
        if let Some(root) = existing {
            if identity.depth == 0 {
                let mut root = root.state.lock().await;
                root.frontend = context.frontend;
                if !root.tree.agents.is_empty() {
                    emit_status(&root);
                }
            }
            return Ok(());
        }
        let mut tree: Tree = context
            .checkpoints
            .load_state(&root_id, STATE_KEY)
            .await?
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        validate_tree(&tree, self.max_agents)?;
        let mut changed = false;
        for entry in tree.agents.values_mut() {
            if entry.status.is_active() {
                entry.status = AgentStatus::Interrupted;
                entry.active_turn_id = None;
                changed = true;
            }
        }
        let root = Root {
            checkpoints: context.checkpoints,
            frontend: context.frontend,
            tree,
            senders: BTreeMap::new(),
        };
        if changed {
            persist(&root_id, &root).await?;
        } else if !root.tree.agents.is_empty() {
            emit_status(&root);
        }
        self.roots.lock().await.entry(root_id).or_insert_with(|| {
            Arc::new(RootSlot {
                state: Mutex::new(root),
                writer: Mutex::new(()),
            })
        });
        Ok(())
    }

    pub(super) async fn remove_root(&self, root_id: &str) {
        self.roots.lock().await.remove(root_id);
        self.changed.notify_waiters();
    }

    pub(super) async fn reserve(
        &self,
        root_id: &str,
        path: &str,
        parent: &str,
        session_id: String,
        depth: u8,
        model: String,
    ) -> Result<()> {
        let max_agents = self.max_agents;
        let max_concurrency = self.max_concurrency;
        self.mutate_root(root_id, |root| {
            if root.tree.agents.contains_key(path) {
                return Err(Error::Tool(format!("agent `{path}` already exists")));
            }
            if root.tree.agents.len() >= max_agents - 1 {
                return Err(Error::Stopped(format!(
                    "subagent limit {max_agents} (including root) reached"
                )));
            }
            ensure_concurrency_available(&root.tree, max_concurrency)?;
            root.tree.agents.insert(
                path.into(),
                AgentRecord {
                    parent: parent.into(),
                    session_id,
                    depth,
                    model,
                    active_turn_id: None,
                    status: AgentStatus::PendingInit,
                    last_message: None,
                },
            );
            Ok(())
        })
        .await
    }

    pub(super) async fn remove(&self, root_id: &str, path: &str) -> Result<()> {
        self.cleanup_root(root_id, |root| {
            root.tree.agents.remove(path);
            root.senders.remove(path);
            Ok(())
        })
        .await
    }

    pub(super) async fn attach(
        &self,
        root_id: &str,
        path: &str,
        sender: AgentSender,
    ) -> Result<()> {
        self.mutate_root(root_id, |root| {
            let entry = root
                .tree
                .agents
                .get_mut(path)
                .ok_or_else(|| Error::Unknown(format!("agent `{path}`")))?;
            entry.status = AgentStatus::Running;
            root.senders.insert(path.into(), sender);
            Ok(())
        })
        .await
    }

    pub(super) async fn rollback(
        &self,
        root_id: &str,
        path: &str,
        status: AgentStatus,
    ) -> Result<()> {
        self.cleanup_root(root_id, |root| {
            if let Some(entry) = root.tree.agents.get_mut(path) {
                entry.status = status;
            }
            root.senders.remove(path);
            Ok(())
        })
        .await
    }

    pub(super) async fn interrupt(&self, root_id: &str, target: &str) -> Result<String> {
        if target == "/root" {
            return Err(Error::Tool("the root agent cannot interrupt itself".into()));
        }
        let (sender, turn_id, status) = {
            let root = self.root(root_id).await?;
            let root = root.state.lock().await;
            let entry = root
                .tree
                .agents
                .get(target)
                .ok_or_else(|| Error::Unknown(format!("agent `{target}`")))?;
            (
                root.senders.get(target).cloned(),
                entry.active_turn_id.clone(),
                entry.status.label(),
            )
        };
        match (sender, turn_id) {
            (Some(sender), Some(turn_id)) => {
                sender.submit(Op::Interrupt { turn_id })?;
            }
            (Some(_), None) => {
                return Err(Error::Tool(format!(
                    "agent `{target}` has no active turn to interrupt"
                )));
            }
            (None, _) => {}
        }
        Ok(status.into())
    }

    async fn sender(&self, root_id: &str, path: &str) -> Result<AgentSender> {
        self.root(root_id)
            .await?
            .state
            .lock()
            .await
            .senders
            .get(path)
            .cloned()
            .ok_or_else(|| Error::Stopped("agent runtime is unavailable".into()))
    }

    pub(super) async fn list(&self, root_id: &str, prefix: Option<&str>) -> Result<Vec<Value>> {
        let root = self.root(root_id).await?;
        let root = root.state.lock().await;
        Ok(root
            .tree
            .agents
            .iter()
            .filter(|(path, _)| prefix.is_none_or(|prefix| path.starts_with(prefix)))
            .map(|(path, entry)| {
                serde_json::json!({
                    "task_name": path,
                    "status": entry.status.label(),
                    "model": entry.model,
                    "last_message": entry.last_message.as_deref()
                })
            })
            .collect())
    }

    pub(super) async fn resume_options(&self, root_id: &str) -> Result<Vec<FrontendPickerOption>> {
        let root = self.root(root_id).await?;
        let root = root.state.lock().await;
        Ok(root
            .tree
            .agents
            .iter()
            .map(|(path, entry)| FrontendPickerOption {
                label: path.clone(),
                description: format!("{} · {}", entry.status.label(), entry.model),
                op: Op::CapabilityCommand {
                    capability: "subagents".into(),
                    command: "subagents".into(),
                    arguments: path.clone(),
                },
            })
            .collect())
    }

    pub(super) async fn preview(&self, root_id: &str, path: &str) -> Result<Vec<EventMsg>> {
        let (checkpoints, session_id) = {
            let root = self.root(root_id).await?;
            let root = root.state.lock().await;
            let entry = root
                .tree
                .agents
                .get(path)
                .ok_or_else(|| Error::Unknown(format!("agent `{path}`")))?;
            (Arc::clone(&root.checkpoints), entry.session_id.clone())
        };
        let transcript = checkpoints
            .transcript_page(
                &session_id,
                TranscriptPageRequest {
                    before_sequence: None,
                    max_batches: PREVIEW_TRANSCRIPT_BATCHES,
                },
            )
            .await?
            .into_items_chronological();
        Ok(replay_events(&transcript, &session_id))
    }

    async fn root(&self, root_id: &str) -> Result<Arc<RootSlot>> {
        self.roots
            .lock()
            .await
            .get(root_id)
            .cloned()
            .ok_or_else(|| Error::Unknown(format!("agent tree `{root_id}`")))
    }

    /// Strict mutation: the durable write commits before runtime state changes.
    async fn mutate_root<T>(
        &self,
        root_id: &str,
        mutate: impl FnOnce(&mut Root) -> Result<T>,
    ) -> Result<T> {
        self.commit_root(
            root_id,
            |root| mutate(root).map(Stage::Changed),
            OnPersistFailure::Abort,
        )
        .await
        .map(Stage::into_output)
    }

    /// Best-effort cleanup: runtime state commits even when the durable write fails.
    async fn cleanup_root<T>(
        &self,
        root_id: &str,
        cleanup: impl FnOnce(&mut Root) -> Result<T>,
    ) -> Result<T> {
        self.commit_root(
            root_id,
            |root| cleanup(root).map(Stage::Changed),
            OnPersistFailure::CommitWithStatus,
        )
        .await
        .map(Stage::into_output)
    }

    /// Serializes one root mutation: clone, mutate, persist, then commit in memory.
    /// The writer lock orders mutations; the state lock alone never guards a write.
    async fn commit_root<T>(
        &self,
        root_id: &str,
        mutate: impl FnOnce(&mut Root) -> Result<Stage<T>>,
        on_failure: OnPersistFailure,
    ) -> Result<Stage<T>> {
        let root = self.root(root_id).await?;
        let _writer = root.writer.lock().await;
        let (mut candidate, output) = {
            let current = root.state.lock().await;
            let mut candidate = current.clone();
            match mutate(&mut candidate)? {
                Stage::Unchanged(output) => return Ok(Stage::Unchanged(output)),
                Stage::Changed(output) => (candidate, output),
            }
        };
        let error = match persist(root_id, &candidate).await {
            Ok(()) => {
                *root.state.lock().await = candidate;
                return Ok(Stage::Changed(output));
            }
            Err(error) => error,
        };
        match on_failure {
            OnPersistFailure::Abort => Err(error),
            OnPersistFailure::CommitWithStatus => {
                emit_status(&candidate);
                *root.state.lock().await = candidate;
                Err(error)
            }
            OnPersistFailure::RepairRetry(repair) => {
                let retry_message = repair(&mut candidate, &error);
                if let Err(retry_error) = persist(root_id, &candidate).await {
                    (candidate.frontend)(FrontendEvent::Render {
                        capability: "subagents".into(),
                        block: FrontendBlock {
                            id: None,
                            group: None,
                            append: false,
                            pending: false,
                            text: format!("{retry_message}: {retry_error}"),
                            format: crate::protocol::FrontendBlockFormat::PlainText,
                            tone: FrontendTone::Error,
                        },
                    });
                }
                *root.state.lock().await = candidate;
                Ok(Stage::Changed(output))
            }
        }
    }
}

/// One staged root mutation handed to `Shared::commit_root`.
enum Stage<T> {
    /// No durable write is needed; return the output without persisting.
    Unchanged(T),
    /// Persist first, then commit runtime state.
    Changed(T),
}

impl<T> Stage<T> {
    fn into_output(self) -> T {
        match self {
            Self::Unchanged(output) | Self::Changed(output) => output,
        }
    }
}

/// Repairs runtime state after a failed durable write; returns the message
/// surfaced when the retry also fails.
type PersistRepair = Box<dyn FnOnce(&mut Root, &Error) -> String + Send>;

/// How `Shared::commit_root` reacts when the durable write fails.
enum OnPersistFailure {
    /// Leave runtime state untouched and return the error.
    Abort,
    /// Commit runtime state, surface its status widget, and return the error.
    CommitWithStatus,
    /// Repair runtime state, retry the write once, and commit regardless.
    RepairRetry(PersistRepair),
}

fn validate_tree(tree: &Tree, max_agents: usize) -> Result<()> {
    if tree.agents.len() >= max_agents || tree.mailbox.len() > MAX_MAILBOX_ITEMS {
        return Err(Error::Config(
            "subagent checkpoint exceeds safety limits".into(),
        ));
    }
    Ok(())
}

fn active_count(tree: &Tree) -> usize {
    tree.agents
        .values()
        .filter(|entry| entry.status.is_active())
        .count()
}

fn ensure_concurrency_available(tree: &Tree, max_concurrency: usize) -> Result<()> {
    if active_count(tree) >= max_concurrency - 1 {
        return Err(Error::Stopped(format!(
            "subagent concurrency limit {max_concurrency} (including root) reached"
        )));
    }
    Ok(())
}

fn status_widget(tree: &Tree) -> FrontendWidget {
    let active = active_count(tree);
    let failed = tree
        .agents
        .values()
        .any(|agent| matches!(agent.status, AgentStatus::Errored));
    FrontendWidget {
        id: "status".into(),
        slot: FrontendSlot::ComposerFooter,
        text: format!("subagents {} ({active})", tree.agents.len()),
        tone: if failed {
            FrontendTone::Error
        } else if active > 0 {
            FrontendTone::Success
        } else {
            FrontendTone::Neutral
        },
        action: Some(Op::CapabilityCommand {
            capability: "subagents".into(),
            command: "subagents".into(),
            arguments: String::new(),
        }),
    }
}

fn emit_status(root: &Root) {
    (root.frontend)(if root.tree.agents.is_empty() {
        FrontendEvent::RemoveWidget {
            capability: "subagents".into(),
            id: "status".into(),
        }
    } else {
        FrontendEvent::Widget {
            capability: "subagents".into(),
            item: status_widget(&root.tree),
        }
    });
}

async fn persist(root_id: &str, root: &Root) -> Result<()> {
    root.checkpoints
        .save_state(root_id, STATE_KEY, &serde_json::to_value(&root.tree)?)
        .await?;
    emit_status(root);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::*;
    use crate::BoxFuture;
    use crate::backend::checkpoint::Checkpoint;

    struct FailOnceStore {
        fail_next_save: AtomicBool,
        saved_state: StdMutex<Option<Value>>,
    }

    struct BlockingRetryStore {
        saves: AtomicUsize,
        retry_started: Notify,
        release_retry: Notify,
    }

    #[test]
    fn status_widget_owns_its_frontend_action() {
        assert!(matches!(
            status_widget(&Tree::default()).action,
            Some(Op::CapabilityCommand {
                capability,
                command,
                arguments,
            }) if capability == "subagents" && command == "subagents" && arguments.is_empty()
        ));
    }

    impl CheckpointStore for BlockingRetryStore {
        fn load<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, Result<Option<Checkpoint>>> {
            Box::pin(async { Ok(None) })
        }

        fn save<'a>(
            &'a self,
            _checkpoint: &'a Checkpoint,
            _transcript_delta: &'a [Value],
        ) -> BoxFuture<'a, Result<()>> {
            Box::pin(async { Ok(()) })
        }

        fn load_state<'a>(
            &'a self,
            _scope: &'a str,
            _key: &'a str,
        ) -> BoxFuture<'a, Result<Option<Value>>> {
            Box::pin(async { Ok(None) })
        }

        fn save_state<'a>(
            &'a self,
            _scope: &'a str,
            _key: &'a str,
            _value: &'a Value,
        ) -> BoxFuture<'a, Result<()>> {
            let save = self.saves.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                match save {
                    1 => Err(Error::Checkpoint("forced state save failure".into())),
                    2 => {
                        self.retry_started.notify_one();
                        self.release_retry.notified().await;
                        Ok(())
                    }
                    _ => Ok(()),
                }
            })
        }
    }

    impl CheckpointStore for FailOnceStore {
        fn load<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, Result<Option<Checkpoint>>> {
            Box::pin(async { Ok(None) })
        }

        fn save<'a>(
            &'a self,
            _checkpoint: &'a Checkpoint,
            _transcript_delta: &'a [Value],
        ) -> BoxFuture<'a, Result<()>> {
            Box::pin(async { Ok(()) })
        }

        fn load_state<'a>(
            &'a self,
            _scope: &'a str,
            _key: &'a str,
        ) -> BoxFuture<'a, Result<Option<Value>>> {
            Box::pin(async { Ok(None) })
        }

        fn save_state<'a>(
            &'a self,
            _scope: &'a str,
            _key: &'a str,
            value: &'a Value,
        ) -> BoxFuture<'a, Result<()>> {
            let fail = self.fail_next_save.swap(false, Ordering::SeqCst);
            if !fail {
                *self.saved_state.lock().expect("saved state") = Some(value.clone());
            }
            Box::pin(async move {
                if fail {
                    Err(Error::Checkpoint("forced state save failure".into()))
                } else {
                    Ok(())
                }
            })
        }
    }

    #[tokio::test]
    async fn failed_persist_does_not_mutate_runtime_state() {
        let shared = test_shared();
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(FailOnceStore {
            fail_next_save: AtomicBool::new(true),
            saved_state: StdMutex::new(None),
        });
        shared
            .initialize(test_context(checkpoints, Arc::new(|_| {})))
            .await
            .expect("initialize runtime");

        let failed = shared
            .reserve(
                "root",
                "/root/child",
                "/root",
                "child".into(),
                1,
                "test".into(),
            )
            .await
            .is_err();
        let after_failure = shared.list("root", None).await.expect("list agents").len();
        let retried = shared
            .reserve(
                "root",
                "/root/child",
                "/root",
                "child".into(),
                1,
                "test".into(),
            )
            .await
            .is_ok();
        let after_retry = shared.list("root", None).await.expect("list agents").len();

        assert_eq!(
            (failed, after_failure, retried, after_retry),
            (true, 0, true, 1)
        );
    }

    #[tokio::test]
    async fn empty_initial_tree_is_silent_and_empty_transition_removes_widget() {
        let shared = test_shared();
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(FailOnceStore {
            fail_next_save: AtomicBool::new(false),
            saved_state: StdMutex::new(None),
        });
        let frontend_events = Arc::new(StdMutex::new(Vec::new()));
        let events = Arc::clone(&frontend_events);
        shared
            .initialize(test_context(
                checkpoints,
                Arc::new(move |event| events.lock().expect("frontend events").push(event)),
            ))
            .await
            .expect("initialize runtime");
        assert!(frontend_events.lock().expect("frontend events").is_empty());

        shared
            .reserve(
                "root",
                "/root/child",
                "/root",
                "child".into(),
                1,
                "test".into(),
            )
            .await
            .expect("reserve child");
        shared
            .remove("root", "/root/child")
            .await
            .expect("remove child");

        let events = frontend_events.lock().expect("frontend events");
        assert!(matches!(
            events.as_slice(),
            [
                FrontendEvent::Widget { capability, .. },
                FrontendEvent::RemoveWidget {
                    capability: removed_capability,
                    id,
                },
            ] if capability == "subagents"
                && removed_capability == "subagents"
                && id == "status"
        ));
    }

    #[tokio::test]
    async fn wait_returns_immediately_without_an_active_peer() {
        let shared = test_shared();
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(FailOnceStore {
            fail_next_save: AtomicBool::new(false),
            saved_state: StdMutex::new(None),
        });
        shared
            .initialize(test_context(checkpoints, Arc::new(|_| {})))
            .await
            .expect("initialize runtime");
        shared
            .reserve(
                "root",
                "/root/child",
                "/root",
                "child".into(),
                1,
                "test".into(),
            )
            .await
            .expect("reserve child");
        shared
            .rollback("root", "/root/child", AgentStatus::Completed)
            .await
            .expect("complete child");

        let updates = tokio::time::timeout(
            Duration::from_millis(100),
            shared.wait("root", "/root", Duration::from_secs(10)),
        )
        .await
        .expect("wait should not sleep without active peers")
        .expect("wait for updates");

        assert!(updates.is_empty());
    }

    #[tokio::test]
    async fn reserve_enforces_configured_concurrency_including_root() {
        let shared = Shared::new(3, 4).expect("valid limits");
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(FailOnceStore {
            fail_next_save: AtomicBool::new(false),
            saved_state: StdMutex::new(None),
        });
        shared
            .initialize(test_context(checkpoints, Arc::new(|_| {})))
            .await
            .expect("initialize runtime");
        for index in 0..2 {
            shared
                .reserve(
                    "root",
                    &format!("/root/child_{index}"),
                    "/root",
                    format!("child-{index}"),
                    1,
                    "test".into(),
                )
                .await
                .expect("reserve within concurrency limit");
        }

        let error = shared
            .reserve(
                "root",
                "/root/overflow",
                "/root",
                "overflow".into(),
                1,
                "test".into(),
            )
            .await
            .expect_err("reject agent beyond concurrency limit");

        assert_eq!(
            error.to_string(),
            "agent stopped: subagent concurrency limit 3 (including root) reached"
        );
    }

    #[tokio::test]
    async fn reserve_enforces_configured_agent_limit_including_root() {
        let shared = Shared::new(2, 3).expect("valid limits");
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(FailOnceStore {
            fail_next_save: AtomicBool::new(false),
            saved_state: StdMutex::new(None),
        });
        shared
            .initialize(test_context(checkpoints, Arc::new(|_| {})))
            .await
            .expect("initialize runtime");
        for index in 0..2 {
            let path = format!("/root/child_{index}");
            shared
                .reserve(
                    "root",
                    &path,
                    "/root",
                    format!("child-{index}"),
                    1,
                    "test".into(),
                )
                .await
                .expect("reserve within agent limit");
            shared
                .rollback("root", &path, AgentStatus::Completed)
                .await
                .expect("complete child");
        }

        let error = shared
            .reserve(
                "root",
                "/root/overflow",
                "/root",
                "overflow".into(),
                1,
                "test".into(),
            )
            .await
            .expect_err("reject agent beyond agent limit");

        assert_eq!(
            error.to_string(),
            "agent stopped: subagent limit 3 (including root) reached"
        );
    }

    #[tokio::test]
    async fn mail_is_retained_until_its_checkpoint_marker_is_acknowledged() {
        let shared = test_shared();
        let store = Arc::new(FailOnceStore {
            fail_next_save: AtomicBool::new(false),
            saved_state: StdMutex::new(None),
        });
        let checkpoints: Arc<dyn CheckpointStore> = store.clone();
        shared
            .initialize(test_context(checkpoints, Arc::new(|_| {})))
            .await
            .expect("initialize runtime");
        shared
            .queue_message("root", "/root/child", "/root", "done".into())
            .await
            .expect("queue mail");

        let pending = shared
            .receive_mail("root", "/root", &BTreeSet::new())
            .await
            .expect("receive mail");
        let id = pending[0].id.clone();
        let mailbox_len = store
            .saved_state
            .lock()
            .expect("saved state")
            .as_ref()
            .and_then(|state| state["mailbox"].as_array())
            .map(Vec::len);
        assert_eq!(mailbox_len, Some(1));

        shared
            .receive_mail("root", "/root", &BTreeSet::from([id]))
            .await
            .expect("acknowledge mail");

        let mailbox_len = store
            .saved_state
            .lock()
            .expect("saved state")
            .as_ref()
            .and_then(|state| state["mailbox"].as_array())
            .map(Vec::len);
        assert_eq!(mailbox_len, Some(0));
    }

    #[tokio::test]
    async fn remove_root_evicts_runtime_state() {
        let shared = test_shared();
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(FailOnceStore {
            fail_next_save: AtomicBool::new(false),
            saved_state: StdMutex::new(None),
        });
        shared
            .initialize(test_context(checkpoints, Arc::new(|_| {})))
            .await
            .expect("initialize runtime");

        shared.remove_root("root").await;

        assert!(shared.root("root").await.is_err());
    }

    #[tokio::test]
    async fn terminal_persist_failure_is_retried_as_a_durable_error() {
        let shared = test_shared();
        let store = Arc::new(FailOnceStore {
            fail_next_save: AtomicBool::new(false),
            saved_state: StdMutex::new(None),
        });
        let frontend_events = Arc::new(StdMutex::new(Vec::new()));
        let events = Arc::clone(&frontend_events);
        let checkpoints: Arc<dyn CheckpointStore> = store.clone();
        shared
            .initialize(test_context(
                checkpoints,
                Arc::new(move |event| events.lock().expect("frontend events").push(event)),
            ))
            .await
            .expect("initialize runtime");
        shared
            .reserve(
                "root",
                "/root/child",
                "/root",
                "child".into(),
                1,
                "test".into(),
            )
            .await
            .expect("reserve child");
        store.fail_next_save.store(true, Ordering::SeqCst);

        shared
            .finished(
                "root",
                "/root/child",
                AgentStatus::Completed,
                Some("done".into()),
            )
            .await;

        let agents = shared.list("root", None).await.expect("list agents");
        let updates = shared
            .wait("root", "/root", Duration::ZERO)
            .await
            .expect("parent update");
        let durable = store
            .saved_state
            .lock()
            .expect("saved state")
            .clone()
            .expect("retried state");
        let rendered_error = frontend_events
            .lock()
            .expect("frontend events")
            .iter()
            .any(|event| {
                matches!(
                    event,
                    FrontendEvent::Render { block, .. }
                        if block.text.contains("state persistence failed")
                )
            });

        assert_eq!(
            (
                agents[0]["status"].as_str(),
                agents[0]["last_message"]
                    .as_str()
                    .is_some_and(|message| message.contains("state persistence failed")),
                durable["agents"]["/root/child"]["status"].as_str(),
                rendered_error,
                updates == vec!["/root/child".to_string()],
            ),
            (Some("errored"), true, Some("errored"), true, true)
        );
    }

    #[tokio::test]
    async fn terminal_persist_failure_notifies_after_the_retry_commits() {
        let shared = Arc::new(test_shared());
        let store = Arc::new(BlockingRetryStore {
            saves: AtomicUsize::new(0),
            retry_started: Notify::new(),
            release_retry: Notify::new(),
        });
        let checkpoints: Arc<dyn CheckpointStore> = store.clone();
        shared
            .initialize(test_context(checkpoints, Arc::new(|_| {})))
            .await
            .expect("initialize runtime");
        shared
            .reserve(
                "root",
                "/root/child",
                "/root",
                "child".into(),
                1,
                "test".into(),
            )
            .await
            .expect("reserve child");
        let before_commit = shared.changed.notified();
        tokio::pin!(before_commit);
        before_commit.as_mut().enable();
        let finishing = {
            let shared = Arc::clone(&shared);
            tokio::spawn(async move {
                shared
                    .finished(
                        "root",
                        "/root/child",
                        AgentStatus::Completed,
                        Some("done".into()),
                    )
                    .await;
            })
        };
        store.retry_started.notified().await;
        let premature =
            tokio::time::timeout(Duration::from_millis(10), before_commit.as_mut()).await;
        let agents = shared.list("root", None).await.expect("pre-commit state");
        assert!(premature.is_err() && agents[0]["status"] == "pending_init");

        store.release_retry.notify_one();
        finishing.await.expect("finish task");

        tokio::time::timeout(Duration::from_millis(100), before_commit)
            .await
            .expect("terminal commit notification");
    }

    fn test_context(
        checkpoints: Arc<dyn CheckpointStore>,
        frontend: crate::middleware::FrontendEventSink,
    ) -> RuntimeContext {
        RuntimeContext {
            checkpoints,
            session_id: "root".into(),
            model_route: "test".into(),
            session_context: Default::default(),
            metadata: Default::default(),
            frontend,
        }
    }

    fn test_shared() -> Shared {
        Shared::new(2, 2).expect("valid test limits")
    }
}
