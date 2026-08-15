//! Local filesystem adapter with policy-selected command isolation.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::NetworkAccess;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::ProcessGroupGuard;
use super::SandboxBackend;
use super::SandboxMode;
use super::{
    CommandMode, CommandOutput, CommandOutputSink, CommandStream, MAX_BINARY_FILE_BYTES,
    MAX_FILE_BYTES,
};
#[cfg(target_os = "macos")]
use super::{MACOS_COMMAND_WRAPPER, MACOS_SEATBELT_BASE_POLICY, MACOS_SEATBELT_NETWORK_POLICY};
use crate::BoxFuture;
use crate::Error;
use crate::Result;

#[path = "local_files.rs"]
mod files;

use self::files::atomic_write;
use self::files::read_binary_file;
use self::files::read_file;
use self::files::read_file_range;

const MAX_COMMAND_OUTPUT_BYTES: usize = 40_000;
/// Read-only inspection feeds a UI rather than a model context, so it keeps a larger budget.
const MAX_READ_ONLY_OUTPUT_BYTES: usize = 1024 * 1024;
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const ISOLATED_ENVIRONMENT: [&str; 8] = [
    "PATH",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "TERM",
    "DEVELOPER_DIR",
    "SDKROOT",
];
#[cfg(target_os = "linux")]
const ISOLATED_HOME: &str = "/tmp/horus-home";
#[cfg(target_os = "macos")]
const SEATBELT_POLICY_SUFFIX: &str = r#"
(allow file-read*)
(allow file-write*
  (subpath (param "TEMP_ROOT"))
  (subpath (param "WRITABLE_ROOT")))
"#;

/// Provides capability-safe file tools and policy-selected command execution.
pub struct LocalSandbox {
    root: PathBuf,
    root_dir: Dir,
    temp: tempfile::TempDir,
    command_timeout: Duration,
    denied_reads: Vec<DeniedRead>,
    denied_environment: BTreeSet<String>,
    isolated_home: bool,
}

struct DeniedRead {
    path: PathBuf,
    directory: bool,
}

enum Invocation<'a> {
    Shell(&'a str),
    Argv {
        executable: &'a Path,
        arguments: &'a [&'a str],
    },
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn append_invocation(command: &mut Command, invocation: &Invocation<'_>, isolated_home: bool) {
    match invocation {
        Invocation::Shell(script) => {
            command.arg("/bin/bash");
            if isolated_home {
                command.args(["--noprofile", "--norc", "-c", script]);
            } else {
                command.args(["-lc", script]);
            }
        }
        Invocation::Argv {
            executable,
            arguments,
        } => {
            command.arg(executable).args(arguments.iter().copied());
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceAccess {
    ReadOnly,
    Writable,
}

#[derive(Clone, Copy)]
struct CommandIsolation {
    sandbox_mode: SandboxMode,
    network_access: NetworkAccess,
}

impl LocalSandbox {
    /// Creates a local sandbox rooted at an existing directory.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = root;
            Err(Error::Config(
                "the local sandbox requires a Unix platform".into(),
            ))
        }
        #[cfg(unix)]
        {
            let root = std::fs::canonicalize(root)?;
            if !root.is_dir() {
                return Err(Error::Config(format!(
                    "sandbox root is not a directory: {}",
                    root.display()
                )));
            }
            let root_dir = Dir::open_ambient_dir(&root, ambient_authority())?;
            validate_root(&root, &root_dir)?;
            let temp = tempfile::Builder::new().prefix("horus-").tempdir()?;
            Ok(Self {
                root,
                root_dir,
                temp,
                command_timeout: DEFAULT_COMMAND_TIMEOUT,
                denied_reads: Vec::new(),
                denied_environment: BTreeSet::new(),
                isolated_home: false,
            })
        }
    }

    /// Sets the hard timeout applied to each sandboxed command.
    pub fn command_timeout(mut self, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() {
            return Err(Error::Config("command timeout must be positive".into()));
        }
        self.command_timeout = timeout;
        Ok(self)
    }

    /// Hides one canonical file or directory from sandboxed commands.
    pub fn deny_read(mut self, path: impl AsRef<Path>) -> Result<Self> {
        let path = std::fs::canonicalize(path)?;
        if paths_overlap(&self.root, &path) {
            return Err(Error::Config(
                "sandbox root and denied read path must not overlap".into(),
            ));
        }
        let metadata = std::fs::metadata(&path)?;
        if !metadata.is_dir() && !metadata.is_file() {
            return Err(Error::Config(format!(
                "denied read path is not a file or directory: {}",
                path.display()
            )));
        }
        if self
            .denied_reads
            .iter()
            .any(|denied| path == denied.path || path.starts_with(&denied.path))
        {
            return Ok(self);
        }
        if metadata.is_dir() {
            self.denied_reads
                .retain(|denied| !denied.path.starts_with(&path));
        }
        self.denied_reads.push(DeniedRead {
            path,
            directory: metadata.is_dir(),
        });
        Ok(self)
    }

    /// Removes one inherited environment variable from every command.
    #[must_use]
    pub fn deny_environment(mut self, name: impl Into<String>) -> Self {
        self.denied_environment.insert(name.into());
        self
    }

    /// Uses a private writable home and excludes host user configuration variables.
    #[must_use]
    pub fn isolated_home(mut self) -> Self {
        self.isolated_home = true;
        self
    }

    /// Runs one argv command with a read-only workspace and no network access.
    pub async fn execute_read_only(
        &self,
        executable: &str,
        arguments: &[&str],
        environment: &[(&str, &str)],
    ) -> Result<CommandOutput> {
        let executable = self.find_executable(executable)?;
        self.execute_invocation(
            Invocation::Argv {
                executable: &executable,
                arguments,
            },
            CommandIsolation {
                sandbox_mode: SandboxMode::WorkspaceWrite,
                network_access: NetworkAccess::Denied,
            },
            CommandMode::Foreground,
            CommandOutputSink::default(),
            environment,
            WorkspaceAccess::ReadOnly,
        )
        .await
    }

    /// Reads one bounded binary range through the sandbox's pinned workspace root.
    pub async fn read_range(
        &self,
        path: &str,
        offset: u64,
        max_bytes: usize,
    ) -> Result<(Vec<u8>, Option<u64>)> {
        if max_bytes == 0 || max_bytes > MAX_FILE_BYTES {
            return Err(Error::Sandbox(format!(
                "file read size must be 1–{MAX_FILE_BYTES} bytes"
            )));
        }
        validate_root(&self.root, &self.root_dir)?;
        let root = self.root_dir.try_clone()?;
        let relative = self.relative(path)?;
        let requested = path.to_string();
        tokio::task::spawn_blocking(move || {
            read_file_range(root, &relative, &requested, offset, max_bytes)
        })
        .await
        .map_err(|error| Error::Sandbox(format!("file reader failed: {error}")))?
    }

    /// Runs Git argv with a writable workspace and no network access.
    pub async fn execute_git_mutation(
        &self,
        arguments: &[&str],
        environment: &[(&str, &str)],
    ) -> Result<CommandOutput> {
        let executable = self.find_executable("git")?;
        self.execute_invocation(
            Invocation::Argv {
                executable: &executable,
                arguments,
            },
            CommandIsolation {
                sandbox_mode: SandboxMode::WorkspaceWrite,
                network_access: NetworkAccess::Denied,
            },
            CommandMode::Foreground,
            CommandOutputSink::default(),
            environment,
            WorkspaceAccess::Writable,
        )
        .await
    }

    fn relative(&self, path: &str) -> Result<PathBuf> {
        let path = Path::new(path);
        if path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err(Error::Sandbox(path.display().to_string()));
        }
        Ok(path.to_path_buf())
    }

    fn find_executable(&self, name: &str) -> Result<PathBuf> {
        if name.is_empty() || Path::new(name).file_name() != Some(OsStr::new(name)) {
            return Err(Error::Sandbox(format!("invalid executable name: {name}")));
        }
        let path =
            std::env::var_os("PATH").ok_or_else(|| Error::Sandbox("PATH is unavailable".into()))?;
        find_executable_in(name, &self.root, &self.denied_reads, &path)
            .ok_or_else(|| Error::Sandbox(format!("{name} is unavailable outside protected paths")))
    }

    #[cfg(target_os = "linux")]
    fn sandboxed_command(
        &self,
        invocation: &Invocation<'_>,
        network_access: NetworkAccess,
        workspace_access: WorkspaceAccess,
    ) -> Result<Command> {
        let bwrap = self
            .find_executable("bwrap")
            .map_err(|_| Error::Sandbox("bubblewrap (`bwrap`) is required on Linux".into()))?;
        let mut command = Command::new(bwrap);
        command.args([
            "--new-session",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
        ]);
        command.args(["--tmpfs", "/tmp"]);
        if self.isolated_home {
            command.args(["--dir", ISOLATED_HOME]);
        }
        if network_access == NetworkAccess::Denied && Path::new("/run").is_dir() {
            command.args(["--tmpfs", "/run"]);
        }
        command
            .arg(if workspace_access == WorkspaceAccess::ReadOnly {
                "--ro-bind"
            } else {
                "--bind"
            })
            .arg(&self.root)
            .arg(&self.root);
        for denied in &self.denied_reads {
            if denied.directory {
                command.arg("--tmpfs").arg(&denied.path);
            } else {
                command.arg("--ro-bind").arg("/dev/null").arg(&denied.path);
            }
        }
        command.args(["--unshare-user", "--unshare-pid"]);
        if network_access == NetworkAccess::Denied {
            command.arg("--unshare-net");
        }
        command.args(["--proc", "/proc", "--chdir"]);
        command.arg(&self.root);
        command.arg("--");
        append_invocation(&mut command, invocation, self.isolated_home);
        Ok(command)
    }

    fn host_command(&self, invocation: &Invocation<'_>) -> Command {
        match invocation {
            Invocation::Shell(script) => {
                let mut command = Command::new("/bin/bash");
                if self.isolated_home {
                    command.args(["--noprofile", "--norc", "-c", script]);
                } else {
                    command.args(["-lc", script]);
                }
                command
            }
            Invocation::Argv {
                executable,
                arguments,
            } => {
                let mut command = Command::new(executable);
                command.args(arguments.iter().copied());
                command
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn protected_full_access_command(&self, invocation: &Invocation<'_>) -> Result<Command> {
        let bwrap = self
            .find_executable("bwrap")
            .map_err(|_| Error::Sandbox("bubblewrap (`bwrap`) is required on Linux".into()))?;
        let mut command = Command::new(bwrap);
        command.args([
            "--new-session",
            "--die-with-parent",
            "--bind",
            "/",
            "/",
            "--dev-bind",
            "/dev",
            "/dev",
        ]);
        for denied in &self.denied_reads {
            if denied.directory {
                command.arg("--tmpfs").arg(&denied.path);
            } else {
                command.arg("--ro-bind").arg("/dev/null").arg(&denied.path);
            }
        }
        command.args([
            "--unshare-user",
            "--unshare-pid",
            "--proc",
            "/proc",
            "--chdir",
        ]);
        command.arg(&self.root).arg("--");
        append_invocation(&mut command, invocation, self.isolated_home);
        Ok(command)
    }

    #[cfg(target_os = "macos")]
    fn protected_full_access_command(&self, invocation: &Invocation<'_>) -> Result<Command> {
        let executable = Path::new("/usr/bin/sandbox-exec");
        if !executable.is_file() {
            return Err(Error::Sandbox(
                "/usr/bin/sandbox-exec is unavailable".into(),
            ));
        }
        let mut policy = String::from(
            "(version 1)\n(allow default)\n\
             (deny signal (require-not (target same-sandbox)))\n\
             (deny process-info* (require-not (target same-sandbox)))\n\
             (deny mach-task-name (require-not (target same-sandbox)))\n",
        );
        for (index, denied) in self.denied_reads.iter().enumerate() {
            let parameter = format!("DENIED_READ_{index}");
            policy.push_str(&format!(
                "\n(deny file-read* file-write*\n  (literal (param \"{parameter}\")){}\n)",
                if denied.directory {
                    format!("\n  (subpath (param \"{parameter}\"))")
                } else {
                    String::new()
                }
            ));
        }
        let mut command = Command::new(executable);
        command.arg("-p").arg(policy);
        for (index, denied) in self.denied_reads.iter().enumerate() {
            let path = denied
                .path
                .to_str()
                .ok_or_else(|| Error::Sandbox("sandbox path is not UTF-8".into()))?;
            command.arg(format!("-DDENIED_READ_{index}={path}"));
        }
        command.args([
            "--",
            "/bin/bash",
            "--noprofile",
            "--norc",
            "-c",
            MACOS_COMMAND_WRAPPER,
            "horus-command",
        ]);
        append_invocation(&mut command, invocation, self.isolated_home);
        Ok(command)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn protected_full_access_command(&self, _invocation: &Invocation<'_>) -> Result<Command> {
        Err(Error::Sandbox(
            "protected full-access execution requires Linux or macOS".into(),
        ))
    }

    #[cfg(target_os = "macos")]
    fn sandboxed_command(
        &self,
        invocation: &Invocation<'_>,
        network_access: NetworkAccess,
        workspace_access: WorkspaceAccess,
    ) -> Result<Command> {
        let executable = Path::new("/usr/bin/sandbox-exec");
        if !executable.is_file() {
            return Err(Error::Sandbox(
                "/usr/bin/sandbox-exec is unavailable".into(),
            ));
        }
        let temp = std::fs::canonicalize(self.temp.path())?;
        let mut command = Command::new(executable);
        let mut policy = format!("{MACOS_SEATBELT_BASE_POLICY}{SEATBELT_POLICY_SUFFIX}");
        for (index, denied) in self.denied_reads.iter().enumerate() {
            let parameter = format!("DENIED_READ_{index}");
            policy.push_str(&format!(
                "\n(deny file-read*\n  (literal (param \"{parameter}\")){}\n)",
                if denied.directory {
                    format!("\n  (subpath (param \"{parameter}\"))")
                } else {
                    String::new()
                }
            ));
        }
        if network_access == NetworkAccess::Allowed {
            policy.push_str("\n(allow network-outbound)\n(allow network-inbound)\n");
            policy.push_str(MACOS_SEATBELT_NETWORK_POLICY);
        }
        if workspace_access == WorkspaceAccess::ReadOnly {
            policy.push_str(
                r#"
(deny file-write*
  (literal (param "WRITABLE_ROOT"))
  (subpath (param "WRITABLE_ROOT")))"#,
            );
        }
        command.arg("-p").arg(policy);
        for (name, path) in [("WRITABLE_ROOT", self.root.clone()), ("TEMP_ROOT", temp)] {
            let path = path
                .to_str()
                .ok_or_else(|| Error::Sandbox("sandbox path is not UTF-8".into()))?;
            command.arg(format!("-D{name}={path}"));
        }
        for (index, denied) in self.denied_reads.iter().enumerate() {
            let path = denied
                .path
                .to_str()
                .ok_or_else(|| Error::Sandbox("sandbox path is not UTF-8".into()))?;
            command.arg(format!("-DDENIED_READ_{index}={path}"));
        }
        command.args([
            "--",
            "/bin/bash",
            "--noprofile",
            "--norc",
            "-c",
            MACOS_COMMAND_WRAPPER,
            "horus-command",
        ]);
        append_invocation(&mut command, invocation, self.isolated_home);
        Ok(command)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn sandboxed_command(
        &self,
        _invocation: &Invocation<'_>,
        _network_access: NetworkAccess,
        _workspace_access: WorkspaceAccess,
    ) -> Result<Command> {
        Err(Error::Sandbox(
            "local code execution requires Linux or macOS".into(),
        ))
    }

    async fn execute_invocation(
        &self,
        invocation: Invocation<'_>,
        isolation: CommandIsolation,
        mode: CommandMode,
        output_sink: CommandOutputSink,
        environment: &[(&str, &str)],
        workspace_access: WorkspaceAccess,
    ) -> Result<CommandOutput> {
        if matches!(&invocation, Invocation::Shell(script) if script.trim().is_empty()) {
            return Err(Error::Sandbox("command is empty".into()));
        }
        validate_root(&self.root, &self.root_dir)?;
        let output_limit = if workspace_access == WorkspaceAccess::ReadOnly {
            MAX_READ_ONLY_OUTPUT_BYTES
        } else {
            MAX_COMMAND_OUTPUT_BYTES
        };
        async {
            validate_root(&self.root, &self.root_dir)?;
            let mut command = match isolation.sandbox_mode {
                SandboxMode::WorkspaceWrite => {
                    self.sandboxed_command(&invocation, isolation.network_access, workspace_access)?
                }
                SandboxMode::DangerFullAccess if self.denied_reads.is_empty() => {
                    self.host_command(&invocation)
                }
                SandboxMode::DangerFullAccess => self.protected_full_access_command(&invocation)?,
            };
            command.current_dir(&self.root);
            if self.isolated_home {
                let inherited = ISOLATED_ENVIRONMENT
                    .into_iter()
                    .filter_map(|name| std::env::var_os(name).map(|value| (name, value)));
                command.env_clear().envs(inherited);
            }
            command
                .envs(environment.iter().copied())
                .env("TMPDIR", command_temp(self.temp.path()));
            if self.isolated_home {
                command
                    .env("HOME", command_home(self.temp.path()))
                    .env("SHELL", "/bin/bash");
            }
            for name in &self.denied_environment {
                command.env_remove(name);
            }
            command.kill_on_drop(true);
            #[cfg(target_os = "macos")]
            let uses_cleanup_lease = isolation.sandbox_mode == SandboxMode::WorkspaceWrite
                || (isolation.sandbox_mode == SandboxMode::DangerFullAccess
                    && !self.denied_reads.is_empty());
            #[cfg(target_os = "linux")]
            let uses_process_group = true;
            #[cfg(target_os = "macos")]
            let uses_process_group = !uses_cleanup_lease;
            #[cfg(target_os = "macos")]
            command.stdin(if uses_cleanup_lease {
                Stdio::piped()
            } else {
                Stdio::null()
            });
            #[cfg(not(target_os = "macos"))]
            command.stdin(Stdio::null());
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if uses_process_group {
                command.process_group(0);
            }
            let mut child = command.spawn()?;
            #[cfg(target_os = "macos")]
            let cleanup_lease =
                if uses_cleanup_lease {
                    Some(child.stdin.take().ok_or_else(|| {
                        Error::Sandbox("command cleanup lease unavailable".into())
                    })?)
                } else {
                    None
                };
            #[cfg(not(target_os = "macos"))]
            let cleanup_lease = None::<tokio::process::ChildStdin>;
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let mut process_group = uses_process_group
                .then(|| ProcessGroupGuard::new(&child))
                .transpose()?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| Error::Sandbox("command stdout unavailable".into()))?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| Error::Sandbox("command stderr unavailable".into()))?;
            let execution = async {
                let wait = async {
                    let status = child.wait().await;
                    drop(cleanup_lease);
                    status
                };
                let (stdout, stderr, status) = tokio::join!(
                    read_output(
                        stdout,
                        CommandStream::Stdout,
                        output_sink.clone(),
                        output_limit
                    ),
                    read_output(stderr, CommandStream::Stderr, output_sink, output_limit),
                    wait
                );
                let stdout = stdout?;
                let stderr = stderr?;
                Ok(CommandOutput {
                    exit_code: status?.code().unwrap_or(-1),
                    stdout: stdout.text,
                    stdout_truncated: stdout.truncated,
                    stderr: stderr.text,
                    stderr_truncated: stderr.truncated,
                })
            };
            let output = match mode {
                CommandMode::Background => execution.await,
                CommandMode::Foreground => {
                    match tokio::time::timeout(self.command_timeout, execution).await {
                        Ok(output) => output,
                        Err(_) => {
                            #[cfg(any(target_os = "linux", target_os = "macos"))]
                            if let Some(process_group) = &mut process_group {
                                process_group.kill();
                            }
                            if tokio::time::timeout(Duration::from_secs(1), child.wait())
                                .await
                                .is_err()
                            {
                                let _ = child.kill().await;
                            }
                            return Err(Error::Sandbox(format!(
                                "command exceeded {} seconds",
                                self.command_timeout.as_secs_f64()
                            )));
                        }
                    }
                }
            };
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if let Some(process_group) = &mut process_group {
                process_group.kill();
            }
            output
        }
        .await
    }
}

impl SandboxBackend for LocalSandbox {
    fn read<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            validate_root(&self.root, &self.root_dir)?;
            let root = self.root_dir.try_clone()?;
            let relative = self.relative(path)?;
            let requested = path.to_string();
            tokio::task::spawn_blocking(move || read_file(root, &relative, &requested))
                .await
                .map_err(|error| Error::Sandbox(format!("file reader failed: {error}")))?
        })
    }

    fn read_bytes<'a>(&'a self, path: &'a str, max_bytes: usize) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            if max_bytes == 0 || max_bytes > MAX_BINARY_FILE_BYTES {
                return Err(Error::Sandbox(format!(
                    "binary file read size must be 1–{MAX_BINARY_FILE_BYTES} bytes"
                )));
            }
            validate_root(&self.root, &self.root_dir)?;
            let root = self.root_dir.try_clone()?;
            let relative = self.relative(path)?;
            let requested = path.to_string();
            tokio::task::spawn_blocking(move || {
                read_binary_file(root, &relative, &requested, max_bytes)
            })
            .await
            .map_err(|error| Error::Sandbox(format!("file reader failed: {error}")))?
        })
    }

    fn write<'a>(&'a self, path: &'a str, content: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if content.len() > MAX_FILE_BYTES {
                return Err(Error::Sandbox("file exceeds write limit".into()));
            }
            validate_root(&self.root, &self.root_dir)?;
            let relative = self.relative(path)?;
            let root = self.root_dir.try_clone()?;
            let requested = path.to_string();
            let content = content.as_bytes().to_vec();
            tokio::task::spawn_blocking(move || atomic_write(root, &relative, &content, &requested))
                .await
                .map_err(|error| Error::Sandbox(format!("file writer failed: {error}")))?
        })
    }

    fn execute<'a>(
        &'a self,
        script: &'a str,
        sandbox_mode: SandboxMode,
        network_access: NetworkAccess,
        mode: CommandMode,
        output_sink: CommandOutputSink,
    ) -> BoxFuture<'a, Result<CommandOutput>> {
        Box::pin(self.execute_invocation(
            Invocation::Shell(script),
            CommandIsolation {
                sandbox_mode,
                network_access,
            },
            mode,
            output_sink,
            &[],
            WorkspaceAccess::Writable,
        ))
    }
}

fn find_executable_in(
    name: &str,
    root: &Path,
    denied_reads: &[DeniedRead],
    path: &OsStr,
) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|directory| directory.is_absolute())
        .filter_map(|directory| std::fs::canonicalize(directory.join(name)).ok())
        .find(|candidate| {
            candidate.is_file()
                && !candidate.starts_with(root)
                && denied_reads
                    .iter()
                    .all(|denied| !candidate.starts_with(&denied.path))
        })
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

#[cfg(target_os = "linux")]
fn command_temp(_private_temp: &Path) -> &Path {
    Path::new("/tmp")
}

#[cfg(not(target_os = "linux"))]
fn command_temp(private_temp: &Path) -> &Path {
    private_temp
}

#[cfg(target_os = "linux")]
fn command_home(_private_temp: &Path) -> &Path {
    Path::new(ISOLATED_HOME)
}

#[cfg(not(target_os = "linux"))]
fn command_home(private_temp: &Path) -> &Path {
    private_temp
}

async fn read_output(
    mut reader: impl AsyncRead + Unpin,
    stream: CommandStream,
    sink: CommandOutputSink,
    limit: usize,
) -> Result<BoundedOutput> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        sink.write(stream, &buffer[..read]);
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(remaining)]);
        truncated |= read > remaining;
    }
    let mut output = String::from_utf8_lossy(&output).into_owned();
    if truncated {
        output.push_str("\n[output truncated]");
    }
    Ok(BoundedOutput {
        text: output,
        truncated,
    })
}

struct BoundedOutput {
    text: String,
    truncated: bool,
}

#[cfg(unix)]
fn validate_root(path: &Path, directory: &Dir) -> Result<()> {
    let path = std::fs::metadata(path)?;
    let directory = directory.dir_metadata()?;
    if std::os::unix::fs::MetadataExt::dev(&path) != cap_std::fs::MetadataExt::dev(&directory)
        || std::os::unix::fs::MetadataExt::ino(&path) != cap_std::fs::MetadataExt::ino(&directory)
    {
        return Err(Error::Sandbox(
            "sandbox root changed after initialization".into(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_root(_path: &Path, _directory: &Dir) -> Result<()> {
    Err(Error::Sandbox(
        "the local sandbox requires a Unix platform".into(),
    ))
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
