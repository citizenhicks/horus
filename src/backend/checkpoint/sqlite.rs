//! Durable SQLite checkpoint storage.

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
use super::SessionCursor;
use super::SessionPage;
use super::SessionPageRequest;
use super::SessionSummary;
use super::TranscriptBatch;
use super::TranscriptPage;
use super::TranscriptPageRequest;
use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::protocol::SessionContext;

const SCHEMA_VERSION: i64 = 3;
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
    latest_checkpoint_json TEXT NOT NULL,
    session_context_json TEXT NOT NULL,
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
CREATE INDEX IF NOT EXISTS sessions_recent_idx
    ON sessions(updated_at DESC, latest_sequence DESC, session_id DESC);
PRAGMA user_version = 3;
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

    fn save<'a>(
        &'a self,
        checkpoint: &'a Checkpoint,
        transcript_delta: &'a [Value],
    ) -> BoxFuture<'a, Result<()>> {
        let checkpoint = checkpoint.clone();
        let transcript_delta = transcript_delta.to_vec();
        Box::pin(self.run(move |connection| {
            validate_checkpoint(&checkpoint)?;
            let sequence = i64::try_from(checkpoint.sequence).map_err(|_| {
                Error::Checkpoint("checkpoint sequence exceeds SQLite INTEGER".into())
            })?;
            let checkpoint_json = serde_json::to_string(&checkpoint)?;
            let session_context_json = serde_json::to_string(&checkpoint.session_context)?;
            let transcript_json = (!transcript_delta.is_empty())
                .then(|| serde_json::to_string(&transcript_delta))
                .transpose()?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            store_checkpoint(
                &transaction,
                &checkpoint,
                sequence,
                &checkpoint_json,
                &session_context_json,
                transcript_json.as_deref(),
            )?;
            transaction.commit()?;
            Ok(())
        }))
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
                            sessions.session_context_json, sessions.created_at,
                            sessions.updated_at
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
                    "SELECT sequence, items_json
                     FROM transcript_delta
                     WHERE session_id = ?1
                       AND (?2 IS NULL OR sequence < ?2)
                     ORDER BY sequence DESC
                     LIMIT ?3",
                )?;
                let mut batches = statement
                    .query_map(params![session_id, before_sequence, query_limit], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let has_more = batches.len() > request.max_batches;
                batches.truncate(request.max_batches);
                let batches = batches
                    .into_iter()
                    .map(|(sequence, json)| {
                        Ok(TranscriptBatch {
                            sequence: u64::try_from(sequence).map_err(|_| {
                                Error::Checkpoint("transcript row has a negative sequence".into())
                            })?,
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
            && checkpoint.active_turn_id.is_none()
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
                    .optional()?;
                if durable_parent != Some(parent_sequence) {
                    return Err(Error::Checkpoint(
                        "parent checkpoint changed before the fork".into(),
                    ));
                }
                transaction.execute(
                    "INSERT INTO sessions (
                         session_id, parent_session_id, parent_sequence, latest_sequence,
                         latest_checkpoint_json, session_context_json, catalog_visible,
                         first_user_message
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        session_id,
                        parent_session_id,
                        parent_sequence,
                        sequence,
                        checkpoint_json,
                        session_context_json,
                        catalog_visible,
                        checkpoint.first_user_message,
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
                            sessions.session_context_json, sessions.created_at,
                            sessions.updated_at
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

fn store_checkpoint(
    transaction: &Transaction<'_>,
    checkpoint: &Checkpoint,
    sequence: i64,
    json: &str,
    session_context_json: &str,
    transcript_json: Option<&str>,
) -> Result<()> {
    let changed = transaction.execute(
        "INSERT INTO sessions (
             session_id, latest_sequence, latest_checkpoint_json, session_context_json,
             catalog_visible, first_user_message
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(session_id) DO UPDATE SET
             latest_sequence = excluded.latest_sequence,
             latest_checkpoint_json = excluded.latest_checkpoint_json,
             session_context_json = excluded.session_context_json,
             catalog_visible = excluded.catalog_visible,
             first_user_message = COALESCE(sessions.first_user_message, excluded.first_user_message),
             updated_at = unixepoch()
         WHERE excluded.latest_sequence > sessions.latest_sequence",
        params![
            checkpoint.session_id,
            sequence,
            json,
            session_context_json,
            checkpoint.catalog_visible,
            checkpoint.first_user_message,
        ],
    )?;
    if changed == 0 {
        return Err(Error::Checkpoint(
            "checkpoint sequence did not advance".into(),
        ));
    }
    let Some(transcript_json) = transcript_json else {
        return Ok(());
    };
    transaction.execute(
        "INSERT INTO transcript_delta (session_id, sequence, items_json)
         VALUES (?1, ?2, ?3)",
        params![checkpoint.session_id, sequence, transcript_json],
    )?;
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
    ))
}

fn summary_from_row(row: SessionRow) -> Result<SessionSummary> {
    let session_context: SessionContext = serde_json::from_str(&row.6)?;
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
        created_at: row.7,
        updated_at: row.8,
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

fn validate_checkpoint(checkpoint: &Checkpoint) -> Result<()> {
    if checkpoint.version != CHECKPOINT_VERSION {
        return Err(Error::Checkpoint(format!(
            "unsupported checkpoint version {}",
            checkpoint.version
        )));
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

    #[tokio::test]
    async fn load_completes_while_another_connection_holds_a_write_transaction() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = Arc::new(
            SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
                .expect("open checkpoint database"),
        );
        let checkpoint = Checkpoint::empty("session");
        store.save(&checkpoint, &[]).await.expect("seed checkpoint");
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

        store.save(&checkpoint, &[]).await.expect("initial save");

        assert!(store.save(&checkpoint, &[]).await.is_err());
        let mut older = checkpoint.clone();
        older.sequence = 1;
        assert!(store.save(&older, &[]).await.is_err());
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
        store.save(&parent, &[]).await.expect("save parent");
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
    async fn metadata_round_trips_through_save_and_fork() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        let mut parent = Checkpoint::empty("parent");
        parent
            .metadata
            .insert("gateway.chat".into(), json!({"workspace": "/srv/project"}));
        store.save(&parent, &[]).await.expect("save parent");
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
        store.save(&checkpoint, &[]).await.expect("save session");
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
                .save(&Checkpoint::empty(session_id), &[])
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
    async fn transcript_page_bounds_batches_and_continues_backward() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let store = SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("open checkpoint database");
        let mut checkpoint = Checkpoint::empty("session");
        store.save(&checkpoint, &[]).await.expect("save session");
        for sequence in 1..=3 {
            checkpoint.sequence = sequence;
            let item = json!({"sequence": sequence});
            store
                .save(&checkpoint, std::slice::from_ref(&item))
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
    }
}
