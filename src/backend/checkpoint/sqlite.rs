//! Durable SQLite checkpoint storage.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::Connection;
use rusqlite::OpenFlags;
use rusqlite::OptionalExtension;
use rusqlite::Transaction;
use rusqlite::TransactionBehavior;
use rusqlite::params;
use serde_json::Value;

use super::CHECKPOINT_VERSION;
use super::Checkpoint;
use super::CheckpointStore;
use super::EventPage;
use super::EventPageRequest;
use super::ExecutionPage;
use super::ExecutionPageRequest;
use super::ExecutionRecord;
use super::ExecutionStats;
use super::JournalEvent;
use super::SessionCursor;
use super::SessionPage;
use super::SessionPageRequest;
use super::SessionSummary;
use super::StreamMetrics;
use super::TimestampedEvent;
use super::TranscriptBatch;
use super::TranscriptPage;
use super::TranscriptPageRequest;
use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::protocol::AgentMessagePhase;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::FrontendEvent;
use crate::protocol::ModelStepContentPhase;
use crate::protocol::ModelStepOutcome;
use crate::protocol::SessionContext;

const SCHEMA_VERSION: i64 = 5;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const SCHEMA: &str = "
BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS middleware_state (
    scope TEXT NOT NULL,
    key TEXT NOT NULL,
    value_json TEXT NOT NULL,
    PRIMARY KEY (scope, key)
);
CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    parent_session_id TEXT REFERENCES sessions(session_id),
    parent_sequence INTEGER CHECK (parent_sequence IS NULL OR parent_sequence >= 0),
    latest_sequence INTEGER NOT NULL CHECK (latest_sequence >= 0),
    latest_event_sequence INTEGER NOT NULL DEFAULT 0 CHECK (latest_event_sequence >= 0),
    latest_checkpoint_json TEXT NOT NULL,
    session_context_json TEXT NOT NULL,
    execution_stats_json TEXT NOT NULL,
    catalog_visible INTEGER NOT NULL CHECK (catalog_visible IN (0, 1)),
    first_user_message TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    CHECK ((parent_session_id IS NULL) = (parent_sequence IS NULL))
);
CREATE TABLE IF NOT EXISTS transcript_delta (
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    items_json TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (session_id, sequence)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS execution_journal (
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    record_json TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL CHECK (started_at_ms >= 0),
    PRIMARY KEY (session_id, sequence)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS event_journal (
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    recorded_at_ms INTEGER NOT NULL CHECK (recorded_at_ms >= 0),
    event_kind TEXT NOT NULL,
    model_step_id TEXT,
    stream_phase TEXT,
    delta_bytes INTEGER CHECK (delta_bytes IS NULL OR delta_bytes >= 0),
    event_json TEXT NOT NULL,
    stream_metrics_json TEXT NOT NULL,
    PRIMARY KEY (session_id, sequence)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS sessions_recent_idx
    ON sessions(updated_at DESC, latest_sequence DESC, session_id DESC);
CREATE INDEX IF NOT EXISTS execution_journal_recent_idx
    ON execution_journal(started_at_ms DESC, session_id DESC, sequence DESC);
CREATE INDEX IF NOT EXISTS event_journal_step_idx
    ON event_journal(session_id, model_step_id, event_kind);
PRAGMA user_version = 5;
COMMIT;
";

/// Stores latest checkpoints, transcripts, and middleware state in SQLite.
pub struct SqliteCheckpoint {
    path: PathBuf,
    idle_connection: Arc<Mutex<Option<Connection>>>,
}

impl SqliteCheckpoint {
    /// Opens or creates a durable checkpoint database.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        prepare_path(&path)?;
        let connection = Connection::open(&path)?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 0
            && connection.query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_schema LIMIT 1)",
                [],
                |row| row.get::<_, bool>(0),
            )?
        {
            return Err(Error::Checkpoint(format!(
                "unversioned SQLite database is not empty; expected schema version \
                 {SCHEMA_VERSION} (start with a fresh database)"
            )));
        }
        if version != 0 && version != SCHEMA_VERSION {
            return Err(Error::Checkpoint(format!(
                "unsupported SQLite schema version {version}; expected {SCHEMA_VERSION} \
                 (start with a fresh database)"
            )));
        }
        let journal_mode: String =
            connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            return Err(Error::Checkpoint(format!(
                "SQLite could not enable WAL mode: {journal_mode}"
            )));
        }
        configure_connection(&connection)?;
        if version == 0 {
            connection.execute_batch(SCHEMA)?;
        }
        Ok(Self {
            path,
            idle_connection: Arc::new(Mutex::new(Some(connection))),
        })
    }

    async fn run<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T>
    where
        T: Send + 'static,
    {
        let path = self.path.clone();
        let idle_connection = Arc::clone(&self.idle_connection);
        tokio::task::spawn_blocking(move || {
            let cached = {
                let mut idle = idle_connection.lock().map_err(|_| {
                    Error::Checkpoint("SQLite connection cache lock poisoned".into())
                })?;
                idle.take()
            };
            let mut connection = cached.map_or_else(|| open_existing_connection(&path), Ok)?;
            let result = operation(&mut connection);
            let mut idle = idle_connection
                .lock()
                .map_err(|_| Error::Checkpoint("SQLite connection cache lock poisoned".into()))?;
            // One idle connection stays warm by design; a pool is only justified if
            // connection-open churn is ever measured.
            if idle.is_none() {
                *idle = Some(connection);
            }
            result
        })
        .await
        .map_err(|error| Error::Checkpoint(format!("SQLite worker failed: {error}")))?
    }
}

impl CheckpointStore for SqliteCheckpoint {
    fn load<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<Option<Checkpoint>>> {
        let session_id = session_id.to_string();
        Box::pin(self.run(move |connection| {
            let row = connection
                .query_row(
                    "SELECT latest_sequence, latest_checkpoint_json
                     FROM sessions WHERE session_id = ?1",
                    [&session_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            row.map(|(sequence, json)| decode_checkpoint(&session_id, sequence, &json))
                .transpose()
        }))
    }

    fn delete_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<bool>> {
        let session_id = session_id.to_string();
        Box::pin(self.run(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let session_ids = {
                let mut statement = transaction.prepare(
                    "WITH RECURSIVE session_tree(session_id) AS (
                         SELECT session_id FROM sessions WHERE session_id = ?1
                         UNION ALL
                         SELECT child.session_id
                         FROM sessions AS child
                         JOIN session_tree AS parent
                           ON child.parent_session_id = parent.session_id
                     )
                     SELECT session_id FROM session_tree",
                )?;
                statement
                    .query_map([&session_id], |row| row.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            if session_ids.is_empty() {
                return Ok(false);
            }
            for id in &session_ids {
                transaction.execute("DELETE FROM middleware_state WHERE scope = ?1", [id])?;
            }
            let deleted = transaction.execute(
                "WITH RECURSIVE session_tree(session_id) AS (
                     SELECT session_id FROM sessions WHERE session_id = ?1
                     UNION ALL
                     SELECT child.session_id
                     FROM sessions AS child
                     JOIN session_tree AS parent
                       ON child.parent_session_id = parent.session_id
                 )
                 DELETE FROM sessions
                 WHERE session_id IN (SELECT session_id FROM session_tree)",
                [&session_id],
            )?;
            if deleted != session_ids.len() {
                return Err(Error::Checkpoint(
                    "session tree changed during deletion".into(),
                ));
            }
            transaction.commit()?;
            Ok(true)
        }))
    }

    fn save<'a>(
        &'a self,
        checkpoint: &'a Checkpoint,
        transcript_delta: &'a [Value],
        execution: Option<&'a ExecutionRecord>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.save_with_events(checkpoint, transcript_delta, execution, &[])
                .await?;
            Ok(())
        })
    }

    fn save_with_events<'a>(
        &'a self,
        checkpoint: &'a Checkpoint,
        transcript_delta: &'a [Value],
        execution: Option<&'a ExecutionRecord>,
        events: &'a [TimestampedEvent],
    ) -> BoxFuture<'a, Result<Vec<JournalEvent>>> {
        let checkpoint = checkpoint.clone();
        let transcript_delta = transcript_delta.to_vec();
        let execution = execution.cloned();
        let events = events.to_vec();
        Box::pin(self.run(move |connection| {
            validate_checkpoint(&checkpoint)?;
            if let Some(execution) = &execution {
                validate_execution(&checkpoint, execution)?;
            }
            let sequence = i64::try_from(checkpoint.sequence).map_err(|_| {
                Error::Checkpoint("checkpoint sequence exceeds SQLite INTEGER".into())
            })?;
            let checkpoint_json = serde_json::to_string(&checkpoint)?;
            let session_context_json = serde_json::to_string(&checkpoint.session_context)?;
            let execution_stats_json = serde_json::to_string(&checkpoint.execution_stats)?;
            let transcript_json = (!transcript_delta.is_empty())
                .then(|| serde_json::to_string(&transcript_delta))
                .transpose()?;
            let execution_json = execution.as_ref().map(serde_json::to_string).transpose()?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            store_checkpoint(
                &transaction,
                &checkpoint,
                sequence,
                SerializedCheckpoint {
                    checkpoint: &checkpoint_json,
                    session_context: &session_context_json,
                    execution_stats: &execution_stats_json,
                    transcript: transcript_json.as_deref(),
                    execution: execution
                        .as_ref()
                        .zip(execution_json.as_deref())
                        .map(|(record, json)| (record.started_at_ms, json)),
                },
            )?;
            let records = events
                .into_iter()
                .map(|event| store_event(&transaction, &checkpoint.session_id, event))
                .collect::<Result<Vec<_>>>()?;
            transaction.commit()?;
            Ok(records)
        }))
    }

    fn append_event<'a>(
        &'a self,
        session_id: &'a str,
        recorded_at_ms: i64,
        event: &'a Event,
    ) -> BoxFuture<'a, Result<JournalEvent>> {
        let session_id = session_id.to_string();
        let event = TimestampedEvent {
            recorded_at_ms,
            event: event.clone(),
        };
        Box::pin(self.run(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let record = store_event(&transaction, &session_id, event)?;
            transaction.commit()?;
            Ok(record)
        }))
    }

    fn event_page<'a>(
        &'a self,
        session_id: &'a str,
        request: EventPageRequest,
    ) -> BoxFuture<'a, Result<EventPage>> {
        let session_id = session_id.to_string();
        Box::pin(async move {
            if request.limit == 0 {
                return Err(Error::Checkpoint(
                    "event journal page limit must be positive".into(),
                ));
            }
            let query_limit = request
                .limit
                .checked_add(1)
                .and_then(|limit| i64::try_from(limit).ok())
                .ok_or_else(|| Error::Checkpoint("event journal page limit is too large".into()))?;
            let before_sequence = request
                .before_sequence
                .map(i64::try_from)
                .transpose()
                .map_err(|_| {
                    Error::Checkpoint("event journal cursor exceeds SQLite INTEGER".into())
                })?;
            self.run(move |connection| {
                let latest_sequence = connection
                    .query_row(
                        "SELECT latest_event_sequence FROM sessions WHERE session_id = ?1",
                        [&session_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .ok_or_else(|| {
                        Error::Checkpoint("event journal session does not exist".into())
                    })?;
                let latest_sequence = u64::try_from(latest_sequence).map_err(|_| {
                    Error::Checkpoint("event journal sequence became negative".into())
                })?;
                let mut statement = connection.prepare(
                    "SELECT sequence, recorded_at_ms, event_json, stream_metrics_json
                     FROM event_journal
                     WHERE session_id = ?1
                       AND (?2 IS NULL OR sequence < ?2)
                     ORDER BY sequence DESC
                     LIMIT ?3",
                )?;
                let mut rows = statement
                    .query_map(params![session_id, before_sequence, query_limit], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let has_more = rows.len() > request.limit;
                rows.truncate(request.limit);
                let events = rows
                    .into_iter()
                    .map(|(sequence, recorded_at_ms, json, metrics_json)| {
                        Ok(JournalEvent {
                            sequence: u64::try_from(sequence).map_err(|_| {
                                Error::Checkpoint(
                                    "event journal row has a negative sequence".into(),
                                )
                            })?,
                            recorded_at_ms,
                            event: serde_json::from_str(&json)?,
                            stream_metrics: serde_json::from_str(&metrics_json)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                let next_before_sequence = has_more
                    .then(|| events.last().map(|event| event.sequence))
                    .flatten();
                Ok(EventPage {
                    latest_sequence,
                    events,
                    next_before_sequence,
                })
            })
            .await
        })
    }

    fn list_sessions_page(
        &self,
        request: SessionPageRequest,
    ) -> BoxFuture<'_, Result<SessionPage>> {
        Box::pin(async move {
            if request.limit == 0 {
                return Err(Error::Checkpoint(
                    "session page limit must be positive".into(),
                ));
            }
            let query_limit = request
                .limit
                .checked_add(1)
                .and_then(|limit| i64::try_from(limit).ok())
                .ok_or_else(|| Error::Checkpoint("session page limit is too large".into()))?;
            let (cursor_updated_at, cursor_sequence, cursor_session_id) = match request.cursor {
                Some(cursor) => (
                    Some(cursor.updated_at),
                    Some(i64::try_from(cursor.sequence).map_err(|_| {
                        Error::Checkpoint("session cursor sequence exceeds SQLite INTEGER".into())
                    })?),
                    Some(cursor.session_id),
                ),
                None => (None, None, None),
            };
            self.run(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT sessions.session_id, sessions.parent_session_id,
                            sessions.parent_sequence, sessions.latest_sequence,
                            sessions.catalog_visible, sessions.first_user_message,
                            sessions.session_context_json, sessions.execution_stats_json,
                            sessions.created_at, sessions.updated_at
                     FROM sessions
                     WHERE ?1 IS NULL
                        OR (
                            sessions.updated_at,
                            sessions.latest_sequence,
                            sessions.session_id
                        ) < (?1, ?2, ?3)
                     ORDER BY sessions.updated_at DESC, sessions.latest_sequence DESC,
                              sessions.session_id DESC
                     LIMIT ?4",
                )?;
                let mut sessions = statement
                    .query_map(
                        params![
                            cursor_updated_at,
                            cursor_sequence,
                            cursor_session_id,
                            query_limit
                        ],
                        session_row,
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?
                    .into_iter()
                    .map(summary_from_row)
                    .collect::<Result<Vec<_>>>()?;
                let has_more = sessions.len() > request.limit;
                sessions.truncate(request.limit);
                let next_cursor = has_more
                    .then(|| sessions.last().map(session_cursor))
                    .flatten();
                Ok(SessionPage {
                    sessions,
                    next_cursor,
                })
            })
            .await
        })
    }

    fn transcript_page<'a>(
        &'a self,
        session_id: &'a str,
        request: TranscriptPageRequest,
    ) -> BoxFuture<'a, Result<TranscriptPage>> {
        let session_id = session_id.to_string();
        Box::pin(async move {
            if request.max_batches == 0 {
                return Err(Error::Checkpoint(
                    "transcript page limit must be positive".into(),
                ));
            }
            let query_limit = request
                .max_batches
                .checked_add(1)
                .and_then(|limit| i64::try_from(limit).ok())
                .ok_or_else(|| Error::Checkpoint("transcript page limit is too large".into()))?;
            let before_sequence = request
                .before_sequence
                .map(i64::try_from)
                .transpose()
                .map_err(|_| {
                    Error::Checkpoint("transcript cursor exceeds SQLite INTEGER".into())
                })?;
            self.run(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT sequence, created_at, items_json
                     FROM transcript_delta
                     WHERE session_id = ?1
                       AND (?2 IS NULL OR sequence < ?2)
                     ORDER BY sequence DESC
                     LIMIT ?3",
                )?;
                let mut batches = statement
                    .query_map(params![session_id, before_sequence, query_limit], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let has_more = batches.len() > request.max_batches;
                batches.truncate(request.max_batches);
                let batches = batches
                    .into_iter()
                    .map(|(sequence, created_at, json)| {
                        Ok(TranscriptBatch {
                            sequence: u64::try_from(sequence).map_err(|_| {
                                Error::Checkpoint("transcript row has a negative sequence".into())
                            })?,
                            created_at,
                            items: serde_json::from_str(&json)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                let next_before_sequence = has_more
                    .then(|| batches.last().map(|batch| batch.sequence))
                    .flatten();
                Ok(TranscriptPage {
                    batches,
                    next_before_sequence,
                })
            })
            .await
        })
    }

    fn execution_page<'a>(
        &'a self,
        session_id: &'a str,
        request: ExecutionPageRequest,
    ) -> BoxFuture<'a, Result<ExecutionPage>> {
        let session_id = session_id.to_string();
        Box::pin(async move {
            if request.limit == 0 {
                return Err(Error::Checkpoint(
                    "execution page limit must be positive".into(),
                ));
            }
            let query_limit = request
                .limit
                .checked_add(1)
                .and_then(|limit| i64::try_from(limit).ok())
                .ok_or_else(|| Error::Checkpoint("execution page limit is too large".into()))?;
            let before_sequence = request
                .before_sequence
                .map(i64::try_from)
                .transpose()
                .map_err(|_| Error::Checkpoint("execution cursor exceeds SQLite INTEGER".into()))?;
            self.run(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT sequence, record_json
                     FROM execution_journal
                     WHERE session_id = ?1
                       AND (?2 IS NULL OR sequence < ?2)
                     ORDER BY sequence DESC
                     LIMIT ?3",
                )?;
                let mut records = statement
                    .query_map(params![session_id, before_sequence, query_limit], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let has_more = records.len() > request.limit;
                records.truncate(request.limit);
                let next_before_sequence = has_more
                    .then(|| records.last().map(|(sequence, _)| *sequence))
                    .flatten()
                    .map(u64::try_from)
                    .transpose()
                    .map_err(|_| {
                        Error::Checkpoint("execution row has a negative sequence".into())
                    })?;
                let executions = records
                    .into_iter()
                    .map(|(_, json)| decode_execution(&json))
                    .collect::<Result<Vec<_>>>()?;
                Ok(ExecutionPage {
                    executions,
                    next_before_sequence,
                })
            })
            .await
        })
    }

    fn recent_executions(&self, limit: usize) -> BoxFuture<'_, Result<Vec<ExecutionRecord>>> {
        Box::pin(async move {
            if limit == 0 {
                return Err(Error::Checkpoint(
                    "recent execution limit must be positive".into(),
                ));
            }
            let query_limit = i64::try_from(limit)
                .map_err(|_| Error::Checkpoint("recent execution limit is too large".into()))?;
            self.run(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT record_json
                     FROM execution_journal
                     ORDER BY started_at_ms DESC, session_id DESC, sequence DESC
                     LIMIT ?1",
                )?;
                statement
                    .query_map([query_limit], |row| row.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
                    .into_iter()
                    .map(|json| decode_execution(&json))
                    .collect()
            })
            .await
        })
    }

    fn fork<'a>(
        &'a self,
        parent_session_id: &'a str,
        parent_sequence: u64,
        checkpoint: &'a Checkpoint,
    ) -> BoxFuture<'a, Result<SessionSummary>> {
        let parent_session_id = parent_session_id.to_string();
        let parent_sequence = i64::try_from(parent_sequence);
        let session_id = checkpoint.session_id.clone();
        let sequence = i64::try_from(checkpoint.sequence);
        let clean = checkpoint.sequence == 0
            && checkpoint.active_execution.is_none()
            && checkpoint.pending_approval.is_none()
            && checkpoint.pending_input.is_empty();
        let validation = validate_checkpoint(checkpoint);
        let catalog_visible = checkpoint.catalog_visible;
        let checkpoint = checkpoint.clone();
        Box::pin(async move {
            if parent_session_id == session_id {
                return Err(Error::Checkpoint("a session cannot fork itself".into()));
            }
            if !clean {
                return Err(Error::Checkpoint(
                    "a fork must begin at a clean sequence-zero checkpoint".into(),
                ));
            }
            let parent_sequence = parent_sequence
                .map_err(|_| Error::Checkpoint("parent sequence exceeds SQLite INTEGER".into()))?;
            let sequence = sequence.map_err(|_| {
                Error::Checkpoint("checkpoint sequence exceeds SQLite INTEGER".into())
            })?;
            validation?;
            self.run(move |connection| {
                let checkpoint_json = serde_json::to_string(&checkpoint)?;
                let session_context_json = serde_json::to_string(&checkpoint.session_context)?;
                let execution_stats_json = serde_json::to_string(&checkpoint.execution_stats)?;
                let context_json = (!checkpoint.context.is_empty())
                    .then(|| serde_json::to_string(&checkpoint.context))
                    .transpose()?;
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let durable_parent = transaction
                    .query_row(
                        "SELECT latest_sequence FROM sessions WHERE session_id = ?1",
                        [&parent_session_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .ok_or_else(|| Error::Checkpoint("fork parent does not exist".into()))?;
                if parent_sequence > durable_parent {
                    return Err(Error::Checkpoint(
                        "fork point is newer than the parent checkpoint".into(),
                    ));
                }
                transaction.execute(
                    "INSERT INTO sessions (
                         session_id, parent_session_id, parent_sequence, latest_sequence,
                         latest_checkpoint_json, session_context_json, catalog_visible,
                         first_user_message, execution_stats_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        session_id,
                        parent_session_id,
                        parent_sequence,
                        sequence,
                        checkpoint_json,
                        session_context_json,
                        catalog_visible,
                        checkpoint.first_user_message,
                        execution_stats_json,
                    ],
                )?;
                if let Some(context_json) = context_json {
                    transaction.execute(
                        "INSERT INTO transcript_delta (session_id, sequence, items_json)
                         VALUES (?1, ?2, ?3)",
                        params![session_id, sequence, context_json,],
                    )?;
                }
                let row = transaction.query_row(
                    "SELECT sessions.session_id, sessions.parent_session_id,
                            sessions.parent_sequence, sessions.latest_sequence,
                            sessions.catalog_visible, sessions.first_user_message,
                            sessions.session_context_json, sessions.execution_stats_json,
                            sessions.created_at, sessions.updated_at
                     FROM sessions
                     WHERE sessions.session_id = ?1",
                    [&session_id],
                    session_row,
                )?;
                let summary = summary_from_row(row)?;
                transaction.commit()?;
                Ok(summary)
            })
            .await
        })
    }

    fn load_state<'a>(
        &'a self,
        scope: &'a str,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Option<Value>>> {
        let scope = scope.to_string();
        let key = key.to_string();
        Box::pin(self.run(move |connection| {
            let json = connection
                .query_row(
                    "SELECT value_json FROM middleware_state WHERE scope = ?1 AND key = ?2",
                    params![scope, key],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            Ok(json.as_deref().map(serde_json::from_str).transpose()?)
        }))
    }

    fn save_state<'a>(
        &'a self,
        scope: &'a str,
        key: &'a str,
        value: &'a Value,
    ) -> BoxFuture<'a, Result<()>> {
        let scope = scope.to_string();
        let key = key.to_string();
        let value = value.clone();
        Box::pin(self.run(move |connection| {
            let json = serde_json::to_string(&value)?;
            connection.execute(
                "INSERT INTO middleware_state (scope, key, value_json)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(scope, key) DO UPDATE SET value_json = excluded.value_json",
                params![scope, key, json],
            )?;
            Ok(())
        }))
    }
}

fn open_existing_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    configure_connection(&connection)?;
    Ok(connection)
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    Ok(())
}

struct SerializedCheckpoint<'a> {
    checkpoint: &'a str,
    session_context: &'a str,
    execution_stats: &'a str,
    transcript: Option<&'a str>,
    execution: Option<(i64, &'a str)>,
}

fn store_checkpoint(
    transaction: &Transaction<'_>,
    checkpoint: &Checkpoint,
    sequence: i64,
    serialized: SerializedCheckpoint<'_>,
) -> Result<()> {
    let changed = transaction.execute(
        "INSERT INTO sessions (
             session_id, latest_sequence, latest_checkpoint_json, session_context_json,
             execution_stats_json, catalog_visible, first_user_message
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(session_id) DO UPDATE SET
             latest_sequence = excluded.latest_sequence,
             latest_checkpoint_json = excluded.latest_checkpoint_json,
             session_context_json = excluded.session_context_json,
             execution_stats_json = excluded.execution_stats_json,
             catalog_visible = excluded.catalog_visible,
             first_user_message = COALESCE(sessions.first_user_message, excluded.first_user_message),
             updated_at = unixepoch()
         WHERE excluded.latest_sequence > sessions.latest_sequence",
        params![
            checkpoint.session_id,
            sequence,
            serialized.checkpoint,
            serialized.session_context,
            serialized.execution_stats,
            checkpoint.catalog_visible,
            checkpoint.first_user_message,
        ],
    )?;
    if changed == 0 {
        return Err(Error::Checkpoint(
            "checkpoint sequence did not advance".into(),
        ));
    }
    if let Some(transcript_json) = serialized.transcript {
        transaction.execute(
            "INSERT INTO transcript_delta (session_id, sequence, items_json)
             VALUES (?1, ?2, ?3)",
            params![checkpoint.session_id, sequence, transcript_json],
        )?;
    }
    if let Some((started_at_ms, record_json)) = serialized.execution {
        transaction.execute(
            "INSERT INTO execution_journal (
                 session_id, sequence, record_json, started_at_ms
             ) VALUES (?1, ?2, ?3, ?4)",
            params![checkpoint.session_id, sequence, record_json, started_at_ms,],
        )?;
    }
    Ok(())
}

type SessionRow = (
    String,
    Option<String>,
    Option<i64>,
    i64,
    bool,
    Option<String>,
    String,
    String,
    i64,
    i64,
);

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

fn summary_from_row(row: SessionRow) -> Result<SessionSummary> {
    let session_context: SessionContext = serde_json::from_str(&row.6)?;
    let execution_stats: ExecutionStats = serde_json::from_str(&row.7)?;
    Ok(SessionSummary {
        session_id: row.0,
        session_context,
        parent_session_id: row.1,
        parent_sequence: row
            .2
            .map(u64::try_from)
            .transpose()
            .map_err(|_| Error::Checkpoint("session has a negative parent sequence".into()))?,
        sequence: u64::try_from(row.3)
            .map_err(|_| Error::Checkpoint("session has a negative sequence".into()))?,
        catalog_visible: row.4,
        first_user_message: row.5,
        execution_stats,
        created_at: row.8,
        updated_at: row.9,
    })
}

fn session_cursor(session: &SessionSummary) -> SessionCursor {
    SessionCursor {
        updated_at: session.updated_at,
        sequence: session.sequence,
        session_id: session.session_id.clone(),
    }
}

fn decode_checkpoint(session_id: &str, sequence: i64, json: &str) -> Result<Checkpoint> {
    let checkpoint: Checkpoint = serde_json::from_str(json)?;
    let sequence = u64::try_from(sequence)
        .map_err(|_| Error::Checkpoint("checkpoint row has a negative sequence".into()))?;
    validate_checkpoint(&checkpoint)?;
    if checkpoint.session_id != session_id || checkpoint.sequence != sequence {
        return Err(Error::Checkpoint(
            "checkpoint row does not match its index".into(),
        ));
    }
    Ok(checkpoint)
}

fn decode_execution(json: &str) -> Result<ExecutionRecord> {
    let execution: ExecutionRecord = serde_json::from_str(json)?;
    validate_execution_record(&execution)?;
    Ok(execution)
}

fn store_event(
    transaction: &Transaction<'_>,
    session_id: &str,
    timestamped: TimestampedEvent,
) -> Result<JournalEvent> {
    let TimestampedEvent {
        recorded_at_ms,
        event,
    } = timestamped;
    if recorded_at_ms < 0 {
        return Err(Error::Checkpoint(
            "event journal timestamp cannot be negative".into(),
        ));
    }
    let has_authoritative_snapshot = matches!(
        &event.msg,
        EventMsg::ModelStepCompleted(step)
            if matches!(&step.outcome, ModelStepOutcome::Completed { .. })
    );
    let discard_after_delivery = is_transient_event(&event.msg);
    let index = event_index(&event.msg)?;
    let event_json = serde_json::to_string(&event)?;
    let latest = transaction
        .query_row(
            "SELECT latest_event_sequence FROM sessions WHERE session_id = ?1",
            [session_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or_else(|| Error::Checkpoint("event journal session does not exist".into()))?;
    let sequence = latest
        .checked_add(1)
        .ok_or_else(|| Error::Checkpoint("event journal sequence overflow".into()))?;
    let stream_metrics = if index.kind == "model_step_completed" {
        index
            .model_step_id
            .map(|model_step_id| load_stream_metrics(transaction, session_id, model_step_id))
            .transpose()?
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let stream_metrics_json = serde_json::to_string(&stream_metrics)?;
    transaction.execute(
        "INSERT INTO event_journal (
             session_id, sequence, recorded_at_ms, event_kind, model_step_id,
             stream_phase, delta_bytes, event_json, stream_metrics_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            session_id,
            sequence,
            recorded_at_ms,
            index.kind,
            index.model_step_id,
            index.stream_phase.map(stream_phase_name),
            index.delta_bytes,
            event_json,
            stream_metrics_json,
        ],
    )?;
    transaction.execute(
        "UPDATE sessions SET latest_event_sequence = ?2 WHERE session_id = ?1",
        params![session_id, sequence],
    )?;
    if has_authoritative_snapshot && let Some(model_step_id) = index.model_step_id {
        transaction.execute(
            "DELETE FROM event_journal
             WHERE session_id = ?1 AND model_step_id = ?2
               AND event_kind IN (
                   'agent_message_content_delta',
                   'agent_reasoning_content_delta'
               )",
            params![session_id, model_step_id],
        )?;
    } else if index.kind == "token_count" {
        transaction.execute(
            "DELETE FROM event_journal
             WHERE session_id = ?1 AND event_kind = 'token_count' AND sequence < ?2",
            params![session_id, sequence],
        )?;
    }
    if discard_after_delivery {
        transaction.execute(
            "DELETE FROM event_journal WHERE session_id = ?1 AND sequence = ?2",
            params![session_id, sequence],
        )?;
    }
    Ok(JournalEvent {
        sequence: u64::try_from(sequence)
            .map_err(|_| Error::Checkpoint("event journal sequence became negative".into()))?,
        recorded_at_ms,
        event,
        stream_metrics,
    })
}

struct EventIndex<'a> {
    kind: &'static str,
    model_step_id: Option<&'a str>,
    stream_phase: Option<ModelStepContentPhase>,
    delta_bytes: Option<i64>,
}

fn event_index(event: &EventMsg) -> Result<EventIndex<'_>> {
    let plain = |kind| EventIndex {
        kind,
        model_step_id: None,
        stream_phase: None,
        delta_bytes: None,
    };
    let step = |kind, model_step_id| EventIndex {
        kind,
        model_step_id: Some(model_step_id),
        stream_phase: None,
        delta_bytes: None,
    };
    Ok(match event {
        EventMsg::Error(_) => plain("error"),
        EventMsg::Warning(_) => plain("warning"),
        EventMsg::SessionConfigured(_) => plain("session_configured"),
        EventMsg::TurnStarted(_) => plain("task_started"),
        EventMsg::TurnComplete(_) => plain("task_complete"),
        EventMsg::TurnAborted(_) => plain("turn_aborted"),
        EventMsg::UserMessage(_) => plain("user_message"),
        EventMsg::AgentMessage(message) => step("agent_message", &message.model_step_id),
        EventMsg::AgentMessageContentDelta(delta) => EventIndex {
            kind: "agent_message_content_delta",
            model_step_id: Some(&delta.model_step_id),
            stream_phase: Some(match delta.phase {
                AgentMessagePhase::Commentary => ModelStepContentPhase::Commentary,
                AgentMessagePhase::FinalAnswer => ModelStepContentPhase::FinalAnswer,
            }),
            delta_bytes: Some(i64::try_from(delta.delta.len()).map_err(|_| {
                Error::Checkpoint("stream delta length exceeds SQLite INTEGER".into())
            })?),
        },
        EventMsg::AgentReasoningContentDelta(delta) => EventIndex {
            kind: "agent_reasoning_content_delta",
            model_step_id: Some(&delta.model_step_id),
            stream_phase: Some(ModelStepContentPhase::Reasoning),
            delta_bytes: Some(i64::try_from(delta.delta.len()).map_err(|_| {
                Error::Checkpoint("stream delta length exceeds SQLite INTEGER".into())
            })?),
        },
        EventMsg::ModelStepStarted(model_step) => {
            step("model_step_started", &model_step.model_step_id)
        }
        EventMsg::ModelStepCompleted(model_step) => {
            step("model_step_completed", &model_step.model_step_id)
        }
        EventMsg::SessionHistory(_) => plain("session_history"),
        EventMsg::ModelChanged(_) => plain("model_changed"),
        EventMsg::SessionResumeRequested(_) => plain("session_resume_requested"),
        EventMsg::ToolCallBegin(_) => plain("tool_call_begin"),
        EventMsg::ToolCallEnd(_) => plain("tool_call_end"),
        EventMsg::ExecApprovalRequest(_) => plain("exec_approval_request"),
        EventMsg::TokenCount(_) => plain("token_count"),
        EventMsg::ContextCompacted => plain("context_compacted"),
        EventMsg::WebSearchBegin(search) => step("web_search_begin", &search.model_step_id),
        EventMsg::WebSearchEnd(search) => step("web_search_end", &search.model_step_id),
        EventMsg::Frontend(_) => plain("frontend"),
    })
}

fn is_transient_event(event: &EventMsg) -> bool {
    matches!(
        event,
        EventMsg::SessionHistory(_)
            | EventMsg::SessionResumeRequested(_)
            | EventMsg::Frontend(
                FrontendEvent::Picker { .. }
                    | FrontendEvent::Preview { .. }
                    | FrontendEvent::Widget { .. }
                    | FrontendEvent::RemoveWidget { .. }
            )
    )
}

fn stream_phase_name(phase: ModelStepContentPhase) -> &'static str {
    match phase {
        ModelStepContentPhase::Reasoning => "reasoning",
        ModelStepContentPhase::Commentary => "commentary",
        ModelStepContentPhase::FinalAnswer => "final_answer",
    }
}

#[derive(Default)]
struct StreamMetricAccumulator {
    first_delta_at_ms: Option<i64>,
    last_delta_at_ms: Option<i64>,
    chunk_count: u64,
    utf8_bytes: u64,
    longest_gap_ms: u64,
}

impl StreamMetricAccumulator {
    fn observe(&mut self, recorded_at_ms: i64, bytes: i64) -> Result<()> {
        let bytes = u64::try_from(bytes)
            .map_err(|_| Error::Checkpoint("stream delta has a negative length".into()))?;
        if let Some(previous) = self.last_delta_at_ms {
            let recorded_at_ms = recorded_at_ms.max(previous);
            let gap = recorded_at_ms - previous;
            self.longest_gap_ms =
                self.longest_gap_ms
                    .max(u64::try_from(gap).map_err(|_| {
                        Error::Checkpoint("stream delta gap is unsupported".into())
                    })?);
            self.last_delta_at_ms = Some(recorded_at_ms);
        } else {
            self.first_delta_at_ms = Some(recorded_at_ms);
            self.last_delta_at_ms = Some(recorded_at_ms);
        }
        self.chunk_count = self
            .chunk_count
            .checked_add(1)
            .ok_or_else(|| Error::Checkpoint("stream chunk count overflow".into()))?;
        self.utf8_bytes = self
            .utf8_bytes
            .checked_add(bytes)
            .ok_or_else(|| Error::Checkpoint("stream byte count overflow".into()))?;
        Ok(())
    }

    fn finish(self, phase: ModelStepContentPhase) -> Option<StreamMetrics> {
        Some(StreamMetrics {
            phase,
            first_delta_at_ms: self.first_delta_at_ms?,
            last_delta_at_ms: self.last_delta_at_ms?,
            chunk_count: self.chunk_count,
            utf8_bytes: self.utf8_bytes,
            longest_gap_ms: self.longest_gap_ms,
        })
    }
}

fn load_stream_metrics(
    transaction: &Transaction<'_>,
    session_id: &str,
    model_step_id: &str,
) -> Result<Vec<StreamMetrics>> {
    let mut statement = transaction.prepare(
        "SELECT stream_phase, recorded_at_ms, delta_bytes
         FROM event_journal
         WHERE session_id = ?1 AND model_step_id = ?2 AND stream_phase IS NOT NULL
         ORDER BY sequence",
    )?;
    let rows = statement
        .query_map(params![session_id, model_step_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut reasoning = StreamMetricAccumulator::default();
    let mut commentary = StreamMetricAccumulator::default();
    let mut final_answer = StreamMetricAccumulator::default();
    for (phase, recorded_at_ms, bytes) in rows {
        match phase.as_str() {
            "reasoning" => reasoning.observe(recorded_at_ms, bytes)?,
            "commentary" => commentary.observe(recorded_at_ms, bytes)?,
            "final_answer" => final_answer.observe(recorded_at_ms, bytes)?,
            _ => {
                return Err(Error::Checkpoint(
                    "event journal contains an unknown stream phase".into(),
                ));
            }
        }
    }
    Ok([
        reasoning.finish(ModelStepContentPhase::Reasoning),
        commentary.finish(ModelStepContentPhase::Commentary),
        final_answer.finish(ModelStepContentPhase::FinalAnswer),
    ]
    .into_iter()
    .flatten()
    .collect())
}

fn validate_checkpoint(checkpoint: &Checkpoint) -> Result<()> {
    if checkpoint.version != CHECKPOINT_VERSION {
        return Err(Error::Checkpoint(format!(
            "unsupported checkpoint version {}",
            checkpoint.version
        )));
    }
    if let Some(active) = &checkpoint.active_execution {
        if active.submission_id.trim().is_empty() || active.turn_id.trim().is_empty() {
            return Err(Error::Checkpoint(
                "active execution identifiers cannot be empty".into(),
            ));
        }
        if active.started_at_ms < 0 {
            return Err(Error::Checkpoint(
                "active execution start time cannot be negative".into(),
            ));
        }
        if active.failed_tool_calls > active.tool_calls {
            return Err(Error::Checkpoint(
                "active execution failed-tool count exceeds tool count".into(),
            ));
        }
    }
    if let Some(step) = &checkpoint.active_model_step {
        let execution = checkpoint
            .active_execution
            .as_ref()
            .ok_or_else(|| Error::Checkpoint("active model step has no active execution".into()))?;
        if step.model_step_id.trim().is_empty() {
            return Err(Error::Checkpoint(
                "active model-step identifier cannot be empty".into(),
            ));
        }
        if step.started_at_ms < execution.started_at_ms {
            return Err(Error::Checkpoint(
                "active model step predates its execution".into(),
            ));
        }
    }
    if checkpoint.pending_input.len() > super::MAX_QUEUED_INPUTS {
        return Err(Error::Checkpoint(
            "queued input exceeds the durable item limit".into(),
        ));
    }
    let mut pending_ids = BTreeSet::new();
    for input in &checkpoint.pending_input {
        input
            .validate()
            .map_err(|message| Error::Checkpoint(message.into()))?;
        if !pending_ids.insert((input.owner(), input.id())) {
            return Err(Error::Checkpoint(format!(
                "duplicate queued input `{}/{}`",
                input.owner(),
                input.id(),
            )));
        }
    }
    Ok(())
}

fn validate_execution(checkpoint: &Checkpoint, execution: &ExecutionRecord) -> Result<()> {
    if checkpoint.active_execution.is_some() {
        return Err(Error::Checkpoint(
            "a terminal execution requires an idle checkpoint".into(),
        ));
    }
    if execution.session_id != checkpoint.session_id {
        return Err(Error::Checkpoint(
            "execution record does not match its checkpoint".into(),
        ));
    }
    validate_execution_record(execution)
}

fn validate_execution_record(execution: &ExecutionRecord) -> Result<()> {
    if execution.session_id.trim().is_empty()
        || execution.submission_id.trim().is_empty()
        || execution.turn_id.trim().is_empty()
    {
        return Err(Error::Checkpoint(
            "execution record identifiers cannot be empty".into(),
        ));
    }
    if execution.started_at_ms < 0 || execution.finished_at_ms < execution.started_at_ms {
        return Err(Error::Checkpoint(
            "execution record has invalid timestamps".into(),
        ));
    }
    let elapsed_ms = u64::try_from(execution.finished_at_ms - execution.started_at_ms)
        .map_err(|_| Error::Checkpoint("execution elapsed time is unsupported".into()))?;
    if execution.elapsed_ms != elapsed_ms {
        return Err(Error::Checkpoint(
            "execution elapsed time does not match its timestamps".into(),
        ));
    }
    if execution.failed_tool_calls > execution.tool_calls {
        return Err(Error::Checkpoint(
            "execution failed-tool count exceeds tool count".into(),
        ));
    }
    Ok(())
}

fn prepare_path(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::fs::PermissionsExt;

        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use serde_json::json;
    use tokio::sync::oneshot;
    use tokio::time::timeout;

    use super::*;
    use crate::backend::checkpoint::ExecutionOutcome;

    fn execution(session_id: &str, turn: u64) -> ExecutionRecord {
        let started_at_ms = i64::try_from(turn * 100).expect("execution start");
        ExecutionRecord {
            session_id: session_id.into(),
            submission_id: format!("submission-{turn}"),
            turn_id: format!("turn-{turn}"),
            started_at_ms,
            finished_at_ms: started_at_ms + 25,
            elapsed_ms: 25,
            outcome: ExecutionOutcome::Completed,
            model_calls: 1,
            tool_calls: turn,
            failed_tool_calls: 0,
            usage: crate::protocol::TokenUsage {
                total_tokens: 1,
                ..crate::protocol::TokenUsage::default()
            },
        }
    }

    #[test]
    fn stream_metrics_tolerate_wall_clock_regression() {
        let mut metrics = StreamMetricAccumulator::default();
        metrics.observe(20, 2).expect("first chunk");
        metrics.observe(10, 3).expect("regressed clock chunk");

        assert_eq!(
            metrics.finish(ModelStepContentPhase::Reasoning),
            Some(StreamMetrics {
                phase: ModelStepContentPhase::Reasoning,
                first_delta_at_ms: 20,
                last_delta_at_ms: 20,
                chunk_count: 2,
                utf8_bytes: 5,
                longest_gap_ms: 0,
            })
        );
    }

    #[test]
    fn open_rejects_a_nonempty_unversioned_database() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let path = workspace.path().join("checkpoints.sqlite3");
        let connection = Connection::open(&path).expect("create unversioned database");
        connection
            .execute("CREATE TABLE legacy_state (value TEXT)", [])
            .expect("create legacy schema");
        drop(connection);

        let error = SqliteCheckpoint::new(path)
            .err()
            .expect("nonempty unversioned database must fail");

        assert_eq!(
            error.to_string(),
            "checkpoint error: unversioned SQLite database is not empty; expected schema version \
             5 (start with a fresh database)"
        );
    }

    #[tokio::test]
    async fn load_completes_while_another_connection_holds_a_write_transaction() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = Arc::new(
            SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
                .expect("open checkpoint database"),
        );
        let checkpoint = Checkpoint::empty("session");
        store
            .save(&checkpoint, &[], None)
            .await
            .expect("seed checkpoint");
        let (ready_tx, ready_rx) = oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer = tokio::spawn({
            let store = Arc::clone(&store);
            async move {
                store
                    .run(move |connection| {
                        let transaction =
                            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                        ready_tx.send(()).expect("signal held transaction");
                        release_rx.recv().expect("release held transaction");
                        transaction.commit()?;
                        Ok(())
                    })
                    .await
            }
        });

        ready_rx.await.expect("wait for held transaction");
        let loaded = timeout(Duration::from_secs(1), store.load("session")).await;
        release_tx.send(()).expect("release held transaction");
        writer
            .await
            .expect("join held transaction")
            .expect("commit held transaction");

        assert_eq!(
            loaded
                .expect("reader blocked behind writer")
                .expect("load checkpoint"),
            Some(checkpoint)
        );
    }

    #[tokio::test]
    async fn save_rejects_a_nonadvancing_sequence() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        let mut checkpoint = Checkpoint::empty("session");
        checkpoint.sequence = 2;

        store
            .save(&checkpoint, &[], None)
            .await
            .expect("initial save");

        assert!(store.save(&checkpoint, &[], None).await.is_err());
        let mut older = checkpoint.clone();
        older.sequence = 1;
        assert!(store.save(&older, &[], None).await.is_err());
        assert_eq!(
            store.load("session").await.expect("load checkpoint"),
            Some(checkpoint)
        );
    }

    #[tokio::test]
    async fn session_context_round_trips_through_save_catalog_and_fork() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        let context = crate::protocol::SessionContext {
            workspace_id: Some("workspace-1".into()),
            workspace_label: Some("Project One".into()),
            origin_label: Some("cron".into()),
            ..crate::protocol::SessionContext::default()
        };
        let mut parent = Checkpoint::empty("parent");
        parent.session_context.clone_from(&context);
        store.save(&parent, &[], None).await.expect("save parent");
        let mut child = Checkpoint::empty("child");
        child.session_context.clone_from(&context);

        let fork = store
            .fork(&parent.session_id, parent.sequence, &child)
            .await
            .expect("fork session");
        let page = store
            .list_sessions_page(SessionPageRequest {
                cursor: None,
                limit: 10,
            })
            .await
            .expect("list sessions");

        assert_eq!(
            (
                store
                    .load(&parent.session_id)
                    .await
                    .expect("load parent")
                    .expect("parent checkpoint")
                    .session_context,
                fork.session_context,
                page.sessions
                    .iter()
                    .map(|session| &session.session_context)
                    .collect::<Vec<_>>(),
            ),
            (context.clone(), context.clone(), vec![&context, &context])
        );
    }

    #[tokio::test]
    async fn delete_session_removes_the_complete_session_tree() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        let mut parent = Checkpoint::empty("parent");
        parent.sequence = 1;
        store
            .save(
                &parent,
                &[json!({"role": "user", "content": "hello"})],
                None,
            )
            .await
            .expect("save parent");
        store
            .fork("parent", 1, &Checkpoint::empty("child"))
            .await
            .expect("fork child");
        store
            .fork("child", 0, &Checkpoint::empty("grandchild"))
            .await
            .expect("fork grandchild");
        for session_id in ["parent", "child", "grandchild"] {
            store
                .append_event(
                    session_id,
                    1,
                    &Event {
                        submission_id: None,
                        msg: EventMsg::Warning(crate::protocol::WarningEvent {
                            message: session_id.into(),
                        }),
                    },
                )
                .await
                .expect("append event");
            store
                .save_state(session_id, "owned", &json!(session_id))
                .await
                .expect("save session state");
        }
        store
            .save_state("global", "retained", &json!(true))
            .await
            .expect("save global state");

        assert!(store.delete_session("parent").await.expect("delete tree"));

        let counts = store
            .run(|connection| {
                let sessions =
                    connection.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
                let transcripts =
                    connection.query_row("SELECT COUNT(*) FROM transcript_delta", [], |row| {
                        row.get(0)
                    })?;
                let events =
                    connection
                        .query_row("SELECT COUNT(*) FROM event_journal", [], |row| row.get(0))?;
                let session_state = connection.query_row(
                    "SELECT COUNT(*) FROM middleware_state WHERE scope != 'global'",
                    [],
                    |row| row.get(0),
                )?;
                Ok((sessions, transcripts, events, session_state))
            })
            .await
            .expect("count remaining rows");
        assert_eq!(counts, (0_i64, 0_i64, 0_i64, 0_i64));
        assert_eq!(
            store
                .load_state("global", "retained")
                .await
                .expect("load global state"),
            Some(json!(true))
        );
        assert!(
            !store
                .delete_session("parent")
                .await
                .expect("delete absent tree")
        );
    }

    #[tokio::test]
    async fn fork_preserves_a_historical_parent_sequence() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        let mut parent = Checkpoint::empty("parent");
        store.save(&parent, &[], None).await.expect("save parent");
        for sequence in 1..=2 {
            parent.sequence = sequence;
            store
                .save(&parent, &[], None)
                .await
                .expect("advance parent");
        }

        let fork = store
            .fork("parent", 1, &Checkpoint::empty("child"))
            .await
            .expect("fork historical checkpoint");

        assert_eq!(fork.parent_sequence, Some(1));
    }

    #[tokio::test]
    async fn fork_rejects_a_future_parent_sequence() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        store
            .save(&Checkpoint::empty("parent"), &[], None)
            .await
            .expect("save parent");

        let error = store
            .fork("parent", 1, &Checkpoint::empty("child"))
            .await
            .expect_err("future fork must fail");

        assert_eq!(
            error.to_string(),
            "checkpoint error: fork point is newer than the parent checkpoint"
        );
    }

    #[tokio::test]
    async fn fork_rejects_a_missing_parent() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");

        let error = store
            .fork("missing", 0, &Checkpoint::empty("child"))
            .await
            .expect_err("missing parent must fail");

        assert_eq!(
            error.to_string(),
            "checkpoint error: fork parent does not exist"
        );
    }

    #[tokio::test]
    async fn metadata_round_trips_through_save_and_fork() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        let mut parent = Checkpoint::empty("parent");
        parent
            .metadata
            .insert("gateway.chat".into(), json!({"workspace": "/srv/project"}));
        store.save(&parent, &[], None).await.expect("save parent");
        let mut child = Checkpoint::empty("child");
        child.metadata.clone_from(&parent.metadata);

        store
            .fork(&parent.session_id, parent.sequence, &child)
            .await
            .expect("fork session");
        let loaded_parent = store
            .load("parent")
            .await
            .expect("load parent")
            .expect("parent checkpoint");
        let loaded_child = store
            .load("child")
            .await
            .expect("load child")
            .expect("child checkpoint");

        assert_eq!(
            (loaded_parent.metadata, loaded_child.metadata),
            (parent.metadata, child.metadata)
        );
    }

    #[tokio::test]
    async fn session_catalog_reads_context_without_decoding_the_checkpoint() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        let context = SessionContext {
            workspace_id: Some("workspace-1".into()),
            ..SessionContext::default()
        };
        let mut checkpoint = Checkpoint::empty("session");
        checkpoint.session_context.clone_from(&context);
        store
            .save(&checkpoint, &[], None)
            .await
            .expect("save session");
        store
            .run(|connection| {
                connection.execute(
                    "UPDATE sessions SET latest_checkpoint_json = ?1 WHERE session_id = ?2",
                    ["invalid", "session"],
                )?;
                Ok(())
            })
            .await
            .expect("replace full checkpoint payload");

        let page = store
            .list_sessions_page(SessionPageRequest {
                cursor: None,
                limit: 1,
            })
            .await
            .expect("list sessions");

        assert_eq!(page.sessions[0].session_context, context);
    }

    #[tokio::test]
    async fn session_catalog_continues_from_a_stable_cursor() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        for session_id in ["a", "b", "c"] {
            store
                .save(&Checkpoint::empty(session_id), &[], None)
                .await
                .expect("save session");
        }

        let first = store
            .list_sessions_page(SessionPageRequest {
                cursor: None,
                limit: 2,
            })
            .await
            .expect("load first page");
        let second = store
            .list_sessions_page(SessionPageRequest {
                cursor: first.next_cursor.clone(),
                limit: 2,
            })
            .await
            .expect("load second page");

        assert_eq!(
            (
                first
                    .sessions
                    .iter()
                    .map(|session| session.session_id.as_str())
                    .collect::<Vec<_>>(),
                first
                    .next_cursor
                    .as_ref()
                    .map(|cursor| cursor.session_id.as_str()),
                second
                    .sessions
                    .iter()
                    .map(|session| session.session_id.as_str())
                    .collect::<Vec<_>>(),
                second.next_cursor,
            ),
            (vec!["c", "b"], Some("b"), vec!["a"], None)
        );
    }

    #[tokio::test]
    async fn event_journal_sequences_and_pages_normalized_events() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        store
            .save(&Checkpoint::empty("session"), &[], None)
            .await
            .expect("save session");

        for (recorded_at_ms, message) in [(10, "first"), (20, "second")] {
            store
                .append_event(
                    "session",
                    recorded_at_ms,
                    &Event {
                        submission_id: None,
                        msg: EventMsg::Warning(crate::protocol::WarningEvent {
                            message: message.into(),
                        }),
                    },
                )
                .await
                .expect("append event");
        }

        let newest = store
            .event_page(
                "session",
                EventPageRequest {
                    before_sequence: None,
                    limit: 1,
                },
            )
            .await
            .expect("newest event");
        let older = store
            .event_page(
                "session",
                EventPageRequest {
                    before_sequence: newest.next_before_sequence,
                    limit: 1,
                },
            )
            .await
            .expect("older event");

        assert_eq!(newest.events[0].sequence, 2);
        assert_eq!(newest.latest_sequence, 2);
        assert_eq!(newest.events[0].recorded_at_ms, 20);
        assert_eq!(newest.next_before_sequence, Some(2));
        assert_eq!(older.events[0].sequence, 1);
        assert_eq!(older.next_before_sequence, None);
    }

    #[tokio::test]
    async fn transient_controls_advance_sequence_without_entering_history() {
        use crate::protocol::FrontendEvent;
        use crate::protocol::SessionContext;
        use crate::protocol::SessionResumeRequestedEvent;

        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        store
            .save(&Checkpoint::empty("session"), &[], None)
            .await
            .expect("save session");
        let events = [
            EventMsg::Warning(crate::protocol::WarningEvent {
                message: "durable".into(),
            }),
            EventMsg::SessionResumeRequested(SessionResumeRequestedEvent {
                session_id: "session".into(),
                context: SessionContext::default(),
            }),
            EventMsg::Frontend(FrontendEvent::Picker {
                title: "Choose".into(),
                options: Vec::new(),
            }),
            EventMsg::Frontend(FrontendEvent::Preview {
                id: "preview".into(),
                title: "Preview".into(),
                subtitle: String::new(),
                page_id: "preview:latest".into(),
                update: crate::protocol::FrontendPreviewUpdate::Replace,
                events: Vec::new(),
                next: None,
            }),
            EventMsg::Frontend(FrontendEvent::Widget {
                capability: "test".into(),
                item: crate::protocol::FrontendWidget {
                    id: "status".into(),
                    slot: crate::protocol::FrontendSlot::Header,
                    text: "Current".into(),
                    tone: crate::protocol::FrontendTone::Neutral,
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
        for (index, msg) in events.into_iter().enumerate() {
            store
                .append_event(
                    "session",
                    i64::try_from(index).expect("timestamp"),
                    &Event {
                        submission_id: None,
                        msg,
                    },
                )
                .await
                .expect("append event");
        }

        let page = store
            .event_page(
                "session",
                EventPageRequest {
                    before_sequence: None,
                    limit: 10,
                },
            )
            .await
            .expect("event page");

        assert_eq!(page.latest_sequence, 6);
        assert_eq!(
            page.events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [1]
        );
    }

    #[tokio::test]
    async fn completed_model_step_compacts_progressive_deltas() {
        use crate::protocol::AgentMessageContentDeltaEvent;
        use crate::protocol::AgentMessagePhase;
        use crate::protocol::AgentReasoningContentDeltaEvent;
        use crate::protocol::ModelStepAnnotation;
        use crate::protocol::ModelStepCompletedEvent;
        use crate::protocol::ModelStepContent;
        use crate::protocol::ModelStepContentPhase;
        use crate::protocol::ModelStepOutcome;
        use crate::protocol::ModelStepStartedEvent;

        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        store
            .save(&Checkpoint::empty("session"), &[], None)
            .await
            .expect("save session");
        let event = |msg| Event {
            submission_id: Some("submission".into()),
            msg,
        };
        let events = [
            event(EventMsg::ModelStepStarted(ModelStepStartedEvent {
                session_id: "session".into(),
                turn_id: "turn".into(),
                model_step_id: "step".into(),
                step_index: 0,
                started_at_ms: 10,
            })),
            event(EventMsg::AgentReasoningContentDelta(
                AgentReasoningContentDeltaEvent {
                    session_id: "session".into(),
                    turn_id: "turn".into(),
                    model_step_id: "step".into(),
                    delta: "Plan".into(),
                },
            )),
            event(EventMsg::AgentMessageContentDelta(
                AgentMessageContentDeltaEvent {
                    session_id: "session".into(),
                    turn_id: "turn".into(),
                    model_step_id: "step".into(),
                    delta: "Done".into(),
                    phase: AgentMessagePhase::FinalAnswer,
                },
            )),
            event(EventMsg::ModelStepCompleted(ModelStepCompletedEvent {
                session_id: "session".into(),
                turn_id: "turn".into(),
                model_step_id: "step".into(),
                step_index: 0,
                started_at_ms: 10,
                completed_at_ms: 20,
                outcome: ModelStepOutcome::Completed {
                    end_turn: true,
                    tool_call_ids: Vec::new(),
                    usage: crate::protocol::TokenUsage::default(),
                    content: vec![
                        ModelStepContent {
                            output_index: 0,
                            part_index: 0,
                            phase: ModelStepContentPhase::Reasoning,
                            text: "Plan".into(),
                            annotations: Vec::new(),
                        },
                        ModelStepContent {
                            output_index: 1,
                            part_index: 0,
                            phase: ModelStepContentPhase::FinalAnswer,
                            text: "Done".into(),
                            annotations: vec![ModelStepAnnotation::UrlCitation {
                                url: "https://example.com".into(),
                                title: "Example".into(),
                                start_index: 0,
                                end_index: 4,
                            }],
                        },
                    ],
                },
            })),
        ];
        for (index, event) in events.iter().enumerate() {
            store
                .append_event(
                    "session",
                    10 + i64::try_from(index).expect("timestamp"),
                    event,
                )
                .await
                .expect("append event");
        }

        let page = store
            .event_page(
                "session",
                EventPageRequest {
                    before_sequence: None,
                    limit: 10,
                },
            )
            .await
            .expect("event page")
            .into_chronological();

        assert_eq!(
            page.iter().map(|event| event.sequence).collect::<Vec<_>>(),
            [1, 4]
        );
        let EventMsg::ModelStepCompleted(ModelStepCompletedEvent {
            outcome: ModelStepOutcome::Completed { content, .. },
            ..
        }) = &page[1].event.msg
        else {
            panic!("expected completed model step");
        };
        assert_eq!(
            content,
            &[
                ModelStepContent {
                    output_index: 0,
                    part_index: 0,
                    phase: ModelStepContentPhase::Reasoning,
                    text: "Plan".into(),
                    annotations: Vec::new(),
                },
                ModelStepContent {
                    output_index: 1,
                    part_index: 0,
                    phase: ModelStepContentPhase::FinalAnswer,
                    text: "Done".into(),
                    annotations: vec![ModelStepAnnotation::UrlCitation {
                        url: "https://example.com".into(),
                        title: "Example".into(),
                        start_index: 0,
                        end_index: 4,
                    }],
                },
            ]
        );
        assert_eq!(
            page[1]
                .stream_metrics
                .iter()
                .map(|metrics| (metrics.phase, metrics.chunk_count, metrics.utf8_bytes))
                .collect::<Vec<_>>(),
            [
                (ModelStepContentPhase::Reasoning, 1, 4),
                (ModelStepContentPhase::FinalAnswer, 1, 4),
            ]
        );
    }

    #[tokio::test]
    async fn incomplete_model_steps_retain_progressive_deltas() {
        use crate::protocol::AgentMessageContentDeltaEvent;
        use crate::protocol::AgentMessagePhase;
        use crate::protocol::ModelStepCompletedEvent;

        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        store
            .save(&Checkpoint::empty("session"), &[], None)
            .await
            .expect("save session");
        for (model_step_id, outcome) in [
            ("failed", ModelStepOutcome::Failed),
            ("interrupted", ModelStepOutcome::Interrupted),
        ] {
            let delta = Event {
                submission_id: Some("submission".into()),
                msg: EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
                    session_id: "session".into(),
                    turn_id: "turn".into(),
                    model_step_id: model_step_id.into(),
                    delta: format!("partial {model_step_id}"),
                    phase: AgentMessagePhase::FinalAnswer,
                }),
            };
            let completed = Event {
                submission_id: Some("submission".into()),
                msg: EventMsg::ModelStepCompleted(ModelStepCompletedEvent {
                    session_id: "session".into(),
                    turn_id: "turn".into(),
                    model_step_id: model_step_id.into(),
                    step_index: 0,
                    started_at_ms: 10,
                    completed_at_ms: 20,
                    outcome,
                }),
            };
            store
                .append_event("session", 10, &delta)
                .await
                .expect("append partial delta");
            store
                .append_event("session", 20, &completed)
                .await
                .expect("append incomplete terminal event");
        }

        let page = store
            .event_page(
                "session",
                EventPageRequest {
                    before_sequence: None,
                    limit: 10,
                },
            )
            .await
            .expect("event page")
            .into_chronological();

        assert_eq!(
            page.iter().map(|event| event.sequence).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert!(matches!(
            page[0].event.msg,
            EventMsg::AgentMessageContentDelta(_)
        ));
        assert!(matches!(
            page[2].event.msg,
            EventMsg::AgentMessageContentDelta(_)
        ));
    }

    #[tokio::test]
    async fn transcript_page_bounds_batches_and_continues_backward() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        let mut checkpoint = Checkpoint::empty("session");
        store
            .save(&checkpoint, &[], None)
            .await
            .expect("save session");
        for sequence in 1..=3 {
            checkpoint.sequence = sequence;
            let item = json!({"sequence": sequence});
            store
                .save(&checkpoint, std::slice::from_ref(&item), None)
                .await
                .expect("append transcript");
        }

        let first = store
            .transcript_page(
                "session",
                TranscriptPageRequest {
                    before_sequence: None,
                    max_batches: 2,
                },
            )
            .await
            .expect("load first page");
        let second = store
            .transcript_page(
                "session",
                TranscriptPageRequest {
                    before_sequence: first.next_before_sequence,
                    max_batches: 2,
                },
            )
            .await
            .expect("load second page");

        assert_eq!(
            (
                first
                    .batches
                    .iter()
                    .map(|batch| batch.sequence)
                    .collect::<Vec<_>>(),
                first.next_before_sequence,
                second
                    .batches
                    .iter()
                    .map(|batch| batch.sequence)
                    .collect::<Vec<_>>(),
                second.next_before_sequence,
            ),
            (vec![3, 2], Some(2), vec![1], None)
        );
        assert!(first.batches.iter().all(|batch| batch.created_at > 0));
    }

    #[tokio::test]
    async fn execution_journal_pages_records_and_updates_catalog_stats() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        let mut checkpoint = Checkpoint::empty("session");
        store
            .save(&checkpoint, &[], None)
            .await
            .expect("save session");
        for turn in 1..=3 {
            let record = execution("session", turn);
            checkpoint.sequence = turn;
            checkpoint
                .execution_stats
                .checked_record(&record)
                .expect("record execution stats");
            store
                .save(&checkpoint, &[], Some(&record))
                .await
                .expect("save execution");
        }

        let first = store
            .execution_page(
                "session",
                ExecutionPageRequest {
                    before_sequence: None,
                    limit: 2,
                },
            )
            .await
            .expect("first execution page");
        let second = store
            .execution_page(
                "session",
                ExecutionPageRequest {
                    before_sequence: first.next_before_sequence,
                    limit: 2,
                },
            )
            .await
            .expect("second execution page");
        let catalog = store
            .list_sessions_page(SessionPageRequest {
                cursor: None,
                limit: 1,
            })
            .await
            .expect("session catalog");
        let recent = store.recent_executions(2).await.expect("recent executions");

        assert_eq!(
            (
                first
                    .executions
                    .iter()
                    .map(|record| record.turn_id.as_str())
                    .collect::<Vec<_>>(),
                first.next_before_sequence,
                second
                    .executions
                    .iter()
                    .map(|record| record.turn_id.as_str())
                    .collect::<Vec<_>>(),
                catalog.sessions[0].execution_stats.run_count,
                recent
                    .iter()
                    .map(|record| record.turn_id.as_str())
                    .collect::<Vec<_>>(),
            ),
            (
                vec!["turn-3", "turn-2"],
                Some(2),
                vec!["turn-1"],
                3,
                vec!["turn-3", "turn-2"],
            )
        );
    }

    #[tokio::test]
    async fn execution_insert_failure_rolls_back_checkpoint_and_transcript() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        let original = Checkpoint::empty("session");
        store
            .save(&original, &[], None)
            .await
            .expect("save session");
        store
            .run(|connection| {
                connection.execute_batch(
                    "CREATE TRIGGER reject_execution
                     BEFORE INSERT ON execution_journal
                     BEGIN
                         SELECT RAISE(ABORT, 'forced execution failure');
                     END;",
                )?;
                Ok(())
            })
            .await
            .expect("install failure trigger");
        let record = execution("session", 1);
        let mut next = original.clone();
        next.sequence = 1;
        next.execution_stats
            .checked_record(&record)
            .expect("record execution stats");

        assert!(
            store
                .save(&next, &[json!({"role": "assistant"})], Some(&record))
                .await
                .is_err()
        );
        assert_eq!(
            store.load("session").await.expect("load session"),
            Some(original)
        );
        assert!(
            store
                .transcript_page(
                    "session",
                    TranscriptPageRequest {
                        before_sequence: None,
                        max_batches: 1,
                    },
                )
                .await
                .expect("load transcript")
                .batches
                .is_empty()
        );
    }

    #[tokio::test]
    async fn event_insert_failure_rolls_back_checkpoint_and_event_batch() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        let original = Checkpoint::empty("session");
        store
            .save(&original, &[], None)
            .await
            .expect("save session");
        let mut next = original.clone();
        next.sequence = 1;
        let warning = |recorded_at_ms, message: &str| TimestampedEvent {
            recorded_at_ms,
            event: Event {
                submission_id: None,
                msg: EventMsg::Warning(crate::protocol::WarningEvent {
                    message: message.into(),
                }),
            },
        };

        let error = store
            .save_with_events(
                &next,
                &[json!({"role": "assistant"})],
                None,
                &[warning(10, "first"), warning(-1, "invalid")],
            )
            .await
            .expect_err("invalid event must roll back the transaction");
        let saved = store.load("session").await.expect("load session");
        let events = store
            .event_page(
                "session",
                EventPageRequest {
                    before_sequence: None,
                    limit: 1,
                },
            )
            .await
            .expect("load event page");

        assert!(matches!(error, Error::Checkpoint(_)));
        assert_eq!(
            (saved, events.latest_sequence, events.events),
            (Some(original), 0, Vec::new())
        );
    }
}
