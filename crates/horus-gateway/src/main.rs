use std::ffi::OsString;
#[cfg(any(unix, test))]
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write as _;
#[cfg(any(unix, test))]
use std::io::{Read as _, Seek as _, SeekFrom};
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, Instant};

use horus_gateway::auth::AuthStore;
use horus_gateway::config::{ConfigStore, DEFAULT_LISTEN, TlsConfig, state_dir};
use horus_gateway::server::GatewayServer;
use horus_gateway::wire::BootstrapPayload;
use horus_gateway::{Error, Result};
#[cfg(unix)]
use nix::sys::signal::{Signal, kill};
#[cfg(unix)]
use nix::unistd::Pid;
#[cfg(any(unix, test))]
use serde::{Deserialize, Serialize};

const USAGE: &str = "usage: horus-gateway init [--workspace PATH] [--state-dir PATH] \
                     [--listen ADDR] [--tls-cert PATH --tls-key PATH]\n       \
                     horus-gateway bootstrap [--workspace PATH] [--state-dir PATH] \
                     [--listen ADDR]\n       \
                     horus-gateway serve [--state-dir PATH]\n       \
                     horus-gateway pair-code [--state-dir PATH]\n       \
                     horus-gateway status [--state-dir PATH]\n       \
                     horus-gateway exit [--state-dir PATH]";

#[cfg(any(unix, test))]
const PROCESS_FILE: &str = "gateway-process.json";
#[cfg(any(unix, test))]
const MAX_PROCESS_RECORD_BYTES: usize = 4 * 1024;
#[cfg(unix)]
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug)]
enum Command {
    Init(InitOptions),
    Bootstrap(InitOptions),
    Serve { state_dir: PathBuf },
    PairCode { state_dir: PathBuf },
    Status { state_dir: PathBuf },
    Exit { state_dir: PathBuf },
}

#[derive(Debug)]
struct InitOptions {
    workspace: PathBuf,
    state_dir: PathBuf,
    listen: SocketAddr,
    tls: Option<TlsConfig>,
}

#[cfg(any(unix, test))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessRecord {
    pid: u32,
}

struct ProcessRecordGuard {
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(unix)]
    file: File,
}

#[tokio::main]
async fn main() -> Result<()> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if matches!(arguments.as_slice(), [flag] if flag == "--help" || flag == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    if matches!(arguments.as_slice(), [flag] if flag == "--version" || flag == "-V") {
        println!("horus-gateway {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    match parse(arguments)? {
        Command::Init(options) => initialize(options),
        Command::Bootstrap(options) => bootstrap(options).await,
        Command::Serve { state_dir } => serve(state_dir).await,
        Command::PairCode { state_dir } => create_pairing_code(state_dir),
        Command::Status { state_dir } => show_status(state_dir),
        Command::Exit { state_dir } => exit_gateway(state_dir),
    }
}

async fn bootstrap(options: InitOptions) -> Result<()> {
    if options.tls.is_some() || !options.listen.ip().is_loopback() {
        return Err(Error::Config(
            "automatic bootstrap supports only the local plaintext gateway".into(),
        ));
    }
    let state_dir = options.state_dir.clone();
    let (server, grant) =
        GatewayServer::bootstrap(options.state_dir, options.workspace, options.listen).await?;
    let _process_record = ProcessRecordGuard::create(&state_dir)?;
    serde_json::to_writer(
        std::io::stdout().lock(),
        &BootstrapPayload {
            pairing_code: grant.code,
        },
    )?;
    println!();
    std::io::stdout().flush()?;
    server.serve().await
}

fn initialize(options: InitOptions) -> Result<()> {
    let (store, config) = ConfigStore::initialize(
        options.state_dir,
        options.workspace,
        options.listen,
        options.tls,
    )?;
    let (_, pairing) = AuthStore::initialize(store.auth_path())?;
    let scheme = if config.tls.is_some() { "tls" } else { "tcp" };
    println!(
        "initialized Horus gateway for {}",
        config.workspace.display()
    );
    println!("endpoint: {scheme}://{}", config.listen);
    println!("pairing code: {}", pairing.code);
    println!("pairing code expires at Unix time {}", pairing.expires_at);
    Ok(())
}

async fn serve(state_dir: PathBuf) -> Result<()> {
    let server = GatewayServer::open(state_dir.clone()).await?;
    let _process_record = ProcessRecordGuard::create(&state_dir)?;
    server.serve().await
}

fn create_pairing_code(state_dir: PathBuf) -> Result<()> {
    let (store, config) = ConfigStore::open(state_dir)?;
    let _listener = std::net::TcpListener::bind(config.listen).map_err(|error| {
        Error::Config(format!(
            "stop the gateway before creating a pairing code: {error}"
        ))
    })?;
    let grant = AuthStore::open(store.auth_path())?.create_pairing_code()?;
    println!("pairing code: {}", grant.code);
    println!("pairing code expires at Unix time {}", grant.expires_at);
    Ok(())
}

#[cfg(unix)]
fn show_status(state_dir: PathBuf) -> Result<()> {
    let (store, config) = ConfigStore::open(state_dir)?;
    let path = store.state_dir().join(PROCESS_FILE);
    let running = match open_process_record(&path)? {
        Some((_, file)) => process_is_running(&file)?,
        None => false,
    };
    let scheme = if config.tls.is_some() { "tls" } else { "tcp" };
    println!("workspace: {}", config.workspace.display());
    println!("endpoint: {scheme}://{}", config.listen);
    println!("status: {}", if running { "running" } else { "stopped" });
    Ok(())
}

#[cfg(not(unix))]
fn show_status(_state_dir: PathBuf) -> Result<()> {
    Err(unsupported_lifecycle())
}

#[cfg(unix)]
fn exit_gateway(state_dir: PathBuf) -> Result<()> {
    let (store, _) = ConfigStore::open(state_dir)?;
    let path = store.state_dir().join(PROCESS_FILE);
    let Some((record, file)) = open_process_record(&path)? else {
        println!("gateway is stopped");
        return Ok(());
    };
    if !process_is_running(&file)? {
        println!("gateway is stopped");
        return Ok(());
    }
    let pid = i32::try_from(record.pid)
        .map(Pid::from_raw)
        .map_err(|_| Error::Config("invalid gateway process record".into()))?;
    if let Err(error) = kill(pid, Signal::SIGINT) {
        if !process_is_running(&file)? {
            println!("gateway is stopped");
            return Ok(());
        }
        return Err(Error::Config(format!(
            "failed to interrupt gateway: {error}"
        )));
    }
    let started = Instant::now();
    while process_is_running(&file)? {
        if started.elapsed() >= EXIT_TIMEOUT {
            return Err(Error::Config(format!(
                "gateway process {} did not stop within {} seconds",
                record.pid,
                EXIT_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(EXIT_POLL_INTERVAL);
    }
    println!("gateway stopped");
    Ok(())
}

#[cfg(not(unix))]
fn exit_gateway(_state_dir: PathBuf) -> Result<()> {
    Err(unsupported_lifecycle())
}

#[cfg(any(unix, test))]
impl ProcessRecord {
    fn validate(&self) -> Result<()> {
        if self.pid == 0 || i32::try_from(self.pid).is_err() {
            return Err(Error::Config("invalid gateway process record".into()));
        }
        Ok(())
    }
}

impl ProcessRecordGuard {
    #[cfg(unix)]
    fn create(state_dir: &Path) -> Result<Self> {
        let state_dir = fs::canonicalize(state_dir)?;
        let path = state_dir.join(PROCESS_FILE);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => Error::Config("gateway is already running".into()),
            TryLockError::Error(error) => error.into(),
        })?;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        let record = ProcessRecord {
            pid: std::process::id(),
        };
        serde_json::to_writer(&mut file, &record)?;
        file.flush()?;
        file.sync_all()?;
        Ok(Self { path, file })
    }

    #[cfg(not(unix))]
    fn create(_state_dir: &Path) -> Result<Self> {
        Ok(Self {})
    }
}

impl Drop for ProcessRecordGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = fs::remove_file(&self.path);
            let _ = self.file.unlock();
        }
    }
}

#[cfg(any(unix, test))]
fn open_process_record(path: &Path) -> Result<Option<(ProcessRecord, File)>> {
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() > MAX_PROCESS_RECORD_BYTES as u64 {
        return Err(Error::Config("gateway process record is too large".into()));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut contents = Vec::new();
    (&mut file)
        .take(MAX_PROCESS_RECORD_BYTES as u64 + 1)
        .read_to_end(&mut contents)?;
    if contents.len() > MAX_PROCESS_RECORD_BYTES {
        return Err(Error::Config("gateway process record is too large".into()));
    }
    let record: ProcessRecord = serde_json::from_slice(&contents)?;
    record.validate()?;
    Ok(Some((record, file)))
}

#[cfg(any(unix, test))]
fn process_is_running(file: &File) -> Result<bool> {
    match file.try_lock() {
        Ok(()) => {
            file.unlock()?;
            Ok(false)
        }
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

#[cfg(not(unix))]
fn unsupported_lifecycle() -> Error {
    Error::Config("gateway process lifecycle commands require macOS or Linux".into())
}

fn parse(arguments: Vec<OsString>) -> Result<Command> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Err(Error::Config(USAGE.into()));
    };
    if command == "init" {
        parse_init(arguments.collect()).map(Command::Init)
    } else if command == "bootstrap" {
        parse_init(arguments.collect()).map(Command::Bootstrap)
    } else if command == "serve" {
        parse_state_dir(arguments.collect()).map(|state_dir| Command::Serve { state_dir })
    } else if command == "pair-code" {
        parse_state_dir(arguments.collect()).map(|state_dir| Command::PairCode { state_dir })
    } else if command == "status" {
        parse_state_dir(arguments.collect()).map(|state_dir| Command::Status { state_dir })
    } else if command == "exit" {
        parse_state_dir(arguments.collect()).map(|state_dir| Command::Exit { state_dir })
    } else {
        Err(Error::Config(USAGE.into()))
    }
}

fn parse_init(arguments: Vec<OsString>) -> Result<InitOptions> {
    let mut workspace = None;
    let mut configured_state_dir = None;
    let mut listen = None;
    let mut certificate = None;
    let mut private_key = None;
    let mut arguments = arguments.into_iter();
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| Error::Config(format!("{} requires a value", flag.to_string_lossy())))?;
        if flag == "--workspace" {
            set_once(&mut workspace, PathBuf::from(value), "--workspace")?;
        } else if flag == "--state-dir" {
            set_once(
                &mut configured_state_dir,
                PathBuf::from(value),
                "--state-dir",
            )?;
        } else if flag == "--listen" {
            let value = value
                .to_str()
                .ok_or_else(|| Error::Config("--listen is not valid UTF-8".into()))?
                .parse()
                .map_err(|_| Error::Config("--listen is not a socket address".into()))?;
            set_once(&mut listen, value, "--listen")?;
        } else if flag == "--tls-cert" {
            set_once(&mut certificate, PathBuf::from(value), "--tls-cert")?;
        } else if flag == "--tls-key" {
            set_once(&mut private_key, PathBuf::from(value), "--tls-key")?;
        } else {
            return Err(Error::Config(USAGE.into()));
        }
    }
    let workspace = match workspace {
        Some(workspace) => workspace,
        None => std::env::current_dir()?,
    };
    let state_dir = configured_state_dir.map_or_else(state_dir, Ok)?;
    let listen = listen.unwrap_or(DEFAULT_LISTEN);
    let tls = match (certificate, private_key) {
        (Some(certificate), Some(private_key)) => Some(TlsConfig {
            certificate: std::fs::canonicalize(certificate)?,
            private_key: std::fs::canonicalize(private_key)?,
        }),
        (None, None) => None,
        _ => {
            return Err(Error::Config(
                "--tls-cert and --tls-key must be supplied together".into(),
            ));
        }
    };
    Ok(InitOptions {
        workspace,
        state_dir,
        listen,
        tls,
    })
}

fn parse_state_dir(arguments: Vec<OsString>) -> Result<PathBuf> {
    let state_dir = match arguments.as_slice() {
        [] => state_dir()?,
        [flag, path] if flag == "--state-dir" => PathBuf::from(path),
        _ => return Err(Error::Config(USAGE.into())),
    };
    Ok(state_dir)
}

fn set_once<T>(target: &mut Option<T>, value: T, flag: &str) -> Result<()> {
    if target.replace(value).is_some() {
        return Err(Error::Config(format!("{flag} was supplied more than once")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_serve_accepts_an_explicit_state_directory() {
        let command = parse(vec![
            "serve".into(),
            "--state-dir".into(),
            "/tmp/horus".into(),
        ])
        .expect("parse serve");

        assert!(matches!(
            command,
            Command::Serve { state_dir } if state_dir == std::path::Path::new("/tmp/horus")
        ));
    }

    #[test]
    fn parse_bootstrap_uses_explicit_workspace_and_state() {
        let command = parse(vec![
            "bootstrap".into(),
            "--workspace".into(),
            "/tmp/workspace".into(),
            "--state-dir".into(),
            "/tmp/horus".into(),
        ])
        .expect("parse bootstrap");

        assert!(matches!(
            command,
            Command::Bootstrap(InitOptions { workspace, state_dir, .. })
                if workspace == std::path::Path::new("/tmp/workspace")
                    && state_dir == std::path::Path::new("/tmp/horus")
        ));
    }

    #[test]
    fn parse_pair_code_accepts_an_explicit_state_directory() {
        let command = parse(vec![
            "pair-code".into(),
            "--state-dir".into(),
            "/tmp/horus".into(),
        ])
        .expect("parse pair-code");

        assert!(matches!(
            command,
            Command::PairCode { state_dir } if state_dir == std::path::Path::new("/tmp/horus")
        ));
    }

    #[test]
    fn parse_status_accepts_an_explicit_state_directory() {
        let command = parse(vec![
            "status".into(),
            "--state-dir".into(),
            "/tmp/horus".into(),
        ])
        .expect("parse status");

        assert!(matches!(
            command,
            Command::Status { state_dir } if state_dir == std::path::Path::new("/tmp/horus")
        ));
    }

    #[test]
    fn parse_exit_accepts_an_explicit_state_directory() {
        let command = parse(vec![
            "exit".into(),
            "--state-dir".into(),
            "/tmp/horus".into(),
        ])
        .expect("parse exit");

        assert!(matches!(
            command,
            Command::Exit { state_dir } if state_dir == std::path::Path::new("/tmp/horus")
        ));
    }

    #[test]
    fn process_record_rejects_an_invalid_pid() {
        let directory = tempfile::tempdir().expect("process record directory");
        let path = directory.path().join(PROCESS_FILE);
        std::fs::write(&path, r#"{"pid":0}"#).expect("write process record");

        let error = open_process_record(&path).expect_err("invalid PID must fail");

        assert!(error.to_string().contains("invalid gateway process record"));
    }

    #[cfg(unix)]
    #[test]
    fn process_record_lock_tracks_the_gateway_lifetime() {
        let directory = tempfile::tempdir().expect("process record directory");
        let guard = ProcessRecordGuard::create(directory.path()).expect("process record");
        let (_, file) = open_process_record(&guard.path)
            .expect("read process record")
            .expect("process record");

        assert!(process_is_running(&file).expect("locked process record"));
        drop(guard);
        assert!(!directory.path().join(PROCESS_FILE).exists());
    }

    #[test]
    fn parse_init_requires_both_tls_paths() {
        let error = parse(vec![
            "init".into(),
            "--tls-cert".into(),
            "/tmp/certificate.pem".into(),
        ])
        .expect_err("partial TLS config must fail");

        assert!(error.to_string().contains("supplied together"));
    }
}
