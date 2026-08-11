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
use crate::backend::checkpoint::TranscriptBatch;
use crate::backend::checkpoint::TranscriptPageRequest;
use crate::middleware::RuntimeContext;
use crate::protocol::EventMsg;
use crate::protocol::FrontendBlock;
use crate::protocol::FrontendEvent;
use crate::protocol::FrontendPickerOption;
use crate::protocol::FrontendSlot;
use crate::protocol::FrontendSymbol;
use crate::protocol::FrontendTone;
use crate::protocol::FrontendWidget;
use crate::protocol::FrontendWidgetContent;
use crate::protocol::MessageTarget;
use crate::protocol::Op;
use crate::protocol::replay_events;

use super::PreviewPosition;

mod coordination;
mod monitor;

pub(super) use coordination::Followup;
pub(super) use coordination::Mail;
pub(super) use monitor::monitor_agent;

const STATE_KEY: &str = "subagents.v2";
const MAX_MAILBOX_ITEMS: usize = 256;
pub(super) const PREVIEW_TRANSCRIPT_BATCHES: usize = 50;
pub(super) const MAX_PREVIEW_PAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PREVIEW_INPUT_BYTES: usize = 7 * 1024 * 1024;
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
    spawn_context: String,
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

pub(super) struct PreviewPage {
    pub(super) subtitle: String,
    pub(super) page_id: String,
    pub(super) events: Vec<EventMsg>,
    pub(super) next: Option<PreviewPosition>,
}

pub(super) struct AgentPresentation {
    pub(super) model: String,
    pub(super) spawn_context: String,
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
                    emit_status(&root)?;
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
        }
        if !root.tree.agents.is_empty() {
            emit_status(&root)?;
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
        presentation: AgentPresentation,
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
                    model: presentation.model,
                    spawn_context: presentation.spawn_context,
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
        model: Option<String>,
    ) -> Result<()> {
        self.mutate_root(root_id, |root| {
            let entry = root
                .tree
                .agents
                .get_mut(path)
                .ok_or_else(|| Error::Unknown(format!("agent `{path}`")))?;
            if let Some(model) = model {
                entry.model = model;
            }
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
        Ok(picker_options(&root.tree))
    }

    pub(super) async fn preview(
        &self,
        root_id: &str,
        path: &str,
        position: Option<PreviewPosition>,
    ) -> Result<PreviewPage> {
        let (checkpoints, session_id, subtitle, terminal_error) = {
            let root = self.root(root_id).await?;
            let root = root.state.lock().await;
            let entry = root
                .tree
                .agents
                .get(path)
                .ok_or_else(|| Error::Unknown(format!("agent `{path}`")))?;
            (
                Arc::clone(&root.checkpoints),
                entry.session_id.clone(),
                entry.spawn_context.clone(),
                if position.is_none() && matches!(&entry.status, AgentStatus::Errored) {
                    entry
                        .last_message
                        .clone()
                        .filter(|message| !message.is_empty())
                } else {
                    None
                },
            )
        };
        let before_sequence = match position {
            None => None,
            Some(PreviewPosition::BeforeSequence { before_sequence }) => Some(before_sequence),
            Some(PreviewPosition::BeforeItem { sequence, .. }) => {
                Some(sequence.checked_add(1).ok_or_else(invalid_preview_cursor)?)
            }
        };
        let page = checkpoints
            .transcript_page(
                &session_id,
                TranscriptPageRequest {
                    before_sequence,
                    max_batches: PREVIEW_TRANSCRIPT_BATCHES + 1,
                },
            )
            .await?;
        let store_has_more = page.next_before_sequence.is_some();
        let mut batches = page.batches;
        if let Some(PreviewPosition::BeforeItem {
            sequence,
            before_item,
        }) = position
        {
            let Some(batch) = batches
                .first_mut()
                .filter(|batch| batch.sequence == sequence)
            else {
                return Err(invalid_preview_cursor());
            };
            if before_item >= batch.items.len() {
                return Err(invalid_preview_cursor());
            }
            batch.items.truncate(before_item);
        }
        let omitted_seed = position.is_none() && batches.iter().any(|batch| batch.sequence == 0);
        if omitted_seed {
            batches.retain(|batch| batch.sequence != 0);
        }
        let continuation_before_sequence = batches
            .get(PREVIEW_TRANSCRIPT_BATCHES.saturating_sub(1))
            .or_else(|| batches.last())
            .map(|batch| batch.sequence);
        let minimum_start = batches
            .get(PREVIEW_TRANSCRIPT_BATCHES..)
            .unwrap_or_default()
            .iter()
            .map(|batch| batch.items.len())
            .sum();
        let transcript = positioned_items_chronological(batches);
        let terminal_error =
            terminal_error.map(|message| EventMsg::Frontend(subagent_error_notice(message)));
        let trailing_bytes = terminal_error
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()?
            .map_or(0, |event| event.len());
        let (start, mut events) =
            preview_events(&transcript, minimum_start, &session_id, trailing_bytes)?;
        events.extend(terminal_error);
        let has_more = store_has_more || omitted_seed || start > 0;
        let next = if has_more {
            transcript
                .get(start)
                .map(|(target, _)| position_before(*target))
                .or_else(|| {
                    omitted_seed.then_some(PreviewPosition::BeforeSequence { before_sequence: 1 })
                })
                .or_else(|| {
                    continuation_before_sequence
                        .map(|before_sequence| PreviewPosition::BeforeSequence { before_sequence })
                })
        } else {
            None
        };
        Ok(PreviewPage {
            subtitle,
            page_id: position.map_or_else(|| format!("{path}:latest"), |at| at.page_id(path)),
            events,
            next,
        })
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
                let frontend = Arc::clone(&candidate.frontend);
                let status = status_event(&candidate.tree);
                *root.state.lock().await = candidate;
                frontend(status)?;
                return Ok(Stage::Changed(output));
            }
            Err(error) => error,
        };
        match on_failure {
            OnPersistFailure::Abort => Err(error),
            OnPersistFailure::CommitWithStatus => {
                let frontend = Arc::clone(&candidate.frontend);
                let status = status_event(&candidate.tree);
                *root.state.lock().await = candidate;
                if let Err(delivery) = frontend(status) {
                    return Err(Error::Stopped(format!(
                        "{error}; frontend status delivery failed: {delivery}"
                    )));
                }
                Err(error)
            }
            OnPersistFailure::RepairRetry(repair) => {
                let (retry_message, failure_event) = repair(&mut candidate, &error);
                let retry = persist(root_id, &candidate).await;
                let frontend = Arc::clone(&candidate.frontend);
                let status = status_event(&candidate.tree);
                *root.state.lock().await = candidate;
                frontend(status)?;
                frontend(failure_event)?;
                if let Err(retry_error) = retry {
                    frontend(subagent_error_notice(format!(
                        "{retry_message}: {retry_error}"
                    )))?;
                }
                Ok(Stage::Changed(output))
            }
        }
    }
}

fn positioned_items_chronological(batches: Vec<TranscriptBatch>) -> Vec<(MessageTarget, Value)> {
    batches
        .into_iter()
        .rev()
        .flat_map(|batch| {
            batch
                .items
                .into_iter()
                .enumerate()
                .map(move |(index, item)| {
                    (
                        MessageTarget {
                            checkpoint_sequence: batch.sequence,
                            batch_item_count: index + 1,
                        },
                        item,
                    )
                })
        })
        .collect()
}

fn preview_events(
    transcript: &[(MessageTarget, Value)],
    minimum_start: usize,
    session_id: &str,
    trailing_bytes: usize,
) -> Result<(usize, Vec<EventMsg>)> {
    if transcript.is_empty() || minimum_start >= transcript.len() {
        return Ok((transcript.len(), Vec::new()));
    }
    let mut input_bytes: usize = 0;
    let mut candidate = transcript.len();
    while candidate > minimum_start {
        let item = &transcript[candidate - 1].1;
        let item_bytes = serde_json::to_vec(item)?.len();
        if candidate < transcript.len()
            && input_bytes.saturating_add(item_bytes) > MAX_PREVIEW_INPUT_BYTES
        {
            break;
        }
        candidate -= 1;
        input_bytes = input_bytes.saturating_add(item_bytes);
        if input_bytes >= MAX_PREVIEW_INPUT_BYTES {
            break;
        }
    }

    let boundaries =
        crate::backend::model::tool_complete_boundaries(transcript.iter().map(|(_, item)| item));
    let previous = boundaries
        .iter()
        .copied()
        .chain(std::iter::once(0))
        .filter(|boundary| *boundary <= candidate)
        .max()
        .unwrap_or(candidate);
    let next = boundaries
        .iter()
        .copied()
        .find(|boundary| *boundary >= candidate)
        .unwrap_or(transcript.len());

    let events = replay_events(&transcript[previous..], session_id);
    if preview_page_fits(&events, trailing_bytes)? {
        return Ok((previous, events));
    }
    if next >= transcript.len() {
        return Err(Error::Checkpoint(format!(
            "one subagent transcript item group exceeds the {MAX_PREVIEW_PAGE_BYTES}-byte preview limit"
        )));
    }
    let events = replay_events(&transcript[next..], session_id);
    if events.is_empty() || preview_page_fits(&events, trailing_bytes)? {
        Ok((next, events))
    } else {
        Err(Error::Checkpoint(format!(
            "one subagent transcript item exceeds the {MAX_PREVIEW_PAGE_BYTES}-byte preview limit"
        )))
    }
}

fn preview_page_fits(events: &[EventMsg], trailing_bytes: usize) -> Result<bool> {
    let separator_bytes = usize::from(!events.is_empty() && trailing_bytes > 0);
    Ok(serde_json::to_vec(events)?
        .len()
        .saturating_add(trailing_bytes)
        .saturating_add(separator_bytes)
        <= MAX_PREVIEW_PAGE_BYTES)
}

fn subagent_error_notice(message: String) -> FrontendEvent {
    FrontendEvent::Render {
        capability: "subagents".into(),
        block: FrontendBlock {
            id: None,
            group: None,
            update: crate::protocol::FrontendBlockUpdate::Replace,
            state: crate::protocol::FrontendBlockState::Complete,
            role: crate::protocol::FrontendBlockRole::Notice,
            title: "Subagent error".into(),
            text: message,
            symbol: Some(FrontendSymbol::Agent),
            files: Vec::new(),
            format: crate::protocol::FrontendBlockFormat::PlainText,
            tone: FrontendTone::Error,
        },
    }
}

fn position_before(target: MessageTarget) -> PreviewPosition {
    if target.batch_item_count == 1 {
        PreviewPosition::BeforeSequence {
            before_sequence: target.checkpoint_sequence,
        }
    } else {
        PreviewPosition::BeforeItem {
            sequence: target.checkpoint_sequence,
            before_item: target.batch_item_count - 1,
        }
    }
}

fn invalid_preview_cursor() -> Error {
    Error::Tool("invalid subagent preview cursor".into())
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
type PersistRepair = Box<dyn FnOnce(&mut Root, &Error) -> (String, FrontendEvent) + Send>;

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
    if tree.agents.len() >= max_agents
        || tree.mailbox.len() > MAX_MAILBOX_ITEMS
        || tree.agents.values().any(|entry| {
            entry
                .last_message
                .as_ref()
                .is_some_and(|message| message.len() > MAX_MESSAGE_BYTES)
        })
    {
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
        text: tree.agents.len().to_string(),
        tone: if failed {
            FrontendTone::Error
        } else if active > 0 {
            FrontendTone::Success
        } else {
            FrontendTone::Neutral
        },
        symbol: Some(FrontendSymbol::Agent),
        icon_only: false,
        progress: None,
        content: Some(FrontendWidgetContent::Picker {
            title: "Subagents".into(),
            options: picker_options(tree),
        }),
        action: None,
    }
}

fn picker_options(tree: &Tree) -> Vec<FrontendPickerOption> {
    tree.agents
        .iter()
        .map(|(path, entry)| FrontendPickerOption {
            label: path.rsplit('/').next().unwrap_or(path).into(),
            description: entry.status.label().into(),
            detail: entry.model.clone(),
            symbol: Some(FrontendSymbol::Agent),
            shows_detail: false,
            op: Op::CapabilityCommand {
                capability: "subagents".into(),
                command: "subagents".into(),
                arguments: path.clone(),
                input: None,
                target: None,
            },
        })
        .collect()
}

fn status_event(tree: &Tree) -> FrontendEvent {
    if tree.agents.is_empty() {
        FrontendEvent::RemoveWidget {
            capability: "subagents".into(),
            id: "status".into(),
        }
    } else {
        FrontendEvent::Widget {
            capability: "subagents".into(),
            item: status_widget(tree),
        }
    }
}

fn emit_status(root: &Root) -> Result<()> {
    (root.frontend)(status_event(&root.tree))
}

async fn persist(root_id: &str, root: &Root) -> Result<()> {
    root.checkpoints
        .save_state(root_id, STATE_KEY, &serde_json::to_value(&root.tree)?)
        .await
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
    use crate::backend::checkpoint::EventPage;
    use crate::backend::checkpoint::EventPageRequest;
    use crate::backend::checkpoint::ExecutionRecord;
    use crate::backend::checkpoint::JournalEvent;
    use crate::backend::checkpoint::TimestampedEvent;
    use crate::protocol::Event;

    struct FailOnceStore {
        fail_next_save: AtomicBool,
        saved_state: StdMutex<Option<Value>>,
    }

    struct BlockingRetryStore {
        saves: AtomicUsize,
        retry_started: Notify,
        release_retry: Notify,
    }

    fn test_presentation() -> AgentPresentation {
        AgentPresentation {
            model: "test".into(),
            spawn_context: String::new(),
        }
    }

    #[test]
    fn status_widget_owns_its_picker() {
        let mut tree = Tree::default();
        tree.agents.insert(
            "/root/team/reviewer".into(),
            AgentRecord {
                parent: String::new(),
                session_id: "child-1".into(),
                depth: 1,
                model: "openai::gpt-5::high".into(),
                spawn_context: "Full context".into(),
                active_turn_id: None,
                status: AgentStatus::Running,
                last_message: None,
            },
        );

        let widget = status_widget(&tree);
        assert_eq!(widget.symbol, Some(FrontendSymbol::Agent));
        assert_eq!(widget.text, "1");
        assert!(!widget.icon_only);
        assert!(matches!(
            widget.content,
            Some(FrontendWidgetContent::Picker { title, options })
                if title == "Subagents"
                    && options.len() == 1
                    && options[0].label == "reviewer"
                    && options[0].description == "running"
                    && options[0].detail == "openai::gpt-5::high"
                    && options[0].symbol == Some(FrontendSymbol::Agent)
                    && !options[0].shows_detail
                    && matches!(
                        &options[0].op,
                        Op::CapabilityCommand { arguments, .. }
                            if arguments == "/root/team/reviewer"
                    )
        ));
    }

    #[test]
    fn persisted_tree_rejects_an_oversized_last_message() {
        let mut tree = Tree::default();
        tree.agents.insert(
            "/root/reviewer".into(),
            AgentRecord {
                parent: "/root".into(),
                session_id: "child".into(),
                depth: 1,
                model: "test".into(),
                spawn_context: String::new(),
                active_turn_id: None,
                status: AgentStatus::Errored,
                last_message: Some("x".repeat(MAX_MESSAGE_BYTES + 1)),
            },
        );

        assert!(matches!(validate_tree(&tree, 2), Err(Error::Config(_))));
    }

    #[tokio::test]
    async fn errored_subagent_preview_ends_with_its_terminal_message() {
        let workspace = tempfile::tempdir().expect("workspace");
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(
            crate::backend::checkpoint::sqlite::SqliteCheckpoint::new(
                workspace.path().join("checkpoints.sqlite3"),
            )
            .expect("checkpoint store"),
        );
        let root = Checkpoint::empty("root");
        checkpoints.save(&root, &[], None).await.expect("save root");
        let transcript = serde_json::json!({"role": "user", "content": "review this"});
        let mut child = Checkpoint::empty("child");
        child.sequence = 1;
        child.context.push(transcript.clone());
        checkpoints
            .save(&child, &[transcript], None)
            .await
            .expect("save child");
        let shared = test_shared();
        shared
            .initialize(test_context(checkpoints, Arc::new(|_| Ok(()))))
            .await
            .expect("initialize runtime");
        shared
            .reserve(
                "root",
                "/root/reviewer",
                "/root",
                "child".into(),
                1,
                test_presentation(),
            )
            .await
            .expect("reserve child");
        shared
            .finished(
                "root",
                "/root/reviewer",
                AgentStatus::Errored,
                Some("provider error: servers are currently overloaded".into()),
            )
            .await
            .expect("fail child");

        let preview = shared
            .preview("root", "/root/reviewer", None)
            .await
            .expect("preview errored child");

        assert!(matches!(
            preview.events.as_slice(),
            [
                EventMsg::UserMessage(message),
                EventMsg::Frontend(FrontendEvent::Render { capability, block }),
            ] if message.message == "review this"
                && capability == "subagents"
                && block.title == "Subagent error"
                && block.text == "provider error: servers are currently overloaded"
                && block.symbol == Some(FrontendSymbol::Agent)
                && block.tone == FrontendTone::Error
        ));
    }

    impl CheckpointStore for BlockingRetryStore {
        fn load<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, Result<Option<Checkpoint>>> {
            Box::pin(async { Ok(None) })
        }

        fn delete_session<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, Result<bool>> {
            Box::pin(async { Ok(false) })
        }

        fn save<'a>(
            &'a self,
            _checkpoint: &'a Checkpoint,
            _transcript_delta: &'a [Value],
            _execution: Option<&'a ExecutionRecord>,
        ) -> BoxFuture<'a, Result<()>> {
            Box::pin(async { Ok(()) })
        }

        fn save_with_events<'a>(
            &'a self,
            checkpoint: &'a Checkpoint,
            transcript_delta: &'a [Value],
            execution: Option<&'a ExecutionRecord>,
            events: &'a [TimestampedEvent],
        ) -> BoxFuture<'a, Result<Vec<JournalEvent>>> {
            Box::pin(async move {
                self.save(checkpoint, transcript_delta, execution).await?;
                let mut records = Vec::with_capacity(events.len());
                for event in events {
                    records.push(
                        self.append_event(
                            &checkpoint.session_id,
                            event.recorded_at_ms,
                            &event.event,
                        )
                        .await?,
                    );
                }
                Ok(records)
            })
        }

        fn append_event<'a>(
            &'a self,
            _session_id: &'a str,
            recorded_at_ms: i64,
            event: &'a Event,
        ) -> BoxFuture<'a, Result<JournalEvent>> {
            let event = event.clone();
            Box::pin(async move {
                Ok(JournalEvent {
                    sequence: 1,
                    recorded_at_ms,
                    event,
                    stream_metrics: Vec::new(),
                })
            })
        }

        fn event_page<'a>(
            &'a self,
            _session_id: &'a str,
            _request: EventPageRequest,
        ) -> BoxFuture<'a, Result<EventPage>> {
            Box::pin(async { Ok(EventPage::default()) })
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

        fn delete_session<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, Result<bool>> {
            Box::pin(async { Ok(false) })
        }

        fn save<'a>(
            &'a self,
            _checkpoint: &'a Checkpoint,
            _transcript_delta: &'a [Value],
            _execution: Option<&'a ExecutionRecord>,
        ) -> BoxFuture<'a, Result<()>> {
            Box::pin(async { Ok(()) })
        }

        fn save_with_events<'a>(
            &'a self,
            checkpoint: &'a Checkpoint,
            transcript_delta: &'a [Value],
            execution: Option<&'a ExecutionRecord>,
            events: &'a [TimestampedEvent],
        ) -> BoxFuture<'a, Result<Vec<JournalEvent>>> {
            Box::pin(async move {
                self.save(checkpoint, transcript_delta, execution).await?;
                let mut records = Vec::with_capacity(events.len());
                for event in events {
                    records.push(
                        self.append_event(
                            &checkpoint.session_id,
                            event.recorded_at_ms,
                            &event.event,
                        )
                        .await?,
                    );
                }
                Ok(records)
            })
        }

        fn append_event<'a>(
            &'a self,
            _session_id: &'a str,
            recorded_at_ms: i64,
            event: &'a Event,
        ) -> BoxFuture<'a, Result<JournalEvent>> {
            let event = event.clone();
            Box::pin(async move {
                Ok(JournalEvent {
                    sequence: 1,
                    recorded_at_ms,
                    event,
                    stream_metrics: Vec::new(),
                })
            })
        }

        fn event_page<'a>(
            &'a self,
            _session_id: &'a str,
            _request: EventPageRequest,
        ) -> BoxFuture<'a, Result<EventPage>> {
            Box::pin(async { Ok(EventPage::default()) })
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
            .initialize(test_context(checkpoints, Arc::new(|_| Ok(()))))
            .await
            .expect("initialize runtime");

        let failed = shared
            .reserve(
                "root",
                "/root/child",
                "/root",
                "child".into(),
                1,
                test_presentation(),
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
                test_presentation(),
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
                Arc::new(move |event| {
                    events.lock().expect("frontend events").push(event);
                    Ok(())
                }),
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
                test_presentation(),
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
            .initialize(test_context(checkpoints, Arc::new(|_| Ok(()))))
            .await
            .expect("initialize runtime");
        shared
            .reserve(
                "root",
                "/root/child",
                "/root",
                "child".into(),
                1,
                test_presentation(),
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
            .initialize(test_context(checkpoints, Arc::new(|_| Ok(()))))
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
                    test_presentation(),
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
                test_presentation(),
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
            .initialize(test_context(checkpoints, Arc::new(|_| Ok(()))))
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
                    test_presentation(),
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
                test_presentation(),
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
            .initialize(test_context(checkpoints, Arc::new(|_| Ok(()))))
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
            .initialize(test_context(checkpoints, Arc::new(|_| Ok(()))))
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
                Arc::new(move |event| {
                    events.lock().expect("frontend events").push(event);
                    Ok(())
                }),
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
                test_presentation(),
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
            .await
            .expect("finish child");

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
            .initialize(test_context(checkpoints, Arc::new(|_| Ok(()))))
            .await
            .expect("initialize runtime");
        shared
            .reserve(
                "root",
                "/root/child",
                "/root",
                "child".into(),
                1,
                test_presentation(),
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
                    .await
            })
        };
        store.retry_started.notified().await;
        let premature =
            tokio::time::timeout(Duration::from_millis(10), before_commit.as_mut()).await;
        let agents = shared.list("root", None).await.expect("pre-commit state");
        assert!(premature.is_err() && agents[0]["status"] == "pending_init");

        store.release_retry.notify_one();
        finishing.await.expect("finish task").expect("finish child");

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
            queued_input: crate::middleware::QueuedInputSnapshot::default(),
            frontend,
        }
    }

    fn test_shared() -> Shared {
        Shared::new(2, 2).expect("valid test limits")
    }
}
