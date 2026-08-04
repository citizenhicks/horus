//! Workspace-confined local filesystem Adapter.

#[cfg(target_os = "linux")]
use std::collections::HashMap;
use std::ffi::OsStr;
#[cfg(target_os = "linux")]
use std::fs::File;
use std::io::{Read as _, Write as _};
#[cfg(target_os = "linux")]
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
#[cfg(target_os = "linux")]
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use cap_std::fs::OpenOptions;
#[cfg(target_os = "linux")]
use cap_std::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::NetworkAccess;
#[cfg(target_os = "linux")]
use super::ProcessGroupGuard;
use super::SandboxBackend;
use super::{CommandMode, CommandOutput, CommandOutputSink, CommandStream, MAX_FILE_BYTES};
#[cfg(target_os = "macos")]
use super::{MACOS_COMMAND_WRAPPER, MACOS_SEATBELT_BASE_POLICY};
use crate::BoxFuture;
use crate::Error;
use crate::Result;

const MAX_COMMAND_OUTPUT_BYTES: usize = 40_000;
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const PROTECTED_METADATA: [&str; 3] = [".git", ".agents", ".codex"];
#[cfg(target_os = "linux")]
const ISOLATED_HOME: &str = "/tmp/horus-home";
#[cfg(target_os = "linux")]
const PROTECTED_MARKER: &[u8] = b"horus sandbox protected metadata placeholder v1\n";
#[cfg(target_os = "linux")]
const MAX_JOURNAL_BYTES: u64 = 512;
#[cfg(target_os = "linux")]
const PROTECTION_STATE_PARENT: &str = "/var/tmp";
#[cfg(target_os = "macos")]
const SEATBELT_POLICY_SUFFIX: &str = r#"
(allow file-read*)
(allow file-write*
  (subpath (param "TEMP_ROOT"))
  (require-all
    (subpath (param "WRITABLE_ROOT"))
    (require-not (literal (param "GIT_PATH")))
    (require-not (subpath (param "GIT_PATH")))
    (require-not (literal (param "AGENTS_PATH")))
    (require-not (subpath (param "AGENTS_PATH")))
    (require-not (literal (param "CODEX_PATH")))
    (require-not (subpath (param "CODEX_PATH")))))
"#;

/// Restricts file operations to one canonical workspace root.
pub struct LocalSandbox {
    root: PathBuf,
    root_dir: Dir,
    #[cfg(target_os = "linux")]
    protection_dir: Dir,
    temp: tempfile::TempDir,
    command_timeout: Duration,
    denied_reads: Vec<DeniedRead>,
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

#[cfg(target_os = "linux")]
#[derive(Default)]
struct ProtectedState {
    active_commands: usize,
    created: Vec<ProtectedFile>,
    journal: Option<ProtectionJournal>,
}

#[cfg(target_os = "linux")]
struct ProtectedFile {
    name: &'static str,
    dev: u64,
    ino: u64,
}

#[cfg(target_os = "linux")]
struct ProtectedLease {
    workspace: (u64, u64),
    root_dir: Dir,
    active: bool,
}

#[cfg(target_os = "linux")]
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ProtectionRecord {
    stage: String,
    identity: Option<(u64, u64)>,
    targets: u8,
}

#[cfg(target_os = "linux")]
struct ProtectionJournal {
    _lock: File,
    directory: Dir,
    name: String,
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
            #[cfg(target_os = "linux")]
            let protection_dir = protection_dir(&root)?;
            let temp = tempfile::Builder::new().prefix("horus-").tempdir()?;
            Ok(Self {
                root,
                root_dir,
                #[cfg(target_os = "linux")]
                protection_dir,
                temp,
                command_timeout: DEFAULT_COMMAND_TIMEOUT,
                denied_reads: Vec::new(),
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
            NetworkAccess::Denied,
            CommandMode::Foreground,
            CommandOutputSink::default(),
            environment,
            false,
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
        workspace_writable: bool,
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
            for name in PROTECTED_METADATA {
                let path = self.root.join(name);
                let metadata = std::fs::symlink_metadata(&path)
                    .map_err(|_| Error::Sandbox(format!("{name} protection is unavailable")))?;
                if metadata.file_type().is_symlink() {
                    return Err(Error::Sandbox(format!(
                        "protected metadata path is a symlink: {}",
                        path.display()
                    )));
                }
                command.arg("--ro-bind").arg(&path).arg(&path);
            }
        }
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

    #[cfg(target_os = "macos")]
    fn sandboxed_command(
        &self,
        invocation: &Invocation<'_>,
        network_access: NetworkAccess,
        workspace_writable: bool,
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
            policy.push_str("\n(allow network*)");
        }
        if !workspace_writable {
            policy.push_str(
                r#"
(deny file-write*
  (literal (param "WRITABLE_ROOT"))
  (subpath (param "WRITABLE_ROOT")))"#,
            );
        }
        command.arg("-p").arg(policy);
        for (name, path) in [
            ("WRITABLE_ROOT", self.root.clone()),
            ("TEMP_ROOT", temp),
            ("GIT_PATH", self.root.join(".git")),
            ("AGENTS_PATH", self.root.join(".agents")),
            ("CODEX_PATH", self.root.join(".codex")),
        ] {
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
        _workspace_writable: bool,
    ) -> Result<Command> {
        Err(Error::Sandbox(
            "local code execution requires Linux or macOS".into(),
        ))
    }

    async fn execute_invocation(
        &self,
        invocation: Invocation<'_>,
        network_access: NetworkAccess,
        mode: CommandMode,
        output_sink: CommandOutputSink,
        environment: &[(&str, &str)],
        workspace_writable: bool,
    ) -> Result<CommandOutput> {
        if matches!(&invocation, Invocation::Shell(script) if script.trim().is_empty()) {
            return Err(Error::Sandbox("command is empty".into()));
        }
        validate_root(&self.root, &self.root_dir)?;
        #[cfg(target_os = "linux")]
        let protected = if workspace_writable {
            Some(self.protect_command_metadata().await?)
        } else {
            None
        };
        let output =
            async {
                validate_root(&self.root, &self.root_dir)?;
                let mut command =
                    self.sandboxed_command(&invocation, network_access, workspace_writable)?;
                let mut inherited = [
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
                .filter_map(|name| std::env::var_os(name).map(|value| (name, value)))
                .collect::<Vec<_>>();
                if !self.isolated_home {
                    inherited.extend(
                        ["HOME", "SHELL", "CARGO_HOME", "RUSTUP_HOME"]
                            .into_iter()
                            .filter_map(|name| std::env::var_os(name).map(|value| (name, value))),
                    );
                }
                command
                    .current_dir(&self.root)
                    .env_clear()
                    .envs(inherited)
                    .envs(environment.iter().copied())
                    .env("TMPDIR", command_temp(self.temp.path()));
                if self.isolated_home {
                    command
                        .env("HOME", command_home(self.temp.path()))
                        .env("SHELL", "/bin/bash");
                }
                #[cfg(target_os = "macos")]
                command.stdin(Stdio::piped());
                #[cfg(not(target_os = "macos"))]
                command.stdin(Stdio::null());
                command.stdout(Stdio::piped()).stderr(Stdio::piped());
                #[cfg(target_os = "linux")]
                command.process_group(0);
                let mut child = command.spawn()?;
                #[cfg(target_os = "macos")]
                let cleanup_lease =
                    Some(child.stdin.take().ok_or_else(|| {
                        Error::Sandbox("command cleanup lease unavailable".into())
                    })?);
                #[cfg(not(target_os = "macos"))]
                let cleanup_lease = None::<tokio::process::ChildStdin>;
                #[cfg(target_os = "linux")]
                let mut process_group = ProcessGroupGuard::new(&child)?;
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
                        read_output(stdout, CommandStream::Stdout, output_sink.clone()),
                        read_output(stderr, CommandStream::Stderr, output_sink),
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
                                #[cfg(target_os = "linux")]
                                process_group.kill();
                                #[cfg(target_os = "macos")]
                                if tokio::time::timeout(Duration::from_secs(1), child.wait())
                                    .await
                                    .is_err()
                                {
                                    let _ = child.kill().await;
                                }
                                #[cfg(not(target_os = "macos"))]
                                let _ = child.kill().await;
                                return Err(Error::Sandbox(format!(
                                    "command exceeded {} seconds",
                                    self.command_timeout.as_secs_f64()
                                )));
                            }
                        }
                    }
                };
                #[cfg(target_os = "linux")]
                process_group.kill();
                output
            }
            .await;
        #[cfg(target_os = "linux")]
        {
            let Some(protected) = protected else {
                return output;
            };
            match (output, protected.finish().await) {
                (output, Ok(())) => output,
                (Ok(_), Err(cleanup)) => Err(cleanup),
                (Err(error), Err(cleanup)) => Err(Error::Sandbox(format!(
                    "{error}; protected metadata cleanup failed: {cleanup}"
                ))),
            }
        }
        #[cfg(not(target_os = "linux"))]
        output
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
        network_access: NetworkAccess,
        mode: CommandMode,
        output_sink: CommandOutputSink,
    ) -> BoxFuture<'a, Result<CommandOutput>> {
        Box::pin(self.execute_invocation(
            Invocation::Shell(script),
            network_access,
            mode,
            output_sink,
            &[],
            true,
        ))
    }
}

fn read_file(root: Dir, relative: &Path, requested: &str) -> Result<String> {
    let file = root
        .open(relative)
        .map_err(|_| Error::Sandbox(requested.to_string()))?;
    if !file.metadata()?.is_file() {
        return Err(Error::Sandbox(requested.to_string()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::Sandbox("file exceeds read limit".into()));
    }
    String::from_utf8(bytes).map_err(|_| Error::Sandbox(format!("{requested} is not valid UTF-8")))
}

fn atomic_write(root: Dir, relative: &Path, content: &[u8], requested: &str) -> Result<()> {
    if is_protected_metadata(relative) {
        return Err(Error::Sandbox(requested.to_string()));
    }
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

#[cfg(target_os = "linux")]
impl LocalSandbox {
    async fn protect_command_metadata(&self) -> Result<ProtectedLease> {
        let root_dir = self.root_dir.try_clone()?;
        let protection_dir = self.protection_dir.try_clone()?;
        tokio::task::spawn_blocking(move || ProtectedLease::acquire(root_dir, protection_dir))
            .await
            .map_err(|error| Error::Sandbox(format!("metadata protector failed: {error}")))?
    }
}

#[cfg(target_os = "linux")]
impl ProtectedLease {
    fn acquire(root_dir: Dir, protection_dir: Dir) -> Result<Self> {
        let metadata = root_dir.dir_metadata()?;
        let workspace = (metadata.dev(), metadata.ino());
        let mut states = protected_states()
            .lock()
            .map_err(|_| Error::Sandbox("protected metadata lock poisoned".into()))?;
        let state = states.entry(workspace).or_default();
        let active_commands = state
            .active_commands
            .checked_add(1)
            .ok_or_else(|| Error::Sandbox("too many active sandbox commands".into()))?;
        if state.active_commands == 0 {
            let journal = workspace_journal(&protection_dir, &root_dir)?;
            cleanup_protected_files(&root_dir, &mut state.created)?;
            recover_protected_files(&root_dir, &journal)?;
            match publish_protected_files(&root_dir, &journal) {
                Ok(created) => state.created = created,
                Err(error) => {
                    let cleanup = recover_protected_files(&root_dir, &journal);
                    if let Err(cleanup) = cleanup {
                        return Err(Error::Sandbox(format!(
                            "{error}; protected metadata cleanup failed: {cleanup}"
                        )));
                    }
                    let remove = state.created.is_empty();
                    if remove {
                        states.remove(&workspace);
                    }
                    return Err(error);
                }
            }
            if state.created.is_empty() {
                write_protection_record(&journal, None)?;
            }
            state.journal = Some(journal);
        }
        state.active_commands = active_commands;
        Ok(Self {
            workspace,
            root_dir,
            active: true,
        })
    }

    async fn finish(mut self) -> Result<()> {
        self.active = false;
        tokio::task::spawn_blocking(move || {
            release_command_metadata(self.workspace, &self.root_dir)
        })
        .await
        .map_err(|error| Error::Sandbox(format!("metadata cleanup failed: {error}")))?
    }
}

#[cfg(target_os = "linux")]
fn release_command_metadata(workspace: (u64, u64), root_dir: &Dir) -> Result<()> {
    let mut states = protected_states()
        .lock()
        .map_err(|_| Error::Sandbox("protected metadata lock poisoned".into()))?;
    let (cleanup, remove) = {
        let state = states
            .get_mut(&workspace)
            .ok_or_else(|| Error::Sandbox("protected metadata state is missing".into()))?;
        state.active_commands = state
            .active_commands
            .checked_sub(1)
            .ok_or_else(|| Error::Sandbox("protected metadata guard underflow".into()))?;
        if state.active_commands != 0 {
            return Ok(());
        }
        let cleanup = cleanup_protected_files(root_dir, &mut state.created);
        let journal = if cleanup.is_ok() && state.created.is_empty() {
            state
                .journal
                .as_ref()
                .ok_or_else(|| Error::Sandbox("sandbox protection journal is missing".into()))
                .and_then(|journal| write_protection_record(journal, None))
        } else {
            Ok(())
        };
        state.journal.take();
        let cleanup = match (cleanup, journal) {
            (cleanup, Ok(())) => cleanup,
            (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(journal)) => Err(Error::Sandbox(format!(
                "{error}; protection journal cleanup failed: {journal}"
            ))),
        };
        (cleanup, state.created.is_empty())
    };
    if remove {
        states.remove(&workspace);
    }
    cleanup
}

#[cfg(target_os = "linux")]
impl Drop for ProtectedLease {
    fn drop(&mut self) {
        // Runs when a cancelled `execute` future abandons the lease. Async cleanup is
        // impossible in `drop`, so this does bounded blocking work (a few unlinks and
        // one directory sync) on the executor thread; `finish` remains the async happy
        // path. The protection journal keeps a skipped cleanup recoverable on restart.
        if self.active {
            let _ = release_command_metadata(self.workspace, &self.root_dir);
        }
    }
}

#[cfg(target_os = "linux")]
fn protected_states() -> &'static Mutex<HashMap<(u64, u64), ProtectedState>> {
    static STATES: OnceLock<Mutex<HashMap<(u64, u64), ProtectedState>>> = OnceLock::new();
    STATES.get_or_init(Mutex::default)
}

#[cfg(target_os = "linux")]
fn protection_dir(root: &Path) -> Result<Dir> {
    let uid = current_uid()?;
    let path = Path::new(PROTECTION_STATE_PARENT).join(format!("horus-sandbox-{uid}"));
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let before = std::fs::symlink_metadata(&path)?;
    if before.file_type().is_symlink()
        || !before.is_dir()
        || before.uid() != uid
        || before.permissions().mode() & 0o077 != 0
    {
        return Err(Error::Sandbox(
            "sandbox state directory is not private".into(),
        ));
    }
    let path = std::fs::canonicalize(path)?;
    if path.starts_with(root) {
        return Err(Error::Sandbox(
            "sandbox state directory must be outside the workspace".into(),
        ));
    }
    let directory = Dir::open_ambient_dir(&path, ambient_authority())?;
    let opened = directory.dir_metadata()?;
    let after = std::fs::symlink_metadata(&path)?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || opened.dev() != after.dev()
        || opened.ino() != after.ino()
    {
        return Err(Error::Sandbox(
            "sandbox state directory changed while opening".into(),
        ));
    }
    Ok(directory)
}

#[cfg(target_os = "linux")]
fn workspace_journal(directory: &Dir, root: &Dir) -> Result<ProtectionJournal> {
    let root = root.dir_metadata()?;
    let key = format!("{}-{}", root.dev(), root.ino());
    let lock = open_private_lock(directory, &format!("{key}.lock"))?;
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => {
            Error::Sandbox("another process is executing in this workspace".into())
        }
        std::fs::TryLockError::Error(error) => error.into(),
    })?;
    Ok(ProtectionJournal {
        _lock: lock,
        directory: directory.try_clone()?,
        name: format!("{key}.journal"),
    })
}

#[cfg(target_os = "linux")]
fn open_private_lock(directory: &Dir, name: &str) -> Result<File> {
    loop {
        match directory.symlink_metadata(name) {
            Ok(before) => {
                validate_private_file(&before)?;
                let mut options = OpenOptions::new();
                options.read(true).write(true);
                let file = directory.open_with(name, &options)?;
                let opened = file.metadata()?;
                let after = directory.symlink_metadata(name)?;
                validate_private_file(&after)?;
                if !same_cap_file(&before, &opened) || !same_cap_file(&opened, &after) {
                    return Err(Error::Sandbox(
                        "sandbox protection lock changed while opening".into(),
                    ));
                }
                return Ok(file.into_std());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut options = OpenOptions::new();
                options.read(true).write(true).create_new(true).mode(0o600);
                match directory.open_with(name, &options) {
                    Ok(file) => {
                        let opened = file.metadata()?;
                        let current = directory.symlink_metadata(name)?;
                        validate_private_file(&current)?;
                        if !same_cap_file(&opened, &current) {
                            return Err(Error::Sandbox(
                                "sandbox protection lock changed while creating".into(),
                            ));
                        }
                        sync_directory(directory)?;
                        return Ok(file.into_std());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
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

#[cfg(target_os = "linux")]
fn read_protection_record(journal: &ProtectionJournal) -> Result<Option<ProtectionRecord>> {
    let before = match journal.directory.symlink_metadata(&journal.name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    validate_private_file(&before)?;
    let file = journal.directory.open(&journal.name)?;
    let opened = file.metadata()?;
    let after = journal.directory.symlink_metadata(&journal.name)?;
    validate_private_file(&after)?;
    if !same_cap_file(&before, &opened) || !same_cap_file(&opened, &after) {
        return Err(Error::Sandbox(
            "sandbox protection journal changed while opening".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(invalid_journal());
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    let record =
        serde_json::from_slice::<ProtectionRecord>(&bytes).map_err(|_| invalid_journal())?;
    if !valid_stage_name(&record.stage) || record.targets == 0 || record.targets & !0b111 != 0 {
        return Err(invalid_journal());
    }
    Ok(Some(record))
}

#[cfg(target_os = "linux")]
fn write_protection_record(
    journal: &ProtectionJournal,
    record: Option<&ProtectionRecord>,
) -> Result<()> {
    let Some(record) = record else {
        match journal.directory.symlink_metadata(&journal.name) {
            Ok(metadata) => {
                validate_private_file(&metadata)?;
                journal.directory.remove_file(&journal.name)?;
                sync_directory(&journal.directory)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    };
    let content = serde_json::to_vec(record).map_err(|_| invalid_journal())?;
    if content.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(invalid_journal());
    }
    let temporary = format!("{}.{}.tmp", journal.name, uuid::Uuid::new_v4());
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let mut file = journal.directory.open_with(&temporary, &options)?;
        file.write_all(&content)?;
        file.sync_all()?;
        let metadata = file.metadata()?;
        let current = journal.directory.symlink_metadata(&temporary)?;
        validate_private_file(&current)?;
        if !same_cap_file(&metadata, &current) {
            return Err(Error::Sandbox(
                "sandbox protection journal changed while writing".into(),
            ));
        }
        drop(file);
        match journal.directory.symlink_metadata(&journal.name) {
            Ok(metadata) => validate_private_file(&metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        journal
            .directory
            .rename(&temporary, &journal.directory, &journal.name)?;
        sync_directory(&journal.directory)
    })();
    if result.is_err() {
        let _ = journal.directory.remove_file(&temporary);
    }
    result
}

#[cfg(target_os = "linux")]
fn invalid_journal() -> Error {
    Error::Sandbox("sandbox protection journal is invalid".into())
}

#[cfg(target_os = "linux")]
fn validate_private_file(metadata: &cap_std::fs::Metadata) -> Result<()> {
    if metadata.is_symlink()
        || !metadata.is_file()
        || metadata.uid() != current_uid()?
        || metadata.permissions().mode() & 0o177 != 0
    {
        return Err(Error::Sandbox(
            "sandbox protection state is not private".into(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn current_uid() -> Result<u32> {
    Ok(std::fs::metadata("/proc/self")?.uid())
}

fn sync_directory(directory: &Dir) -> Result<()> {
    directory.open(".")?.sync_all()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn valid_stage_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    name.starts_with(".horus-protected-")
        && name.ends_with(".tmp")
        && matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
}

#[cfg(target_os = "linux")]
fn recover_protected_files(root: &Dir, journal: &ProtectionJournal) -> Result<()> {
    let Some(record) = read_protection_record(journal)? else {
        return Ok(());
    };
    match record.identity {
        Some((dev, ino)) => {
            for (index, name) in PROTECTED_METADATA.into_iter().enumerate() {
                if record.targets & (1 << index) != 0 {
                    remove_owned_file(root, name, dev, ino)?;
                }
            }
            remove_owned_file(root, &record.stage, dev, ino)?;
        }
        None => remove_planned_stage(root, &record.stage)?,
    }
    sync_directory(root)?;
    write_protection_record(journal, None)
}

#[cfg(target_os = "linux")]
fn remove_owned_file(root: &Dir, name: &str, dev: u64, ino: u64) -> Result<()> {
    match root.symlink_metadata(name) {
        Ok(metadata) if metadata.dev() == dev && metadata.ino() == ino => {
            if !metadata.is_file() || metadata.is_symlink() {
                return Err(Error::Sandbox(format!(
                    "owned sandbox metadata has an invalid type: {name}"
                )));
            }
            root.remove_file(name)?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn remove_planned_stage(root: &Dir, name: &str) -> Result<()> {
    let metadata = match root.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() && !metadata.is_symlink() => metadata,
        Ok(_) => {
            return Err(Error::Sandbox(
                "planned sandbox metadata has an invalid type".into(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let file = root.open(name)?;
    if !same_cap_file(&metadata, &file.metadata()?) {
        return Err(Error::Sandbox(
            "planned sandbox metadata changed while recovering".into(),
        ));
    }
    let mut content = Vec::new();
    file.take(PROTECTED_MARKER.len() as u64 + 1)
        .read_to_end(&mut content)?;
    if !PROTECTED_MARKER.starts_with(&content) {
        return Err(Error::Sandbox(
            "planned sandbox metadata is not owned by Horus".into(),
        ));
    }
    let current = root.symlink_metadata(name)?;
    if !same_cap_file(&metadata, &current) {
        return Err(Error::Sandbox(
            "planned sandbox metadata changed while recovering".into(),
        ));
    }
    root.remove_file(name)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn publish_protected_files(root: &Dir, journal: &ProtectionJournal) -> Result<Vec<ProtectedFile>> {
    let mut targets = 0;
    for (index, name) in PROTECTED_METADATA.into_iter().enumerate() {
        match root.symlink_metadata(name) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => targets |= 1 << index,
            Err(error) => return Err(error.into()),
        }
    }
    if targets == 0 {
        return Ok(Vec::new());
    }
    let stage = format!(".horus-protected-{}.tmp", uuid::Uuid::new_v4());
    let mut record = ProtectionRecord {
        stage: stage.clone(),
        identity: None,
        targets,
    };
    write_protection_record(journal, Some(&record))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = root.open_with(&stage, &options)?;
    file.write_all(PROTECTED_MARKER)?;
    file.sync_all()?;
    let mut permissions = file.metadata()?.permissions();
    permissions.set_readonly(true);
    file.set_permissions(permissions)?;
    let metadata = file.metadata()?;
    let identity = (metadata.dev(), metadata.ino());
    record.identity = Some(identity);
    write_protection_record(journal, Some(&record))?;
    let mut created = Vec::new();
    for (index, name) in PROTECTED_METADATA.into_iter().enumerate() {
        if targets & (1 << index) == 0 {
            continue;
        }
        match root.hard_link(&stage, root, name) {
            Ok(()) => created.push(ProtectedFile {
                name,
                dev: identity.0,
                ino: identity.1,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    remove_owned_file(root, &stage, identity.0, identity.1)?;
    Ok(created)
}

#[cfg(target_os = "linux")]
fn cleanup_protected_files(root: &Dir, created: &mut Vec<ProtectedFile>) -> Result<()> {
    let mut cleanup_error = None;
    let mut changed = false;
    created.retain(|file| match root.symlink_metadata(file.name) {
        Ok(current) if current.dev() == file.dev && current.ino() == file.ino => {
            if let Err(error) = root.remove_file(file.name) {
                cleanup_error.get_or_insert(error);
                true
            } else {
                changed = true;
                false
            }
        }
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            cleanup_error.get_or_insert(error);
            true
        }
    });
    let durability = if changed {
        sync_directory(root)
    } else {
        Ok(())
    };
    match (cleanup_error, durability) {
        (None, durability) => durability,
        (Some(error), Ok(())) => Err(error.into()),
        (Some(error), Err(durability)) => Err(Error::Sandbox(format!(
            "{error}; workspace directory sync failed: {durability}"
        ))),
    }
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

fn is_protected_metadata(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component,
            Component::Normal(name)
                if PROTECTED_METADATA
                    .iter()
                    .any(|protected| name == std::ffi::OsStr::new(protected))
        )
    })
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
                NetworkAccess::Denied,
                CommandMode::Background,
                CommandOutputSink::default(),
            )
            .await
            .expect("background command");

        assert_eq!(output.exit_code, 0);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test(start_paused = true)]
    async fn command_cleanup_reaps_daemonized_descendants() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = LocalSandbox::new(workspace.path())
            .expect("sandbox")
            .command_timeout(Duration::from_millis(100))
            .expect("timeout");
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
                    NetworkAccess::Denied,
                    CommandMode::Foreground,
                    CommandOutputSink::default(),
                )
                .await
        });
        let pid_path = workspace.path().join("daemon.pid");
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

    fn local_sandbox(workspace: &Path) -> LocalSandbox {
        LocalSandbox::new(workspace).expect("sandbox")
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
        #[cfg(target_os = "linux")]
        let _protected = sandbox
            .protect_command_metadata()
            .await
            .expect("protect command metadata");
        let denied = sandbox
            .sandboxed_command(&Invocation::Shell("true"), NetworkAccess::Denied, true)
            .expect("network-disabled command");
        let allowed = sandbox
            .sandboxed_command(&Invocation::Shell("true"), NetworkAccess::Allowed, true)
            .expect("network-enabled command");
        #[cfg(target_os = "linux")]
        _protected
            .finish()
            .await
            .expect("release protected metadata");

        #[cfg(target_os = "linux")]
        {
            let denied = denied
                .as_std()
                .get_args()
                .any(|argument| argument == "--unshare-net");
            let allowed = allowed
                .as_std()
                .get_args()
                .any(|argument| argument == "--unshare-net");
            assert_eq!((denied, allowed), (true, false));
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
            assert!(!policy(&denied).contains("(allow network*)"));
            assert!(policy(&allowed).contains("(allow network*)"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn filesystem_handles_reject_symlink_escapes_and_protected_aliases() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().expect("parent");
        let workspace = parent.path().join("workspace");
        let outside = parent.path().join("outside.txt");
        let outside_directory = parent.path().join("outside");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(&outside_directory).expect("outside directory");
        std::fs::write(&outside, "outside").expect("outside");
        std::fs::create_dir(workspace.join(".git")).expect("metadata");
        std::fs::write(workspace.join(".git/config"), "protected").expect("protected file");
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
            std::fs::read_to_string(workspace.join(".git/config")).expect("protected"),
            "protected"
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
    #[tokio::test]
    async fn commands_cannot_create_absent_protected_metadata() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = local_sandbox(workspace.path());

        let output = sandbox
            .execute(
                "mkdir .git .agents .codex",
                NetworkAccess::Denied,
                CommandMode::Foreground,
                CommandOutputSink::default(),
            )
            .await
            .expect("sandboxed command");

        assert_ne!(output.exit_code, 0);
        assert!(
            PROTECTED_METADATA
                .iter()
                .all(|name| !workspace.path().join(name).exists())
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn sandbox_instances_coordinate_protected_metadata() {
        let workspace = tempfile::tempdir().expect("workspace");
        let first = local_sandbox(workspace.path());
        let second = local_sandbox(workspace.path());
        let first_guard = first
            .protect_command_metadata()
            .await
            .expect("first metadata guard");
        let second_guard = second
            .protect_command_metadata()
            .await
            .expect("second metadata guard");

        first_guard.finish().await.expect("release first guard");
        assert!(
            PROTECTED_METADATA
                .iter()
                .all(|name| workspace.path().join(name).is_file())
        );
        second_guard.finish().await.expect("release second guard");
        assert!(
            PROTECTED_METADATA
                .iter()
                .all(|name| !workspace.path().join(name).exists())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn failed_protected_cleanup_is_retained_for_retry() {
        let workspace = tempfile::tempdir().expect("workspace");
        let root = Dir::open_ambient_dir(workspace.path(), ambient_authority()).expect("open root");
        root.create_dir(".git").expect("create protected directory");
        let metadata = root.symlink_metadata(".git").expect("metadata");
        let mut created = vec![ProtectedFile {
            name: ".git",
            dev: metadata.dev(),
            ino: metadata.ino(),
        }];

        assert!(cleanup_protected_files(&root, &mut created).is_err());
        assert_eq!(created.len(), 1);
        root.remove_dir(".git").expect("remove protected directory");
        cleanup_protected_files(&root, &mut created).expect("retry cleanup");
        assert!(created.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn orphaned_protected_metadata_is_recovered_under_a_process_lock() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = LocalSandbox::new(workspace.path()).expect("create local sandbox");
        let first =
            workspace_journal(&sandbox.protection_dir, &sandbox.root_dir).expect("first journal");
        assert!(workspace_journal(&sandbox.protection_dir, &sandbox.root_dir).is_err());
        let created =
            publish_protected_files(&sandbox.root_dir, &first).expect("publish protected files");
        assert_eq!(created.len(), 3);
        drop(first);

        let recovered =
            workspace_journal(&sandbox.protection_dir, &sandbox.root_dir).expect("next journal");
        recover_protected_files(&sandbox.root_dir, &recovered).expect("recover files");
        assert!(
            PROTECTED_METADATA
                .iter()
                .all(|name| !workspace.path().join(name).exists())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn partial_planned_stage_is_recovered_after_a_crash() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = LocalSandbox::new(workspace.path()).expect("create local sandbox");
        let journal =
            workspace_journal(&sandbox.protection_dir, &sandbox.root_dir).expect("journal");
        let record = ProtectionRecord {
            stage: format!(".horus-protected-{}.tmp", uuid::Uuid::new_v4()),
            identity: None,
            targets: 0b111,
        };
        write_protection_record(&journal, Some(&record)).expect("planned record");
        sandbox
            .root_dir
            .write(&record.stage, &PROTECTED_MARKER[..8])
            .expect("partial stage");

        recover_protected_files(&sandbox.root_dir, &journal).expect("recover partial stage");

        assert!(!workspace.path().join(record.stage).exists());
        assert!(
            read_protection_record(&journal)
                .expect("read journal")
                .is_none()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn protection_journal_never_follows_a_symlink() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = LocalSandbox::new(workspace.path()).expect("create local sandbox");
        let journal =
            workspace_journal(&sandbox.protection_dir, &sandbox.root_dir).expect("journal");
        write_protection_record(&journal, None).expect("clear journal");
        let victim = format!("victim-{}", uuid::Uuid::new_v4());
        journal
            .directory
            .write(&victim, b"untouched")
            .expect("victim");
        journal
            .directory
            .symlink(&victim, &journal.name)
            .expect("journal symlink");

        assert!(recover_protected_files(&sandbox.root_dir, &journal).is_err());
        assert_eq!(
            journal.directory.read(&victim).expect("read victim"),
            b"untouched"
        );

        journal
            .directory
            .remove_file(&journal.name)
            .expect("remove journal symlink");
        journal
            .directory
            .remove_file(victim)
            .expect("remove victim");
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
