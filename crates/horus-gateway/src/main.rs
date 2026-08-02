use std::ffi::OsString;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;

use horus_gateway::auth::AuthStore;
use horus_gateway::config::{ConfigStore, DEFAULT_LISTEN, TlsConfig, state_dir};
use horus_gateway::server::GatewayServer;
use horus_gateway::wire::BootstrapPayload;
use horus_gateway::{Error, Result};

const USAGE: &str = "usage: horus-gateway init [--workspace PATH] [--state-dir PATH] \
                     [--listen ADDR] [--tls-cert PATH --tls-key PATH]\n       \
                     horus-gateway bootstrap [--workspace PATH] [--state-dir PATH] \
                     [--listen ADDR]\n       \
                     horus-gateway serve [--state-dir PATH]\n       \
                     horus-gateway pair-code [--state-dir PATH]";

#[derive(Debug)]
enum Command {
    Init(InitOptions),
    Bootstrap(InitOptions),
    Serve { state_dir: PathBuf },
    PairCode { state_dir: PathBuf },
}

#[derive(Debug)]
struct InitOptions {
    workspace: PathBuf,
    state_dir: PathBuf,
    listen: SocketAddr,
    tls: Option<TlsConfig>,
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
    }
}

async fn bootstrap(options: InitOptions) -> Result<()> {
    if options.tls.is_some() || !options.listen.ip().is_loopback() {
        return Err(Error::Config(
            "automatic bootstrap supports only the local plaintext gateway".into(),
        ));
    }
    let (server, grant) =
        GatewayServer::bootstrap(options.state_dir, options.workspace, options.listen).await?;
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
    GatewayServer::open(state_dir).await?.serve().await
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
        parse_serve(arguments.collect())
    } else if command == "pair-code" {
        parse_pair_code(arguments.collect())
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

fn parse_serve(arguments: Vec<OsString>) -> Result<Command> {
    let state_dir = match arguments.as_slice() {
        [] => state_dir()?,
        [flag, path] if flag == "--state-dir" => PathBuf::from(path),
        _ => return Err(Error::Config(USAGE.into())),
    };
    Ok(Command::Serve { state_dir })
}

fn parse_pair_code(arguments: Vec<OsString>) -> Result<Command> {
    let state_dir = match arguments.as_slice() {
        [] => state_dir()?,
        [flag, path] if flag == "--state-dir" => PathBuf::from(path),
        _ => return Err(Error::Config(USAGE.into())),
    };
    Ok(Command::PairCode { state_dir })
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
