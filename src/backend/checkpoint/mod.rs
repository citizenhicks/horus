//! Durable agent checkpoints.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::backend::model::ToolCall;
use crate::backend::sandbox::NetworkAccess;
use crate::protocol::MAX_CAPABILITY_INPUT_BYTES;
use crate::protocol::MessageTarget;
use crate::protocol::SessionContext;
use crate::protocol::TokenUsage;

pub mod sqlite;

pub(crate) const CHECKPOINT_VERSION: u32 = 5;
pub(crate) const MAX_QUEUED_INPUTS: usize = 1_024;
const MAX_QUEUED_OWNER_BYTES: usize = 256;
const MAX_QUEUED_ID_BYTES: usize = 4 * 1024;

/// Mutable counters for the user turn currently running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveExecution {
    pub submission_id: String,
    pub turn_id: String,
    pub started_at_ms: i64,
    pub model_calls: u64,
    pub tool_calls: u64,
    pub failed_tool_calls: u64,
    pub usage: TokenUsage,
}

/// Terminal outcome of one user turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    Completed,
    Aborted,
    Failed,
}

/// Durable observability record for one completed user turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub session_id: String,
    pub submission_id: String,
    pub turn_id: String,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub elapsed_ms: u64,
    pub outcome: ExecutionOutcome,
    pub model_calls: u64,
    pub tool_calls: u64,
    pub failed_tool_calls: u64,
    pub usage: TokenUsage,
}

/// Aggregate execution metrics for one durable session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionStats {
    pub run_count: u64,
    pub failed_run_count: u64,
    pub aborted_run_count: u64,
    pub model_calls: u64,
    pub tool_calls: u64,
    pub failed_tool_calls: u64,
    pub elapsed_ms: u64,
    pub usage: TokenUsage,
}

impl ExecutionStats {
    pub(crate) fn checked_record(&mut self, record: &ExecutionRecord) -> Option<()> {
        let run_count = self.run_count.checked_add(1)?;
        let failed_run_count = self
            .failed_run_count
            .checked_add(u64::from(record.outcome == ExecutionOutcome::Failed))?;
        let aborted_run_count = self
            .aborted_run_count
            .checked_add(u64::from(record.outcome == ExecutionOutcome::Aborted))?;
        let model_calls = self.model_calls.checked_add(record.model_calls)?;
        let tool_calls = self.tool_calls.checked_add(record.tool_calls)?;
        let failed_tool_calls = self
            .failed_tool_calls
            .checked_add(record.failed_tool_calls)?;
        let elapsed_ms = self.elapsed_ms.checked_add(record.elapsed_ms)?;
        let mut usage = self.usage.clone();
        usage.checked_add(&record.usage)?;
        *self = Self {
            run_count,
            failed_run_count,
            aborted_run_count,
            model_calls,
            tool_calls,
            failed_tool_calls,
            elapsed_ms,
            usage,
        };
        Some(())
    }
}

/// A tool batch waiting for a frontend decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub submission_id: String,
    pub turn_id: String,
    pub request_id: String,
    pub approval_call_ids: Vec<String>,
    pub authorized_call_ids: Vec<String>,
    pub calls: Vec<ToolCall>,
    pub reason: String,
    pub network_access: NetworkAccess,
    pub decision_received: bool,
}

/// One active-turn message waiting for its next model boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedInput {
    owner: String,
    id: String,
    text: String,
}

impl QueuedInput {
    pub(crate) fn new(owner: &str, id: &str, text: &str) -> Result<Self> {
        validate_queued_input(owner, id, text).map_err(|message| Error::Config(message.into()))?;
        Ok(Self {
            owner: owner.into(),
            id: id.into(),
            text: text.into(),
        })
    }

    pub(crate) fn validate(&self) -> std::result::Result<(), &'static str> {
        validate_queued_input(&self.owner, &self.id, &self.text)
    }

    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    /// Returns the submission that owns this queued input.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the exact queued text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn into_text(self) -> String {
        self.text
    }

    pub(crate) fn into_id_and_text(self) -> (String, String) {
        (self.id, self.text)
    }
}

fn validate_queued_input(
    owner: &str,
    id: &str,
    text: &str,
) -> std::result::Result<(), &'static str> {
    if owner.trim().is_empty() || owner.len() > MAX_QUEUED_OWNER_BYTES {
        return Err("queued input owner is invalid");
    }
    if id.trim().is_empty() || id.len() > MAX_QUEUED_ID_BYTES {
        return Err("queued input ID is invalid");
    }
    if text.trim().is_empty() || text.len() > MAX_CAPABILITY_INPUT_BYTES {
        return Err("queued input text is invalid");
    }
    Ok(())
}

/// Versioned state persisted at each durable loop boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub version: u32,
    pub session_id: String,
    pub session_context: SessionContext,
    pub metadata: BTreeMap<String, Value>,
    pub catalog_visible: bool,
    pub first_user_message: Option<String>,
    pub model_route: Option<String>,
    pub sequence: u64,
    pub context: Vec<Value>,
    pub total_usage: TokenUsage,
    pub last_usage: Option<TokenUsage>,
    pub pending_input: Vec<QueuedInput>,
    pub active_execution: Option<ActiveExecution>,
    pub execution_stats: ExecutionStats,
    pub pending_tools: Vec<ToolCall>,
    pub pending_approval: Option<PendingApproval>,
}

impl Checkpoint {
    /// Creates an empty session checkpoint.
    #[must_use]
    pub fn empty(session_id: impl Into<String>) -> Self {
        Self {
            version: CHECKPOINT_VERSION,
            session_id: session_id.into(),
            session_context: SessionContext::default(),
            metadata: BTreeMap::new(),
            catalog_visible: true,
            first_user_message: None,
            model_route: None,
            sequence: 0,
            context: Vec::new(),
            total_usage: TokenUsage::default(),
            last_usage: None,
            pending_input: Vec::new(),
            active_execution: None,
            execution_stats: ExecutionStats::default(),
            pending_tools: Vec::new(),
            pending_approval: None,
        }
    }

    pub(crate) fn finish_execution(
        &mut self,
        outcome: ExecutionOutcome,
        finished_at_ms: i64,
    ) -> Result<ExecutionRecord> {
        let active = self
            .active_execution
            .as_ref()
            .ok_or_else(|| Error::Checkpoint("turn ended without an active execution".into()))?;
        let finished_at_ms = finished_at_ms.max(active.started_at_ms);
        let elapsed_ms = u64::try_from(finished_at_ms - active.started_at_ms)
            .map_err(|_| Error::Checkpoint("execution elapsed time is unsupported".into()))?;
        let record = ExecutionRecord {
            session_id: self.session_id.clone(),
            submission_id: active.submission_id.clone(),
            turn_id: active.turn_id.clone(),
            started_at_ms: active.started_at_ms,
            finished_at_ms,
            elapsed_ms,
            outcome,
            model_calls: active.model_calls,
            tool_calls: active.tool_calls,
            failed_tool_calls: active.failed_tool_calls,
            usage: active.usage.clone(),
        };
        let mut stats = self.execution_stats.clone();
        stats.checked_record(&record).ok_or_else(|| {
            Error::Checkpoint("execution statistics exceed the supported range".into())
        })?;
        self.active_execution = None;
        self.execution_stats = stats;
        Ok(record)
    }
}

/// Catalog metadata for one durable session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub session_context: SessionContext,
    pub parent_session_id: Option<String>,
    pub parent_sequence: Option<u64>,
    pub sequence: u64,
    pub catalog_visible: bool,
    pub first_user_message: Option<String>,
    pub execution_stats: ExecutionStats,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Stable key for continuing a newest-first session catalog query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCursor {
    pub updated_at: i64,
    pub sequence: u64,
    pub session_id: String,
}

/// Bounds one newest-first session catalog query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPageRequest {
    pub cursor: Option<SessionCursor>,
    pub limit: usize,
}

/// One page of durable sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPage {
    pub sessions: Vec<SessionSummary>,
    pub next_cursor: Option<SessionCursor>,
}

/// One append-only transcript delta at its durable checkpoint sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptBatch {
    pub sequence: u64,
    pub created_at: i64,
    pub items: Vec<Value>,
}

/// Bounds one newest-first execution-journal query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPageRequest {
    pub before_sequence: Option<u64>,
    pub limit: usize,
}

/// One newest-first page of terminal user-turn records.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPage {
    pub executions: Vec<ExecutionRecord>,
    pub next_before_sequence: Option<u64>,
}

/// Bounds one newest-first transcript query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptPageRequest {
    pub before_sequence: Option<u64>,
    pub max_batches: usize,
}

/// One newest-first page of transcript deltas.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptPage {
    pub batches: Vec<TranscriptBatch>,
    pub next_before_sequence: Option<u64>,
}

impl TranscriptPage {
    /// Flattens this newest-first page into chronological items with durable positions.
    #[must_use]
    pub fn into_positioned_items_chronological(self) -> Vec<(MessageTarget, Value)> {
        self.batches
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
}

/// Stores durable session checkpoints and middleware state.
pub trait CheckpointStore: Send + Sync {
    /// Loads the latest checkpoint for a session.
    fn load<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<Option<Checkpoint>>>;

    /// Atomically replaces the checkpoint, appends transcript items, and records a finished turn.
    fn save<'a>(
        &'a self,
        checkpoint: &'a Checkpoint,
        transcript_delta: &'a [Value],
        execution: Option<&'a ExecutionRecord>,
    ) -> BoxFuture<'a, Result<()>>;

    /// Lists one page of the most recently updated sessions, newest first.
    fn list_sessions_page(
        &self,
        _request: SessionPageRequest,
    ) -> BoxFuture<'_, Result<SessionPage>> {
        Box::pin(async {
            Err(Error::Checkpoint(
                "this checkpoint backend has no session catalog".into(),
            ))
        })
    }

    /// Loads one newest-first page of append-only transcript deltas.
    fn transcript_page<'a>(
        &'a self,
        session_id: &'a str,
        request: TranscriptPageRequest,
    ) -> BoxFuture<'a, Result<TranscriptPage>> {
        Box::pin(async move {
            if request.max_batches == 0 {
                return Err(Error::Checkpoint(
                    "transcript page limit must be positive".into(),
                ));
            }
            let Some(checkpoint) = self.load(session_id).await? else {
                return Ok(TranscriptPage::default());
            };
            if checkpoint.context.is_empty()
                || request
                    .before_sequence
                    .is_some_and(|before| checkpoint.sequence >= before)
            {
                return Ok(TranscriptPage::default());
            }
            Ok(TranscriptPage {
                batches: vec![TranscriptBatch {
                    sequence: checkpoint.sequence,
                    created_at: 0,
                    items: checkpoint.context,
                }],
                next_before_sequence: None,
            })
        })
    }

    /// Loads one newest-first page of terminal user-turn records.
    fn execution_page<'a>(
        &'a self,
        _session_id: &'a str,
        _request: ExecutionPageRequest,
    ) -> BoxFuture<'a, Result<ExecutionPage>> {
        Box::pin(async {
            Err(Error::Checkpoint(
                "this checkpoint backend has no execution journal".into(),
            ))
        })
    }

    /// Loads the most recently started terminal user turns across all sessions.
    fn recent_executions(&self, _limit: usize) -> BoxFuture<'_, Result<Vec<ExecutionRecord>>> {
        Box::pin(async {
            Err(Error::Checkpoint(
                "this checkpoint backend has no execution journal".into(),
            ))
        })
    }

    /// Creates a child session at an exact durable parent sequence.
    fn fork<'a>(
        &'a self,
        _parent_session_id: &'a str,
        _parent_sequence: u64,
        _checkpoint: &'a Checkpoint,
    ) -> BoxFuture<'a, Result<SessionSummary>> {
        Box::pin(async {
            Err(Error::Checkpoint(
                "this checkpoint backend cannot fork sessions".into(),
            ))
        })
    }

    /// Loads the latest opaque state owned by one middleware namespace.
    fn load_state<'a>(
        &'a self,
        scope: &'a str,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Option<Value>>>;

    /// Durably replaces opaque middleware state.
    fn save_state<'a>(
        &'a self,
        scope: &'a str,
        key: &'a str,
        value: &'a Value,
    ) -> BoxFuture<'a, Result<()>>;
}
