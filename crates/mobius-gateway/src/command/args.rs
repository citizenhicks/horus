use super::*;

#[derive(Debug)]
pub(super) enum Command {
    Init(InitOptions),
    HostedInit {
        state_dir: PathBuf,
    },
    HostedPair {
        state_dir: PathBuf,
    },
    Connect(ConnectOptions),
    Serve {
        state_dir: PathBuf,
        background: bool,
    },
    ServeChild {
        state_dir: PathBuf,
    },
    Exit {
        state_dir: PathBuf,
    },
}

#[derive(Debug)]
pub(super) struct InitOptions {
    pub(super) state_dir: PathBuf,
    pub(super) listen: SocketAddr,
    pub(super) tls: Option<TlsConfig>,
    pub(super) cloudflare: Option<CloudflareInit>,
}

pub(super) enum CloudflareInit {
    Quick,
    Named { hostname: String, token: String },
}

impl std::fmt::Debug for CloudflareInit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quick => formatter.write_str("CloudflareInit::Quick"),
            Self::Named { hostname, .. } => formatter
                .debug_struct("CloudflareInit::Named")
                .field("hostname", hostname)
                .field("token", &"[redacted]")
                .finish(),
        }
    }
}

#[derive(Debug)]
pub(super) struct ConnectOptions {
    pub(super) state_dir: PathBuf,
    pub(super) endpoint: Option<Endpoint>,
}

pub(super) fn parse(arguments: Vec<OsString>) -> Result<Command> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Err(Error::Config(USAGE.into()));
    };
    if command == "init" {
        parse_init(arguments.collect()).map(Command::Init)
    } else if command == "hosted-init" {
        parse_state_dir(arguments.collect()).map(|state_dir| Command::HostedInit { state_dir })
    } else if command == "hosted-pair" {
        parse_hosted_pair(arguments.collect()).map(|state_dir| Command::HostedPair { state_dir })
    } else if command == "connect" {
        parse_connect(arguments.collect()).map(Command::Connect)
    } else if command == "serve" {
        parse_serve(arguments.collect())
    } else if command == "__serve" {
        parse_state_dir(arguments.collect()).map(|state_dir| Command::ServeChild { state_dir })
    } else if command == "exit" {
        parse_state_dir(arguments.collect()).map(|state_dir| Command::Exit { state_dir })
    } else {
        Err(Error::Config(USAGE.into()))
    }
}

pub(super) fn parse_hosted_pair(arguments: Vec<OsString>) -> Result<PathBuf> {
    match arguments.as_slice() {
        [json] if json == "--json" => state_dir(),
        [state_dir, path, json] if state_dir == "--state-dir" && json == "--json" => {
            Ok(PathBuf::from(path))
        }
        _ => Err(Error::Config(USAGE.into())),
    }
}

pub(super) fn parse_connect(arguments: Vec<OsString>) -> Result<ConnectOptions> {
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

pub(super) fn parse_serve(arguments: Vec<OsString>) -> Result<Command> {
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

pub(super) fn parse_init(arguments: Vec<OsString>) -> Result<InitOptions> {
    let mut configured_state_dir = None;
    let mut listen = None;
    let mut certificate = None;
    let mut private_key = None;
    let mut cloudflare_hostname = None;
    let mut cloudflare_token_file = None;
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
        } else if flag == "--cloudflare-hostname" {
            let value = value
                .into_string()
                .map_err(|_| Error::Config("--cloudflare-hostname is not valid UTF-8".into()))?;
            set_once(&mut cloudflare_hostname, value, "--cloudflare-hostname")?;
        } else if flag == "--cloudflare-token-file" {
            set_once(
                &mut cloudflare_token_file,
                PathBuf::from(value),
                "--cloudflare-token-file",
            )?;
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
    let cloudflare = match (cloudflare_hostname, cloudflare_token_file) {
        (Some(hostname), Some(path)) => {
            if tls.is_some() {
                return Err(Error::Config(
                    "Cloudflare and direct TLS listener options cannot be combined".into(),
                ));
            }
            Some(CloudflareInit::Named {
                hostname,
                token: load_cloudflare_token(&path)?,
            })
        }
        (None, None) => None,
        _ => {
            return Err(Error::Config(
                "--cloudflare-hostname and --cloudflare-token-file must be supplied together"
                    .into(),
            ));
        }
    };
    Ok(InitOptions {
        state_dir,
        listen,
        tls,
        cloudflare,
    })
}

pub(super) fn parse_state_dir(arguments: Vec<OsString>) -> Result<PathBuf> {
    let state_dir = match arguments.as_slice() {
        [] => state_dir()?,
        [flag, path] if flag == "--state-dir" => PathBuf::from(path),
        _ => return Err(Error::Config(USAGE.into())),
    };
    Ok(state_dir)
}

pub(super) fn set_once<T>(target: &mut Option<T>, value: T, flag: &str) -> Result<()> {
    if target.replace(value).is_some() {
        return Err(Error::Config(format!("{flag} was supplied more than once")));
    }
    Ok(())
}
