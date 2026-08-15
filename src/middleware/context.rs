use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::sync::Arc;

use serde_json::Value;

use super::approximate_tokens;
use super::tools::Catalog;
use crate::backend::checkpoint::{
    Checkpoint, CheckpointStore, ContextRewriteReason, MAX_QUEUED_INPUTS,
    QueuedInput as DurableQueuedInput,
};
use crate::backend::model::ModelRouter;
use crate::protocol::{EventMsg, FrontendEvent, MessageTarget, SessionContext, TokenUsage};
use crate::{Error, Result};

/// Sends middleware-owned UI updates without depending on a concrete frontend.
pub type FrontendEventSink = Arc<dyn Fn(FrontendEvent) -> Result<()> + Send + Sync>;

/// Read-only queued input owned by the middleware receiving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedInputView<'a> {
    id: &'a str,
    text: &'a str,
}

impl<'a> QueuedInputView<'a> {
    /// Returns the identity token required by a conditional queue mutation.
    #[must_use]
    pub fn id(&self) -> &'a str {
        self.id
    }

    /// Returns the exact queued text.
    #[must_use]
    pub fn text(&self) -> &'a str {
        self.text
    }
}

/// One queued input removed by its owning middleware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedInputValue {
    id: String,
    text: String,
}

impl QueuedInputValue {
    /// Returns the identity token for this queued input.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the exact queued text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Consumes the item and returns its text.
    #[must_use]
    pub fn into_text(self) -> String {
        self.text
    }
}

/// Read-only startup snapshot containing only one middleware's queued input.
#[derive(Clone, Default)]
pub struct QueuedInputSnapshot {
    items: Vec<QueuedInputValue>,
}

impl QueuedInputSnapshot {
    /// Returns every queued item owned by this middleware, oldest first.
    pub fn views(&self) -> impl Iterator<Item = QueuedInputView<'_>> {
        self.items.iter().map(|item| QueuedInputView {
            id: &item.id,
            text: item.text(),
        })
    }

    pub(super) fn for_owner(owner: &str, items: &[DurableQueuedInput]) -> Self {
        Self {
            items: items
                .iter()
                .filter(|item| item.owner() == owner)
                .map(|item| QueuedInputValue {
                    id: item.id().into(),
                    text: item.text().into(),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct QueuedInputBaseline {
    ids_by_owner: BTreeMap<String, BTreeSet<String>>,
    total_count: usize,
}

impl QueuedInputBaseline {
    pub(crate) fn from_items(items: &[DurableQueuedInput]) -> Self {
        let mut ids_by_owner: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for item in items {
            ids_by_owner
                .entry(item.owner().to_string())
                .or_default()
                .insert(item.id().to_string());
        }
        Self {
            ids_by_owner,
            total_count: items.len(),
        }
    }
}

/// Mutable scoped view of active input retained until the next model boundary.
pub struct QueuedInputQueue<'a> {
    items: &'a mut Vec<DurableQueuedInput>,
    baseline: QueuedInputBaseline,
    owner: Option<&'static str>,
}

impl<'a> QueuedInputQueue<'a> {
    pub(crate) fn new(
        items: &'a mut Vec<DurableQueuedInput>,
        baseline: QueuedInputBaseline,
    ) -> Self {
        Self {
            items,
            baseline,
            owner: None,
        }
    }

    pub(super) fn scope(&mut self, owner: &'static str) {
        self.owner = Some(owner);
    }

    fn owner(&self) -> Result<&'static str> {
        self.owner
            .ok_or_else(|| Error::Config("queued input is not scoped to a middleware".into()))
    }

    /// Returns the total queue size, including input being drained concurrently.
    #[must_use]
    pub fn count(&self) -> usize {
        let Some(owner) = self.owner else {
            return 0;
        };
        self.baseline
            .ids_by_owner
            .get(owner)
            .map_or(0, BTreeSet::len)
            .saturating_add(
                self.items
                    .iter()
                    .filter(|item| item.owner() == owner)
                    .count(),
            )
    }

    /// Returns the newest input available to this context.
    #[must_use]
    pub fn latest(&self) -> Option<QueuedInputView<'_>> {
        let owner = self.owner?;
        self.items
            .iter()
            .rev()
            .find(|item| item.owner() == owner)
            .map(|item| QueuedInputView {
                id: item.id(),
                text: item.text(),
            })
    }

    /// Appends one correlated active input, or returns `false` when it is full or duplicated.
    pub fn enqueue(&mut self, id: &str, text: &str) -> Result<bool> {
        let owner = self.owner()?;
        let item = DurableQueuedInput::new(owner, id, text)?;
        if self.baseline.total_count.saturating_add(self.items.len()) >= MAX_QUEUED_INPUTS {
            return Ok(false);
        }
        if self
            .baseline
            .ids_by_owner
            .get(owner)
            .is_some_and(|ids| ids.contains(id))
            || self
                .items
                .iter()
                .any(|item| item.owner() == owner && item.id() == id)
        {
            return Ok(false);
        }
        self.items.push(item);
        Ok(true)
    }

    /// Removes the item with the matching identity.
    pub fn take(&mut self, id: &str) -> Result<Option<QueuedInputValue>> {
        let owner = self.owner()?;
        let Some(index) = self
            .items
            .iter()
            .position(|item| item.owner() == owner && item.id() == id)
        else {
            return Ok(None);
        };
        let (id, text) = self.items.remove(index).into_id_and_text();
        Ok(Some(QueuedInputValue { id, text }))
    }

    /// Atomically replaces one owned item while preserving its queue position.
    pub fn replace(&mut self, id: &str, replacement_id: &str, text: &str) -> Result<bool> {
        let owner = self.owner()?;
        let Some(index) = self
            .items
            .iter()
            .position(|item| item.owner() == owner && item.id() == id)
        else {
            return Ok(false);
        };
        let replacement = DurableQueuedInput::new(owner, replacement_id, text)?;
        if self
            .baseline
            .ids_by_owner
            .get(owner)
            .is_some_and(|ids| ids.contains(replacement_id))
            || self.items.iter().enumerate().any(|(candidate, item)| {
                candidate != index && item.owner() == owner && item.id() == replacement_id
            })
        {
            return Ok(false);
        }
        self.items[index] = replacement;
        Ok(true)
    }

    /// Removes and returns every item owned by this middleware.
    pub fn drain(&mut self) -> Vec<QueuedInputValue> {
        let Some(owner) = self.owner else {
            return Vec::new();
        };
        self.items
            .extract_if(.., |item| item.owner() == owner)
            .map(|item| {
                let (id, text) = item.into_id_and_text();
                QueuedInputValue { id, text }
            })
            .collect()
    }
}

/// Durable runtime identity exposed while middleware is initialized.
#[derive(Clone)]
pub struct RuntimeContext {
    pub checkpoints: Arc<dyn CheckpointStore>,
    pub session_id: String,
    pub model_route: String,
    pub session_context: SessionContext,
    pub metadata: BTreeMap<String, Value>,
    pub queued_input: QueuedInputSnapshot,
    pub frontend: FrontendEventSink,
}

/// Mutable state exposed immediately before a model request.
pub struct ModelContext<'a> {
    pub model: &'a ModelRouter,
    pub provider: &'a str,
    pub session_id: &'a str,
    pub session_context: &'a SessionContext,
    pub metadata: &'a BTreeMap<String, Value>,
    pub turn_id: &'a str,
    pub model_step: usize,
    pub context_window: i64,
    pub instructions: &'a str,
    pub(crate) checkpoint_sequence: u64,
    pub(crate) request_input: &'a mut Vec<Value>,
    pub(crate) durable_input: &'a mut Vec<Value>,
    pub(crate) transcript_delta: &'a mut Vec<Value>,
    pub(crate) context_epoch: &'a mut u64,
    pub(crate) compaction_count: &'a mut u64,
    pub(crate) rewrite_reasons: &'a mut Vec<ContextRewriteReason>,
    pub queued_input: QueuedInputQueue<'a>,
    pub last_usage: Option<&'a TokenUsage>,
    pub tools: &'a Catalog,
    pub events: &'a mut Vec<EventMsg>,
    pub usage: &'a mut Vec<TokenUsage>,
    /// Set when this hook changes durable checkpoint state.
    pub checkpoint_changed: &'a mut bool,
}

impl ModelContext<'_> {
    /// Returns durable provider-neutral model context.
    #[must_use]
    pub fn input(&self) -> &[Value] {
        self.durable_input
    }

    /// Returns the request input including earlier request-only middleware additions.
    #[must_use]
    pub fn request_input(&self) -> &[Value] {
        self.request_input
    }

    /// Replaces active model context and advances its rewrite epoch once per boundary.
    pub fn rewrite_input(&mut self, reason: ContextRewriteReason, input: Vec<Value>) -> Result<()> {
        if *self.durable_input == input {
            return Ok(());
        }
        if self.rewrite_reasons.is_empty() {
            *self.context_epoch = self
                .context_epoch
                .checked_add(1)
                .ok_or_else(|| Error::Checkpoint("context rewrite epoch overflow".into()))?;
        }
        if !self.rewrite_reasons.contains(&reason) {
            self.rewrite_reasons.push(reason);
        }
        self.durable_input.clone_from(&input);
        *self.request_input = input;
        *self.checkpoint_changed = true;
        Ok(())
    }

    /// Replaces only the input sent by the next model request.
    pub fn replace_request_input(&mut self, input: Vec<Value>) {
        *self.request_input = input;
    }

    /// Appends a durable replay item without adding it to provider context.
    pub(crate) fn record_transcript_item(&mut self, item: Value) {
        self.transcript_delta.push(item);
        *self.checkpoint_changed = true;
    }

    /// Appends durable provider context without adding synthetic replay history.
    pub fn append_model_input(&mut self, item: Value) {
        self.request_input.push(item.clone());
        self.durable_input.push(item);
        *self.checkpoint_changed = true;
    }

    /// Appends durable input to model context and its transcript journal.
    pub fn push_input(&mut self, item: Value) -> Result<MessageTarget> {
        self.request_input.push(item.clone());
        self.durable_input.push(item.clone());
        self.transcript_delta.push(item);
        *self.checkpoint_changed = true;
        provisional_message_target(self.checkpoint_sequence, self.transcript_delta.len())
    }

    /// Estimates serialized model input at four bytes per token.
    #[must_use]
    pub fn estimated_input_tokens(&self) -> i64 {
        let mut bytes = ByteCounter::default();
        if serde_json::to_writer(&mut bytes, self.durable_input).is_err() {
            return i64::MAX;
        }
        i64::try_from(approximate_tokens(bytes.0)).unwrap_or(i64::MAX)
    }
}

pub(super) fn provisional_message_target(
    checkpoint_sequence: u64,
    batch_item_count: usize,
) -> Result<MessageTarget> {
    Ok(MessageTarget {
        checkpoint_sequence: checkpoint_sequence
            .checked_add(1)
            .ok_or_else(|| Error::Checkpoint("checkpoint sequence overflow".into()))?,
        batch_item_count,
    })
}

#[derive(Default)]
struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Mutable turn state exposed to the middleware owning an active operation.
pub struct ActiveSubmissionContext<'a> {
    pub submission_id: &'a str,
    pub operation: &'a str,
    pub active_turn_id: &'a str,
    pub target_turn_id: &'a str,
    pub text: &'a str,
    pub queued_input: QueuedInputQueue<'a>,
    pub events: &'a mut Vec<EventMsg>,
}

/// Mutable turn state exposed to a capability command that can run immediately.
pub struct ActiveCommandContext<'a> {
    pub submission_id: &'a str,
    pub session_id: &'a str,
    pub metadata: &'a BTreeMap<String, Value>,
    pub active_turn_id: &'a str,
    pub command: &'a str,
    pub arguments: &'a str,
    pub input: Option<&'a str>,
    pub target: Option<MessageTarget>,
    pub queued_input: QueuedInputQueue<'a>,
    pub events: &'a mut Vec<EventMsg>,
}

/// Result of a middleware-owned active operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveSubmissionResult {
    Accepted,
    /// The operation completed without changing durable turn state; publish its events now.
    Handled,
    Rejected(String),
}

/// State exposed when the loop finishes or aborts a turn.
pub struct TurnEndContext<'a> {
    pub session_id: &'a str,
    pub turn_id: &'a str,
    pub(crate) queued_input: &'a [DurableQueuedInput],
    pub(crate) owner: Option<&'static str>,
    pub events: &'a mut Vec<EventMsg>,
}

impl TurnEndContext<'_> {
    /// Returns queued input still pending for this middleware, oldest first.
    pub fn queued_input(&self) -> impl Iterator<Item = QueuedInputView<'_>> {
        let owner = self.owner;
        self.queued_input
            .iter()
            .filter(move |item| owner.is_some_and(|owner| item.owner() == owner))
            .map(|item| QueuedInputView {
                id: item.id(),
                text: item.text(),
            })
    }
}

/// Durable identity exposed when one agent runtime stops.
#[derive(Clone)]
pub struct SessionEndContext {
    pub session_id: String,
    pub metadata: BTreeMap<String, Value>,
}

/// State available to a middleware-owned frontend command.
pub struct MiddlewareCommandContext<'a> {
    pub command: &'a str,
    pub arguments: &'a str,
    pub input: Option<&'a str>,
    pub target: Option<MessageTarget>,
    pub session_id: &'a str,
    pub session_context: &'a SessionContext,
    pub checkpoint: &'a Checkpoint,
    pub checkpoints: Arc<dyn CheckpointStore>,
}
