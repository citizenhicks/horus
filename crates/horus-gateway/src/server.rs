//! Authenticated plaintext-loopback and TLS gateway listeners.

use std::fs;
use std::fs::File;
use std::future::Future;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, broadcast};
use tokio_rustls::TlsAcceptor;

use crate::auth::{AuthStore, PairingGrant};
use crate::config::{ConfigStore, CredentialStore, GatewayConfig, TlsConfig};
use crate::cron::CronStore;
use crate::host::{HostHandle, Rejection};
use crate::wire::{
    ClientFrame, ClientMessage, DirectoryEntry, DirectoryListing, ServerFrame, ServerMessage,
    read_frame, validate_version, write_frame,
};
use crate::{Error, Result};

const AUTH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONNECTIONS: usize = 32;
const SCHEDULER_TICK: Duration = Duration::from_secs(15);
const MAX_DIRECTORY_ENTRIES: usize = 512;

/// Fully assembled gateway listener and its single shared agent host.
pub struct GatewayServer {
    config: GatewayConfig,
    listener: TcpListener,
    auth: Arc<AuthStore>,
    host: HostHandle,
    cron: Arc<CronStore>,
}

impl GatewayServer {
    /// Opens protected state and starts the sole agent event owner.
    pub async fn open(state_dir: PathBuf) -> Result<Self> {
        let (store, config) = ConfigStore::open(state_dir)?;
        let listener = TcpListener::bind(config.listen).await?;
        Self::assemble(store, config, listener).await
    }

    /// Binds and initializes a fresh local gateway before exposing its one-use pairing grant.
    pub async fn bootstrap(
        state_dir: PathBuf,
        workspace: PathBuf,
        listen: std::net::SocketAddr,
    ) -> Result<(Self, PairingGrant)> {
        let listener = TcpListener::bind(listen).await?;
        let (store, config) = ConfigStore::initialize(state_dir, workspace, listen, None)?;
        let initialized_state = store.state_dir().to_path_buf();
        let result = match AuthStore::initialize(store.auth_path()) {
            Ok((_, grant)) => Self::assemble(store, config, listener)
                .await
                .map(|server| (server, grant)),
            Err(error) => Err(error),
        };
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                fs::remove_dir_all(&initialized_state).map_err(|cleanup| {
                    Error::Config(format!(
                        "{error}; failed to remove incomplete gateway state at {}: {cleanup}",
                        initialized_state.display()
                    ))
                })?;
                Err(error)
            }
        }
    }

    async fn assemble(
        store: ConfigStore,
        config: GatewayConfig,
        listener: TcpListener,
    ) -> Result<Self> {
        let auth = Arc::new(AuthStore::open(store.auth_path())?);
        let credentials = Arc::new(CredentialStore::open(store.credentials_path())?);
        let cron = Arc::new(CronStore::open(store.state_dir(), &config.workspace)?);
        let host = HostHandle::start(store, config.clone(), credentials, Arc::clone(&cron)).await?;
        Ok(Self {
            config,
            listener,
            auth,
            host,
            cron,
        })
    }

    /// Serves until the process receives Ctrl-C.
    pub async fn serve(self) -> Result<()> {
        self.serve_until(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
    }

    /// Serves until the supplied shutdown signal resolves.
    pub async fn serve_until(self, shutdown: impl Future<Output = ()>) -> Result<()> {
        self.config.validate()?;
        let tls = self.config.tls.as_ref().map(tls_acceptor).transpose()?;
        if tls.is_none() && !self.listener.local_addr()?.ip().is_loopback() {
            return Err(Error::Config(
                "plaintext listeners are restricted to loopback".into(),
            ));
        }
        let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let mut scheduler = tokio::time::interval(SCHEDULER_TICK);
        scheduler.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut scheduled_minute = None;
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => return Ok(()),
                _ = scheduler.tick() => {
                    let minute = CronStore::current_unix_minute();
                    if scheduled_minute != Some(minute) {
                        scheduled_minute = Some(minute);
                        let due = self.cron.due_at_minute(minute)?;
                        if !due.is_empty() {
                            let host = self.host.clone();
                            tokio::spawn(async move {
                                for task in due {
                                    let _ = host.run_cron(task.id).await;
                                }
                            });
                        }
                    }
                }
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted?;
                    let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                        drop(stream);
                        continue;
                    };
                    let auth = Arc::clone(&self.auth);
                    let host = self.host.clone();
                    let cron = Arc::clone(&self.cron);
                    let tls = tls.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Some(tls) = tls {
                            if let Ok(Ok(stream)) =
                                tokio::time::timeout(AUTH_TIMEOUT, tls.accept(stream)).await
                            {
                                let _ = serve_connection(stream, auth, host, cron).await;
                            }
                        } else {
                            let _ = serve_connection(stream, auth, host, cron).await;
                        }
                    });
                }
            }
        }
    }

    /// Returns the bound address from persisted configuration.
    #[must_use]
    pub const fn listen_addr(&self) -> std::net::SocketAddr {
        self.config.listen
    }
}

async fn serve_connection<S>(
    stream: S,
    auth: Arc<AuthStore>,
    host: HostHandle,
    cron: Arc<CronStore>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let first = tokio::time::timeout(AUTH_TIMEOUT, read_frame::<ClientFrame>(&mut reader))
        .await
        .map_err(|_| Error::Unauthorized)??
        .ok_or(Error::Unauthorized)?;
    if let Err(error) = validate_version(first.version) {
        write_server_error(&mut writer, "protocol_version", error.to_string(), true).await?;
        return Ok(());
    }
    let last_sequence = match first.message {
        ClientMessage::Pair {
            code,
            client_label,
            last_sequence,
        } => match auth.pair(&code, &client_label) {
            Ok(issued) => {
                write_frame(
                    &mut writer,
                    &ServerFrame::new(ServerMessage::Paired {
                        client_id: issued.client_id,
                        token: issued.token,
                    }),
                )
                .await?;
                last_sequence
            }
            Err(_) => {
                write_server_error(&mut writer, "unauthorized", "pairing failed", true).await?;
                return Ok(());
            }
        },
        ClientMessage::Authenticate {
            token,
            last_sequence,
        } => match auth.authenticate(&token) {
            Ok(_identity) => last_sequence,
            Err(_) => {
                write_server_error(&mut writer, "unauthorized", "authentication failed", true)
                    .await?;
                return Ok(());
            }
        },
        _ => {
            write_server_error(
                &mut writer,
                "authentication_required",
                "the first frame must authenticate or pair",
                true,
            )
            .await?;
            return Ok(());
        }
    };

    let mut broadcasts = host.subscribe();
    write_frame(&mut writer, &ServerFrame::new(ServerMessage::Authenticated)).await?;
    let snapshot = match host.snapshot(last_sequence).await {
        Ok(snapshot) => snapshot,
        Err(rejection) => {
            write_rejection_as_error(&mut writer, &rejection).await?;
            host.snapshot(None)
                .await
                .map_err(|rejection| Error::Protocol(rejection.message))?
        }
    };
    write_frame(
        &mut writer,
        &ServerFrame::new(ServerMessage::Ready {
            payload: snapshot.ready,
        }),
    )
    .await?;
    let mut delivered_sequence = 0;
    for frame in snapshot.replay {
        if let Some(sequence) = sequence(&frame) {
            delivered_sequence = delivered_sequence.max(sequence);
        }
        write_frame(&mut writer, &frame).await?;
    }

    loop {
        tokio::select! {
            incoming = read_frame::<ClientFrame>(&mut reader) => {
                let Some(frame) = incoming? else {
                    return Ok(());
                };
                if let Err(error) = validate_version(frame.version) {
                    write_server_error(&mut writer, "protocol_version", error.to_string(), true).await?;
                    return Ok(());
                }
                handle_message(frame.message, &auth, &host, &cron, &mut writer).await?;
            }
            outgoing = broadcasts.recv() => match outgoing {
                Ok(frame) => {
                    if sequence(&frame).is_some_and(|value| value <= delivered_sequence) {
                        continue;
                    }
                    if let Some(value) = sequence(&frame) {
                        delivered_sequence = value;
                    }
                    write_frame(&mut writer, &frame).await?;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    write_server_error(
                        &mut writer,
                        "client_lagged",
                        "the client fell behind the event stream; reconnect with the last sequence",
                        true,
                    ).await?;
                    return Ok(());
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    }
}

async fn handle_message(
    message: ClientMessage,
    auth: &AuthStore,
    host: &HostHandle,
    cron: &CronStore,
    writer: &mut (impl AsyncWrite + Unpin),
) -> Result<()> {
    match message {
        ClientMessage::Pair { .. } | ClientMessage::Authenticate { .. } => {
            write_server_error(
                writer,
                "already_authenticated",
                "this connection is already authenticated",
                false,
            )
            .await
        }
        ClientMessage::OpenSession {
            request_id,
            session_id,
        } => write_result(writer, request_id, host.open_session(session_id).await).await,
        ClientMessage::RenameSession {
            request_id,
            session_id,
            title,
        } => {
            write_result(
                writer,
                request_id,
                host.rename_session(session_id, title).await,
            )
            .await
        }
        ClientMessage::SetSessionPinned {
            request_id,
            session_id,
            pinned,
        } => {
            write_result(
                writer,
                request_id,
                host.set_session_pinned(session_id, pinned).await,
            )
            .await
        }
        ClientMessage::DeleteSession {
            request_id,
            session_id,
        } => write_result(writer, request_id, host.delete_session(session_id).await).await,
        ClientMessage::Submit { submission } => {
            let request_id = submission.id.clone();
            write_result(writer, request_id, host.submit(submission).await).await
        }
        ClientMessage::ConfigureAgent {
            request_id,
            expected_revision,
            config,
        } => {
            write_result(
                writer,
                request_id,
                host.configure(expected_revision, config).await,
            )
            .await
        }
        ClientMessage::SetWorkspace { request_id, path } => {
            write_result(writer, request_id, host.set_workspace(path).await).await
        }
        ClientMessage::GetGitDiff { request_id } => match host.git_diff().await {
            Ok(diff) => {
                write_frame(
                    writer,
                    &ServerFrame::new(ServerMessage::GitDiff { request_id, diff }),
                )
                .await
            }
            Err(rejection) => write_rejection(writer, request_id, rejection).await,
        },
        ClientMessage::ListDirectories {
            request_id,
            path,
            include_files,
        } => {
            let result =
                tokio::task::spawn_blocking(move || list_directories(&path, include_files))
                    .await
                    .map_err(|error| internal_rejection(error.to_string()))
                    .and_then(std::convert::identity);
            match result {
                Ok(listing) => {
                    write_frame(
                        writer,
                        &ServerFrame::new(ServerMessage::Directories {
                            request_id,
                            listing,
                        }),
                    )
                    .await
                }
                Err(rejection) => write_rejection(writer, request_id, rejection).await,
            }
        }
        ClientMessage::SetProviderCredential {
            request_id,
            provider,
            api_key,
        } => match host.set_credential(provider.clone(), api_key, None).await {
            Ok(()) => {
                write_frame(
                    writer,
                    &ServerFrame::new(ServerMessage::ProviderCredentialStatus {
                        request_id,
                        provider,
                        configured: true,
                    }),
                )
                .await
            }
            Err(rejection) => write_rejection(writer, request_id, rejection).await,
        },
        ClientMessage::SetProviderEndpointCredential {
            request_id,
            provider,
            base_url,
            api_key,
        } => match host
            .set_credential(provider.clone(), api_key, Some(base_url))
            .await
        {
            Ok(()) => {
                write_frame(
                    writer,
                    &ServerFrame::new(ServerMessage::ProviderCredentialStatus {
                        request_id,
                        provider,
                        configured: true,
                    }),
                )
                .await
            }
            Err(rejection) => write_rejection(writer, request_id, rejection).await,
        },
        ClientMessage::CreatePairingCode { request_id } => match auth.create_pairing_code() {
            Ok(grant) => {
                write_frame(
                    writer,
                    &ServerFrame::new(ServerMessage::PairingCode {
                        request_id,
                        code: grant.code,
                        expires_at: grant.expires_at,
                    }),
                )
                .await
            }
            Err(error) => {
                write_rejection(writer, request_id, internal_rejection(error.to_string())).await
            }
        },
        ClientMessage::StartProviderLogin {
            request_id,
            provider,
        } => {
            write_result(
                writer,
                request_id.clone(),
                host.start_provider_login(request_id, provider).await,
            )
            .await
        }
        ClientMessage::GetProfile { request_id } => match host.profile().await {
            Ok(profile) => {
                write_frame(
                    writer,
                    &ServerFrame::new(ServerMessage::Profile {
                        request_id,
                        profile,
                    }),
                )
                .await
            }
            Err(rejection) => write_rejection(writer, request_id, rejection).await,
        },
        ClientMessage::ListArtifacts { request_id } => match host.artifacts().await {
            Ok(artifacts) => {
                write_frame(
                    writer,
                    &ServerFrame::new(ServerMessage::Artifacts {
                        request_id,
                        artifacts,
                    }),
                )
                .await
            }
            Err(rejection) => write_rejection(writer, request_id, rejection).await,
        },
        ClientMessage::AddCron {
            request_id,
            task,
            schedule,
        } => {
            let result = cron
                .add(&task, &schedule)
                .map(|_| ())
                .map_err(cron_rejection);
            write_result(writer, request_id, result).await
        }
        ClientMessage::ListCron { request_id } => match cron.list() {
            Ok(tasks) => {
                write_frame(
                    writer,
                    &ServerFrame::new(ServerMessage::CronTasks { request_id, tasks }),
                )
                .await
            }
            Err(error) => write_rejection(writer, request_id, cron_rejection(error)).await,
        },
        ClientMessage::RescheduleCron {
            request_id,
            id,
            schedule,
        } => {
            let result = cron
                .reschedule(&id, &schedule)
                .map(|_| ())
                .map_err(cron_rejection);
            write_result(writer, request_id, result).await
        }
        ClientMessage::DeleteCron { request_id, id } => {
            let result = cron.delete(&id).map(|_| ()).map_err(cron_rejection);
            write_result(writer, request_id, result).await
        }
        ClientMessage::RunCron { request_id, id } => {
            write_result(writer, request_id, host.run_cron(id).await).await
        }
        ClientMessage::ListCronHistory { request_id, id } => match cron.history(id.as_deref()) {
            Ok(runs) => {
                write_frame(
                    writer,
                    &ServerFrame::new(ServerMessage::CronHistory { request_id, runs }),
                )
                .await
            }
            Err(error) => write_rejection(writer, request_id, cron_rejection(error)).await,
        },
    }
}

async fn write_result(
    writer: &mut (impl AsyncWrite + Unpin),
    request_id: String,
    result: std::result::Result<(), Rejection>,
) -> Result<()> {
    match result {
        Ok(()) => {
            write_frame(
                writer,
                &ServerFrame::new(ServerMessage::Accepted { request_id }),
            )
            .await
        }
        Err(rejection) => write_rejection(writer, request_id, rejection).await,
    }
}

async fn write_rejection(
    writer: &mut (impl AsyncWrite + Unpin),
    request_id: String,
    rejection: Rejection,
) -> Result<()> {
    write_frame(
        writer,
        &ServerFrame::new(ServerMessage::Rejected {
            request_id,
            code: rejection.code.into(),
            message: rejection.message,
            fatal: rejection.fatal,
        }),
    )
    .await
}

async fn write_rejection_as_error(
    writer: &mut (impl AsyncWrite + Unpin),
    rejection: &Rejection,
) -> Result<()> {
    write_server_error(
        writer,
        rejection.code,
        rejection.message.clone(),
        rejection.fatal,
    )
    .await
}

async fn write_server_error(
    writer: &mut (impl AsyncWrite + Unpin),
    code: impl Into<String>,
    message: impl Into<String>,
    fatal: bool,
) -> Result<()> {
    write_frame(
        writer,
        &ServerFrame::new(ServerMessage::Error {
            code: code.into(),
            message: message.into(),
            fatal,
        }),
    )
    .await
}

fn internal_rejection(message: String) -> Rejection {
    Rejection {
        code: "gateway_error",
        message,
        fatal: false,
    }
}

fn cron_rejection(error: Error) -> Rejection {
    Rejection {
        code: "invalid_cron",
        message: error.to_string(),
        fatal: false,
    }
}

fn list_directories(
    path: &Path,
    include_files: bool,
) -> std::result::Result<DirectoryListing, Rejection> {
    let path = fs::canonicalize(path).map_err(directory_rejection)?;
    if !path.is_dir() {
        return Err(directory_rejection("path is not a directory"));
    }
    let mut entries = fs::read_dir(&path)
        .map_err(directory_rejection)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let is_directory = entry.path().is_dir();
            if !is_directory && !include_files {
                return None;
            }
            Some(DirectoryEntry {
                name: entry.file_name().to_str()?.to_owned(),
                path: entry.path().to_str().map(PathBuf::from)?,
                is_directory,
            })
        })
        .take(MAX_DIRECTORY_ENTRIES)
        .collect::<Vec<_>>();
    entries.sort_unstable_by_key(|entry| entry.name.to_lowercase());
    Ok(DirectoryListing {
        parent: path.parent().map(Path::to_path_buf),
        path,
        entries,
    })
}

fn directory_rejection(error: impl std::fmt::Display) -> Rejection {
    Rejection {
        code: "invalid_directory",
        message: error.to_string(),
        fatal: false,
    }
}

fn sequence(frame: &ServerFrame) -> Option<u64> {
    match frame.message {
        ServerMessage::AgentEvent { sequence, .. } => Some(sequence),
        _ => None,
    }
}

fn tls_acceptor(config: &TlsConfig) -> Result<TlsAcceptor> {
    let certificates = load_certificates(&config.certificate)?;
    let private_key = load_private_key(&config.private_key)?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|error| Error::Config(format!("invalid TLS certificate or key: {error}")))?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

fn load_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let mut reader = BufReader::new(File::open(path)?);
    let certificates = rustls_pemfile::certs(&mut reader).collect::<std::io::Result<Vec<_>>>()?;
    if certificates.is_empty() {
        return Err(Error::Config("TLS certificate file is empty".into()));
    }
    Ok(certificates)
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    let mut reader = BufReader::new(File::open(path)?);
    rustls_pemfile::private_key(&mut reader)?
        .ok_or_else(|| Error::Config("TLS private-key file is empty".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bootstrap_owns_the_listener_before_creating_state() {
        let root = tempfile::tempdir().expect("temporary directory");
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).expect("workspace");
        let occupied = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("occupied listener");
        let listen = occupied.local_addr().expect("listen address");
        let state = root.path().join("state");

        let result = GatewayServer::bootstrap(state.clone(), workspace, listen).await;

        assert!(matches!(result, Err(Error::Io(_))));
        assert!(!state.exists());
    }

    #[test]
    fn tls_loader_rejects_empty_pem_files() {
        let file = tempfile::NamedTempFile::new().expect("temporary PEM");

        let error = load_certificates(file.path()).expect_err("empty PEM must fail");

        assert!(error.to_string().contains("certificate file is empty"));
    }

    #[test]
    fn directory_listing_is_sorted_and_excludes_files() {
        let root = tempfile::tempdir().expect("temporary directory");
        fs::create_dir(root.path().join("zeta")).expect("create directory");
        fs::create_dir(root.path().join("Alpha")).expect("create directory");
        fs::write(root.path().join("notes.txt"), b"not a folder").expect("create file");

        let listing = list_directories(root.path(), false).expect("list directories");

        assert_eq!(
            listing
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["Alpha", "zeta"]
        );
    }

    #[test]
    fn directory_listing_can_include_files() {
        let root = tempfile::tempdir().expect("temporary directory");
        fs::create_dir(root.path().join("tasks")).expect("create directory");
        fs::write(root.path().join("daily.md"), b"task").expect("create file");

        let listing = list_directories(root.path(), true).expect("list directory entries");

        assert_eq!(listing.entries.len(), 2);
        assert!(
            listing
                .entries
                .iter()
                .any(|entry| { entry.name == "tasks" && entry.is_directory })
        );
        assert!(
            listing
                .entries
                .iter()
                .any(|entry| { entry.name == "daily.md" && !entry.is_directory })
        );
    }

    #[tokio::test]
    async fn rejection_frames_preserve_request_correlation() {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        write_rejection(
            &mut writer,
            "request-7".into(),
            Rejection {
                code: "agent_busy",
                message: "busy".into(),
                fatal: false,
            },
        )
        .await
        .expect("write rejection");

        let frame = read_frame::<ServerFrame>(&mut reader)
            .await
            .expect("read rejection")
            .expect("frame");

        assert!(matches!(
            frame.message,
            ServerMessage::Rejected { request_id, .. } if request_id == "request-7"
        ));
    }
}
