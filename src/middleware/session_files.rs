//! Protected, session-bound storage shared by uploads and agent artifacts.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::SystemTime;

use base64::Engine as _;
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tempfile::TempPath;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};
use tokio::sync::{Mutex, OnceCell};
use uuid::Uuid;

use crate::protocol::SessionFileReference;
use crate::{Error, Result};

pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_SESSION_BYTES: u64 = 250 * 1024 * 1024;
pub const MAX_UPLOAD_CHUNK_BYTES: usize = 256 * 1024;
pub const MAX_READ_CHUNK_BYTES: usize = 256 * 1024;
const MAX_SESSION_FILES: usize = 128;
const MAX_SESSION_ID_BYTES: usize = 4 * 1024;
const MAX_VALIDATED_BLOBS: usize = 1_024;
const BLOB_DIR: &str = "blobs";
const ATTACHMENT_WORKSPACE_FILE: &str = ".attachment-workspace.json";
const METADATA_FILE: &str = ".session-file.json";

/// One bounded range read from a stored session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFileChunk {
    pub offset: u64,
    pub data: Vec<u8>,
    pub next_offset: Option<u64>,
}

/// Protected immutable file storage shared by inbound and outbound transports.
///
/// Display names live only in metadata; payloads always use an internal filename.
#[derive(Clone)]
pub struct SessionFileStore {
    root: Arc<PathBuf>,
    // ponytail: one commit lock keeps quota checks and publication atomic.
    commits: Arc<Mutex<()>>,
    reservations: Arc<StdMutex<BTreeMap<String, ReservationTotals>>>,
    // ponytail: immutable private blobs reuse one verified SHA-256 while metadata is unchanged.
    validated_blobs: Arc<StdMutex<BTreeMap<String, BlobValidationStamp>>>,
    initialized: Arc<OnceCell<()>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct BlobValidationStamp {
    size: u64,
    modified: SystemTime,
}

#[derive(Default)]
struct ReservationTotals {
    files: usize,
    bytes: u64,
}

struct SessionFileReservation {
    reservations: Arc<StdMutex<BTreeMap<String, ReservationTotals>>>,
    session_id: String,
    size: u64,
    active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionFileOrigin {
    Upload,
    Artifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSessionFile {
    origin: SessionFileOrigin,
    file: SessionFileReference,
    content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAttachmentWorkspace {
    path: PathBuf,
}

impl SessionFileStore {
    /// Creates a store below the gateway's already protected state directory.
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            root: Arc::new(state_dir.join("session-files")),
            commits: Arc::new(Mutex::new(())),
            reservations: Arc::new(StdMutex::new(BTreeMap::new())),
            validated_blobs: Arc::new(StdMutex::new(BTreeMap::new())),
            initialized: Arc::new(OnceCell::new()),
        }
    }

    /// Starts one connection-owned user upload.
    pub async fn begin_upload(
        &self,
        session_id: &str,
        name: String,
        size: u64,
        media_type: String,
    ) -> Result<PendingSessionFileWrite> {
        self.begin(
            session_id,
            name,
            size,
            media_type,
            SessionFileOrigin::Upload,
        )
        .await
    }

    /// Publishes one immutable agent artifact.
    pub async fn publish_artifact(
        &self,
        session_id: &str,
        name: String,
        media_type: String,
        bytes: &[u8],
    ) -> Result<SessionFileReference> {
        let size = u64::try_from(bytes.len())
            .map_err(|_| Error::Tool("artifact size is unsupported".into()))?;
        let mut pending = self
            .begin(
                session_id,
                name,
                size,
                media_type,
                SessionFileOrigin::Artifact,
            )
            .await?;
        for chunk in bytes.chunks(MAX_UPLOAD_CHUNK_BYTES) {
            let offset = pending.written;
            pending.append(offset, chunk).await?;
        }
        pending.finish().await
    }

    /// Lists completed user uploads for one session.
    pub async fn list_uploads(&self, session_id: &str) -> Result<Vec<SessionFileReference>> {
        self.list_origin(session_id, SessionFileOrigin::Upload)
            .await
    }

    /// Lists completed agent artifacts for one session.
    pub async fn list_artifacts(&self, session_id: &str) -> Result<Vec<SessionFileReference>> {
        self.list_origin(session_id, SessionFileOrigin::Artifact)
            .await
    }

    pub(crate) async fn register_attachment_workspace(
        &self,
        session_id: &str,
        workspace: &Path,
    ) -> Result<()> {
        validate_session_id(session_id)?;
        let workspace = tokio::fs::canonicalize(workspace).await?;
        if !workspace.is_dir() {
            return Err(Error::Config(
                "attachment workspace is not a directory".into(),
            ));
        }
        self.ensure_initialized().await?;
        let _commit = self.commits.lock().await;
        let directory = self.session_dir(session_id);
        ensure_private_dir(&directory).await?;
        let stored = StoredAttachmentWorkspace { path: workspace };
        let destination = directory.join(ATTACHMENT_WORKSPACE_FILE);
        match load_attachment_workspace(&destination).await {
            Ok(existing) if existing == stored => Ok(()),
            Ok(_) => Err(Error::Config(
                "attachment workspace changed for the active session".into(),
            )),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                save_attachment_workspace(&directory, &stored).await
            }
            Err(error) => Err(error),
        }
    }

    /// Permanently removes every upload and artifact owned by one idle session.
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        validate_session_id(session_id)?;
        self.ensure_initialized().await?;
        let _commit = self.commits.lock().await;
        if self
            .reservations
            .lock()
            .map_err(|_| Error::Tool("session file reservation lock is poisoned".into()))?
            .contains_key(session_id)
        {
            return Err(Error::Tool(
                "session files cannot be deleted while an upload is active".into(),
            ));
        }
        let directory = self.session_dir(session_id);
        match tokio::fs::symlink_metadata(&directory).await {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                let workspace = load_optional_attachment_workspace(&directory).await?;
                if let Some(workspace) = workspace {
                    remove_staged_attachments(&workspace.path, session_id).await?;
                }
                tokio::fs::remove_dir_all(directory).await?;
            }
            Ok(_) => {
                return Err(Error::Tool(
                    "session file directory is not a protected directory".into(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        gc_unreferenced_blobs(&self.root).await?;
        Ok(())
    }

    /// Reads one bounded byte range from either kind of stored session file.
    pub async fn read_chunk(
        &self,
        session_id: &str,
        file_id: &str,
        offset: u64,
        max_bytes: usize,
    ) -> Result<SessionFileChunk> {
        let (record, path) = self.resolve(session_id, file_id).await?;
        read_resolved_chunk(record.file, path, offset, max_bytes).await
    }

    /// Reads and validates one complete user upload for model input.
    pub async fn read_upload_all(
        &self,
        session_id: &str,
        expected: &SessionFileReference,
    ) -> Result<(SessionFileReference, Vec<u8>)> {
        let (actual, path) = self.resolve_upload(session_id, &expected.id).await?;
        if &actual != expected {
            return Err(Error::Tool(
                "session file metadata does not match the uploaded file".into(),
            ));
        }
        let bytes = tokio::fs::read(path).await?;
        if bytes.len() as u64 != actual.size {
            return Err(Error::Tool(
                "uploaded file size changed after upload".into(),
            ));
        }
        Ok((actual, bytes))
    }

    /// Verifies that a frontend reference names the exact user upload.
    pub async fn verify_upload(
        &self,
        session_id: &str,
        expected: &SessionFileReference,
    ) -> Result<()> {
        let (actual, _) = self.resolve_upload(session_id, &expected.id).await?;
        if &actual != expected {
            return Err(Error::Tool(
                "session file metadata does not match the uploaded file".into(),
            ));
        }
        Ok(())
    }

    /// Resolves an owned upload to its private content-addressed identity.
    pub(crate) async fn upload_content_hash(
        &self,
        session_id: &str,
        expected: &SessionFileReference,
    ) -> Result<String> {
        let (record, _) = self.resolve_upload_record(session_id, &expected.id).await?;
        if &record.file != expected {
            return Err(Error::Tool(
                "session file metadata does not match the uploaded file".into(),
            ));
        }
        Ok(record.content_hash)
    }

    /// Reads a content-addressed blob after validating its size and SHA-256 identity.
    pub(crate) async fn read_content_blob(&self, content_hash: &str, size: u64) -> Result<Vec<u8>> {
        let path = self.content_blob_path(content_hash, size).await?;
        Ok(tokio::fs::read(path).await?)
    }

    /// Resolves a validated content blob for workspace staging.
    pub(crate) async fn content_blob_path(&self, content_hash: &str, size: u64) -> Result<PathBuf> {
        validate_content_hash(content_hash)?;
        let path = self.blob_path(content_hash);
        validate_content_blob(&path, content_hash, size, &self.validated_blobs).await?;
        Ok(path)
    }

    async fn begin(
        &self,
        session_id: &str,
        name: String,
        size: u64,
        media_type: String,
        origin: SessionFileOrigin,
    ) -> Result<PendingSessionFileWrite> {
        validate_session_id(session_id)?;
        validate_name(&name)?;
        validate_media_type(&media_type)?;
        if !(1..=MAX_FILE_BYTES).contains(&size) {
            return Err(Error::Tool(format!(
                "session file size must be 1–{MAX_FILE_BYTES} bytes"
            )));
        }
        self.ensure_initialized().await?;
        let _commit = self.commits.lock().await;
        let session_dir = self.session_dir(session_id);
        ensure_private_dir(&session_dir).await?;
        let existing = list_completed(&session_dir, &self.blob_dir()).await?;
        let reservation = self.reserve(session_id, size, &existing)?;
        let record = StoredSessionFile {
            origin,
            file: SessionFileReference {
                id: Uuid::new_v4().to_string(),
                name,
                size,
                media_type,
            },
            content_hash: String::new(),
        };
        let temporary = tempfile::NamedTempFile::new_in(&session_dir)?;
        set_private_file(temporary.path()).await?;
        let (file, path) = temporary.into_parts();
        Ok(PendingSessionFileWrite {
            store: self.clone(),
            session_id: session_id.into(),
            record,
            reservation,
            written: 0,
            file: Some(tokio::fs::File::from_std(file)),
            path: Some(path),
        })
    }

    async fn list_origin(
        &self,
        session_id: &str,
        origin: SessionFileOrigin,
    ) -> Result<Vec<SessionFileReference>> {
        validate_session_id(session_id)?;
        self.ensure_initialized().await?;
        Ok(
            list_completed(&self.session_dir(session_id), &self.blob_dir())
                .await?
                .into_iter()
                .filter(|record| record.origin == origin)
                .map(|record| record.file)
                .collect(),
        )
    }

    async fn resolve(
        &self,
        session_id: &str,
        file_id: &str,
    ) -> Result<(StoredSessionFile, PathBuf)> {
        validate_session_id(session_id)?;
        validate_file_id(file_id)?;
        self.ensure_initialized().await?;
        let directory = self.session_dir(session_id).join(file_id);
        require_directory(&directory).await?;
        let metadata = load_metadata(&directory.join(METADATA_FILE)).await?;
        if metadata.file.id != file_id {
            return Err(Error::Tool(
                "session file metadata has an invalid ID".into(),
            ));
        }
        validate_stored_file(&metadata)?;
        let path = self.blob_path(&metadata.content_hash);
        validate_content_blob(
            &path,
            &metadata.content_hash,
            metadata.file.size,
            &self.validated_blobs,
        )
        .await?;
        Ok((metadata, path))
    }

    async fn resolve_upload(
        &self,
        session_id: &str,
        file_id: &str,
    ) -> Result<(SessionFileReference, PathBuf)> {
        let (record, path) = self.resolve_upload_record(session_id, file_id).await?;
        Ok((record.file, path))
    }

    async fn resolve_upload_record(
        &self,
        session_id: &str,
        file_id: &str,
    ) -> Result<(StoredSessionFile, PathBuf)> {
        let (record, path) = self.resolve(session_id, file_id).await?;
        if record.origin != SessionFileOrigin::Upload {
            return Err(Error::Tool(
                "session file is not a file uploaded by the user".into(),
            ));
        }
        Ok((record, path))
    }

    fn session_dir(&self, session_id: &str) -> PathBuf {
        let digest = Sha256::digest(session_id.as_bytes());
        self.root
            .join(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest))
    }

    fn blob_dir(&self) -> PathBuf {
        self.root.join(BLOB_DIR)
    }

    fn blob_path(&self, content_hash: &str) -> PathBuf {
        self.blob_dir().join(content_hash)
    }

    async fn ensure_initialized(&self) -> Result<()> {
        self.initialized
            .get_or_try_init(|| async {
                ensure_private_dir(&self.root).await?;
                cleanup_stale_files(&self.root).await
            })
            .await
            .map(|_| ())
    }

    fn reserve(
        &self,
        session_id: &str,
        size: u64,
        completed: &[StoredSessionFile],
    ) -> Result<SessionFileReservation> {
        let completed_bytes = completed.iter().try_fold(0_u64, |total, item| {
            total
                .checked_add(item.file.size)
                .ok_or_else(|| Error::Tool("session file quota overflow".into()))
        })?;
        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| Error::Tool("session file reservation state is unavailable".into()))?;
        let pending_files = reservations
            .get(session_id)
            .map_or(0, |pending| pending.files);
        let pending_bytes = reservations
            .get(session_id)
            .map_or(0, |pending| pending.bytes);
        if completed.len().saturating_add(pending_files) >= MAX_SESSION_FILES {
            return Err(Error::Tool(format!(
                "session cannot contain more than {MAX_SESSION_FILES} files"
            )));
        }
        let reserved_bytes = completed_bytes
            .checked_add(pending_bytes)
            .and_then(|total| total.checked_add(size))
            .ok_or_else(|| Error::Tool("session file quota overflow".into()))?;
        if reserved_bytes > MAX_SESSION_BYTES {
            return Err(Error::Tool(format!(
                "session files exceed {MAX_SESSION_BYTES} bytes"
            )));
        }
        let pending = reservations.entry(session_id.into()).or_default();
        pending.files += 1;
        pending.bytes += size;
        Ok(SessionFileReservation {
            reservations: Arc::clone(&self.reservations),
            session_id: session_id.into(),
            size,
            active: true,
        })
    }

    fn validate_reserved_capacity(
        &self,
        session_id: &str,
        completed: &[StoredSessionFile],
    ) -> Result<()> {
        let completed_bytes = completed.iter().try_fold(0_u64, |total, item| {
            total
                .checked_add(item.file.size)
                .ok_or_else(|| Error::Tool("session file quota overflow".into()))
        })?;
        let reservations = self
            .reservations
            .lock()
            .map_err(|_| Error::Tool("session file reservation state is unavailable".into()))?;
        let pending = reservations.get(session_id);
        let pending_files = pending.map_or(0, |pending| pending.files);
        let pending_bytes = pending.map_or(0, |pending| pending.bytes);
        if completed.len().saturating_add(pending_files) > MAX_SESSION_FILES
            || completed_bytes
                .checked_add(pending_bytes)
                .is_none_or(|bytes| bytes > MAX_SESSION_BYTES)
        {
            return Err(Error::Tool("session file reservation exceeds quota".into()));
        }
        Ok(())
    }
}

/// An incomplete immutable session-file write.
pub struct PendingSessionFileWrite {
    store: SessionFileStore,
    session_id: String,
    record: StoredSessionFile,
    reservation: SessionFileReservation,
    written: u64,
    file: Option<tokio::fs::File>,
    path: Option<TempPath>,
}

impl PendingSessionFileWrite {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.record.file.id
    }

    /// Appends the next exact chunk.
    pub async fn append(&mut self, offset: u64, data: &[u8]) -> Result<u64> {
        if data.is_empty() || data.len() > MAX_UPLOAD_CHUNK_BYTES {
            return Err(Error::Tool(format!(
                "session file chunk must be 1–{MAX_UPLOAD_CHUNK_BYTES} bytes"
            )));
        }
        if offset != self.written {
            return Err(Error::Tool(format!(
                "session file offset must be {}",
                self.written
            )));
        }
        let next = self
            .written
            .checked_add(data.len() as u64)
            .ok_or_else(|| Error::Tool("session file size overflow".into()))?;
        if next > self.record.file.size {
            return Err(Error::Tool(
                "chunk exceeds declared session file size".into(),
            ));
        }
        self.file
            .as_mut()
            .ok_or_else(|| Error::Tool("session file upload is already finished".into()))?
            .write_all(data)
            .await?;
        self.written = next;
        Ok(next)
    }

    /// Atomically publishes a complete session file.
    pub async fn finish(mut self) -> Result<SessionFileReference> {
        if self.written != self.record.file.size {
            return Err(Error::Tool(format!(
                "session file upload has {} of {} bytes",
                self.written, self.record.file.size
            )));
        }
        let mut file = self
            .file
            .take()
            .ok_or_else(|| Error::Tool("session file upload is already finished".into()))?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);

        let _guard = self.store.commits.lock().await;
        let session_dir = self.store.session_dir(&self.session_id);
        let existing = list_completed(&session_dir, &self.store.blob_dir()).await?;
        self.store
            .validate_reserved_capacity(&self.session_id, &existing)?;

        let directory = session_dir.join(&self.record.file.id);
        if tokio::fs::symlink_metadata(&directory).await.is_ok() {
            return Err(Error::Tool("session file ID already exists".into()));
        }
        let source = self
            .path
            .take()
            .ok_or_else(|| Error::Tool("session file temporary file is missing".into()))?;
        let source_path = source.to_path_buf();
        let content_hash = hash_file(&source_path).await?;
        validate_content_hash(&content_hash)?;
        self.record.content_hash = content_hash.clone();

        let blob_dir = self.store.blob_dir();
        ensure_private_dir(&blob_dir).await?;
        let blob_path = self.store.blob_path(&content_hash);
        match tokio::fs::hard_link(&source_path, &blob_path).await {
            Ok(()) => set_private_file(&blob_path).await?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                validate_content_blob(
                    &blob_path,
                    &content_hash,
                    self.record.file.size,
                    &self.store.validated_blobs,
                )
                .await?;
            }
            Err(error) => return Err(error.into()),
        }
        tokio::fs::remove_file(&source_path).await?;

        let staging = session_dir.join(format!(".{}-partial", self.record.file.id));
        create_private_dir(&staging).await?;
        if let Err(error) = save_metadata(&staging, &self.record).await {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            let _ = gc_unreferenced_blobs(&self.store.root).await;
            return Err(error);
        }
        if let Err(error) = tokio::fs::rename(&staging, &directory).await {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            let _ = gc_unreferenced_blobs(&self.store.root).await;
            return Err(error.into());
        }
        self.reservation.release();
        Ok(self.record.file.clone())
    }
}

impl SessionFileReservation {
    fn release(&mut self) {
        if !self.active {
            return;
        }
        let mut reservations = self
            .reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let remove = if let Some(pending) = reservations.get_mut(&self.session_id) {
            pending.files = pending.files.saturating_sub(1);
            pending.bytes = pending.bytes.saturating_sub(self.size);
            pending.files == 0
        } else {
            false
        };
        if remove {
            reservations.remove(&self.session_id);
        }
        self.active = false;
    }
}

impl Drop for SessionFileReservation {
    fn drop(&mut self) {
        self.release();
    }
}

async fn read_resolved_chunk(
    file: SessionFileReference,
    path: PathBuf,
    offset: u64,
    max_bytes: usize,
) -> Result<SessionFileChunk> {
    if max_bytes == 0 || max_bytes > MAX_READ_CHUNK_BYTES {
        return Err(Error::Tool(format!(
            "session file read size must be 1–{MAX_READ_CHUNK_BYTES} bytes"
        )));
    }
    if offset > file.size {
        return Err(Error::Tool("session file offset exceeds file size".into()));
    }
    let mut handle = tokio::fs::File::open(path).await?;
    handle.seek(std::io::SeekFrom::Start(offset)).await?;
    let remaining = file.size.saturating_sub(offset);
    let length = usize::try_from(remaining.min(max_bytes as u64))
        .map_err(|_| Error::Tool("session file range is unsupported".into()))?;
    let mut data = vec![0; length];
    handle.read_exact(&mut data).await?;
    let end = offset.saturating_add(length as u64);
    Ok(SessionFileChunk {
        offset,
        data,
        next_offset: (end < file.size).then_some(end),
    })
}

async fn cleanup_stale_files(root: &Path) -> Result<()> {
    let blob_root = root.join(BLOB_DIR);
    ensure_private_dir(&blob_root).await?;
    let mut sessions = tokio::fs::read_dir(root).await?;
    while let Some(session) = sessions.next_entry().await? {
        if session.file_name() == BLOB_DIR {
            continue;
        }
        let file_type = session.file_type().await?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let mut entries = tokio::fs::read_dir(session.path()).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if file_type.is_file() && name.starts_with(".tmp") {
                tokio::fs::remove_file(entry.path()).await?;
                continue;
            }
            let staging_id = name
                .strip_prefix('.')
                .and_then(|name| name.strip_suffix("-partial"));
            if file_type.is_dir()
                && !file_type.is_symlink()
                && staging_id.is_some_and(|id| Uuid::parse_str(id).is_ok())
            {
                tokio::fs::remove_dir_all(entry.path()).await?;
            }
        }
    }
    let mut blobs = tokio::fs::read_dir(&blob_root).await?;
    while let Some(blob) = blobs.next_entry().await? {
        let file_type = blob.file_type().await?;
        let Some(name) = blob.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if file_type.is_file() && name.starts_with(".tmp") {
            tokio::fs::remove_file(blob.path()).await?;
        }
    }
    gc_unreferenced_blobs(root).await
}

async fn list_completed(session_dir: &Path, blob_root: &Path) -> Result<Vec<StoredSessionFile>> {
    match tokio::fs::symlink_metadata(session_dir).await {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(Error::Tool("session file path is not a directory".into()));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    }
    let mut records = Vec::new();
    let mut entries = tokio::fs::read_dir(session_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let id = entry.file_name();
        let Some(id) = id.to_str() else {
            continue;
        };
        if validate_file_id(id).is_err() {
            continue;
        }
        let record = load_metadata(&entry.path().join(METADATA_FILE)).await?;
        validate_stored_file(&record)?;
        if record.file.id != id {
            return Err(Error::Tool(
                "session file directory and metadata IDs differ".into(),
            ));
        }
        let path = blob_root.join(&record.content_hash);
        validate_content_blob_metadata(&path, record.file.size).await?;
        records.push(record);
    }
    records.sort_by(|left, right| {
        left.file
            .name
            .cmp(&right.file.name)
            .then(left.file.id.cmp(&right.file.id))
    });
    Ok(records)
}

async fn gc_unreferenced_blobs(root: &Path) -> Result<()> {
    let blob_root = root.join(BLOB_DIR);
    let mut referenced = BTreeMap::new();
    let mut sessions = tokio::fs::read_dir(root).await?;
    while let Some(session) = sessions.next_entry().await? {
        if session.file_name() == BLOB_DIR {
            continue;
        }
        let file_type = session.file_type().await?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        for record in list_completed(&session.path(), &blob_root).await? {
            referenced.insert(record.content_hash, ());
        }
    }

    let mut blobs = tokio::fs::read_dir(&blob_root).await?;
    while let Some(blob) = blobs.next_entry().await? {
        let file_type = blob.file_type().await?;
        if file_type.is_symlink() {
            return Err(Error::Tool("session blob path is a symbolic link".into()));
        }
        if !file_type.is_file() {
            return Err(Error::Tool(
                "session blob path is not a regular file".into(),
            ));
        }
        let file_name = blob.file_name();
        let name = file_name
            .to_str()
            .ok_or_else(|| Error::Tool("session blob name is not valid UTF-8".into()))?;
        if name.starts_with(".tmp") {
            tokio::fs::remove_file(blob.path()).await?;
            continue;
        }
        validate_content_hash(name)?;
        if !referenced.contains_key(name) {
            tokio::fs::remove_file(blob.path()).await?;
        }
    }
    Ok(())
}

async fn load_metadata(path: &Path) -> Result<StoredSessionFile> {
    require_regular_file(path).await?;
    let bytes = tokio::fs::read(path).await?;
    if bytes.len() > 4 * 1024 {
        return Err(Error::Tool(
            "session file metadata exceeds size limit".into(),
        ));
    }
    serde_json::from_slice(&bytes).map_err(Into::into)
}

async fn load_attachment_workspace(path: &Path) -> Result<StoredAttachmentWorkspace> {
    require_regular_file(path).await?;
    let bytes = tokio::fs::read(path).await?;
    if bytes.len() > 4 * 1024 {
        return Err(Error::Tool(
            "attachment workspace metadata exceeds size limit".into(),
        ));
    }
    serde_json::from_slice(&bytes).map_err(Into::into)
}

async fn load_optional_attachment_workspace(
    directory: &Path,
) -> Result<Option<StoredAttachmentWorkspace>> {
    let path = directory.join(ATTACHMENT_WORKSPACE_FILE);
    match load_attachment_workspace(&path).await {
        Ok(workspace) => Ok(Some(workspace)),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

async fn save_attachment_workspace(
    directory: &Path,
    workspace: &StoredAttachmentWorkspace,
) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut temporary, workspace)?;
    temporary.as_file().sync_all()?;
    let destination = directory.join(ATTACHMENT_WORKSPACE_FILE);
    temporary
        .persist_noclobber(&destination)
        .map_err(|error| error.error)?;
    set_private_file(&destination).await
}

async fn remove_staged_attachments(workspace: &Path, session_id: &str) -> Result<()> {
    let workspace = workspace.to_path_buf();
    let staged = PathBuf::from(".horus")
        .join("attachments")
        .join(attachment_staging_session_name(session_id));
    tokio::task::spawn_blocking(move || {
        let directory = match Dir::open_ambient_dir(workspace, ambient_authority()) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(Error::from(error)),
        };
        match directory.remove_dir_all(staged) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    })
    .await
    .map_err(|error| Error::Tool(format!("attachment cleanup task failed: {error}")))?
}

async fn save_metadata(directory: &Path, file: &StoredSessionFile) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut temporary, file)?;
    temporary.as_file().sync_all()?;
    let destination = directory.join(METADATA_FILE);
    temporary
        .persist_noclobber(&destination)
        .map_err(|error| error.error)?;
    set_private_file(&destination).await
}

fn validate_reference(file: &SessionFileReference) -> Result<()> {
    validate_file_id(&file.id)?;
    validate_name(&file.name)?;
    validate_media_type(&file.media_type)?;
    if !(1..=MAX_FILE_BYTES).contains(&file.size) {
        return Err(Error::Tool(
            "session file metadata has an invalid size".into(),
        ));
    }
    Ok(())
}

fn validate_stored_file(file: &StoredSessionFile) -> Result<()> {
    validate_reference(&file.file)?;
    validate_content_hash(&file.content_hash)
}

fn validate_content_hash(content_hash: &str) -> Result<()> {
    if content_hash.len() != 64
        || !content_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::Tool("session file content hash is invalid".into()));
    }
    Ok(())
}

pub(crate) fn attachment_staging_session_name(session_id: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(session_id.as_bytes()))
}

async fn hash_file(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_hash(&hasher.finalize()))
}

async fn validate_content_blob(
    path: &Path,
    content_hash: &str,
    size: u64,
    validated_blobs: &StdMutex<BTreeMap<String, BlobValidationStamp>>,
) -> Result<()> {
    let metadata = validate_content_blob_metadata(path, size).await?;
    let stamp = metadata
        .modified()
        .ok()
        .map(|modified| BlobValidationStamp {
            size: metadata.len(),
            modified,
        });
    let cached = stamp.is_some_and(|stamp| {
        validated_blobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(content_hash)
            == Some(&stamp)
    });
    if cached {
        return Ok(());
    }
    if hash_file(path).await? != content_hash {
        return Err(Error::Tool(
            "session file content hash does not match metadata".into(),
        ));
    }
    if let Some(stamp) = stamp {
        remember_validated_blob(validated_blobs, content_hash, stamp);
    }
    Ok(())
}

fn remember_validated_blob(
    validated_blobs: &StdMutex<BTreeMap<String, BlobValidationStamp>>,
    content_hash: &str,
    stamp: BlobValidationStamp,
) {
    let mut validated_blobs = validated_blobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if validated_blobs.len() >= MAX_VALIDATED_BLOBS && !validated_blobs.contains_key(content_hash) {
        validated_blobs.pop_first();
    }
    validated_blobs.insert(content_hash.into(), stamp);
}

async fn validate_content_blob_metadata(path: &Path, size: u64) -> Result<std::fs::Metadata> {
    let metadata = require_regular_file(path).await?;
    if metadata.len() != size {
        return Err(Error::Tool(
            "session file size does not match metadata".into(),
        ));
    }
    Ok(metadata)
}

fn hex_hash(hash: &[u8]) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_session_id(id: &str) -> Result<()> {
    if id.trim().is_empty() || id.len() > MAX_SESSION_ID_BYTES {
        return Err(Error::Tool(format!(
            "session ID must be 1–{MAX_SESSION_ID_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_file_id(id: &str) -> Result<()> {
    Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| Error::Tool("session file ID must be a UUID".into()))
}

fn validate_name(name: &str) -> Result<()> {
    let path = Path::new(name);
    let mut components = path.components();
    let one_normal =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if !one_normal
        || name == "."
        || name == ".."
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
        || name.len() > 255
    {
        return Err(Error::Tool(
            "session file name must be one safe 1–255 byte filename".into(),
        ));
    }
    Ok(())
}

fn validate_media_type(media_type: &str) -> Result<()> {
    let Some((kind, subtype)) = media_type.split_once('/') else {
        return Err(Error::Tool(
            "session file media type must be type/subtype".into(),
        ));
    };
    let token = |value: &str| {
        !value.is_empty()
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                    )
            })
    };
    if media_type.len() > 127 || !token(kind) || !token(subtype) {
        return Err(Error::Tool("session file media type is invalid".into()));
    }
    Ok(())
}

async fn require_directory(path: &Path) -> Result<()> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(Error::Tool(
            "session file path is not a regular directory".into(),
        ))
    }
}

async fn require_regular_file(path: &Path) -> Result<std::fs::Metadata> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
        Ok(metadata)
    } else {
        Err(Error::Tool(
            "session file path is not a regular file".into(),
        ))
    }
}

async fn create_private_dir(path: &Path) -> Result<()> {
    tokio::fs::create_dir(path).await?;
    set_private_dir(path).await
}

async fn ensure_private_dir(path: &Path) -> Result<()> {
    tokio::fs::create_dir_all(path).await?;
    require_directory(path).await?;
    set_private_dir(path).await
}

#[cfg(unix)]
async fn set_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn set_private_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
async fn set_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn set_private_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn upload_round_trip_is_session_scoped_and_atomic() {
        let state = tempfile::tempdir().expect("state");
        let store = SessionFileStore::new(state.path());
        let session_id = "thread:not-a-uuid";
        let mut pending = store
            .begin_upload(session_id, "notes.txt".into(), 5, "text/plain".into())
            .await
            .expect("begin");
        pending.append(0, b"hello").await.expect("append");
        let file = pending.finish().await.expect("finish");

        let (_, bytes) = store
            .read_upload_all(session_id, &file)
            .await
            .expect("read");

        assert_eq!(bytes, b"hello");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            let session = store.session_dir(session_id);
            let directory = session.join(&file.id);
            for path in [store.root.as_path(), &session, &directory] {
                let mode = std::fs::metadata(path)
                    .expect("directory mode")
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o700);
            }
            let metadata = load_metadata(&directory.join(METADATA_FILE))
                .await
                .expect("metadata");
            for path in [
                store.blob_path(&metadata.content_hash),
                directory.join(METADATA_FILE),
            ] {
                let mode = std::fs::metadata(path)
                    .expect("file mode")
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600);
            }
        }
    }

    #[tokio::test]
    async fn delete_session_removes_only_that_sessions_files() {
        let state = tempfile::tempdir().expect("state");
        let store = SessionFileStore::new(state.path());
        for session_id in ["deleted", "retained"] {
            store
                .publish_artifact(
                    session_id,
                    "result.txt".into(),
                    "text/plain".into(),
                    b"result",
                )
                .await
                .expect("publish artifact");
        }

        store
            .delete_session("deleted")
            .await
            .expect("delete session files");

        assert!(
            store
                .list_artifacts("deleted")
                .await
                .expect("deleted artifacts")
                .is_empty()
        );
        assert_eq!(
            store
                .list_artifacts("retained")
                .await
                .expect("retained artifacts")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn identical_payloads_share_one_content_blob() {
        let state = tempfile::tempdir().expect("state");
        let store = SessionFileStore::new(state.path());
        let first = store
            .publish_artifact("first", "one.txt".into(), "text/plain".into(), b"same")
            .await
            .expect("first artifact");
        let second = store
            .publish_artifact("second", "two.txt".into(), "text/plain".into(), b"same")
            .await
            .expect("second artifact");

        assert_ne!(first.id, second.id);
        assert_eq!(blob_entries(&store), 1);
    }

    #[tokio::test]
    async fn tampered_content_blob_is_rejected() {
        let state = tempfile::tempdir().expect("state");
        let store = SessionFileStore::new(state.path());
        let file = store
            .publish_artifact("session", "result.txt".into(), "text/plain".into(), b"safe")
            .await
            .expect("artifact");
        let metadata = load_metadata(
            &store
                .session_dir("session")
                .join(&file.id)
                .join(METADATA_FILE),
        )
        .await
        .expect("metadata");
        std::fs::write(store.blob_path(&metadata.content_hash), b"evil").expect("tamper");

        assert!(store.read_chunk("session", &file.id, 0, 4).await.is_err());
    }

    #[test]
    fn validated_blob_cache_is_bounded() {
        let cache = StdMutex::new(BTreeMap::new());
        let stamp = BlobValidationStamp {
            size: 1,
            modified: SystemTime::now(),
        };

        for index in 0..=MAX_VALIDATED_BLOBS {
            remember_validated_blob(&cache, &format!("{index:064x}"), stamp);
        }

        let cache = cache.into_inner().expect("validation cache");
        assert_eq!(cache.len(), MAX_VALIDATED_BLOBS);
        assert!(cache.contains_key(&format!("{:064x}", MAX_VALIDATED_BLOBS)));
    }

    #[tokio::test]
    async fn private_content_identity_round_trips_without_wire_changes() {
        let state = tempfile::tempdir().expect("state");
        let store = SessionFileStore::new(state.path());
        let file = store
            .publish_artifact(
                "session",
                "upload.txt".into(),
                "text/plain".into(),
                b"hello",
            )
            .await
            .expect("artifact");
        assert!(store.upload_content_hash("session", &file).await.is_err());

        let mut pending = store
            .begin_upload("session", "upload.txt".into(), 5, "text/plain".into())
            .await
            .expect("upload");
        pending.append(0, b"hello").await.expect("append");
        let upload = pending.finish().await.expect("finish");
        let hash = store
            .upload_content_hash("session", &upload)
            .await
            .expect("private hash");
        assert_eq!(
            store
                .read_content_blob(&hash, upload.size)
                .await
                .expect("private blob"),
            b"hello"
        );
    }

    #[tokio::test]
    async fn deleting_last_reference_garbage_collects_the_blob() {
        let state = tempfile::tempdir().expect("state");
        let store = SessionFileStore::new(state.path());
        store
            .publish_artifact("first", "one.txt".into(), "text/plain".into(), b"same")
            .await
            .expect("first artifact");
        store
            .publish_artifact("second", "two.txt".into(), "text/plain".into(), b"same")
            .await
            .expect("second artifact");

        store.delete_session("first").await.expect("delete first");
        assert_eq!(blob_entries(&store), 1);
        store.delete_session("second").await.expect("delete second");
        assert_eq!(blob_entries(&store), 0);
    }

    #[tokio::test]
    async fn accepts_50_mib_and_rejects_larger_files() {
        let state = tempfile::tempdir().expect("state");
        let store = SessionFileStore::new(state.path());
        let pending = store
            .begin_upload(
                "session",
                "large.bin".into(),
                MAX_FILE_BYTES,
                "application/octet-stream".into(),
            )
            .await
            .expect("50 MiB upload");
        drop(pending);
        assert!(
            store
                .begin_upload(
                    "session",
                    "too-large.bin".into(),
                    MAX_FILE_BYTES + 1,
                    "application/octet-stream".into(),
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn delete_session_rejects_an_active_upload() {
        let state = tempfile::tempdir().expect("state");
        let store = SessionFileStore::new(state.path());
        let pending = store
            .begin_upload("session", "pending.txt".into(), 1, "text/plain".into())
            .await
            .expect("begin upload");

        assert!(store.delete_session("session").await.is_err());

        drop(pending);
        store
            .delete_session("session")
            .await
            .expect("delete released session files");
    }

    #[tokio::test]
    async fn display_names_never_select_internal_storage_paths() {
        let state = tempfile::tempdir().expect("state");
        let store = SessionFileStore::new(state.path());

        for name in [METADATA_FILE, ".SESSION-FILE.JSON"] {
            let mut pending = store
                .begin_upload("session", name.into(), 1, "application/octet-stream".into())
                .await
                .expect("begin");
            pending.append(0, b"x").await.expect("append");
            let file = pending.finish().await.expect("finish");

            assert_eq!(file.name, name);
            assert_eq!(
                store
                    .read_upload_all("session", &file)
                    .await
                    .expect("read")
                    .1,
                b"x"
            );
        }
    }

    #[tokio::test]
    async fn artifacts_are_downloadable_but_excluded_from_upload_access() {
        let state = tempfile::tempdir().expect("state");
        let store = SessionFileStore::new(state.path());
        let file = store
            .publish_artifact(
                "session",
                "report.xlsx".into(),
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
                &[0, 255, 1],
            )
            .await
            .expect("publish");

        assert!(
            store
                .list_uploads("session")
                .await
                .expect("uploads")
                .is_empty()
        );
        assert_eq!(
            store
                .read_chunk("session", &file.id, 0, 16)
                .await
                .expect("chunk")
                .data,
            [0, 255, 1]
        );
        assert!(
            store
                .read_chunk("another-session", &file.id, 0, 16)
                .await
                .is_err()
        );
        assert!(store.read_upload_all("session", &file).await.is_err());
        assert!(store.verify_upload("session", &file).await.is_err());
        let reopened = SessionFileStore::new(state.path());
        assert_eq!(
            reopened
                .list_artifacts("session")
                .await
                .expect("reopened artifacts"),
            [file]
        );
    }

    #[tokio::test]
    async fn upload_rejects_traversal_names() {
        let state = tempfile::tempdir().expect("state");
        let store = SessionFileStore::new(state.path());

        assert!(
            store
                .begin_upload("session", "../secret".into(), 1, "text/plain".into())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn pending_uploads_reserve_session_quota_and_release_on_drop() {
        let state = tempfile::tempdir().expect("state");
        let store = SessionFileStore::new(state.path());
        let mut pending = Vec::new();
        for index in 0..(MAX_SESSION_BYTES / MAX_FILE_BYTES) {
            pending.push(
                store
                    .begin_upload(
                        "session",
                        format!("{index}.bin"),
                        MAX_FILE_BYTES,
                        "application/octet-stream".into(),
                    )
                    .await
                    .expect("reserve upload"),
            );
        }

        assert!(
            store
                .begin_upload(
                    "session",
                    "overflow.bin".into(),
                    1,
                    "application/octet-stream".into(),
                )
                .await
                .is_err()
        );

        drop(pending.pop());
        assert!(
            store
                .begin_upload(
                    "session",
                    "replacement.bin".into(),
                    MAX_FILE_BYTES,
                    "application/octet-stream".into(),
                )
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn first_store_access_removes_crash_leftovers() {
        let state = tempfile::tempdir().expect("state");
        let store = SessionFileStore::new(state.path());
        let session_id = Uuid::new_v4().to_string();
        let session = store.session_dir(&session_id);
        std::fs::create_dir_all(&session).expect("session directory");
        let temporary = session.join(".tmp-upload");
        std::fs::write(&temporary, b"partial").expect("temporary upload");
        let staging = session.join(format!(".{}-partial", Uuid::new_v4()));
        std::fs::create_dir(&staging).expect("staging directory");
        std::fs::write(staging.join("payload"), b"partial").expect("staged file");

        assert!(
            store
                .list_uploads(&session_id)
                .await
                .expect("list")
                .is_empty()
        );
        assert!(!temporary.exists());
        assert!(!staging.exists());
    }

    fn blob_entries(store: &SessionFileStore) -> usize {
        std::fs::read_dir(store.blob_dir())
            .expect("blob directory")
            .count()
    }
}
