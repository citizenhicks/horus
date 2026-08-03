//! Gateway sandbox that keeps host credentials outside the agent's read boundary.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

#[cfg(target_os = "macos")]
use horus::backend::sandbox::MACOS_SEATBELT_BASE_POLICY;
use horus::backend::sandbox::{CommandOutput, NetworkAccess, SandboxBackend, local::LocalSandbox};
use horus::{BoxFuture, Error, Result};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;

#[cfg(target_os = "macos")]
use std::os::unix::fs::MetadataExt as _;

pub(crate) const MAX_COMMAND_OUTPUT_BYTES: usize = 40_000;
const GIT_SCRIPT: &str = r#"GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_NO_LAZY_FETCH=1 GIT_TERMINAL_PROMPT=0 GIT_OPTIONAL_LOCKS=0 exec "$0" --no-pager -c core.hooksPath=/dev/null -c core.fsmonitor=false "$@""#;

#[cfg(target_os = "macos")]
const SEATBELT_POLICY_SUFFIX: &str = r#"
(allow file-read-metadata)
(allow file-read*
  (literal "/")
  (literal (param "WORKSPACE_ROOT"))
  (subpath (param "WORKSPACE_ROOT"))
  (literal (param "TEMP_ROOT"))
  (subpath (param "TEMP_ROOT"))
  (subpath "/System")
  (subpath "/Library")
  (subpath "/Applications")
  (subpath "/bin")
  (subpath "/sbin")
  (subpath "/usr")
  (subpath "/opt/homebrew")
  (subpath "/private/etc")
  (subpath "/private/var/db/timezone")
  (subpath "/dev"))
(deny file-read*
  (literal (param "STATE_ROOT"))
  (subpath (param "STATE_ROOT"))
  (literal (param "TLS_KEY")))
(allow file-write*
  (subpath (param "TEMP_ROOT"))
  (require-all
    (subpath (param "WORKSPACE_ROOT"))
    (require-not (literal (param "GIT_PATH")))
    (require-not (subpath (param "GIT_PATH")))
    (require-not (literal (param "AGENTS_PATH")))
    (require-not (subpath (param "AGENTS_PATH")))
    (require-not (literal (param "CODEX_PATH")))
    (require-not (subpath (param "CODEX_PATH")))))
"#;

/// Workspace backend that denies gateway state even to sandboxed shell commands.
pub struct GatewaySandbox {
    delegate: LocalSandbox,
    root: PathBuf,
    state_dir: PathBuf,
    tls_key: PathBuf,
    #[cfg(target_os = "macos")]
    root_identity: (u64, u64),
    #[cfg(target_os = "linux")]
    bwrap: PathBuf,
    temp: tempfile::TempDir,
    command_timeout: Duration,
}

impl GatewaySandbox {
    /// Creates a fail-closed command sandbox for a gateway host.
    pub fn new(
        workspace: &Path,
        state_dir: &Path,
        tls_key: Option<&Path>,
        timeout: Duration,
    ) -> Result<Self> {
        if timeout.is_zero() {
            return Err(Error::Config("command timeout must be positive".into()));
        }
        let root = std::fs::canonicalize(workspace)?;
        let state_dir = std::fs::canonicalize(state_dir)?;
        let tls_key = match tls_key {
            Some(path) => std::fs::canonicalize(path)?,
            None => state_dir.clone(),
        };
        if root.starts_with(&state_dir) || state_dir.starts_with(&root) {
            return Err(Error::Config(
                "gateway state directory and chat workspace must not overlap".into(),
            ));
        }
        if tls_key.starts_with(&root) {
            return Err(Error::Config(
                "TLS private key must be stored outside every chat workspace".into(),
            ));
        }
        let delegate = LocalSandbox::new(&root)?.command_timeout(timeout)?;
        let temp = tempfile::Builder::new()
            .prefix("horus-gateway-")
            .tempdir()?;
        #[cfg(target_os = "linux")]
        let bwrap = find_bwrap(&root, &state_dir)?;
        #[cfg(target_os = "macos")]
        let metadata = std::fs::metadata(&root)?;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(Error::Config(
            "gateway command sandbox supports macOS and Linux only".into(),
        ));
        Ok(Self {
            delegate,
            root,
            state_dir,
            tls_key,
            #[cfg(target_os = "macos")]
            root_identity: (metadata.dev(), metadata.ino()),
            #[cfg(target_os = "linux")]
            bwrap,
            temp,
            command_timeout: timeout,
        })
    }

    #[cfg(target_os = "linux")]
    fn command(
        &self,
        script: &str,
        network_access: NetworkAccess,
        workspace_writable: bool,
    ) -> Result<Command> {
        let mut command = Command::new(&self.bwrap);
        command.args([
            "--new-session",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
        ]);
        if Path::new("/run").is_dir() {
            command.args(["--tmpfs", "/run"]);
        }
        command
            .arg(if workspace_writable {
                "--bind"
            } else {
                "--ro-bind"
            })
            .arg(&self.root)
            .arg(&self.root);
        if workspace_writable {
            for name in [".git", ".agents", ".codex"] {
                let path = self.root.join(name);
                if path.exists() {
                    command.arg("--ro-bind").arg(&path).arg(&path);
                }
            }
        }
        command.arg("--tmpfs").arg(&self.state_dir);
        if !self.tls_key.starts_with(&self.state_dir) {
            command.arg("--ro-bind").arg("/dev/null").arg(&self.tls_key);
        }
        command.args(["--unshare-user", "--unshare-pid"]);
        if network_access == NetworkAccess::Denied {
            command.arg("--unshare-net");
        }
        command.args(["--proc", "/proc", "--chdir"]);
        command.arg(&self.root);
        command.args(["--", "/bin/bash", "--noprofile", "--norc", "-c", script]);
        Ok(command)
    }

    #[cfg(target_os = "macos")]
    fn command(
        &self,
        script: &str,
        network_access: NetworkAccess,
        workspace_writable: bool,
    ) -> Result<Command> {
        let metadata = std::fs::metadata(&self.root)?;
        if (metadata.dev(), metadata.ino()) != self.root_identity {
            return Err(Error::Sandbox(
                "sandbox root changed after initialization".into(),
            ));
        }
        let temp = std::fs::canonicalize(self.temp.path())?;
        let mut command = Command::new("/usr/bin/sandbox-exec");
        let mut policy = format!("{MACOS_SEATBELT_BASE_POLICY}{SEATBELT_POLICY_SUFFIX}");
        if network_access == NetworkAccess::Allowed {
            policy.push_str("\n(allow network*)");
        }
        if !workspace_writable {
            policy.push_str(
                r#"
(deny file-write*
  (literal (param "WORKSPACE_ROOT"))
  (subpath (param "WORKSPACE_ROOT")))"#,
            );
        }
        command.arg("-p").arg(policy);
        for (name, path) in [
            ("WORKSPACE_ROOT", self.root.as_path()),
            ("STATE_ROOT", self.state_dir.as_path()),
            ("TLS_KEY", self.tls_key.as_path()),
            ("TEMP_ROOT", temp.as_path()),
            ("GIT_PATH", self.root.join(".git").as_path()),
            ("AGENTS_PATH", self.root.join(".agents").as_path()),
            ("CODEX_PATH", self.root.join(".codex").as_path()),
        ] {
            let path = path
                .to_str()
                .ok_or_else(|| Error::Sandbox("sandbox path is not UTF-8".into()))?;
            command.arg(format!("-D{name}={path}"));
        }
        command.args(["--", "/bin/bash", "--noprofile", "--norc", "-c", script]);
        Ok(command)
    }

    async fn execute_command(
        &self,
        script: &str,
        network_access: NetworkAccess,
    ) -> Result<CommandOutput> {
        if script.trim().is_empty() {
            return Err(Error::Sandbox("command is empty".into()));
        }
        let command = self.command(script, network_access, true)?;
        self.run_command(command).await
    }

    pub(crate) async fn execute_git(&self, args: &[&str]) -> Result<CommandOutput> {
        let git = find_executable("git", &self.root, &self.state_dir)?;
        let mut command = self.command(GIT_SCRIPT, NetworkAccess::Denied, false)?;
        command.arg(git).args(args);
        self.run_command(command).await
    }

    async fn run_command(&self, mut command: Command) -> Result<CommandOutput> {
        let inherited = [
            "PATH",
            "USER",
            "LOGNAME",
            "LANG",
            "LC_ALL",
            "TERM",
            "DEVELOPER_DIR",
            "SDKROOT",
        ]
        .into_iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (name, value)));
        command
            .current_dir(&self.root)
            .env_clear()
            .envs(inherited)
            .env("HOME", self.temp.path())
            .env("TMPDIR", self.temp.path())
            .env("SHELL", "/bin/bash")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let mut process_group = ProcessGroupGuard::new(&child)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Sandbox("command stdout unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Sandbox("command stderr unavailable".into()))?;
        let result = tokio::time::timeout(self.command_timeout, async {
            let (stdout, stderr, status) =
                tokio::join!(read_output(stdout), read_output(stderr), child.wait());
            Ok(CommandOutput {
                exit_code: status?.code().unwrap_or(-1),
                stdout: stdout?,
                stderr: stderr?,
            })
        })
        .await;
        process_group.kill();
        match result {
            Ok(output) => output,
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                Err(Error::Sandbox(format!(
                    "command exceeded {} seconds",
                    self.command_timeout.as_secs_f64()
                )))
            }
        }
    }
}

impl SandboxBackend for GatewaySandbox {
    fn read<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String>> {
        self.delegate.read(path)
    }

    fn write<'a>(&'a self, path: &'a str, content: &'a str) -> BoxFuture<'a, Result<()>> {
        self.delegate.write(path, content)
    }

    fn execute<'a>(
        &'a self,
        script: &'a str,
        network_access: NetworkAccess,
    ) -> BoxFuture<'a, Result<CommandOutput>> {
        Box::pin(self.execute_command(script, network_access))
    }
}

struct ProcessGroupGuard {
    id: u32,
    armed: bool,
}

impl ProcessGroupGuard {
    fn new(child: &tokio::process::Child) -> Result<Self> {
        let id = child
            .id()
            .ok_or_else(|| Error::Sandbox("command process ID unavailable".into()))?;
        Ok(Self { id, armed: true })
    }

    fn kill(&mut self) {
        if self.armed {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{}", self.id)])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            self.armed = false;
        }
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(target_os = "linux")]
fn find_bwrap(workspace: &Path, state_dir: &Path) -> Result<PathBuf> {
    find_executable("bwrap", workspace, state_dir)
        .map_err(|_| Error::Sandbox("bubblewrap (`bwrap`) is required on Linux".into()))
}

fn find_executable(name: &str, workspace: &Path, state_dir: &Path) -> Result<PathBuf> {
    let path =
        std::env::var_os("PATH").ok_or_else(|| Error::Sandbox("PATH is unavailable".into()))?;
    std::env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .filter_map(|directory| std::fs::canonicalize(directory.join(name)).ok())
        .find(|candidate| {
            candidate.is_file()
                && !candidate.starts_with(workspace)
                && !candidate.starts_with(state_dir)
        })
        .ok_or_else(|| Error::Sandbox(format!("{name} is unavailable outside protected paths")))
}

async fn read_output(mut reader: impl AsyncRead + Unpin) -> Result<String> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = MAX_COMMAND_OUTPUT_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(remaining)]);
        truncated |= read > remaining;
    }
    let mut output = String::from_utf8_lossy(&output).into_owned();
    if truncated {
        output.push_str("\n[output truncated]");
    }
    Ok(output)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn construction_rejects_both_state_workspace_overlap_directions() {
        let workspace_parent = tempfile::tempdir().expect("workspace parent");
        let state_inside = workspace_parent.path().join("state");
        std::fs::create_dir(&state_inside).expect("nested state");
        let state_parent = tempfile::tempdir().expect("state parent");
        let workspace_inside = state_parent.path().join("workspace");
        std::fs::create_dir(&workspace_inside).expect("nested workspace");

        let state_inside_error = match GatewaySandbox::new(
            workspace_parent.path(),
            &state_inside,
            None,
            Duration::from_secs(5),
        ) {
            Ok(_) => panic!("state inside workspace must fail"),
            Err(error) => error,
        };
        let workspace_inside_error = match GatewaySandbox::new(
            &workspace_inside,
            state_parent.path(),
            None,
            Duration::from_secs(5),
        ) {
            Ok(_) => panic!("workspace inside state must fail"),
            Err(error) => error,
        };

        assert!(state_inside_error.to_string().contains("must not overlap"));
        assert!(
            workspace_inside_error
                .to_string()
                .contains("must not overlap")
        );
    }

    #[test]
    fn construction_rejects_a_tls_key_inside_the_chat_workspace() {
        let workspace = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        let private_key = workspace.path().join("private-key.pem");
        std::fs::write(&private_key, "private key").expect("private key");

        let error = match GatewaySandbox::new(
            workspace.path(),
            state.path(),
            Some(&private_key),
            Duration::from_secs(5),
        ) {
            Ok(_) => panic!("workspace TLS key must fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("outside every chat workspace"));
    }

    #[tokio::test]
    async fn commands_cannot_read_gateway_state() {
        let workspace = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        std::fs::write(state.path().join("sentinel"), "gateway-secret").expect("state sentinel");
        let sandbox =
            GatewaySandbox::new(workspace.path(), state.path(), None, Duration::from_secs(5))
                .expect("gateway sandbox");
        let script = format!("cat {}/sentinel", state.path().display());

        let output = sandbox
            .execute(&script, NetworkAccess::Denied)
            .await
            .expect("blocked command still returns status");

        assert_ne!(output.exit_code, 0);
        assert!(!output.stdout.contains("gateway-secret"));
    }
}
