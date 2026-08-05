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
use crate::protocol::MessageTarget;
use crate::protocol::SessionContext;
use crate::protocol::TokenUsage;

pub mod sqlite;

pub(crate) const CHECKPOINT_VERSION: u32 = 3;

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
    pub pending_input: Vec<String>,
    pub active_turn_id: Option<String>,
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
            active_turn_id: None,
            pending_tools: Vec::new(),
            pending_approval: None,
        }
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
    pub items: Vec<Value>,
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

    /// Atomically replaces the checkpoint and appends new transcript items.
    fn save<'a>(
        &'a self,
        checkpoint: &'a Checkpoint,
        transcript_delta: &'a [Value],
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
                    items: checkpoint.context,
                }],
                next_before_sequence: None,
            })
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
