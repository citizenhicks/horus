mod frontend;

use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{IsTerminal as _, Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use frontend::{CronAction, FrontendExit, GatewayAction};
use horus::protocol::{Op, Submission};
use horus::{Error, Result};
use horus_gateway::client::{
    Endpoint, GatewayClient, GatewayEvents, GatewaySender, token_from_env,
};
use horus_gateway::config::state_dir;
use horus_gateway::wire::{BootstrapPayload, ClientMessage, ReadyPayload, ServerMessage};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, BufReader};
use tokio::process::{Child, Command};
use uuid::Uuid;

const USAGE: &str = "usage: horus [run <task-file> | pair <endpoint> <code> | cron <command>]";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
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
        Some(command) if command == OsStr::new("cron") => {
            run_cron(args.collect()).await?;
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
    let (mut sender, mut events, mut ready, local_gateway) = connect().await?;
    loop {
        let (exit, next_sender, next_events) =
            frontend::run(sender, events, ready, local_gateway).await?;
        sender = next_sender;
        events = next_events;
        match exit {
            FrontendExit::Exit => return Ok(()),
            FrontendExit::New(model_route) => {
                ready = open_session(&sender, &mut events, None).await?;
                submit(&sender, Op::SetModel { route: model_route }).await?;
            }
            FrontendExit::Resume(session_id) => {
                ready = open_session(&sender, &mut events, Some(session_id)).await?;
            }
            FrontendExit::Reload(payload) => ready = *payload,
        }
    }
}

async fn run_task(task_file: &Path) -> Result<()> {
    let task = std::fs::read_to_string(task_file)?;
    let (sender, mut events, _, _) = connect().await?;
    let _ready = open_session(&sender, &mut events, None).await?;
    if let Some(message) = frontend::run_headless(sender, events, task).await? {
        print_output(&message);
    }
    Ok(())
}

async fn run_cron(args: Vec<OsString>) -> Result<()> {
    let action = cron_action(&args)?;
    let (sender, mut events, ready, _) = connect().await?;
    let output = frontend::execute_gateway_action(&sender, &mut events, &ready, action).await?;
    print_output(&output);
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
    let (_client, paired) = GatewayClient::pair(&endpoint, code, "horus-cli", None).await?;
    save_token(&token_path()?, &endpoint.to_string(), &paired.token)?;
    println!("paired {} · token saved", paired.client_id);
    Ok(())
}

async fn connect() -> Result<(GatewaySender, GatewayEvents, ReadyPayload, bool)> {
    let endpoint = Endpoint::from_env().map_err(gateway_error)?;
    // ponytail: TLS gateways skip local `@` scanning; use a gateway-backed inventory if needed.
    let local_gateway = endpoint.is_plaintext();
    let token = load_token(&endpoint).map_err(gateway_error)?;
    let connected = if automatically_manage_local_gateway() {
        connect_local(&endpoint, token).await
    } else {
        match token {
            Some(token) => GatewayClient::connect(&endpoint, token, None).await,
            None => Err(missing_token(&endpoint)),
        }
    };
    let client = connected.map_err(gateway_error)?;
    let (sender, mut events) = client.into_parts();
    let ready = wait_ready(&mut events).await?;
    Ok((sender, events, ready, local_gateway))
}

fn automatically_manage_local_gateway() -> bool {
    env::var_os("HORUS_GATEWAY_ENDPOINT").is_none() && env::var_os("HORUS_GATEWAY_TOKEN").is_none()
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
                return start_local_gateway(endpoint, Some(token)).await;
            }
            Err(horus_gateway::Error::Unauthorized) => {
                return Err(missing_local_token(endpoint));
            }
            Err(error) => return Err(error),
        }
    }
    start_local_gateway(endpoint, None).await
}

async fn connect_local_once(
    endpoint: &Endpoint,
    token: &str,
) -> horus_gateway::Result<GatewayClient> {
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        GatewayClient::connect(endpoint, token, None),
    )
    .await
    .map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::TimedOut, "gateway connection timed out")
    })?
}

async fn start_local_gateway(
    endpoint: &Endpoint,
    saved_token: Option<String>,
) -> horus_gateway::Result<GatewayClient> {
    let configured_state_dir = state_dir()?;
    let binary = gateway_binary()?;
    if configured_state_dir.try_exists()? {
        let token = saved_token.ok_or_else(|| missing_local_token(endpoint))?;
        let (child, log) = spawn_gateway(&binary, &configured_state_dir, None)?;
        return connect_started_gateway(endpoint, &token, child, log).await;
    }
    bootstrap_local_gateway(endpoint, &binary, &configured_state_dir).await
}

async fn bootstrap_local_gateway(
    endpoint: &Endpoint,
    binary: &Path,
    state_dir: &Path,
) -> horus_gateway::Result<GatewayClient> {
    let token_path = token_path()?;
    prepare_token_path(&token_path)?;
    let workspace = env::current_dir()?;
    let (mut child, log) = spawn_gateway(binary, state_dir, Some(&workspace))?;
    let pairing_code = read_bootstrap(&mut child, &log).await?;
    let paired = match tokio::time::timeout(
        STARTUP_TIMEOUT,
        GatewayClient::pair(endpoint, pairing_code, "horus-cli", None),
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
    if let Err(error) = save_token(&token_path, &endpoint.to_string(), &paired.token) {
        stop_child(&mut child).await;
        return Err(error);
    }
    detach_child(child);
    Ok(client)
}

fn prepare_token_path(path: &Path) -> horus_gateway::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| horus_gateway::Error::Config("token path has no parent".into()))?;
    std::fs::create_dir_all(parent)?;
    let file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(())
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
                "install horus-gateway beside horus (`cargo install horus-gateway`)".into(),
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
    bootstrap_workspace: Option<&Path>,
) -> horus_gateway::Result<(Child, tempfile::NamedTempFile)> {
    let log = tempfile::NamedTempFile::new()?;
    #[cfg(unix)]
    log.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let mut command = Command::new(binary);
    if let Some(workspace) = bootstrap_workspace {
        command
            .arg("bootstrap")
            .arg("--workspace")
            .arg(workspace)
            .stdout(Stdio::piped());
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
    session_id: Option<String>,
) -> Result<ReadyPayload> {
    sender
        .send(ClientMessage::OpenSession {
            request_id: Uuid::new_v4().to_string(),
            session_id,
        })
        .await
        .map_err(gateway_error)?;
    wait_ready(events).await
}

async fn submit(sender: &GatewaySender, op: Op) -> Result<()> {
    sender
        .send(ClientMessage::Submit {
            submission: Submission {
                id: Uuid::new_v4().to_string(),
                op,
            },
        })
        .await
        .map_err(gateway_error)
}

async fn wait_ready(events: &mut GatewayEvents) -> Result<ReadyPayload> {
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

fn cron_action(args: &[OsString]) -> Result<GatewayAction> {
    let action = match args {
        [command] if command == OsStr::new("list") => CronAction::List,
        [command, id] if command == OsStr::new("delete") => {
            CronAction::Delete(text(id, "task ID")?.into())
        }
        [command, id] if command == OsStr::new("run") => {
            CronAction::Run(text(id, "task ID")?.into())
        }
        [command] if command == OsStr::new("history") => CronAction::History(None),
        [command, id] if command == OsStr::new("history") => {
            CronAction::History(Some(text(id, "task ID")?.into()))
        }
        [command, id, flag, schedule]
            if command == OsStr::new("reschedule") && flag == OsStr::new("--schedule") =>
        {
            CronAction::Reschedule {
                id: text(id, "task ID")?.into(),
                schedule: text(schedule, "schedule")?.into(),
            }
        }
        [task_flag, task, schedule_flag, schedule]
            if task_flag == OsStr::new("--task") && schedule_flag == OsStr::new("--schedule") =>
        {
            CronAction::Add {
                task: PathBuf::from(task),
                schedule: text(schedule, "schedule")?.into(),
            }
        }
        _ => return Err(Error::Config(USAGE.into())),
    };
    Ok(GatewayAction::Cron(action))
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

fn load_token(endpoint: &Endpoint) -> horus_gateway::Result<Option<String>> {
    if env::var_os("HORUS_GATEWAY_TOKEN").is_some() {
        return token_from_env().map(Some);
    }
    let path = token_path()?;
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Err(horus_gateway::Error::Config(
            "gateway token path is not a file".into(),
        ));
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(horus_gateway::Error::Config(
            "gateway token file must be readable only by its owner".into(),
        ));
    }
    let contents = std::fs::read(&path)?;
    if contents.len() > 64 * 1024 {
        return Err(horus_gateway::Error::Config(
            "gateway token file is too large".into(),
        ));
    }
    let tokens: BTreeMap<String, String> = serde_json::from_slice(&contents)?;
    let Some(token) = tokens.get(&endpoint.to_string()) else {
        return Ok(None);
    };
    let token = token.trim();
    if token.is_empty() || token.len() > 512 {
        return Err(horus_gateway::Error::Config(
            "saved gateway token is invalid".into(),
        ));
    }
    Ok(Some(token.into()))
}

fn missing_token(endpoint: &Endpoint) -> horus_gateway::Error {
    horus_gateway::Error::Config(format!("pair horus-cli with {endpoint} before connecting"))
}

fn token_path() -> horus_gateway::Result<PathBuf> {
    if let Some(path) = std::env::var_os("HORUS_GATEWAY_TOKEN_FILE") {
        return Ok(path.into());
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|path| path.join(".horus").join("gateway-tokens.json"))
        .ok_or_else(|| {
            horus_gateway::Error::Config(
                "cannot determine token path; set HORUS_GATEWAY_TOKEN_FILE".into(),
            )
        })
}

fn save_token(path: &Path, endpoint: &str, token: &str) -> horus_gateway::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| horus_gateway::Error::Config("token path has no parent".into()))?;
    std::fs::create_dir_all(parent)?;
    let mut tokens: BTreeMap<String, String> = match std::fs::read(path) {
        Ok(contents) if contents.len() <= 64 * 1024 => serde_json::from_slice(&contents)?,
        Ok(_) => {
            return Err(horus_gateway::Error::Config(
                "gateway token file is too large".into(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => return Err(error.into()),
    };
    if !tokens.contains_key(endpoint) && tokens.len() >= 64 {
        return Err(horus_gateway::Error::Config(
            "gateway token file has too many endpoints".into(),
        ));
    }
    tokens.insert(endpoint.into(), token.into());
    let contents = serde_json::to_vec(&tokens)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(&contents)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn gateway_error(error: horus_gateway::Error) -> Error {
    Error::Stopped(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_cli_maps_every_scheduler_operation_to_gateway_actions() {
        for args in [
            vec!["list"],
            vec!["delete", "abc"],
            vec!["run", "abc"],
            vec!["history"],
            vec!["history", "abc"],
            vec!["reschedule", "abc", "--schedule", "0 4 * * *"],
            vec!["--task", "/work/horus/task.md", "--schedule", "0 3 * * *"],
        ] {
            let args = args.into_iter().map(OsString::from).collect::<Vec<_>>();
            assert!(cron_action(&args).is_ok());
        }
    }

    #[test]
    fn pairing_tokens_are_scoped_to_their_exact_endpoints() {
        let directory = tempfile::tempdir().expect("token directory");
        let path = directory.path().join("tokens.json");
        save_token(&path, "tcp://127.0.0.1:8741", "local-token").expect("local token");
        save_token(&path, "tls://gateway.example:443", "remote-token").expect("remote token");
        let tokens: BTreeMap<String, String> =
            serde_json::from_slice(&std::fs::read(&path).expect("read tokens"))
                .expect("parse tokens");

        assert_eq!(
            tokens,
            BTreeMap::from([
                ("tcp://127.0.0.1:8741".into(), "local-token".into()),
                ("tls://gateway.example:443".into(), "remote-token".into()),
            ])
        );
    }

    #[test]
    fn startup_errors_include_bounded_gateway_diagnostics() {
        let mut log = tempfile::NamedTempFile::new().expect("startup log");
        write!(log, "Bubblewrap is unavailable").expect("write startup log");

        let error = startup_error("gateway exited", &log);

        assert!(error.to_string().contains("Bubblewrap is unavailable"));
    }

    #[test]
    fn gateway_autostart_ignores_path_and_requires_a_sibling_binary() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let error = gateway_binary_beside(&directory.path().join("horus"))
            .expect_err("missing sibling gateway must fail");

        assert!(error.to_string().contains("cargo install horus-gateway"));
    }

    #[test]
    fn stdout_filters_terminal_controls_but_preserves_piped_output() {
        let cron_output = "task: reset\u{1b}[2J.md";

        assert_eq!(output_text(cron_output, true), "task: reset[2J.md");
        assert_eq!(output_text(cron_output, false), cron_output);
    }
}
