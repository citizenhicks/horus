//! Command-line entrypoint shared by the gateway and CLI packages.

use std::ffi::OsString;
#[cfg(any(unix, test))]
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write as _;
#[cfg(any(unix, test))]
use std::io::{Read as _, Seek as _, SeekFrom};
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::auth::AuthStore;
#[cfg(unix)]
use crate::auth::PairingStatus;
use crate::client::Endpoint;
use crate::config::{ConfigStore, DEFAULT_LISTEN, GatewayConfig, TlsConfig, state_dir};
use crate::server::GatewayServer;
use crate::wire::BootstrapPayload;
use crate::{Error, Result};
#[cfg(unix)]
use nix::sys::signal::{Signal, kill};
#[cfg(unix)]
use nix::unistd::Pid;
#[cfg(any(unix, test))]
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use tokio::process::{Child, Command as TokioCommand};
#[cfg(unix)]
use tokio::signal::unix::{Signal as TokioSignal, SignalKind, signal};

const USAGE: &str = "usage: horus-gateway init [--state-dir PATH] [--listen ADDR] \
                     [--tls-cert PATH --tls-key PATH]\n       \
                     horus-gateway bootstrap [--state-dir PATH] [--listen ADDR]\n       \
                     horus-gateway connect [--state-dir PATH] [--endpoint ENDPOINT]\n       \
                     horus-gateway serve [--state-dir PATH] [--background]\n       \
                     horus-gateway status [--state-dir PATH]\n       \
                     horus-gateway exit [--state-dir PATH]";

#[cfg(any(unix, test))]
const PROCESS_FILE: &str = "gateway-process.json";
#[cfg(unix)]
const STARTUP_FILE: &str = "gateway-start.lock";
#[cfg(any(unix, test))]
const MAX_PROCESS_RECORD_BYTES: usize = 4 * 1024;
#[cfg(unix)]
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(unix)]
const BACKGROUND_START_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(unix)]
const BACKGROUND_START_POLL_INTERVAL: Duration = Duration::from_millis(50);
#[cfg(unix)]
const MAX_BACKGROUND_ERROR_BYTES: u64 = 16 * 1024;
#[cfg(unix)]
const CONNECTION_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug)]
enum Command {
    Init(InitOptions),
    Bootstrap(InitOptions),
    Connect(ConnectOptions),
    Serve {
        state_dir: PathBuf,
        background: bool,
    },
    ServeChild {
        state_dir: PathBuf,
    },
    Status {
        state_dir: PathBuf,
    },
    Exit {
        state_dir: PathBuf,
    },
}

#[derive(Debug)]
struct InitOptions {
    state_dir: PathBuf,
    listen: SocketAddr,
    tls: Option<TlsConfig>,
}

#[derive(Debug)]
struct ConnectOptions {
    state_dir: PathBuf,
    endpoint: Option<Endpoint>,
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

#[derive(Debug)]
struct StartupGuard {
    #[cfg(unix)]
    file: File,
}

/// Runs a gateway command with arguments excluding the executable name.
pub async fn run(arguments: Vec<OsString>) -> Result<()> {
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
        Command::Connect(options) => connect(options).await,
        Command::Serve {
            state_dir,
            background,
        } => {
            if background {
                serve_in_background(state_dir).await
            } else {
                serve(state_dir, true).await
            }
        }
        Command::ServeChild { state_dir } => serve(state_dir, false).await,
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
    let (server, grant) = GatewayServer::bootstrap(options.state_dir, options.listen).await?;
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
    let (store, config) = ConfigStore::initialize(options.state_dir, options.listen, options.tls)?;
    AuthStore::initialize(store.auth_path())?;
    println!("initialized Horus gateway");
    print_listener(&config);
    println!("run `horus-gateway connect` to pair a client");
    Ok(())
}

#[cfg(unix)]
async fn connect(options: ConnectOptions) -> Result<()> {
    let (store, config) = ConfigStore::open(options.state_dir)?;
    let endpoint = connection_endpoint(&config, options.endpoint)?;
    let startup = StartupGuard::create(store.state_dir())?;
    ensure_gateway_stopped(&store, &config)?;
    let mut interrupts = signal(SignalKind::interrupt())?;
    let mut terminations = signal(SignalKind::terminate())?;

    let auth = AuthStore::open(store.auth_path())?;
    let grant = auth.create_pairing_code()?;
    let deadline = pairing_deadline(grant.expires_at)?;
    let pid = match start_background_gateway(store.state_dir(), &mut interrupts, &mut terminations)
        .await
    {
        Ok(Some(pid)) => pid,
        Ok(None) => {
            AuthStore::open(store.auth_path())?.revoke_pairing_code(&grant.code)?;
            println!("connection cancelled");
            return Ok(());
        }
        Err(error) => {
            if let Err(revoke) =
                AuthStore::open(store.auth_path())?.revoke_pairing_code(&grant.code)
            {
                return Err(Error::Config(format!(
                    "{error}; failed to revoke one-time code: {revoke}"
                )));
            }
            return Err(error);
        }
    };
    drop(startup);

    if let Err(error) = print_connection(&endpoint, &grant.code) {
        return stop_connect_gateway(&store, pid, &grant.code, error.into());
    }

    let process_path = store.state_dir().join(PROCESS_FILE);
    loop {
        let running = match running_process_pid(&process_path) {
            Ok(running) => running,
            Err(error) => return stop_connect_gateway(&store, pid, &grant.code, error),
        };
        match running {
            Some(running) if running == pid => {}
            Some(running) => {
                return Err(Error::Config(format!(
                    "gateway process changed from {pid} to {running} while waiting for a client"
                )));
            }
            None => {
                return stop_connect_gateway(
                    &store,
                    pid,
                    &grant.code,
                    Error::Config("gateway stopped before a client paired".into()),
                );
            }
        }

        let pairing = match AuthStore::open(store.auth_path())
            .and_then(|auth| auth.pairing_status(&grant.code))
        {
            Ok(pairing) => pairing,
            Err(error) => return stop_connect_gateway(&store, pid, &grant.code, error),
        };
        match pairing {
            PairingStatus::Consumed => {
                println!("paired; gateway running in background (pid {pid})");
                return Ok(());
            }
            PairingStatus::Replaced => {
                return stop_connect_gateway(
                    &store,
                    pid,
                    &grant.code,
                    Error::Config("one-time code was replaced before a client paired".into()),
                );
            }
            PairingStatus::Pending => {}
        }

        if Instant::now() >= deadline {
            return stop_connect_gateway(
                &store,
                pid,
                &grant.code,
                Error::Config("one-time code expired before a client paired".into()),
            );
        }

        tokio::select! {
            () = shutdown_signal(&mut interrupts, &mut terminations) => {
                cleanup_connect(&store, pid, &grant.code)?;
                println!("connection cancelled");
                return Ok(());
            }
            () = tokio::time::sleep(CONNECTION_POLL_INTERVAL) => {}
        }
    }
}

#[cfg(not(unix))]
async fn connect(_options: ConnectOptions) -> Result<()> {
    Err(unsupported_lifecycle())
}

fn connection_endpoint(config: &GatewayConfig, endpoint: Option<Endpoint>) -> Result<Endpoint> {
    match (config.tls.is_some(), endpoint) {
        (true, None) => Err(Error::Config(
            "TLS gateways require --endpoint tls://HOST:PORT using the certificate hostname".into(),
        )),
        (true, Some(endpoint)) if endpoint.is_plaintext() => Err(Error::Config(
            "a TLS gateway connection endpoint must use tls://".into(),
        )),
        (false, Some(endpoint)) if !endpoint.is_plaintext() => Err(Error::Config(
            "a plaintext gateway connection endpoint must use tcp://".into(),
        )),
        (_, Some(endpoint)) => Ok(endpoint),
        (false, None) => format!("tcp://{}", config.listen).parse(),
    }
}

#[cfg(unix)]
fn ensure_gateway_stopped(store: &ConfigStore, config: &GatewayConfig) -> Result<()> {
    if running_process_pid(&store.state_dir().join(PROCESS_FILE))?.is_some() {
        return Err(Error::Config(
            "gateway is already running; create a code from a connected client or run `horus-gateway exit` first"
                .into(),
        ));
    }
    let _listener = std::net::TcpListener::bind(config.listen).map_err(|error| {
        Error::Config(format!(
            "gateway listener {} is unavailable; stop it before connecting: {error}",
            config.listen
        ))
    })?;
    Ok(())
}

#[cfg(unix)]
fn pairing_deadline(expires_at: i64) -> Result<Instant> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Config("system clock is before the Unix epoch".into()))?
        .as_secs();
    let expires_at =
        u64::try_from(expires_at).map_err(|_| Error::Config("pairing expiry is invalid".into()))?;
    let remaining = expires_at
        .checked_sub(now)
        .ok_or_else(|| Error::Config("one-time code expired before startup".into()))?;
    Instant::now()
        .checked_add(Duration::from_secs(remaining))
        .ok_or_else(|| Error::Config("pairing deadline overflow".into()))
}

#[cfg(unix)]
fn stop_connect_gateway<T>(store: &ConfigStore, pid: u32, code: &str, error: Error) -> Result<T> {
    match cleanup_connect(store, pid, code) {
        Ok(()) => Err(error),
        Err(stop) => Err(Error::Config(format!(
            "{error}; failed to clean up connection: {stop}"
        ))),
    }
}

#[cfg(unix)]
fn cleanup_connect(store: &ConfigStore, pid: u32, code: &str) -> Result<()> {
    stop_gateway(store, Some(pid))?;
    AuthStore::open(store.auth_path())?.revoke_pairing_code(code)
}

#[cfg(unix)]
fn print_connection(endpoint: &Endpoint, code: &str) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    writeln!(output, "endpoint: {endpoint}")?;
    writeln!(output, "one-time code: {code}")?;
    writeln!(
        output,
        "iPhone, iPad, or Mac: Add gateway, then enter the endpoint and code above"
    )?;
    writeln!(output, "another terminal: horus pair {endpoint} {code}")?;
    writeln!(output, "waiting for a client…")?;
    output.flush()
}

async fn serve(state_dir: PathBuf, lock_startup: bool) -> Result<()> {
    let (store, config) = ConfigStore::open(state_dir)?;
    let state_dir = store.state_dir().to_path_buf();
    let startup = lock_startup
        .then(|| StartupGuard::create(&state_dir))
        .transpose()?;
    let server = GatewayServer::open(state_dir.clone()).await?;
    let _process_record = ProcessRecordGuard::create(&state_dir)?;
    drop(startup);
    println!("gateway serving in foreground");
    print_listener(&config);
    server.serve().await
}

#[cfg(unix)]
async fn serve_in_background(state_dir: PathBuf) -> Result<()> {
    let (store, config) = ConfigStore::open(state_dir)?;
    let _startup = StartupGuard::create(store.state_dir())?;
    let mut interrupts = signal(SignalKind::interrupt())?;
    let mut terminations = signal(SignalKind::terminate())?;
    let Some(pid) =
        start_background_gateway(store.state_dir(), &mut interrupts, &mut terminations).await?
    else {
        println!("gateway start cancelled");
        return Ok(());
    };
    println!("gateway started in background (pid {pid})");
    print_listener(&config);
    Ok(())
}

#[cfg(unix)]
async fn start_background_gateway(
    state_dir: &Path,
    interrupts: &mut TokioSignal,
    terminations: &mut TokioSignal,
) -> Result<Option<u32>> {
    let state_dir = fs::canonicalize(state_dir)?;
    let process_path = state_dir.join(PROCESS_FILE);
    if running_process_pid(&process_path)?.is_some() {
        return Err(Error::Config("gateway is already running".into()));
    }

    let log = tempfile::NamedTempFile::new_in(&state_dir)?;
    log.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    let mut command = TokioCommand::new(std::env::current_exe()?);
    command
        .arg("__serve")
        .arg("--state-dir")
        .arg(&state_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log.reopen()?));
    command.as_std_mut().process_group(0);

    let mut child = command.spawn()?;
    let Some(pid) = child.id() else {
        stop_background_child(&mut child, &process_path).await;
        return Err(Error::Config("background gateway has no process ID".into()));
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(background_startup_error(
                    format!("background gateway exited during startup with {status}"),
                    &log,
                ));
            }
            Ok(None) => {}
            Err(error) => {
                stop_background_child(&mut child, &process_path).await;
                return Err(error.into());
            }
        }

        let process_error = match running_process_pid(&process_path) {
            Ok(Some(running)) if running == pid => return Ok(Some(pid)),
            Ok(Some(running)) => {
                stop_background_child(&mut child, &process_path).await;
                return Err(background_startup_error(
                    format!("gateway process {running} claimed the process record during startup"),
                    &log,
                ));
            }
            Ok(None) => None,
            Err(error) => Some(error),
        };

        if started.elapsed() >= BACKGROUND_START_TIMEOUT {
            stop_background_child(&mut child, &process_path).await;
            let message = process_error.map_or_else(
                || {
                    format!(
                        "background gateway did not start within {} seconds",
                        BACKGROUND_START_TIMEOUT.as_secs()
                    )
                },
                |error| format!("background gateway process record is invalid: {error}"),
            );
            return Err(background_startup_error(message, &log));
        }
        tokio::select! {
            () = shutdown_signal(interrupts, terminations) => {
                stop_background_child(&mut child, &process_path).await;
                return Ok(None);
            }
            () = tokio::time::sleep(BACKGROUND_START_POLL_INTERVAL) => {}
        }
    }
}

#[cfg(unix)]
async fn shutdown_signal(interrupts: &mut TokioSignal, terminations: &mut TokioSignal) {
    tokio::select! {
        _ = interrupts.recv() => {}
        _ = terminations.recv() => {}
    }
}

#[cfg(not(unix))]
async fn serve_in_background(_state_dir: PathBuf) -> Result<()> {
    Err(unsupported_lifecycle())
}

#[cfg(unix)]
async fn stop_background_child(child: &mut Child, process_path: &Path) {
    let _ = child.kill().await;
    remove_unlocked_process_record(process_path);
}

#[cfg(unix)]
fn remove_unlocked_process_record(path: &Path) {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
        return;
    };
    if file.try_lock().is_ok() {
        let _ = fs::remove_file(path);
    }
}

#[cfg(unix)]
fn background_startup_error(
    message: impl std::fmt::Display,
    log: &tempfile::NamedTempFile,
) -> Error {
    let mut details = String::new();
    if let Ok(file) = File::open(log.path()) {
        let _ = file
            .take(MAX_BACKGROUND_ERROR_BYTES)
            .read_to_string(&mut details);
    }
    let details = details.trim();
    Error::Config(if details.is_empty() {
        message.to_string()
    } else {
        format!("{message}: {details}")
    })
}

#[cfg(unix)]
fn show_status(state_dir: PathBuf) -> Result<()> {
    let (store, config) = ConfigStore::open(state_dir)?;
    let path = store.state_dir().join(PROCESS_FILE);
    let running = match open_process_record(&path)? {
        Some((_, file)) => process_is_running(&file)?,
        None => false,
    };
    print_listener(&config);
    println!("status: {}", if running { "running" } else { "stopped" });
    Ok(())
}

fn print_listener(config: &GatewayConfig) {
    let scheme = if config.tls.is_some() { "tls" } else { "tcp" };
    println!("listener: {scheme}://{}", config.listen);
}

#[cfg(not(unix))]
fn show_status(_state_dir: PathBuf) -> Result<()> {
    Err(unsupported_lifecycle())
}

#[cfg(unix)]
fn exit_gateway(state_dir: PathBuf) -> Result<()> {
    let (store, _) = ConfigStore::open(state_dir)?;
    let _startup = StartupGuard::create(store.state_dir())?;
    stop_gateway(&store, None)
}

#[cfg(unix)]
fn stop_gateway(store: &ConfigStore, expected_pid: Option<u32>) -> Result<()> {
    let path = store.state_dir().join(PROCESS_FILE);
    let Some((record, file)) = open_process_record(&path)? else {
        println!("gateway is stopped");
        return Ok(());
    };
    if !process_is_running(&file)? {
        println!("gateway is stopped");
        return Ok(());
    }
    if let Some(expected_pid) = expected_pid
        && record.pid != expected_pid
    {
        return Err(Error::Config(format!(
            "gateway process changed from {expected_pid} to {}",
            record.pid
        )));
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

impl StartupGuard {
    #[cfg(unix)]
    fn create(state_dir: &Path) -> Result<Self> {
        let path = fs::canonicalize(state_dir)?.join(STARTUP_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => {
                Error::Config("gateway startup is already in progress".into())
            }
            TryLockError::Error(error) => error.into(),
        })?;
        Ok(Self { file })
    }

    #[cfg(not(unix))]
    fn create(_state_dir: &Path) -> Result<Self> {
        Ok(Self {})
    }
}

impl Drop for StartupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = self.file.unlock();
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

#[cfg(unix)]
fn running_process_pid(path: &Path) -> Result<Option<u32>> {
    let Some((record, file)) = open_process_record(path)? else {
        return Ok(None);
    };
    Ok(process_is_running(&file)?.then_some(record.pid))
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
    } else if command == "connect" {
        parse_connect(arguments.collect()).map(Command::Connect)
    } else if command == "serve" {
        parse_serve(arguments.collect())
    } else if command == "__serve" {
        parse_state_dir(arguments.collect()).map(|state_dir| Command::ServeChild { state_dir })
    } else if command == "status" {
        parse_state_dir(arguments.collect()).map(|state_dir| Command::Status { state_dir })
    } else if command == "exit" {
        parse_state_dir(arguments.collect()).map(|state_dir| Command::Exit { state_dir })
    } else {
        Err(Error::Config(USAGE.into()))
    }
}

fn parse_connect(arguments: Vec<OsString>) -> Result<ConnectOptions> {
    let mut configured_state_dir = None;
    let mut endpoint = None;
    let mut arguments = arguments.into_iter();
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| Error::Config(format!("{} requires a value", flag.to_string_lossy())))?;
        if flag == "--state-dir" {
            set_once(
                &mut configured_state_dir,
                PathBuf::from(value),
                "--state-dir",
            )?;
        } else if flag == "--endpoint" {
            let value = value
                .to_str()
                .ok_or_else(|| Error::Config("--endpoint is not valid UTF-8".into()))?
                .parse()?;
            set_once(&mut endpoint, value, "--endpoint")?;
        } else {
            return Err(Error::Config(USAGE.into()));
        }
    }
    Ok(ConnectOptions {
        state_dir: configured_state_dir.map_or_else(state_dir, Ok)?,
        endpoint,
    })
}

fn parse_serve(arguments: Vec<OsString>) -> Result<Command> {
    let (configured_state_dir, background) = match arguments.as_slice() {
        [] => (None, false),
        [flag] if flag == "--background" => (None, true),
        [flag, path] if flag == "--state-dir" => (Some(path), false),
        [background, state_dir, path]
            if background == "--background" && state_dir == "--state-dir" =>
        {
            (Some(path), true)
        }
        [state_dir, path, background]
            if state_dir == "--state-dir" && background == "--background" =>
        {
            (Some(path), true)
        }
        _ => return Err(Error::Config(USAGE.into())),
    };
    let state_dir = configured_state_dir.map_or_else(state_dir, |path| Ok(PathBuf::from(path)))?;
    Ok(Command::Serve {
        state_dir,
        background,
    })
}

fn parse_init(arguments: Vec<OsString>) -> Result<InitOptions> {
    let mut configured_state_dir = None;
    let mut listen = None;
    let mut certificate = None;
    let mut private_key = None;
    let mut arguments = arguments.into_iter();
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| Error::Config(format!("{} requires a value", flag.to_string_lossy())))?;
        if flag == "--state-dir" {
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
            Command::Serve {
                state_dir,
                background: false,
            } if state_dir == std::path::Path::new("/tmp/horus")
        ));
    }

    #[test]
    fn parse_connect_accepts_a_public_endpoint_and_state_directory() {
        let command = parse(vec![
            "connect".into(),
            "--endpoint".into(),
            "tls://gateway.example:443".into(),
            "--state-dir".into(),
            "/tmp/horus".into(),
        ])
        .expect("parse connect");

        assert!(matches!(
            command,
            Command::Connect(ConnectOptions { state_dir, endpoint: Some(endpoint) })
                if state_dir == std::path::Path::new("/tmp/horus")
                    && endpoint.to_string() == "tls://gateway.example:443"
        ));
    }

    #[test]
    fn connection_endpoint_requires_an_explicit_tls_hostname() {
        let certificate = tempfile::NamedTempFile::new().expect("certificate");
        let private_key = tempfile::NamedTempFile::new().expect("private key");
        let config = GatewayConfig::new(
            "0.0.0.0:8741".parse().expect("listen"),
            Some(TlsConfig {
                certificate: certificate.path().to_path_buf(),
                private_key: private_key.path().to_path_buf(),
            }),
        )
        .expect("TLS config");

        let error = connection_endpoint(&config, None).expect_err("endpoint must be explicit");

        assert!(error.to_string().contains("--endpoint tls://HOST:PORT"));
    }

    #[test]
    fn parse_serve_accepts_background_with_an_explicit_state_directory() {
        let command = parse(vec![
            "serve".into(),
            "--background".into(),
            "--state-dir".into(),
            "/tmp/horus".into(),
        ])
        .expect("parse background serve");

        assert!(matches!(
            command,
            Command::Serve {
                state_dir,
                background: true,
            } if state_dir == std::path::Path::new("/tmp/horus")
        ));
    }

    #[test]
    fn parse_serve_rejects_duplicate_background_flags() {
        let error = parse(vec![
            "serve".into(),
            "--background".into(),
            "--background".into(),
        ])
        .expect_err("duplicate background flag must fail");

        assert!(error.to_string().contains("usage:"));
    }

    #[test]
    fn parse_bootstrap_uses_machine_state_without_a_workspace() {
        let command = parse(vec![
            "bootstrap".into(),
            "--state-dir".into(),
            "/tmp/horus".into(),
        ])
        .expect("parse bootstrap");

        assert!(matches!(
            command,
            Command::Bootstrap(InitOptions { state_dir, listen, tls })
                if state_dir == std::path::Path::new("/tmp/horus")
                    && listen == DEFAULT_LISTEN
                    && tls.is_none()
        ));
    }

    #[test]
    fn parse_init_uses_machine_state_without_a_workspace() {
        let command = parse(vec![
            "init".into(),
            "--state-dir".into(),
            "/tmp/horus".into(),
            "--listen".into(),
            "127.0.0.1:9000".into(),
        ])
        .expect("parse init");

        assert!(matches!(
            command,
            Command::Init(InitOptions { state_dir, listen, tls })
                if state_dir == std::path::Path::new("/tmp/horus")
                    && listen == "127.0.0.1:9000".parse().expect("listen")
                    && tls.is_none()
        ));
    }

    #[test]
    fn init_and_bootstrap_reject_the_removed_workspace_flag() {
        for command in ["init", "bootstrap"] {
            let error = parse(vec![
                command.into(),
                "--workspace".into(),
                "/tmp/workspace".into(),
            ])
            .expect_err("workspace flag must be rejected");

            assert!(error.to_string().contains("usage:"));
        }
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
    fn startup_cleanup_removes_only_an_unlocked_process_record() {
        let directory = tempfile::tempdir().expect("process record directory");
        let path = directory.path().join(PROCESS_FILE);
        std::fs::write(&path, b"{").expect("partial process record");

        remove_unlocked_process_record(&path);

        assert!(!path.exists());
        let guard = ProcessRecordGuard::create(directory.path()).expect("locked process record");
        remove_unlocked_process_record(&path);
        assert!(path.exists());
        drop(guard);
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
        assert_eq!(
            running_process_pid(&guard.path).expect("running process ID"),
            Some(std::process::id())
        );
        drop(guard);
        assert!(!directory.path().join(PROCESS_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn startup_lock_allows_only_one_lifecycle_operation() {
        let directory = tempfile::tempdir().expect("startup directory");
        let guard = StartupGuard::create(directory.path()).expect("startup lock");

        let error = StartupGuard::create(directory.path()).expect_err("competing startup");

        assert!(error.to_string().contains("already in progress"));
        drop(guard);
        StartupGuard::create(directory.path()).expect("released startup lock");
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
