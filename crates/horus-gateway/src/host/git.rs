use std::path::{Component, Path};
use std::time::Duration;

use horus::backend::sandbox::CommandOutput;

use crate::sandbox::GatewaySandbox;
use crate::wire::{GitDiffScope, GitStatus};

use super::Rejection;

const MAX_GIT_DIFF_BYTES: usize = 400_000;
const TRUNCATION_NOTE: &[u8] = b"[diff truncated]\n";
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) async fn status(sandbox: &GatewaySandbox) -> Option<GitStatus> {
    tokio::time::timeout(GIT_TIMEOUT, status_inner(sandbox))
        .await
        .ok()?
        .ok()
}

pub(super) async fn switch_branch(
    sandbox: &GatewaySandbox,
    branch: &str,
) -> std::result::Result<(), Rejection> {
    tokio::time::timeout(GIT_TIMEOUT, switch_branch_inner(sandbox, branch))
        .await
        .map_err(|_| timeout())?
}

async fn status_inner(sandbox: &GatewaySandbox) -> std::result::Result<GitStatus, Rejection> {
    let (current, branches) = tokio::join!(
        output(sandbox, &["branch", "--show-current"]),
        output(
            sandbox,
            &["for-each-ref", "--format=%(refname)", "refs/heads/"]
        )
    );
    let current = successful_output(current?, "reading the current Git branch failed")?;
    let branches = successful_output(branches?, "listing local Git branches failed")?;
    let mut branches = String::from_utf8_lossy(&branches)
        .lines()
        .map(|branch| branch.strip_prefix("refs/heads/").map(str::to_owned))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(invalid_branch_output)?;
    branches.sort_unstable();
    branches.dedup();
    Ok(GitStatus {
        current_branch: String::from_utf8_lossy(&current).trim().into(),
        branches,
    })
}

async fn switch_branch_inner(
    sandbox: &GatewaySandbox,
    branch: &str,
) -> std::result::Result<(), Rejection> {
    if !status_inner(sandbox)
        .await?
        .branches
        .iter()
        .any(|candidate| candidate == branch)
    {
        return Err(unknown_branch());
    }
    let output = sandbox
        .switch_git_branch(branch)
        .await
        .map_err(error_rejection)?;
    if output.exit_code == 0 {
        Ok(())
    } else {
        Err(failure("switching Git branches failed", &output.stderr))
    }
}

pub(super) async fn diff(
    sandbox: &GatewaySandbox,
    workspace: &Path,
    scope: GitDiffScope,
) -> std::result::Result<String, Rejection> {
    tokio::time::timeout(GIT_TIMEOUT, diff_inner(sandbox, workspace, scope))
        .await
        .map_err(|_| timeout())?
}

async fn diff_inner(
    sandbox: &GatewaySandbox,
    workspace: &Path,
    scope: GitDiffScope,
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

    let mut diff = match scope {
        GitDiffScope::Staged => successful_output(
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
        )?,
        GitDiffScope::Unstaged => successful_output(
            output(
                sandbox,
                &["diff", "--no-ext-diff", "--no-color", "--no-textconv", "--"],
            )
            .await?,
            "unstaged git diff failed",
        )?,
        GitDiffScope::Committed => {
            let head = output(sandbox, &["rev-parse", "--verify", "--quiet", "HEAD"]).await?;
            if head.exit_code != 0 {
                Vec::new()
            } else {
                successful_output(
                    output(
                        sandbox,
                        &[
                            "show",
                            "--format=",
                            "--no-ext-diff",
                            "--no-color",
                            "--no-textconv",
                            "HEAD",
                            "--",
                        ],
                    )
                    .await?,
                    "committed git diff failed",
                )?
            }
        }
    };

    if scope == GitDiffScope::Unstaged {
        append_untracked(sandbox, workspace, &mut diff).await?;
    }

    truncate_diff(&mut diff);
    Ok(String::from_utf8_lossy(&diff).into_owned())
}

async fn append_untracked(
    sandbox: &GatewaySandbox,
    workspace: &Path,
    diff: &mut Vec<u8>,
) -> std::result::Result<(), Rejection> {
    let untracked = successful_output(
        output(
            sandbox,
            &["ls-files", "--others", "--exclude-standard", "-z", "--"],
        )
        .await?,
        "listing untracked files failed",
    )?;
    for path in untracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        if diff.len() >= MAX_GIT_DIFF_BYTES {
            break;
        }
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
            append_diff(diff, &patch);
        }
    }
    Ok(())
}

async fn output(
    sandbox: &GatewaySandbox,
    args: &[&str],
) -> std::result::Result<CommandOutput, Rejection> {
    sandbox.execute_git(args).await.map_err(error_rejection)
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

fn append_diff(target: &mut Vec<u8>, patch: &[u8]) {
    if patch.is_empty() {
        return;
    }
    if !target.is_empty() && !target.ends_with(b"\n") {
        target.push(b'\n');
    }
    target.extend_from_slice(patch);
}

/// A diff too large to send is still worth reading, so it is cut at the last whole line rather
/// than rejected. The sandbox permits a larger shared read-only output budget.
fn truncate_diff(diff: &mut Vec<u8>) {
    if diff.len() <= MAX_GIT_DIFF_BYTES {
        return;
    }
    let cut = diff[..MAX_GIT_DIFF_BYTES]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    diff.truncate(cut);
    diff.extend_from_slice(TRUNCATION_NOTE);
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
        message: "Git operation exceeded 5 seconds".into(),
        fatal: false,
    }
}

fn unknown_branch() -> Rejection {
    Rejection {
        code: "unknown_git_branch",
        message: "the requested Git branch is not a local branch".into(),
        fatal: false,
    }
}

fn invalid_branch_output() -> Rejection {
    Rejection {
        code: "git_error",
        message: "Git returned an invalid local branch".into(),
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

    fn initialize_repository(workspace: &Path, branch: &str) {
        run_git(workspace, &["init", "--quiet", "--initial-branch", branch]);
        run_git(
            workspace,
            &["config", "user.email", "horus@example.invalid"],
        );
        run_git(workspace, &["config", "user.name", "Horus Test"]);
        run_git(workspace, &["config", "commit.gpgsign", "false"]);
        std::fs::write(workspace.join("tracked.txt"), branch).expect("tracked file");
        run_git(workspace, &["add", "--", "tracked.txt"]);
        run_git(workspace, &["commit", "--quiet", "-m", "initial"]);
    }

    #[tokio::test]
    async fn git_status_lists_sorted_local_branches() {
        let workspace = tempfile::tempdir().expect("workspace");
        initialize_repository(workspace.path(), "middle");
        run_git(workspace.path(), &["branch", "zeta"]);
        run_git(workspace.path(), &["branch", "Alpha"]);
        let (_state, sandbox) = test_sandbox(workspace.path());

        let status = status(&sandbox).await.expect("Git status");

        assert_eq!(status.branches, ["Alpha", "middle", "zeta"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn branch_switch_uses_the_protected_git_sandbox() {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = tempfile::tempdir().expect("workspace");
        initialize_repository(workspace.path(), "main");
        run_git(workspace.path(), &["switch", "--quiet", "-c", "feature"]);
        run_git(
            workspace.path(),
            &[
                "config",
                "filter.horus.smudge",
                "sh -c 'touch .agents/filter-ran .codex/filter-ran 2>/dev/null; cat'",
            ],
        );
        std::fs::write(
            workspace.path().join(".gitattributes"),
            "filtered.txt filter=horus\n",
        )
        .expect("attributes");
        std::fs::write(workspace.path().join("filtered.txt"), "feature\n").expect("filtered file");
        run_git(workspace.path(), &["add", "--", "."]);
        run_git(workspace.path(), &["commit", "--quiet", "-m", "feature"]);
        run_git(workspace.path(), &["switch", "--quiet", "main"]);
        for directory in [".agents", ".codex"] {
            std::fs::create_dir(workspace.path().join(directory)).expect("protected directory");
            std::fs::write(
                workspace.path().join(directory).join("sentinel"),
                "protected",
            )
            .expect("protected sentinel");
        }
        let hook = workspace.path().join(".git/hooks/post-checkout");
        std::fs::write(&hook, "#!/bin/sh\ntouch hook-ran\n").expect("checkout hook");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .expect("hook permissions");
        let (_state, sandbox) = test_sandbox(workspace.path());

        switch_branch(&sandbox, "feature")
            .await
            .expect("switch branch");
        let status = status(&sandbox).await.expect("Git status");

        assert_eq!(
            (
                status.current_branch.as_str(),
                std::fs::read_to_string(workspace.path().join("filtered.txt"))
                    .expect("filtered file"),
                workspace.path().join("hook-ran").exists(),
                workspace.path().join(".agents/filter-ran").exists(),
                workspace.path().join(".codex/filter-ran").exists(),
            ),
            ("feature", "feature\n".into(), false, false, false)
        );
    }

    #[tokio::test]
    async fn branch_switch_rejects_names_outside_advertised_local_heads() {
        let workspace = tempfile::tempdir().expect("workspace");
        initialize_repository(workspace.path(), "main");
        run_git(workspace.path(), &["branch", "feature"]);
        let (_state, sandbox) = test_sandbox(workspace.path());

        let error = switch_branch(&sandbox, "feature ")
            .await
            .expect_err("unadvertised branch");

        assert_eq!(error.code, "unknown_git_branch");
    }

    #[tokio::test]
    async fn workspace_diff_keeps_staged_and_unstaged_scopes_separate() {
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

        let staged = diff(&sandbox, workspace.path(), GitDiffScope::Staged)
            .await
            .expect("staged diff");
        let unstaged = diff(&sandbox, workspace.path(), GitDiffScope::Unstaged)
            .await
            .expect("unstaged diff");

        assert!(
            staged.contains("+staged change")
                && !staged.contains("a/unstaged.txt")
                && unstaged.contains("+unstaged change")
                && unstaged.contains("+untracked content")
                && !unstaged.contains("a/staged.txt")
                && !unstaged.contains("ignored.txt")
                && !unstaged.contains("binary.bin"),
            "unexpected scoped diffs:\nstaged:\n{staged}\nunstaged:\n{unstaged}"
        );
    }

    #[tokio::test]
    async fn workspace_diff_is_empty_outside_a_git_repository() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (_state, sandbox) = test_sandbox(workspace.path());

        let diff = diff(&sandbox, workspace.path(), GitDiffScope::Staged)
            .await
            .expect("non-Git workspace");

        assert!(diff.is_empty());
    }

    #[tokio::test]
    async fn committed_scope_returns_the_head_patch() {
        let workspace = tempfile::tempdir().expect("workspace");
        initialize_repository(workspace.path(), "main");
        let (_state, sandbox) = test_sandbox(workspace.path());

        let diff = diff(&sandbox, workspace.path(), GitDiffScope::Committed)
            .await
            .expect("committed diff");

        assert!(diff.contains("diff --git a/tracked.txt b/tracked.txt"));
    }

    #[tokio::test]
    async fn workspace_diff_truncates_oversized_output_at_a_line_boundary() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (_state, sandbox) = test_sandbox(workspace.path());
        run_git(workspace.path(), &["init", "--quiet"]);
        std::fs::write(
            workspace.path().join("large.txt"),
            "x".repeat(MAX_GIT_DIFF_BYTES),
        )
        .expect("large untracked file");

        let diff = diff(&sandbox, workspace.path(), GitDiffScope::Unstaged)
            .await
            .expect("oversized diff");

        let note = String::from_utf8_lossy(TRUNCATION_NOTE);
        assert!(
            diff.len() <= MAX_GIT_DIFF_BYTES + note.len()
                && diff.ends_with(note.as_ref())
                && diff.contains("diff --git a/large.txt b/large.txt"),
            "unexpected truncation: {} bytes, tail {:?}",
            diff.len(),
            &diff[diff.len().saturating_sub(40)..]
        );
    }
}
