use std::path::{Component, Path};
use std::time::Duration;

use horus::backend::sandbox::CommandOutput;

use crate::sandbox::GatewaySandbox;
use crate::wire::GitStatus;

use super::Rejection;

const MAX_GIT_DIFF_BYTES: usize = 40_000;
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) async fn status(sandbox: &GatewaySandbox) -> Option<GitStatus> {
    let current = tokio::time::timeout(
        GIT_TIMEOUT,
        sandbox.execute_git(&["branch", "--show-current"]),
    )
    .await
    .ok()?
    .ok()?;
    if current.exit_code != 0
        || current.stdout.len() > MAX_GIT_DIFF_BYTES
        || current.stderr.len() > MAX_GIT_DIFF_BYTES
    {
        return None;
    }
    Some(GitStatus {
        current_branch: current.stdout.trim().into(),
    })
}

pub(super) async fn diff(
    sandbox: &GatewaySandbox,
    workspace: &Path,
) -> std::result::Result<String, Rejection> {
    tokio::time::timeout(GIT_TIMEOUT, diff_inner(sandbox, workspace))
        .await
        .map_err(|_| timeout())?
}

async fn diff_inner(
    sandbox: &GatewaySandbox,
    workspace: &Path,
) -> std::result::Result<String, Rejection> {
    let repository = output(sandbox, &["rev-parse", "--is-inside-work-tree"]).await?;
    if repository.exit_code != 0 {
        if repository.stderr.contains("not a git repository") {
            return Ok(String::new());
        }
        return Err(failure(
            "checking the Git workspace failed",
            &repository.stderr,
        ));
    }
    if repository.stdout != "true\n" {
        return Ok(String::new());
    }

    let head = output(sandbox, &["rev-parse", "--verify", "--quiet", "HEAD"]).await?;
    let mut diff = if head.exit_code == 0 {
        successful_output(
            output(
                sandbox,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-color",
                    "--no-textconv",
                    "HEAD",
                    "--",
                ],
            )
            .await?,
            "git diff failed",
        )?
    } else {
        let staged = successful_output(
            output(
                sandbox,
                &[
                    "diff",
                    "--cached",
                    "--no-ext-diff",
                    "--no-color",
                    "--no-textconv",
                    "--",
                ],
            )
            .await?,
            "staged git diff failed",
        )?;
        let unstaged = successful_output(
            output(
                sandbox,
                &["diff", "--no-ext-diff", "--no-color", "--no-textconv", "--"],
            )
            .await?,
            "unstaged git diff failed",
        )?;
        let mut diff = Vec::new();
        append_diff(&mut diff, &staged)?;
        append_diff(&mut diff, &unstaged)?;
        diff
    };
    if diff.len() > MAX_GIT_DIFF_BYTES {
        return Err(too_large());
    }

    let untracked = successful_output(
        output(
            sandbox,
            &["ls-files", "--others", "--exclude-standard", "-z", "--"],
        )
        .await?,
        "listing untracked files failed",
    )?;
    if untracked.len() > MAX_GIT_DIFF_BYTES {
        return Err(too_large());
    }
    for path in untracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).map_err(|_| invalid_path())?;
        let relative = Path::new(path);
        if !safe_path(relative) {
            return Err(invalid_path());
        }
        let metadata = match tokio::fs::symlink_metadata(workspace.join(relative)).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error_rejection(error)),
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        let patch = untracked_diff(sandbox, path).await?;
        if !is_binary_diff(&patch) {
            append_diff(&mut diff, &patch)?;
        }
    }

    Ok(String::from_utf8_lossy(&diff).into_owned())
}

async fn output(
    sandbox: &GatewaySandbox,
    args: &[&str],
) -> std::result::Result<CommandOutput, Rejection> {
    let output = sandbox.execute_git(args).await.map_err(error_rejection)?;
    if output.stdout.len() > MAX_GIT_DIFF_BYTES || output.stderr.len() > MAX_GIT_DIFF_BYTES {
        return Err(too_large());
    }
    Ok(output)
}

async fn untracked_diff(
    sandbox: &GatewaySandbox,
    path: &str,
) -> std::result::Result<Vec<u8>, Rejection> {
    let output = output(
        sandbox,
        &[
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--no-textconv",
            "--no-index",
            "--",
            "/dev/null",
            path,
        ],
    )
    .await?;
    if matches!(output.exit_code, 0 | 1) {
        Ok(output.stdout.into_bytes())
    } else {
        Err(failure("untracked git diff failed", &output.stderr))
    }
}

fn successful_output(
    output: CommandOutput,
    failure_message: &str,
) -> std::result::Result<Vec<u8>, Rejection> {
    if output.exit_code == 0 {
        Ok(output.stdout.into_bytes())
    } else {
        Err(failure(failure_message, &output.stderr))
    }
}

fn append_diff(target: &mut Vec<u8>, patch: &[u8]) -> std::result::Result<(), Rejection> {
    if patch.is_empty() {
        return Ok(());
    }
    let separator = usize::from(!target.is_empty() && !target.ends_with(b"\n"));
    if target
        .len()
        .saturating_add(separator)
        .saturating_add(patch.len())
        > MAX_GIT_DIFF_BYTES
    {
        return Err(too_large());
    }
    if separator == 1 {
        target.push(b'\n');
    }
    target.extend_from_slice(patch);
    Ok(())
}

fn safe_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn is_binary_diff(diff: &[u8]) -> bool {
    diff.split(|byte| *byte == b'\n')
        .any(|line| line.starts_with(b"Binary files "))
}

fn error_rejection(error: impl std::fmt::Display) -> Rejection {
    Rejection {
        code: "git_error",
        message: error.to_string(),
        fatal: false,
    }
}

fn failure(prefix: &str, stderr: &str) -> Rejection {
    let detail = stderr.trim();
    Rejection {
        code: "git_error",
        message: if detail.is_empty() {
            prefix.into()
        } else {
            format!("{prefix}: {detail}")
        },
        fatal: false,
    }
}

fn timeout() -> Rejection {
    Rejection {
        code: "git_timeout",
        message: "Git inspection exceeded 5 seconds".into(),
        fatal: false,
    }
}

fn too_large() -> Rejection {
    Rejection {
        code: "git_diff_too_large",
        message: format!("workspace Git diff exceeds {MAX_GIT_DIFF_BYTES} bytes"),
        fatal: false,
    }
}

fn invalid_path() -> Rejection {
    Rejection {
        code: "git_error",
        message: "Git returned an invalid untracked path".into(),
        fatal: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(workspace: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .env("LC_ALL", "C")
            .current_dir(workspace)
            .output()
            .expect("run Git");
        assert!(
            output.status.success(),
            "Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn test_sandbox(workspace: &Path) -> (tempfile::TempDir, GatewaySandbox) {
        let state = tempfile::tempdir().expect("state");
        let sandbox =
            GatewaySandbox::new(workspace, state.path(), None, GIT_TIMEOUT).expect("Git sandbox");
        (state, sandbox)
    }

    #[tokio::test]
    async fn workspace_diff_includes_staged_unstaged_and_untracked_text() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (_state, sandbox) = test_sandbox(workspace.path());
        run_git(workspace.path(), &["init", "--quiet"]);
        run_git(
            workspace.path(),
            &["config", "user.email", "horus@example.invalid"],
        );
        run_git(workspace.path(), &["config", "user.name", "Horus Test"]);
        run_git(workspace.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::write(workspace.path().join("staged.txt"), "before\n").expect("staged file");
        std::fs::write(workspace.path().join("unstaged.txt"), "before\n").expect("unstaged file");
        std::fs::write(workspace.path().join(".gitignore"), "ignored.txt\n").expect("ignore file");
        run_git(workspace.path(), &["add", "--", "."]);
        run_git(workspace.path(), &["commit", "--quiet", "-m", "initial"]);

        std::fs::write(workspace.path().join("staged.txt"), "staged change\n")
            .expect("change staged file");
        run_git(workspace.path(), &["add", "--", "staged.txt"]);
        std::fs::write(workspace.path().join("unstaged.txt"), "unstaged change\n")
            .expect("change unstaged file");
        std::fs::write(workspace.path().join("new.txt"), "untracked content\n")
            .expect("untracked file");
        std::fs::write(workspace.path().join("ignored.txt"), "ignored\n").expect("ignored file");
        std::fs::write(workspace.path().join("binary.bin"), [0, 1, 2]).expect("binary file");

        let diff = diff(&sandbox, workspace.path())
            .await
            .expect("workspace diff");

        assert!(
            diff.contains("diff --git a/staged.txt b/staged.txt")
                && diff.contains("+staged change")
                && diff.contains("diff --git a/unstaged.txt b/unstaged.txt")
                && diff.contains("+unstaged change")
                && diff.contains("diff --git a/new.txt b/new.txt")
                && diff.contains("--- /dev/null")
                && diff.contains("+untracked content")
                && !diff.contains("ignored.txt")
                && !diff.contains("binary.bin"),
            "unexpected diff:\n{diff}"
        );
    }

    #[tokio::test]
    async fn workspace_diff_is_empty_outside_a_git_repository() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (_state, sandbox) = test_sandbox(workspace.path());

        let diff = diff(&sandbox, workspace.path())
            .await
            .expect("non-Git workspace");

        assert!(diff.is_empty());
    }

    #[tokio::test]
    async fn workspace_diff_rejects_oversized_output() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (_state, sandbox) = test_sandbox(workspace.path());
        run_git(workspace.path(), &["init", "--quiet"]);
        std::fs::write(
            workspace.path().join("large.txt"),
            "x".repeat(MAX_GIT_DIFF_BYTES),
        )
        .expect("large untracked file");

        let error = diff(&sandbox, workspace.path())
            .await
            .expect_err("oversized diff");

        assert_eq!(error.code, "git_diff_too_large");
    }
}
