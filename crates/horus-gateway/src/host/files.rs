use std::path::{Component, Path};
use std::time::Duration;

use crate::sandbox::GatewaySandbox;
use crate::wire::WorkspaceFileRecord;

use super::Rejection;

pub(super) const MAX_WORKSPACE_READ_BYTES: usize = 256 * 1024;
const MAX_WORKSPACE_FILES: usize = 20_000;
const MAX_WORKSPACE_DIRECTORIES: usize = 10_000;
const MAX_WORKSPACE_PATH_BYTES: usize = 1024 * 1024;
const FILE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct WorkspaceRead {
    pub(crate) data: Vec<u8>,
    pub(crate) next_offset: Option<u64>,
}

pub(super) async fn list(
    sandbox: &GatewaySandbox,
    workspace: &Path,
) -> std::result::Result<Vec<WorkspaceFileRecord>, Rejection> {
    tokio::time::timeout(FILE_TIMEOUT, list_inner(sandbox, workspace))
        .await
        .map_err(|_| timeout())?
}

pub(super) async fn read(
    sandbox: &GatewaySandbox,
    path: &str,
    offset: u64,
    max_bytes: usize,
) -> std::result::Result<WorkspaceRead, Rejection> {
    if max_bytes == 0 || max_bytes > MAX_WORKSPACE_READ_BYTES {
        return Err(invalid(format!(
            "workspace read size must be 1–{MAX_WORKSPACE_READ_BYTES} bytes"
        )));
    }
    validate_relative(path)?;
    let (data, next_offset) = sandbox
        .read_workspace_range(path, offset, max_bytes)
        .await
        .map_err(error_rejection)?;
    Ok(WorkspaceRead { data, next_offset })
}

async fn list_inner(
    sandbox: &GatewaySandbox,
    workspace: &Path,
) -> std::result::Result<Vec<WorkspaceFileRecord>, Rejection> {
    let output = sandbox
        .execute_git(&[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
        ])
        .await
        .map_err(error_rejection)?;
    if output.exit_code == 0 {
        let workspace = workspace.to_path_buf();
        return tokio::task::spawn_blocking(move || {
            git_workspace_files(&workspace, &output.stdout)
        })
        .await
        .map_err(error_rejection)?;
    }
    if !output.stderr.contains("not a git repository") {
        return Err(error_rejection(format!(
            "listing Git workspace files failed: {}",
            output.stderr.trim()
        )));
    }
    let workspace = workspace.to_path_buf();
    let mut files = tokio::task::spawn_blocking(move || walk_workspace(&workspace))
        .await
        .map_err(error_rejection)??;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn git_workspace_files(
    workspace: &Path,
    output: &str,
) -> std::result::Result<Vec<WorkspaceFileRecord>, Rejection> {
    if !output.is_empty() && !output.ends_with('\0') {
        return Err(invalid(
            "Git workspace file output is truncated or malformed",
        ));
    }
    let mut files = Vec::new();
    let mut path_bytes = 0_usize;
    for path in output.split_terminator('\0') {
        let relative = validate_relative(path)?;
        let metadata = match std::fs::symlink_metadata(workspace.join(relative)) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error_rejection(error)),
        };
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        path_bytes = path_bytes.saturating_add(path.len());
        if path_bytes > MAX_WORKSPACE_PATH_BYTES {
            return Err(limit());
        }
        files.push(WorkspaceFileRecord {
            path: path.into(),
            size: metadata.len(),
        });
        if files.len() > MAX_WORKSPACE_FILES {
            return Err(limit());
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files.dedup_by(|left, right| left.path == right.path);
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

    fn sandbox(workspace: &Path) -> (tempfile::TempDir, GatewaySandbox) {
        let state = tempfile::tempdir().expect("state");
        let sandbox = GatewaySandbox::new(workspace, state.path(), None, Duration::from_secs(5))
            .expect("gateway sandbox");
        (state, sandbox)
    }

    #[tokio::test]
    async fn read_rejects_parent_traversal() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (_state, sandbox) = sandbox(workspace.path());

        let result = read(&sandbox, "../outside", 0, 16).await;

        assert!(matches!(
            result,
            Err(Rejection {
                code: "invalid_workspace_file",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn git_catalog_excludes_ignored_directories_and_git_internals() {
        let workspace = tempfile::tempdir().expect("workspace");
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(workspace.path())
            .status()
            .expect("initialize Git repository");
        assert!(status.success());
        std::fs::write(workspace.path().join(".gitignore"), b"ignored/\n").expect("ignore rules");
        std::fs::create_dir(workspace.path().join("ignored")).expect("ignored directory");
        std::fs::write(workspace.path().join("ignored/generated.txt"), b"ignored")
            .expect("ignored file");
        std::fs::write(workspace.path().join("included.txt"), b"included").expect("included file");
        let (_state, sandbox) = sandbox(workspace.path());

        let files = list(&sandbox, workspace.path())
            .await
            .expect("workspace files");

        assert!(files.iter().any(|file| file.path == ".gitignore"));
        assert!(files.iter().any(|file| file.path == "included.txt"));
        assert!(files.iter().all(|file| !file.path.starts_with("ignored/")));
        assert!(files.iter().all(|file| !file.path.starts_with(".git/")));
        assert!(read(&sandbox, ".git/config", 0, 16).await.is_err());
    }

    #[tokio::test]
    async fn non_git_catalog_falls_back_to_the_bounded_walk() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::create_dir(workspace.path().join("nested")).expect("nested directory");
        std::fs::write(workspace.path().join("nested/file.txt"), b"included")
            .expect("included file");
        let (_state, sandbox) = sandbox(workspace.path());

        let files = list(&sandbox, workspace.path())
            .await
            .expect("workspace files");

        assert!(files.iter().any(|file| file.path == "nested/file.txt"));
    }

    #[test]
    fn git_catalog_rejects_non_terminated_output() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), b"file").expect("file");

        assert!(git_workspace_files(workspace.path(), "file.txt").is_err());
    }
}
