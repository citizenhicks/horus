//! Protected, session-bound storage shared by uploads and agent artifacts.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tempfile::TempPath;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};
use tokio::sync::{Mutex, OnceCell};
use uuid::Uuid;

use crate::protocol::SessionFileReference;
use crate::{Error, Result};

pub const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_SESSION_BYTES: u64 = 250 * 1024 * 1024;
pub const MAX_UPLOAD_CHUNK_BYTES: usize = 256 * 1024;
pub const MAX_READ_CHUNK_BYTES: usize = 256 * 1024;
const MAX_SESSION_FILES: usize = 128;
const MAX_SESSION_ID_BYTES: usize = 4 * 1024;
const METADATA_FILE: &str = ".session-file.json";
const PAYLOAD_FILE: &str = "payload";

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
    // ponytail: session files are small and infrequent; one lock keeps quota checks atomic.
    commits: Arc<Mutex<()>>,
    reservations: Arc<StdMutex<BTreeMap<String, ReservationTotals>>>,
    initialized: Arc<OnceCell<()>>,
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
}

impl SessionFileStore {
    /// Creates a store below the gateway's already protected state directory.
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            root: Arc::new(state_dir.join("session-files")),
            commits: Arc::new(Mutex::new(())),
            reservations: Arc::new(StdMutex::new(BTreeMap::new())),
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

    pub(crate) async fn read_upload_chunk(
        &self,
        session_id: &str,
        file_id: &str,
        offset: u64,
        max_bytes: usize,
    ) -> Result<SessionFileChunk> {
        let (file, path) = self.resolve_upload(session_id, file_id).await?;
        read_resolved_chunk(file, path, offset, max_bytes).await
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
        let existing = list_completed(&session_dir).await?;
        let reservation = self.reserve(session_id, size, &existing)?;
        let record = StoredSessionFile {
            origin,
            file: SessionFileReference {
                id: Uuid::new_v4().to_string(),
                name,
                size,
                media_type,
            },
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
        Ok(list_completed(&self.session_dir(session_id))
            .await?
            .into_iter()
            .filter(|record| record.origin == origin)
            .map(|record| record.file)
            .collect())
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
        validate_reference(&metadata.file)?;
        let path = directory.join(PAYLOAD_FILE);
        require_regular_file(&path).await?;
        if tokio::fs::metadata(&path).await?.len() != metadata.file.size {
            return Err(Error::Tool(
                "session file size does not match metadata".into(),
            ));
        }
        Ok((metadata, path))
    }

    async fn resolve_upload(
        &self,
        session_id: &str,
        file_id: &str,
    ) -> Result<(SessionFileReference, PathBuf)> {
        let (record, path) = self.resolve(session_id, file_id).await?;
        if record.origin != SessionFileOrigin::Upload {
            return Err(Error::Tool(
                "session file is not a file uploaded by the user".into(),
            ));
        }
        Ok((record.file, path))
    }

    fn session_dir(&self, session_id: &str) -> PathBuf {
        let digest = Sha256::digest(session_id.as_bytes());
        self.root
            .join(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest))
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
        let existing = list_completed(&session_dir).await?;
        self.store
            .validate_reserved_capacity(&self.session_id, &existing)?;

        let directory = session_dir.join(&self.record.file.id);
        let staging = session_dir.join(format!(".{}-partial", self.record.file.id));
        create_private_dir(&staging).await?;
        let destination = staging.join(PAYLOAD_FILE);
        let path = self
            .path
            .take()
            .ok_or_else(|| Error::Tool("session file temporary file is missing".into()))?;
        if let Err(error) = path.persist_noclobber(&destination) {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(error.error.into());
        }
        if let Err(error) = save_metadata(&staging, &self.record).await {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(error);
        }
        set_private_file(&destination).await?;
        if tokio::fs::symlink_metadata(&directory).await.is_ok() {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(Error::Tool("session file ID already exists".into()));
        }
        tokio::fs::rename(&staging, &directory).await?;
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
    let mut sessions = tokio::fs::read_dir(root).await?;
    while let Some(session) = sessions.next_entry().await? {
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
    Ok(())
}

async fn list_completed(session_dir: &Path) -> Result<Vec<StoredSessionFile>> {
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
        validate_reference(&record.file)?;
        if record.file.id != id {
            return Err(Error::Tool(
                "session file directory and metadata IDs differ".into(),
            ));
        }
        let path = entry.path().join(PAYLOAD_FILE);
        require_regular_file(&path).await?;
        if tokio::fs::metadata(path).await?.len() != record.file.size {
            return Err(Error::Tool(
                "session file size does not match metadata".into(),
            ));
        }
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

async fn require_regular_file(path: &Path) -> Result<()> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
        Ok(())
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
            for path in [directory.join(PAYLOAD_FILE), directory.join(METADATA_FILE)] {
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
        assert!(
            store
                .read_upload_chunk("session", &file.id, 0, 16)
                .await
                .is_err()
        );

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
        std::fs::write(staging.join(PAYLOAD_FILE), b"partial").expect("staged file");

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
}
