use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::wire::WorkspaceFileRecord;

use super::Rejection;

pub(super) const MAX_WORKSPACE_READ_BYTES: usize = 256 * 1024;
const MAX_WORKSPACE_FILES: usize = 10_000;
const MAX_WORKSPACE_DIRECTORIES: usize = 10_000;
const MAX_WORKSPACE_PATH_BYTES: usize = 1024 * 1024;
const FILE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct WorkspaceRead {
    pub(crate) data: Vec<u8>,
    pub(crate) next_offset: Option<u64>,
}

pub(super) async fn list(
    workspace: &Path,
) -> std::result::Result<Vec<WorkspaceFileRecord>, Rejection> {
    tokio::time::timeout(FILE_TIMEOUT, list_inner(workspace))
        .await
        .map_err(|_| timeout())?
}

pub(super) async fn read(
    workspace: &Path,
    path: &str,
    offset: u64,
    max_bytes: usize,
) -> std::result::Result<WorkspaceRead, Rejection> {
    if max_bytes == 0 || max_bytes > MAX_WORKSPACE_READ_BYTES {
        return Err(invalid(format!(
            "workspace read size must be 1–{MAX_WORKSPACE_READ_BYTES} bytes"
        )));
    }
    let relative = validate_relative(path)?;
    let (canonical, size) = resolve_regular(workspace, relative).await?;
    if offset > size {
        return Err(invalid("workspace file offset exceeds its size"));
    }
    let mut file = tokio::fs::File::open(canonical)
        .await
        .map_err(error_rejection)?;
    use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(error_rejection)?;
    let length = usize::try_from(size.saturating_sub(offset).min(max_bytes as u64))
        .map_err(|_| invalid("workspace file range is unsupported"))?;
    let mut data = vec![0; length];
    file.read_exact(&mut data).await.map_err(error_rejection)?;
    let end = offset.saturating_add(length as u64);
    Ok(WorkspaceRead {
        data,
        next_offset: (end < size).then_some(end),
    })
}

async fn list_inner(workspace: &Path) -> std::result::Result<Vec<WorkspaceFileRecord>, Rejection> {
    let workspace = workspace.to_path_buf();
    let mut files = tokio::task::spawn_blocking(move || walk_workspace(&workspace))
        .await
        .map_err(error_rejection)??;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn walk_workspace(workspace: &Path) -> std::result::Result<Vec<WorkspaceFileRecord>, Rejection> {
    let root = std::fs::symlink_metadata(workspace).map_err(error_rejection)?;
    if !root.file_type().is_dir() || root.file_type().is_symlink() {
        return Err(invalid("workspace root is not a regular directory"));
    }
    let mut directories = vec![workspace.to_path_buf()];
    let mut files = Vec::new();
    let mut directory_count = 1_usize;
    let mut path_bytes = 0_usize;
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory).map_err(error_rejection)? {
            let entry = entry.map_err(error_rejection)?;
            let file_type = entry.file_type().map_err(error_rejection)?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if entry.file_name() == ".git" {
                    continue;
                }
                directory_count = directory_count.saturating_add(1);
                if directory_count > MAX_WORKSPACE_DIRECTORIES {
                    return Err(limit());
                }
                path_bytes = add_path_bytes(workspace, &entry.path(), path_bytes)?;
                directories.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let path = relative_utf8(workspace, &entry.path())?.to_owned();
            path_bytes = path_bytes.saturating_add(path.len());
            if path_bytes > MAX_WORKSPACE_PATH_BYTES {
                return Err(limit());
            }
            let size = entry.metadata().map_err(error_rejection)?.len();
            files.push(WorkspaceFileRecord { path, size });
            if files.len() > MAX_WORKSPACE_FILES {
                return Err(limit());
            }
        }
    }
    Ok(files)
}

fn add_path_bytes(
    workspace: &Path,
    path: &Path,
    current: usize,
) -> std::result::Result<usize, Rejection> {
    let total = current.saturating_add(relative_utf8(workspace, path)?.len());
    if total > MAX_WORKSPACE_PATH_BYTES {
        return Err(limit());
    }
    Ok(total)
}

fn relative_utf8<'a>(workspace: &Path, path: &'a Path) -> std::result::Result<&'a str, Rejection> {
    path.strip_prefix(workspace)
        .map_err(error_rejection)?
        .to_str()
        .ok_or_else(|| invalid("workspace contains a non-UTF-8 path"))
}

fn validate_relative(path: &str) -> std::result::Result<&Path, Rejection> {
    let relative = Path::new(path);
    let safe = relative.components().all(|component| {
        matches!(component, Component::Normal(value) if value != std::ffi::OsStr::new(".git"))
    });
    if path.is_empty() || path.len() > 4096 || !safe {
        return Err(invalid("workspace file path must be a safe relative path"));
    }
    Ok(relative)
}

async fn resolve_regular(
    workspace: &Path,
    relative: &Path,
) -> std::result::Result<(PathBuf, u64), Rejection> {
    let candidate = workspace.join(relative);
    let metadata = match tokio::fs::symlink_metadata(&candidate).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Rejection {
                code: "workspace_file_missing",
                message: "workspace file no longer exists".into(),
                fatal: false,
            });
        }
        Err(error) => return Err(error_rejection(error)),
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("workspace path is not a regular file"));
    }
    let root = tokio::fs::canonicalize(workspace)
        .await
        .map_err(error_rejection)?;
    let canonical = tokio::fs::canonicalize(candidate)
        .await
        .map_err(error_rejection)?;
    if !canonical.starts_with(&root) {
        return Err(invalid(
            "workspace file resolves outside the selected workspace",
        ));
    }
    Ok((canonical, metadata.len()))
}

fn invalid(message: impl Into<String>) -> Rejection {
    Rejection {
        code: "invalid_workspace_file",
        message: message.into(),
        fatal: false,
    }
}

fn error_rejection(error: impl std::fmt::Display) -> Rejection {
    Rejection {
        code: "workspace_file_error",
        message: error.to_string(),
        fatal: false,
    }
}

fn timeout() -> Rejection {
    Rejection {
        code: "workspace_file_timeout",
        message: "workspace file operation exceeded 5 seconds".into(),
        fatal: false,
    }
}

fn limit() -> Rejection {
    Rejection {
        code: "workspace_file_limit",
        message: "workspace file catalog exceeds its bounded result limit".into(),
        fatal: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_rejects_parent_traversal() {
        let workspace = tempfile::tempdir().expect("workspace");

        let result = read(workspace.path(), "../outside", 0, 16).await;

        assert!(matches!(
            result,
            Err(Rejection {
                code: "invalid_workspace_file",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn list_includes_ignored_files_but_excludes_git_internals() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::create_dir(workspace.path().join(".git")).expect("git directory");
        std::fs::write(workspace.path().join(".git/config"), b"secret").expect("git config");
        std::fs::write(workspace.path().join(".gitignore"), b"ignored.txt\n")
            .expect("ignore rules");
        std::fs::write(workspace.path().join("ignored.txt"), b"included").expect("ignored file");

        let files = list(workspace.path()).await.expect("workspace files");

        assert!(files.iter().any(|file| file.path == ".gitignore"));
        assert!(files.iter().any(|file| file.path == "ignored.txt"));
        assert!(files.iter().all(|file| !file.path.starts_with(".git/")));
        assert!(read(workspace.path(), ".git/config", 0, 16).await.is_err());
    }
}
