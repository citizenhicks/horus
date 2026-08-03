mod frontend;
mod gateway_accounts;

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{IsTerminal as _, Read as _};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use frontend::FrontendExit;
use gateway_accounts::{GatewayAccounts, configured_endpoint, configured_token};
use horus::{Error, Result};
use horus_gateway::client::{
    Endpoint, GatewayClient, GatewayEvents, GatewaySender, MAX_PENDING_FRAMES,
};
use horus_gateway::config::{ConfigStore, state_dir};
use horus_gateway::wire::{
    BootstrapPayload, ClientMessage, ReadyPayload, ServerFrame, ServerMessage, SessionReadyPayload,
};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, BufReader};
use tokio::process::{Child, Command};
use uuid::Uuid;

const USAGE: &str = "usage: horus [run <task-file> | pair <endpoint> <code>]";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_LOCAL_ENDPOINT: &str = "tcp://127.0.0.1:8741";
const STARTUP_RETRY: Duration = Duration::from_millis(50);
const MAX_BOOTSTRAP_BYTES: usize = 4096;
const MAX_STARTUP_ERROR_BYTES: u64 = 8192;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    match args.next() {
        None => run_interactive().await?,
        Some(command) if command == OsStr::new("run") => {
            let task = one_argument(args, USAGE)?;
            run_task(Path::new(&task)).await?;
        }
        Some(command) if command == OsStr::new("pair") => {
            let endpoint = args.next().ok_or_else(|| Error::Config(USAGE.into()))?;
            let code = args.next().ok_or_else(|| Error::Config(USAGE.into()))?;
            if args.next().is_some() {
                return Err(Error::Config(USAGE.into()).into());
            }
            pair(text(&endpoint, "endpoint")?, text(&code, "pairing code")?).await?;
        }
        Some(command) if command == OsStr::new("--help") || command == OsStr::new("-h") => {
            println!("{USAGE}");
        }
        Some(command) if command == OsStr::new("--version") || command == OsStr::new("-V") => {
            println!("horus {}", env!("CARGO_PKG_VERSION"));
        }
        Some(_) => return Err(Error::Config(USAGE.into()).into()),
    }
    Ok(())
}

async fn run_interactive() -> Result<()> {
    let (
        mut sender,
        mut events,
        mut gateway,
        mut session,
        mut disposable_session,
        mut local_gateway,
        mut endpoint,
    ) = connect(None).await?;
    loop {
        let (exit, next_sender, next_events) = frontend::run(
            sender,
            events,
            &mut gateway,
            &mut session,
            local_gateway,
            endpoint.to_string(),
        )
        .await?;
        sender = next_sender;
        events = next_events;
        match exit {
            FrontendExit::Exit => return Ok(()),
            FrontendExit::Discard => {
                if let Some(session_id) = disposable_session.as_deref() {
                    discard_session(&sender, &mut events, &mut gateway, session_id).await?;
                }
                return Ok(());
            }
            FrontendExit::New => {
                session = create_session(
                    &sender,
                    &mut events,
                    &mut gateway,
                    session.workspace.path.clone(),
                )
                .await?;
                disposable_session = None;
            }
            FrontendExit::Resume(session_id) => {
                session = open_session(&sender, &mut events, &mut gateway, session_id).await?;
                disposable_session = None;
            }
            FrontendExit::Reload => {}
            FrontendExit::Reconnect => {
                let selected = Some((endpoint.clone(), session.session.session_id.clone()));
                (
                    sender,
                    events,
                    gateway,
                    session,
                    disposable_session,
                    local_gateway,
                    endpoint,
                ) = connect(selected).await?;
            }
        }
    }
}

async fn run_task(task_file: &Path) -> Result<()> {
    let task = std::fs::read_to_string(task_file)?;
    let (sender, mut events, mut gateway, session, disposable_session, _, _) =
        connect(None).await?;
    if gateway.default_config.is_none() || gateway.models.is_empty() {
        if let Some(session_id) = disposable_session.as_deref() {
            discard_session(&sender, &mut events, &mut gateway, session_id).await?;
        }
        return Err(Error::Config(
            "run `horus` interactively to configure a provider before using `horus run`".into(),
        ));
    }
    if let Some(message) =
        frontend::run_headless(sender, events, session.session.session_id, task).await?
    {
        print_output(&message);
    }
    Ok(())
}

fn print_output(value: &str) {
    println!("{}", output_text(value, std::io::stdout().is_terminal()));
}

fn output_text(value: &str, terminal: bool) -> String {
    if terminal {
        frontend::terminal_text(value)
    } else {
        value.into()
    }
}

async fn pair(endpoint: &str, code: &str) -> std::result::Result<(), horus_gateway::Error> {
    let endpoint = endpoint.parse::<Endpoint>()?;
    let mut accounts = GatewayAccounts::load()?;
    accounts.prepare()?;
    let (_client, paired) = GatewayClient::pair(&endpoint, code, "horus-cli").await?;
    accounts.add(&endpoint, paired.token)?;
    accounts.save()?;
    println!("paired {} · token saved", paired.client_id);
    Ok(())
}

async fn connect(
    selected: Option<(Endpoint, String)>,
) -> Result<(
    GatewaySender,
    GatewayEvents,
    ReadyPayload,
    SessionReadyPayload,
    Option<String>,
    bool,
    Endpoint,
)> {
    let endpoint = configured_endpoint().map_err(gateway_error)?;
    // ponytail: TLS gateways skip local `@` scanning; use a gateway-backed inventory if needed.
    let local_gateway = endpoint.is_plaintext();
    let token = configured_token(&endpoint).map_err(gateway_error)?;
    let connected = if automatically_manage_local_gateway(&endpoint) {
        connect_local(&endpoint, token).await
    } else {
        match token {
            Some(token) => GatewayClient::connect(&endpoint, token).await,
            None => Err(missing_token(&endpoint)),
        }
    };
    let client = connected.map_err(gateway_error)?;
    let (sender, mut events) = client.into_parts();
    let mut gateway = wait_gateway_ready(&mut events).await?;
    let (session, disposable_session) = match selected.filter(|(previous, _)| previous == &endpoint)
    {
        Some((_, session_id)) => {
            let session = open_session(&sender, &mut events, &mut gateway, session_id).await?;
            (session, None)
        }
        None if local_gateway => {
            let session =
                create_session(&sender, &mut events, &mut gateway, env::current_dir()?).await?;
            let disposable_session = (gateway.default_config.is_none()
                || gateway.models.is_empty())
            .then(|| session.session.session_id.clone());
            (session, disposable_session)
        }
        None => {
            let session_id = gateway
                .sessions
                .first()
                .map(|session| session.summary.session_id.clone())
                .ok_or_else(|| {
                    Error::Stopped(
                        "the remote gateway has no chats; create a workspace chat from a local frontend first"
                            .into(),
                    )
                })?;
            let session = open_session(&sender, &mut events, &mut gateway, session_id).await?;
            (session, None)
        }
    };
    Ok((
        sender,
        events,
        gateway,
        session,
        disposable_session,
        local_gateway,
        endpoint,
    ))
}

fn automatically_manage_local_gateway(endpoint: &Endpoint) -> bool {
    endpoint.is_plaintext()
        && env::var_os("HORUS_GATEWAY_ENDPOINT").is_none()
        && env::var_os("HORUS_GATEWAY_TOKEN").is_none()
}

async fn connect_local(
    endpoint: &Endpoint,
    token: Option<String>,
) -> horus_gateway::Result<GatewayClient> {
    if let Some(token) = token {
        match connect_local_once(endpoint, &token).await {
            Ok(client) => return Ok(client),
            Err(horus_gateway::Error::Io(error))
                if error.kind() == std::io::ErrorKind::ConnectionRefused =>
            {
                return start_local_gateway(endpoint).await;
            }
            Err(horus_gateway::Error::Unauthorized) => {
                return Err(missing_local_token(endpoint));
            }
            Err(error) => return Err(error),
        }
    }
    start_local_gateway(endpoint).await
}

async fn connect_local_once(
    endpoint: &Endpoint,
    token: &str,
) -> horus_gateway::Result<GatewayClient> {
    tokio::time::timeout(CONNECT_TIMEOUT, GatewayClient::connect(endpoint, token))
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "gateway connection timed out")
        })?
}

async fn start_local_gateway(endpoint: &Endpoint) -> horus_gateway::Result<GatewayClient> {
    let configured_state_dir = state_dir()?;
    let _startup_lock = lock_local_gateway_startup(&configured_state_dir)?;
    let saved_token = configured_token(endpoint)?;
    if let Some(token) = saved_token.as_deref() {
        match connect_local_once(endpoint, token).await {
            Ok(client) => return Ok(client),
            Err(horus_gateway::Error::Io(error))
                if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(horus_gateway::Error::Unauthorized) => {
                return Err(missing_local_token(endpoint));
            }
            Err(error) => return Err(error),
        }
    }
    let binary = gateway_binary()?;
    if configured_state_dir.try_exists()? {
        let (_, config) = ConfigStore::open(configured_state_dir.clone())?;
        let configured_endpoint = format!(
            "{}://{}",
            if config.tls.is_some() { "tls" } else { "tcp" },
            config.listen
        );
        if endpoint.to_string() != configured_endpoint {
            return Err(horus_gateway::Error::Config(format!(
                "saved endpoint {endpoint} is not the local gateway configured at {configured_endpoint}; start it separately or select the configured endpoint"
            )));
        }
        let token = saved_token.ok_or_else(|| missing_local_token(endpoint))?;
        let (child, log) = spawn_gateway(&binary, &configured_state_dir, false)?;
        return connect_started_gateway(endpoint, &token, child, log).await;
    }
    if endpoint.to_string() != DEFAULT_LOCAL_ENDPOINT {
        return Err(horus_gateway::Error::Config(format!(
            "saved local gateway {endpoint} is stopped; start it separately before reconnecting"
        )));
    }
    bootstrap_local_gateway(endpoint, &binary, &configured_state_dir).await
}

fn lock_local_gateway_startup(state_dir: &Path) -> horus_gateway::Result<File> {
    let mut name = state_dir
        .file_name()
        .ok_or_else(|| {
            horus_gateway::Error::Config("gateway state directory has no file name".into())
        })?
        .to_os_string();
    name.push(".startup.lock");
    let path = state_dir.with_file_name(name);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.lock()?;
    Ok(file)
}

async fn bootstrap_local_gateway(
    endpoint: &Endpoint,
    binary: &Path,
    state_dir: &Path,
) -> horus_gateway::Result<GatewayClient> {
    let mut accounts = GatewayAccounts::load()?;
    accounts.prepare()?;
    let (mut child, log) = spawn_gateway(binary, state_dir, true)?;
    let pairing_code = read_bootstrap(&mut child, &log).await?;
    let paired = match tokio::time::timeout(
        STARTUP_TIMEOUT,
        GatewayClient::pair(endpoint, pairing_code, "horus-cli"),
    )
    .await
    {
        Ok(paired) => paired,
        Err(_) => {
            stop_child(&mut child).await;
            return Err(startup_error("gateway pairing timed out", &log));
        }
    };
    let (client, paired) = match paired {
        Ok(paired) => paired,
        Err(error) => {
            stop_child(&mut child).await;
            return Err(error);
        }
    };
    if let Err(error) = accounts
        .add(endpoint, paired.token)
        .and_then(|()| accounts.save())
    {
        stop_child(&mut child).await;
        return Err(error);
    }
    detach_child(child);
    Ok(client)
}

fn gateway_binary() -> horus_gateway::Result<PathBuf> {
    gateway_binary_beside(&env::current_exe()?)
}

fn gateway_binary_beside(current_executable: &Path) -> horus_gateway::Result<PathBuf> {
    let name = if cfg!(windows) {
        "horus-gateway.exe"
    } else {
        "horus-gateway"
    };
    let candidate = current_executable.with_file_name(name);
    let metadata = std::fs::metadata(&candidate).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            horus_gateway::Error::Config(
                "install horus-cli to provide horus-gateway beside horus (`cargo install --locked horus-cli`)"
                    .into(),
            )
        } else {
            error.into()
        }
    })?;
    if !metadata.is_file() {
        return Err(horus_gateway::Error::Config(
            "the horus-gateway path is not a file".into(),
        ));
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(horus_gateway::Error::Config(
            "the horus-gateway binary is not executable".into(),
        ));
    }
    Ok(std::fs::canonicalize(candidate)?)
}

fn spawn_gateway(
    binary: &Path,
    state_dir: &Path,
    bootstrap: bool,
) -> horus_gateway::Result<(Child, tempfile::NamedTempFile)> {
    let log = tempfile::NamedTempFile::new()?;
    #[cfg(unix)]
    log.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let mut command = Command::new(binary);
    if bootstrap {
        command.arg("bootstrap").stdout(Stdio::piped());
    } else {
        command.arg("serve").stdout(Stdio::null());
    }
    command
        .arg("--state-dir")
        .arg(state_dir)
        .stdin(Stdio::null())
        .stderr(Stdio::from(log.reopen()?));
    #[cfg(unix)]
    command.as_std_mut().process_group(0);
    Ok((command.spawn()?, log))
}

async fn read_bootstrap(
    child: &mut Child,
    log: &tempfile::NamedTempFile,
) -> horus_gateway::Result<String> {
    let stdout = child.stdout.take().ok_or_else(|| {
        horus_gateway::Error::Config("horus-gateway bootstrap output is unavailable".into())
    })?;
    let mut reader = BufReader::new(stdout).take((MAX_BOOTSTRAP_BYTES + 1) as u64);
    let mut output = Vec::new();
    let read =
        match tokio::time::timeout(STARTUP_TIMEOUT, reader.read_until(b'\n', &mut output)).await {
            Ok(Ok(read)) => read,
            Ok(Err(error)) => {
                stop_child(child).await;
                return Err(error.into());
            }
            Err(_) => {
                stop_child(child).await;
                return Err(startup_error("horus-gateway bootstrap timed out", log));
            }
        };
    if read == 0 {
        let status = child.wait().await?;
        return Err(startup_error(
            format!("horus-gateway exited during bootstrap with {status}"),
            log,
        ));
    }
    if output.len() > MAX_BOOTSTRAP_BYTES || !output.ends_with(b"\n") {
        stop_child(child).await;
        return Err(horus_gateway::Error::Protocol(
            "horus-gateway returned invalid bootstrap output".into(),
        ));
    }
    let payload = match serde_json::from_slice::<BootstrapPayload>(&output) {
        Ok(payload) => payload,
        Err(error) => {
            stop_child(child).await;
            return Err(error.into());
        }
    };
    if payload.pairing_code.trim() != payload.pairing_code
        || payload.pairing_code.is_empty()
        || payload.pairing_code.len() > 512
    {
        stop_child(child).await;
        return Err(horus_gateway::Error::Protocol(
            "horus-gateway returned an invalid pairing code".into(),
        ));
    }
    Ok(payload.pairing_code)
}

async fn connect_started_gateway(
    endpoint: &Endpoint,
    token: &str,
    mut child: Child,
    log: tempfile::NamedTempFile,
) -> horus_gateway::Result<GatewayClient> {
    let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
    let mut child_exit = None;
    loop {
        if child_exit.is_none() {
            child_exit = child.try_wait()?;
        }
        match connect_local_once(endpoint, token).await {
            Ok(client) => {
                if child_exit.is_none() {
                    detach_child(child);
                }
                return Ok(client);
            }
            Err(error) if startup_connection_pending(&error) => {}
            Err(horus_gateway::Error::Unauthorized) => {
                if child_exit.is_none() {
                    stop_child(&mut child).await;
                }
                return Err(missing_local_token(endpoint));
            }
            Err(error) => {
                if child_exit.is_none() {
                    stop_child(&mut child).await;
                }
                return Err(error);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let message = if let Some(status) = child_exit {
                format!("horus-gateway exited during startup with {status}")
            } else {
                stop_child(&mut child).await;
                "horus-gateway did not start within 10 seconds".into()
            };
            return Err(startup_error(message, &log));
        }
        tokio::time::sleep(STARTUP_RETRY).await;
    }
}

fn startup_connection_pending(error: &horus_gateway::Error) -> bool {
    matches!(
        error,
        horus_gateway::Error::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut
            )
    )
}

fn detach_child(mut child: Child) {
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
}

async fn stop_child(child: &mut Child) {
    let _ = child.kill().await;
}

fn startup_error(
    message: impl std::fmt::Display,
    log: &tempfile::NamedTempFile,
) -> horus_gateway::Error {
    let mut details = String::new();
    if let Ok(file) = std::fs::File::open(log.path()) {
        let _ = file
            .take(MAX_STARTUP_ERROR_BYTES)
            .read_to_string(&mut details);
    }
    let details = details.trim();
    horus_gateway::Error::Config(if details.is_empty() {
        message.to_string()
    } else {
        format!("{message}: {details}")
    })
}

fn missing_local_token(endpoint: &Endpoint) -> horus_gateway::Error {
    horus_gateway::Error::Config(format!(
        "local gateway state exists but horus-cli is not paired; stop the gateway, run `horus-gateway pair-code`, restart `horus-gateway serve`, then run `horus pair {endpoint} <code>`"
    ))
}

async fn open_session(
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    gateway: &mut ReadyPayload,
    session_id: String,
) -> Result<SessionReadyPayload> {
    let request_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::OpenSession {
            request_id: request_id.clone(),
            session_id,
            last_sequence: None,
        })
        .await
        .map_err(gateway_error)?;
    wait_session_opened(events, gateway, &request_id).await
}

async fn create_session(
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    gateway: &mut ReadyPayload,
    workspace: PathBuf,
) -> Result<SessionReadyPayload> {
    let request_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::CreateSession {
            request_id: request_id.clone(),
            workspace,
        })
        .await
        .map_err(gateway_error)?;
    wait_session_opened(events, gateway, &request_id).await
}

async fn discard_session(
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    gateway: &mut ReadyPayload,
    session_id: &str,
) -> Result<()> {
    let request_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::DeleteSession {
            request_id: request_id.clone(),
            session_id: session_id.into(),
        })
        .await
        .map_err(gateway_error)?;
    let mut deferred = Vec::new();
    let result = loop {
        let frame = events.next().await.map_err(gateway_error)?.ok_or_else(|| {
            Error::Stopped("gateway disconnected before discarding the setup chat".into())
        })?;
        match frame.message {
            ServerMessage::Accepted { request_id: actual } if actual == request_id => break Ok(()),
            ServerMessage::Ready { payload } => *gateway = payload,
            ServerMessage::Sessions { sessions, .. } => gateway.sessions = sessions,
            ServerMessage::Rejected {
                request_id: actual,
                message,
                ..
            } if actual == request_id => break Err(Error::Stopped(message)),
            ServerMessage::Error { message, .. } => break Err(Error::Stopped(message)),
            message if deferred.len() == MAX_PENDING_FRAMES => {
                break Err(Error::Stopped(format!(
                    "gateway event backlog exceeds {MAX_PENDING_FRAMES} frames while discarding the setup chat: {message:?}"
                )));
            }
            message => deferred.push(ServerFrame::new(message)),
        }
    };
    events.prepend(deferred).map_err(gateway_error)?;
    result
}

async fn wait_gateway_ready(events: &mut GatewayEvents) -> Result<ReadyPayload> {
    loop {
        let frame =
            events.next().await.map_err(gateway_error)?.ok_or_else(|| {
                Error::Stopped("gateway disconnected before becoming ready".into())
            })?;
        match frame.message {
            ServerMessage::Ready { payload } => return Ok(payload),
            ServerMessage::Rejected { message, .. } | ServerMessage::Error { message, .. } => {
                return Err(Error::Stopped(message));
            }
            _ => {}
        }
    }
}

async fn wait_session_opened(
    events: &mut GatewayEvents,
    gateway: &mut ReadyPayload,
    request_id: &str,
) -> Result<SessionReadyPayload> {
    let mut deferred = Vec::new();
    let result = loop {
        let frame =
            events.next().await.map_err(gateway_error)?.ok_or_else(|| {
                Error::Stopped("gateway disconnected before opening the chat".into())
            })?;
        match frame.message {
            ServerMessage::SessionOpened {
                request_id: actual,
                payload,
            } if actual == request_id => break Ok(payload),
            ServerMessage::Ready { payload } => *gateway = payload,
            ServerMessage::Sessions { sessions, .. } => gateway.sessions = sessions,
            ServerMessage::Rejected {
                request_id: actual,
                message,
                ..
            } if actual == request_id => break Err(Error::Stopped(message)),
            ServerMessage::Error { message, .. } => break Err(Error::Stopped(message)),
            message if deferred.len() == MAX_PENDING_FRAMES => {
                break Err(Error::Stopped(format!(
                    "gateway event backlog exceeds {MAX_PENDING_FRAMES} frames while opening a chat: {message:?}"
                )));
            }
            message => deferred.push(ServerFrame::new(message)),
        }
    };
    events.prepend(deferred).map_err(gateway_error)?;
    result
}

fn one_argument(mut args: impl Iterator<Item = OsString>, usage: &str) -> Result<OsString> {
    args.next()
        .filter(|_| args.next().is_none())
        .ok_or_else(|| Error::Config(usage.into()))
}

fn text<'a>(value: &'a OsStr, name: &str) -> Result<&'a str> {
    value
        .to_str()
        .ok_or_else(|| Error::Config(format!("{name} is not valid UTF-8")))
}

fn missing_token(endpoint: &Endpoint) -> horus_gateway::Error {
    horus_gateway::Error::Config(format!("pair horus-cli with {endpoint} before connecting"))
}

fn gateway_error(error: horus_gateway::Error) -> Error {
    Error::Stopped(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn startup_errors_include_bounded_gateway_diagnostics() {
        let mut log = tempfile::NamedTempFile::new().expect("startup log");
        write!(log, "Bubblewrap is unavailable").expect("write startup log");

        let error = startup_error("gateway exited", &log);

        assert!(error.to_string().contains("Bubblewrap is unavailable"));
    }

    #[test]
    fn local_gateway_startup_lock_allows_one_of_three_contenders() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let state_dir = directory.path().join("gateway");
        let lock_path = directory.path().join("gateway.startup.lock");
        let first = lock_local_gateway_startup(&state_dir).expect("first startup lock");
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("second contender");
        let third = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("third contender");

        assert!(
            matches!(second.try_lock(), Err(std::fs::TryLockError::WouldBlock))
                && matches!(third.try_lock(), Err(std::fs::TryLockError::WouldBlock))
        );

        drop(first);
        second.lock().expect("released startup lock");
        drop(second);
        drop(third);
        assert!(lock_path.exists(), "startup lock must remain persistent");
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(lock_path)
                .expect("startup lock metadata")
                .permissions()
                .mode()
                & 0o077,
            0,
            "startup lock must be owner-only"
        );
    }

    #[test]
    fn gateway_autostart_ignores_path_and_requires_a_sibling_binary() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let error = gateway_binary_beside(&directory.path().join("horus"))
            .expect_err("missing sibling gateway must fail");

        assert!(
            error
                .to_string()
                .contains("cargo install --locked horus-cli")
        );
    }

    #[test]
    fn stdout_filters_terminal_controls_but_preserves_piped_output() {
        let cron_output = "task: reset\u{1b}[2J.md";

        assert_eq!(output_text(cron_output, true), "task: reset[2J.md");
        assert_eq!(output_text(cron_output, false), cron_output);
    }
}
