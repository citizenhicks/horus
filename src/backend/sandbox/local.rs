//! Local filesystem adapter with policy-selected command isolation.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::{Read as _, Seek as _, Write as _};
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use cap_std::fs::OpenOptions;
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
        match invocation {
            Invocation::Shell(script) => {
                command.arg("/bin/bash");
                if self.isolated_home {
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
        match invocation {
            Invocation::Shell(script) => {
                command.arg("/bin/bash");
                if self.isolated_home {
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
        match invocation {
            Invocation::Shell(script) => {
                command.arg("/bin/bash");
                if self.isolated_home {
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
        match invocation {
            Invocation::Shell(script) => {
                command.arg("/bin/bash");
                if self.isolated_home {
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
                Ok(CommandOutput {
                    exit_code: status?.code().unwrap_or(-1),
                    stdout: stdout?,
                    stderr: stderr?,
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

fn read_file(root: Dir, relative: &Path, requested: &str) -> Result<String> {
    let file = open_regular_file(root, relative, requested)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::Sandbox("file exceeds read limit".into()));
    }
    String::from_utf8(bytes).map_err(|_| Error::Sandbox(format!("{requested} is not valid UTF-8")))
}

fn read_binary_file(
    root: Dir,
    relative: &Path,
    requested: &str,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let file = open_regular_file(root, relative, requested)?;
    let mut bytes = Vec::new();
    file.take(max_bytes as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(Error::Sandbox("file exceeds binary read limit".into()));
    }
    Ok(bytes)
}

fn read_file_range(
    root: Dir,
    relative: &Path,
    requested: &str,
    offset: u64,
    max_bytes: usize,
) -> Result<(Vec<u8>, Option<u64>)> {
    let mut file = open_regular_file(root, relative, requested)?;
    let size = file.metadata()?.len();
    if offset > size {
        return Err(Error::Sandbox("file offset exceeds its size".into()));
    }
    file.seek(std::io::SeekFrom::Start(offset))?;
    let length = usize::try_from(size.saturating_sub(offset).min(max_bytes as u64))
        .map_err(|_| Error::Sandbox("file range is unsupported".into()))?;
    let mut data = vec![0; length];
    file.read_exact(&mut data)?;
    let end = offset.saturating_add(length as u64);
    Ok((data, (end < size).then_some(end)))
}

fn open_regular_file(root: Dir, relative: &Path, requested: &str) -> Result<cap_std::fs::File> {
    let name = relative
        .file_name()
        .ok_or_else(|| Error::Sandbox(requested.to_string()))?;
    let parent = open_parent(root, relative.parent().unwrap_or(Path::new("")), requested)?;
    let before = parent
        .symlink_metadata(name)
        .map_err(|_| Error::Sandbox(requested.to_string()))?;
    if before.is_symlink() || !before.is_file() {
        return Err(Error::Sandbox(requested.to_string()));
    }
    let file = parent
        .open(name)
        .map_err(|_| Error::Sandbox(requested.to_string()))?;
    let opened = file.metadata()?;
    let current = parent
        .symlink_metadata(name)
        .map_err(|_| Error::Sandbox(requested.to_string()))?;
    if !opened.is_file()
        || current.is_symlink()
        || !same_cap_file(&before, &opened)
        || !same_cap_file(&opened, &current)
    {
        return Err(Error::Sandbox(requested.to_string()));
    }
    Ok(file)
}

fn atomic_write(root: Dir, relative: &Path, content: &[u8], requested: &str) -> Result<()> {
    let target = relative
        .file_name()
        .ok_or_else(|| Error::Sandbox(requested.to_string()))?;
    let parent = open_parent(root, relative.parent().unwrap_or(Path::new("")), requested)?;
    let permissions = match parent.symlink_metadata(target) {
        Ok(metadata) if metadata.is_symlink() || !metadata.is_file() => {
            return Err(Error::Sandbox(requested.to_string()));
        }
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let temporary = format!(".horus-write-{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = parent.open_with(&temporary, &options)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        parent.rename(&temporary, &parent, target)?;
        sync_directory(&parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = parent.remove_file(&temporary);
    }
    result
}

fn sync_directory(directory: &Dir) -> Result<()> {
    directory.open(".")?.sync_all()?;
    Ok(())
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

fn open_parent(mut parent: Dir, path: &Path, requested: &str) -> Result<Dir> {
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let before = parent
            .symlink_metadata(name)
            .map_err(|_| Error::Sandbox(requested.to_string()))?;
        if before.is_symlink() || !before.is_dir() {
            return Err(Error::Sandbox(requested.to_string()));
        }
        let next = parent
            .open_dir(name)
            .map_err(|_| Error::Sandbox(requested.to_string()))?;
        if !same_cap_file(&before, &next.dir_metadata()?) {
            return Err(Error::Sandbox(requested.to_string()));
        }
        parent = next;
    }
    Ok(parent)
}

#[cfg(unix)]
fn same_cap_file(left: &cap_std::fs::Metadata, right: &cap_std::fs::Metadata) -> bool {
    use cap_std::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_cap_file(_left: &cap_std::fs::Metadata, _right: &cap_std::fs::Metadata) -> bool {
    false
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
) -> Result<String> {
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
    Ok(output)
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn background_commands_do_not_use_the_foreground_deadline() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = LocalSandbox::new(workspace.path())
            .expect("sandbox")
            .command_timeout(Duration::from_millis(1))
            .expect("timeout");

        let output = sandbox
            .execute(
                "sleep 0.05",
                SandboxMode::WorkspaceWrite,
                NetworkAccess::Denied,
                CommandMode::Background,
                CommandOutputSink::default(),
            )
            .await
            .expect("background command");

        assert_eq!(output.exit_code, 0);
    }

    #[cfg(target_os = "macos")]
    async fn assert_timeout_reaps_daemonized_descendants(
        sandbox: LocalSandbox,
        sandbox_mode: SandboxMode,
    ) {
        let pid_path = sandbox.root.join("daemon.pid");
        let network_access = if sandbox_mode == SandboxMode::DangerFullAccess {
            NetworkAccess::Allowed
        } else {
            NetworkAccess::Denied
        };
        let script = r#"/usr/bin/perl -MPOSIX=setsid -e '
exit if fork; setsid(); exit if fork;
open my $file, ">", "daemon.pid" or die; print $file "$$\n"; close $file;
sleep 30;
' &
sleep 30"#;

        let execution = tokio::spawn(async move {
            sandbox
                .execute(
                    script,
                    sandbox_mode,
                    network_access,
                    CommandMode::Foreground,
                    CommandOutputSink::default(),
                )
                .await
        });
        let pid = tokio::task::spawn_blocking(move || {
            for _ in 0..500 {
                if let Ok(pid) = std::fs::read_to_string(&pid_path)
                    .and_then(|pid| pid.trim().parse::<u32>().map_err(std::io::Error::other))
                {
                    return Some(pid);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            None
        })
        .await
        .expect("daemon readiness task");
        let Some(pid) = pid else {
            execution.abort();
            panic!("daemon pid was not written");
        };

        tokio::time::advance(Duration::from_millis(101)).await;
        let error = execution
            .await
            .expect("command task")
            .expect_err("foreground deadline");
        assert!(error.to_string().contains("exceeded"));
        let alive = tokio::task::spawn_blocking(move || {
            for _ in 0..100 {
                let alive = std::process::Command::new("/bin/kill")
                    .args(["-0", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success());
                if !alive {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            true
        })
        .await
        .expect("daemon cleanup task");
        if alive {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
        assert!(!alive, "daemonized child {pid} survived command cleanup");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test(start_paused = true)]
    async fn command_cleanup_reaps_daemonized_descendants() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = LocalSandbox::new(workspace.path())
            .expect("sandbox")
            .command_timeout(Duration::from_millis(100))
            .expect("timeout");
        assert_timeout_reaps_daemonized_descendants(sandbox, SandboxMode::WorkspaceWrite).await;
    }

    #[cfg(target_os = "macos")]
    #[tokio::test(start_paused = true)]
    async fn protected_full_access_cleanup_reaps_daemonized_descendants() {
        let workspace = tempfile::tempdir().expect("workspace");
        let protected = tempfile::tempdir().expect("protected");
        let sandbox = LocalSandbox::new(workspace.path())
            .expect("sandbox")
            .deny_read(protected.path())
            .expect("protected path")
            .command_timeout(Duration::from_millis(100))
            .expect("timeout");
        assert_timeout_reaps_daemonized_descendants(sandbox, SandboxMode::DangerFullAccess).await;
    }

    fn local_sandbox(workspace: &Path) -> LocalSandbox {
        LocalSandbox::new(workspace).expect("sandbox")
    }

    #[tokio::test]
    async fn binary_range_reads_from_the_same_opened_file() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::create_dir(workspace.path().join("nested")).expect("nested directory");
        std::fs::write(workspace.path().join("nested/data.bin"), b"\0abcdef").expect("binary file");
        let sandbox = local_sandbox(workspace.path());

        let (data, next_offset) = sandbox
            .read_range("nested/data.bin", 1, 3)
            .await
            .expect("range");

        assert_eq!(data, b"abc");
        assert_eq!(next_offset, Some(4));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn binary_range_rejects_symlinked_files_and_parents() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("secret"), b"secret").expect("outside file");
        symlink(
            outside.path().join("secret"),
            workspace.path().join("file-link"),
        )
        .expect("file symlink");
        symlink(outside.path(), workspace.path().join("directory-link"))
            .expect("directory symlink");
        let sandbox = local_sandbox(workspace.path());

        assert!(sandbox.read_range("file-link", 0, 16).await.is_err());
        assert!(
            sandbox
                .read_range("directory-link/secret", 0, 16)
                .await
                .is_err()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn isolated_home_is_private_and_writable() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = local_sandbox(workspace.path()).isolated_home();
        let expected = command_home(sandbox.temp.path())
            .to_string_lossy()
            .into_owned();

        let output = sandbox
            .execute(
                r#"test -d "$HOME" && test -w "$HOME" && printf '%s' "$HOME""#,
                SandboxMode::WorkspaceWrite,
                NetworkAccess::Denied,
                CommandMode::Foreground,
                CommandOutputSink::default(),
            )
            .await
            .expect("isolated home command");

        assert_eq!(output.exit_code, 0, "{}", output.stderr);
        assert_eq!(output.stdout, expected);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn authorized_commands_can_modify_the_whole_workspace() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = local_sandbox(workspace.path()).isolated_home();

        for (label, mode) in [
            ("foreground", CommandMode::Foreground),
            ("background", CommandMode::Background),
        ] {
            let script = format!(
                "mkdir -p .agents .codex && touch .agents/{label} .codex/{label} {label}.txt && git init --quiet && git add -- {label}.txt && git -c user.name=Horus -c user.email=horus@example.invalid commit --quiet -m {label}"
            );
            let output = sandbox
                .execute(
                    &script,
                    SandboxMode::WorkspaceWrite,
                    NetworkAccess::Denied,
                    mode,
                    CommandOutputSink::default(),
                )
                .await
                .expect("authorized workspace command");

            assert_eq!(output.exit_code, 0, "{}", output.stderr);
            assert!(workspace.path().join(".git/index").is_file());
            assert!(workspace.path().join(".agents").join(label).is_file());
            assert!(workspace.path().join(".codex").join(label).is_file());
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn commands_cannot_write_outside_the_workspace_through_symlinks() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        symlink(outside.path(), workspace.path().join("outside")).expect("outside symlink");
        let sandbox = local_sandbox(workspace.path());

        let output = sandbox
            .execute(
                "touch outside/escaped",
                SandboxMode::WorkspaceWrite,
                NetworkAccess::Allowed,
                CommandMode::Foreground,
                CommandOutputSink::default(),
            )
            .await
            .expect("sandboxed command");

        assert_ne!(output.exit_code, 0);
        assert!(!outside.path().join("escaped").exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn full_access_commands_run_outside_the_workspace_sandbox() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let target = outside.path().join("written");
        let sandbox = local_sandbox(workspace.path());

        let output = sandbox
            .execute(
                &format!("touch {}", target.display()),
                SandboxMode::DangerFullAccess,
                NetworkAccess::Allowed,
                CommandMode::Foreground,
                CommandOutputSink::default(),
            )
            .await
            .expect("full access command");

        assert_eq!(output.exit_code, 0, "{}", output.stderr);
        assert!(target.is_file());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn denied_environment_is_removed_from_full_access_commands() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = local_sandbox(workspace.path())
            .isolated_home()
            .deny_environment("HORUS_TEST_SECRET");

        let output = sandbox
            .execute_invocation(
                Invocation::Shell(
                    r#"printf '%s:%s' "${HORUS_TEST_SECRET-unset}" "$HORUS_TEST_VISIBLE""#,
                ),
                CommandIsolation {
                    sandbox_mode: SandboxMode::DangerFullAccess,
                    network_access: NetworkAccess::Allowed,
                },
                CommandMode::Foreground,
                CommandOutputSink::default(),
                &[
                    ("HORUS_TEST_SECRET", "secret"),
                    ("HORUS_TEST_VISIBLE", "visible"),
                ],
                WorkspaceAccess::Writable,
            )
            .await
            .expect("full access command");

        assert_eq!(output.exit_code, 0, "{}", output.stderr);
        assert_eq!(output.stdout, "unset:visible");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(start_paused = true)]
    async fn full_access_timeout_reaps_process_group_descendants() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = local_sandbox(workspace.path())
            .command_timeout(Duration::from_millis(100))
            .expect("timeout");
        let execution = tokio::spawn(async move {
            sandbox
                .execute(
                    "sh -c 'echo $$ > child.pid; sleep 30' & sleep 30",
                    SandboxMode::DangerFullAccess,
                    NetworkAccess::Allowed,
                    CommandMode::Foreground,
                    CommandOutputSink::default(),
                )
                .await
        });
        let pid_path = workspace.path().join("child.pid");
        let pid = tokio::task::spawn_blocking(move || {
            for _ in 0..500 {
                if let Ok(pid) = std::fs::read_to_string(&pid_path)
                    .and_then(|pid| pid.trim().parse::<u32>().map_err(std::io::Error::other))
                {
                    return Some(pid);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            None
        })
        .await
        .expect("child readiness task")
        .expect("child pid");

        tokio::time::advance(Duration::from_millis(101)).await;
        let error = execution
            .await
            .expect("command task")
            .expect_err("foreground deadline");
        assert!(error.to_string().contains("exceeded"));
        let alive = tokio::task::spawn_blocking(move || {
            for _ in 0..100 {
                let alive = std::process::Command::new("/bin/kill")
                    .args(["-0", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success());
                if !alive {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            true
        })
        .await
        .expect("child cleanup task");
        if alive {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
        assert!(!alive, "child {pid} survived full-access timeout cleanup");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn host_commands_do_not_embed_the_seatbelt_cleanup_wrapper() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = local_sandbox(workspace.path());
        let command = sandbox.host_command(&Invocation::Shell("true"));
        let command = command.as_std();

        assert_eq!(command.get_program(), "/bin/bash");
        assert!(
            command
                .get_args()
                .all(|argument| argument != MACOS_COMMAND_WRAPPER)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn protected_full_access_masks_paths_and_scopes_cleanup() {
        let workspace = tempfile::tempdir().expect("workspace");
        let protected = tempfile::tempdir().expect("protected");
        let sandbox = local_sandbox(workspace.path())
            .deny_read(protected.path())
            .expect("protected path");
        let command = sandbox
            .protected_full_access_command(&Invocation::Shell("true"))
            .expect("protected full access command");
        let arguments = command
            .as_std()
            .get_args()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();
        let policy = arguments
            .iter()
            .position(|argument| argument == "-p")
            .and_then(|index| arguments.get(index + 1))
            .expect("Seatbelt policy");

        assert!(policy.contains("(allow default)"));
        assert!(policy.contains("(deny file-read* file-write*"));
        assert!(policy.contains("(deny signal (require-not (target same-sandbox)))"));
        assert!(policy.contains("(deny process-info* (require-not (target same-sandbox)))"));
        assert!(
            arguments
                .iter()
                .any(|argument| argument.as_ref() == MACOS_COMMAND_WRAPPER)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn read_only_argv_cannot_modify_the_workspace() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = local_sandbox(workspace.path());

        let output = sandbox
            .execute_read_only("touch", &["blocked"], &[])
            .await
            .expect("read-only command");

        assert_ne!(output.exit_code, 0);
        assert!(!workspace.path().join("blocked").exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn network_policy_changes_command_isolation() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = local_sandbox(workspace.path());
        let denied = sandbox
            .sandboxed_command(
                &Invocation::Shell("true"),
                NetworkAccess::Denied,
                WorkspaceAccess::Writable,
            )
            .expect("network-disabled command");
        let allowed = sandbox
            .sandboxed_command(
                &Invocation::Shell("true"),
                NetworkAccess::Allowed,
                WorkspaceAccess::Writable,
            )
            .expect("network-enabled command");
        #[cfg(target_os = "linux")]
        {
            let isolation = |command: &Command| {
                let arguments = command.as_std().get_args().collect::<Vec<_>>();
                (
                    arguments.contains(&OsStr::new("--unshare-net")),
                    arguments
                        .windows(2)
                        .any(|pair| pair == [OsStr::new("--tmpfs"), OsStr::new("/run")]),
                )
            };
            assert_eq!(
                (isolation(&denied), isolation(&allowed)),
                ((true, Path::new("/run").is_dir()), (false, false))
            );
        }
        #[cfg(target_os = "macos")]
        {
            let policy = |command: &Command| {
                command
                    .as_std()
                    .get_args()
                    .map(|argument| argument.to_string_lossy())
                    .find(|argument| argument.contains("(deny default)"))
                    .expect("Seatbelt policy")
                    .into_owned()
            };
            assert!(!policy(&denied).contains("com.apple.SecurityServer"));
            assert!(policy(&allowed).contains("com.apple.SecurityServer"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn filesystem_handles_reject_symlink_escapes_and_aliases() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().expect("parent");
        let workspace = parent.path().join("workspace");
        let outside = parent.path().join("outside.txt");
        let outside_directory = parent.path().join("outside");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(&outside_directory).expect("outside directory");
        std::fs::write(&outside, "outside").expect("outside");
        std::fs::create_dir(workspace.join(".git")).expect("metadata");
        std::fs::write(workspace.join(".git/config"), "metadata").expect("metadata file");
        symlink(&outside, workspace.join("outside-link")).expect("outside link");
        symlink(&outside_directory, workspace.join("outside-directory"))
            .expect("outside directory link");
        symlink(workspace.join(".git"), workspace.join("metadata-link")).expect("metadata link");
        let sandbox = local_sandbox(&workspace);

        assert!(sandbox.read("outside-link").await.is_err());
        assert!(sandbox.write("outside-link", "escaped").await.is_err());
        assert!(
            sandbox
                .write("outside-directory/new", "escaped")
                .await
                .is_err()
        );
        assert!(
            sandbox
                .write("metadata-link/config", "escaped")
                .await
                .is_err()
        );
        assert!(sandbox.write("metadata-link/new", "escaped").await.is_err());
        assert_eq!(
            std::fs::read_to_string(outside).expect("outside"),
            "outside"
        );
        assert!(!outside_directory.join("new").exists());
        assert_eq!(
            std::fs::read_to_string(workspace.join(".git/config")).expect("metadata"),
            "metadata"
        );
        assert!(!workspace.join(".git/new").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn filesystem_handles_reject_a_replaced_workspace_root() {
        let parent = tempfile::tempdir().expect("parent");
        let workspace = parent.path().join("workspace");
        let displaced = parent.path().join("displaced");
        std::fs::create_dir(&workspace).expect("workspace");
        let sandbox = local_sandbox(&workspace);
        std::fs::rename(&workspace, &displaced).expect("displace workspace");
        std::fs::create_dir(&workspace).expect("replacement workspace");
        std::fs::write(workspace.join("bait.txt"), "replacement").expect("replacement file");

        assert!(sandbox.read("bait.txt").await.is_err());
        assert!(sandbox.write("new.txt", "escaped").await.is_err());
        assert!(!workspace.join("new.txt").exists());
        assert!(!displaced.join("new.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn writes_replace_files_without_partial_state_or_permission_drift() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().expect("workspace");
        let path = workspace.path().join("state.txt");
        std::fs::write(&path, "old").expect("old file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))
            .expect("permissions");
        let sandbox = local_sandbox(workspace.path());

        sandbox
            .write("state.txt", "complete replacement")
            .await
            .expect("atomic write");

        assert_eq!(
            std::fs::read_to_string(&path).expect("replacement"),
            "complete replacement"
        );
        assert_eq!(
            std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert!(
            workspace
                .path()
                .read_dir()
                .expect("workspace entries")
                .all(|entry| !entry
                    .expect("workspace entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".horus-write-"))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bwrap_discovery_rejects_workspace_path_aliases() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().expect("parent");
        let workspace = parent.path().join("workspace");
        let binaries = workspace.join("bin");
        let alias = parent.path().join("alias");
        std::fs::create_dir_all(&binaries).expect("create binaries");
        std::fs::write(binaries.join("bwrap"), "").expect("create bwrap");
        symlink(&binaries, &alias).expect("create path alias");

        assert!(find_executable_in("bwrap", &workspace, &[], alias.as_os_str()).is_none());
    }
}
