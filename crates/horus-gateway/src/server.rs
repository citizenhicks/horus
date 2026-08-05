//! Authenticated raw, WebSocket-loopback, and TLS gateway listeners.

use std::collections::BTreeMap;
use std::fs;
use std::fs::File;
use std::future::Future;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt as _;
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::accept_hdr_async_with_config;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::http::header::{HOST, ORIGIN};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::auth::{AuthStore, ClientIdentity, PairingGrant};
use crate::config::{ConfigStore, CredentialStore, GatewayConfig, TlsConfig};
use crate::cron::CronStore;
use crate::host::{GatewayHost, HostHandle, Rejection};
use crate::wire::{
    ClientFrame, ClientKind, ClientMessage, ClientStatus, DirectoryEntry, DirectoryListing,
    FrameReader, MAX_FRAME_BYTES, ServerFrame, ServerMessage, framed_to_websocket, read_frame,
    validate_version, websocket_error, websocket_to_framed, write_frame,
};
use crate::{Error, Result};

const AUTH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONNECTIONS: usize = 32;
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(72 * 60 * 60);
const SCHEDULER_TICK: Duration = Duration::from_secs(15);
const MAX_DIRECTORY_ENTRIES: usize = 512;
const WEBSOCKET_BRIDGE_BYTES: usize = 16 * 1024;

const _: () = assert!(MAX_FRAME_BYTES < 1 << 24);

struct WebSocketUpgradePolicy {
    expected_host: Option<String>,
}

struct PlaintextHandshake {
    expected_websocket_host: Option<String>,
    auth_deadline: Instant,
}

impl Callback for WebSocketUpgradePolicy {
    fn on_request(
        self,
        request: &Request,
        response: Response,
    ) -> std::result::Result<Response, ErrorResponse> {
        if request.uri().path_and_query().map(|value| value.as_str()) != Some("/") {
            return Err(websocket_rejection(StatusCode::NOT_FOUND));
        }
        if request.headers().contains_key(ORIGIN) {
            return Err(websocket_rejection(StatusCode::FORBIDDEN));
        }
        if let Some(expected) = self.expected_host
            && !request_host_matches(request, &expected)
        {
            return Err(websocket_rejection(StatusCode::FORBIDDEN));
        }
        Ok(response)
    }
}

fn request_host_matches(request: &Request, expected: &str) -> bool {
    let mut values = request.headers().get_all(HOST).iter();
    let Some(actual) = values.next().and_then(|value| value.to_str().ok()) else {
        return false;
    };
    values.next().is_none()
        && (actual.eq_ignore_ascii_case(expected)
            || actual
                .strip_suffix(":443")
                .is_some_and(|host| host.eq_ignore_ascii_case(expected)))
}

fn websocket_rejection(status: StatusCode) -> ErrorResponse {
    let mut response = ErrorResponse::new(None);
    *response.status_mut() = status;
    response
}

#[derive(Default)]
struct ClientConnections {
    entries: Mutex<BTreeMap<(String, ClientKind), usize>>,
}

struct ClientConnectionGuard {
    connections: Arc<ClientConnections>,
    key: (String, ClientKind),
}

impl ClientConnections {
    fn register(
        self: &Arc<Self>,
        client_id: String,
        kind: ClientKind,
    ) -> Result<ClientConnectionGuard> {
        let key = (client_id, kind);
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| Error::Config("client-connection lock is poisoned".into()))?;
        let connections = entries.entry(key.clone()).or_default();
        *connections = connections
            .checked_add(1)
            .ok_or_else(|| Error::Config("client connection count overflow".into()))?;
        drop(entries);
        Ok(ClientConnectionGuard {
            connections: Arc::clone(self),
            key,
        })
    }

    fn snapshot(&self, paired: &[ClientIdentity]) -> Result<Vec<ClientStatus>> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| Error::Config("client-connection lock is poisoned".into()))?;
        Ok(paired
            .iter()
            .map(|identity| {
                let mut kinds = Vec::new();
                let mut connections = 0;
                for ((client_id, kind), count) in &*entries {
                    if client_id == &identity.id
                        && *kind != ClientKind::GatewayDashboard
                        && *count > 0
                    {
                        kinds.push(*kind);
                        connections += *count;
                    }
                }
                ClientStatus {
                    client_id: identity.id.clone(),
                    label: identity.label.clone(),
                    kinds,
                    connections,
                }
            })
            .collect())
    }
}

impl Drop for ClientConnectionGuard {
    fn drop(&mut self) {
        let Ok(mut entries) = self.connections.entries.lock() else {
            return;
        };
        let Some(connections) = entries.get_mut(&self.key) else {
            return;
        };
        if *connections > 1 {
            *connections -= 1;
        } else {
            entries.remove(&self.key);
        }
    }
}

/// Fully assembled machine gateway and its chat registry.
pub struct GatewayServer {
    config: GatewayConfig,
    listener: TcpListener,
    auth: Arc<AuthStore>,
    host: GatewayHost,
    cron: Arc<CronStore>,
}

impl GatewayServer {
    /// Opens protected state and the machine-wide chat registry.
    pub async fn open(state_dir: PathBuf) -> Result<Self> {
        let (store, config) = ConfigStore::open(state_dir)?;
        let listener = TcpListener::bind(config.listen).await?;
        Self::assemble(store, config, listener).await
    }

    /// Binds and initializes a fresh local gateway before exposing its one-use pairing grant.
    pub async fn bootstrap(
        state_dir: PathBuf,
        listen: std::net::SocketAddr,
    ) -> Result<(Self, PairingGrant)> {
        let listener = TcpListener::bind(listen).await?;
        let listen = listener.local_addr()?;
        let (store, config) = ConfigStore::initialize(state_dir, listen, None)?;
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
        let cron = Arc::new(CronStore::open(store.state_dir())?);
        let host = GatewayHost::start(store, config.clone(), credentials, Arc::clone(&cron))?;
        Ok(Self {
            config,
            listener,
            auth,
            host,
            cron,
        })
    }

    /// Serves until a process shutdown signal or 72 hours of inactivity.
    pub async fn serve(self) -> Result<()> {
        let websocket_host = self.configured_websocket_host()?;
        self.serve_with_host(websocket_host).await
    }

    /// Serves Cloudflare WebSockets using the resolved public hostname.
    pub(crate) async fn serve_cloudflare(self, hostname: String) -> Result<()> {
        let cloudflare = self.config.cloudflare.as_ref().ok_or_else(|| {
            Error::Config("a Cloudflare hostname requires tunnel configuration".into())
        })?;
        if cloudflare
            .hostname()
            .is_some_and(|configured| configured != hostname)
        {
            return Err(Error::Config(
                "runtime Cloudflare hostname does not match gateway configuration".into(),
            ));
        }
        self.serve_with_host(Some(hostname)).await
    }

    async fn serve_with_host(self, websocket_host: Option<String>) -> Result<()> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            let mut interrupts = signal(SignalKind::interrupt())?;
            let mut terminations = signal(SignalKind::terminate())?;
            self.serve_until_inactive_with_host(
                async move {
                    tokio::select! {
                        _ = interrupts.recv() => {}
                        _ = terminations.recv() => {}
                    }
                },
                INACTIVITY_TIMEOUT,
                websocket_host,
            )
            .await
        }
        #[cfg(not(unix))]
        self.serve_until_inactive_with_host(
            async {
                let _ = tokio::signal::ctrl_c().await;
            },
            INACTIVITY_TIMEOUT,
            websocket_host,
        )
        .await
    }

    /// Serves until shutdown or the same inactivity policy as [`Self::serve`].
    pub async fn serve_until(self, shutdown: impl Future<Output = ()>) -> Result<()> {
        let websocket_host = self.configured_websocket_host()?;
        self.serve_until_inactive_with_host(shutdown, INACTIVITY_TIMEOUT, websocket_host)
            .await
    }

    #[cfg(test)]
    async fn serve_until_inactive(
        self,
        shutdown: impl Future<Output = ()>,
        inactivity_timeout: Duration,
    ) -> Result<()> {
        let websocket_host = self.configured_websocket_host()?;
        self.serve_until_inactive_with_host(shutdown, inactivity_timeout, websocket_host)
            .await
    }

    async fn serve_until_inactive_with_host(
        self,
        shutdown: impl Future<Output = ()>,
        inactivity_timeout: Duration,
        websocket_host: Option<String>,
    ) -> Result<()> {
        self.config.validate()?;
        let tls = self.config.tls.as_ref().map(tls_acceptor).transpose()?;
        if tls.is_none() && !self.listener.local_addr()?.ip().is_loopback() {
            return Err(Error::Config(
                "plaintext listeners are restricted to loopback".into(),
            ));
        }
        let mut connections = JoinSet::new();
        let client_connections = Arc::new(ClientConnections::default());
        let (client_revocations, _) = broadcast::channel(MAX_CONNECTIONS);
        let mut has_scheduled_tasks = self.cron.has_tasks()?;
        let inactivity = tokio::time::sleep(inactivity_timeout);
        tokio::pin!(inactivity);
        let mut scheduler = tokio::time::interval(SCHEDULER_TICK);
        scheduler.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut scheduled_minute = None;
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => return Ok(()),
                _ = scheduler.tick() => {
                    let scheduled = self.cron.has_tasks()?;
                    if has_scheduled_tasks && !scheduled && connections.is_empty() {
                        inactivity.as_mut().reset(tokio::time::Instant::now() + inactivity_timeout);
                    }
                    has_scheduled_tasks = scheduled;
                    let minute = CronStore::current_unix_minute();
                    if scheduled_minute != Some(minute) {
                        scheduled_minute = Some(minute);
                        let due = self.cron.due_at_minute(minute)?;
                        if !due.is_empty() {
                            let host = self.host.clone();
                            tokio::spawn(async move {
                                for task in due {
                                    let _ = host.run_cron(task.session_id, task.id).await;
                                }
                            });
                        }
                    }
                }
                Some(_) = connections.join_next(), if !connections.is_empty() => {
                    if connections.is_empty() {
                        has_scheduled_tasks = self.cron.has_tasks()?;
                        if !has_scheduled_tasks {
                            inactivity.as_mut().reset(tokio::time::Instant::now() + inactivity_timeout);
                        }
                    }
                }
                accepted = self.listener.accept(), if connections.len() < MAX_CONNECTIONS => {
                    let (stream, _) = accepted?;
                    let auth = Arc::clone(&self.auth);
                    let host = self.host.clone();
                    let cron = Arc::clone(&self.cron);
                    let client_connections = Arc::clone(&client_connections);
                    let client_revocations = client_revocations.clone();
                    let tls = tls.clone();
                    let websocket_host = websocket_host.clone();
                    connections.spawn(async move {
                        if let Some(tls) = tls {
                            if let Ok(Ok(stream)) =
                                tokio::time::timeout(AUTH_TIMEOUT, tls.accept(stream)).await
                            {
                                let _ = serve_connection(
                                    stream,
                                    auth,
                                    host,
                                    cron,
                                    client_connections,
                                    client_revocations,
                                    Instant::now() + AUTH_TIMEOUT,
                                )
                                .await;
                            }
                        } else {
                            let _ = serve_plaintext_connection(
                                stream,
                                auth,
                                host,
                                cron,
                                client_connections,
                                client_revocations,
                                PlaintextHandshake {
                                    expected_websocket_host: websocket_host,
                                    auth_deadline: Instant::now() + AUTH_TIMEOUT,
                                },
                            )
                            .await;
                        }
                    });
                }
                () = &mut inactivity, if connections.is_empty() && !has_scheduled_tasks => {
                    has_scheduled_tasks = self.cron.has_tasks()?;
                    if !has_scheduled_tasks {
                        return Ok(());
                    }
                }
            }
        }
    }

    fn configured_websocket_host(&self) -> Result<Option<String>> {
        self.config
            .cloudflare
            .as_ref()
            .map(|cloudflare| {
                cloudflare.hostname().map(str::to_owned).ok_or_else(|| {
                    Error::Config(
                        "quick tunnel hostname is unavailable before cloudflared starts".into(),
                    )
                })
            })
            .transpose()
    }

    /// Returns the bound address from persisted configuration.
    #[must_use]
    pub const fn listen_addr(&self) -> std::net::SocketAddr {
        self.config.listen
    }
}

async fn serve_plaintext_connection(
    stream: TcpStream,
    auth: Arc<AuthStore>,
    host: GatewayHost,
    cron: Arc<CronStore>,
    client_connections: Arc<ClientConnections>,
    client_revocations: broadcast::Sender<String>,
    handshake: PlaintextHandshake,
) -> Result<()> {
    let PlaintextHandshake {
        expected_websocket_host,
        auth_deadline,
    } = handshake;
    let mut first = [0_u8; 1];
    let read = tokio::time::timeout_at(auth_deadline, stream.peek(&mut first))
        .await
        .map_err(|_| Error::Unauthorized)??;
    if read == 1 && first[0] == b'G' {
        serve_websocket(
            stream,
            auth,
            host,
            cron,
            client_connections,
            client_revocations,
            PlaintextHandshake {
                expected_websocket_host,
                auth_deadline,
            },
        )
        .await
    } else {
        serve_connection(
            stream,
            auth,
            host,
            cron,
            client_connections,
            client_revocations,
            auth_deadline,
        )
        .await
    }
}

async fn serve_websocket(
    stream: TcpStream,
    auth: Arc<AuthStore>,
    host: GatewayHost,
    cron: Arc<CronStore>,
    client_connections: Arc<ClientConnections>,
    client_revocations: broadcast::Sender<String>,
    handshake: PlaintextHandshake,
) -> Result<()> {
    let PlaintextHandshake {
        expected_websocket_host,
        auth_deadline,
    } = handshake;
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES));
    let websocket = tokio::time::timeout_at(
        auth_deadline,
        accept_hdr_async_with_config(
            stream,
            WebSocketUpgradePolicy {
                expected_host: expected_websocket_host,
            },
            Some(config),
        ),
    )
    .await
    .map_err(|_| Error::Unauthorized)?
    .map_err(websocket_error)?;
    let (outgoing, incoming) = websocket.split();
    let (gateway_stream, bridge_stream) = tokio::io::duplex(WEBSOCKET_BRIDGE_BYTES);
    let (bridge_reader, bridge_writer) = tokio::io::split(bridge_stream);
    let gateway_and_outgoing = async {
        let (gateway, outgoing) = tokio::join!(
            serve_connection(
                gateway_stream,
                auth,
                host,
                cron,
                client_connections,
                client_revocations,
                auth_deadline,
            ),
            framed_to_websocket(bridge_reader, outgoing),
        );
        gateway?;
        outgoing
    };
    tokio::pin!(gateway_and_outgoing);
    tokio::select! {
        result = &mut gateway_and_outgoing => result,
        result = websocket_to_framed(incoming, bridge_writer) => {
            result?;
            gateway_and_outgoing.await
        }
    }
}

async fn serve_connection<S>(
    stream: S,
    auth: Arc<AuthStore>,
    host: GatewayHost,
    cron: Arc<CronStore>,
    client_connections: Arc<ClientConnections>,
    client_revocations: broadcast::Sender<String>,
    auth_deadline: Instant,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut revocations = client_revocations.subscribe();
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = FrameReader::new(reader);
    let first = tokio::time::timeout_at(auth_deadline, read_frame::<ClientFrame>(&mut reader))
        .await
        .map_err(|_| Error::Unauthorized)??
        .ok_or(Error::Unauthorized)?;
    if let Err(error) = validate_version(first.version) {
        write_server_error(&mut writer, "protocol_version", error.to_string(), true).await?;
        return Ok(());
    }
    let (client_id, client_kind) = match first.message {
        ClientMessage::Pair {
            code,
            client_label,
            client_kind,
        } => match auth.pair(&code, &client_label) {
            Ok(issued) => {
                let client_id = issued.client_id.clone();
                write_frame(
                    &mut writer,
                    &ServerFrame::new(ServerMessage::Paired {
                        client_id: issued.client_id,
                        token: issued.token,
                    }),
                )
                .await?;
                (client_id, client_kind)
            }
            Err(_) => {
                write_server_error(&mut writer, "unauthorized", "pairing failed", true).await?;
                return Ok(());
            }
        },
        ClientMessage::Authenticate { token, client_kind } => match auth.authenticate(&token) {
            Ok(identity) => (identity.id, client_kind),
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

    let _client_connection = client_connections.register(client_id.clone(), client_kind)?;

    write_frame(&mut writer, &ServerFrame::new(ServerMessage::Authenticated)).await?;
    let mut gateway_broadcasts = host.subscribe();
    let ready = host
        .ready()
        .await
        .map_err(|rejection| Error::Protocol(rejection.message))?;
    write_frame(
        &mut writer,
        &ServerFrame::new(ServerMessage::Ready { payload: ready }),
    )
    .await?;
    let mut selected: Option<SelectedChat> = None;

    loop {
        let incoming = tokio::select! {
            biased;
            revoked = revocations.recv() => {
                match revoked {
                    Ok(revoked) if revoked == client_id => return Ok(()),
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)
                        | broadcast::error::RecvError::Closed) => return Ok(()),
                }
                None
            }
            incoming = read_frame::<ClientFrame>(&mut reader) => Some(incoming),
            outgoing = gateway_broadcasts.recv() => {
                match outgoing {
                    Ok(frame) => write_frame(&mut writer, &frame).await?,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let ready = host
                            .ready()
                            .await
                            .map_err(|rejection| Error::Protocol(rejection.message))?;
                        write_frame(
                            &mut writer,
                            &ServerFrame::new(ServerMessage::Ready { payload: ready }),
                        )
                        .await?;
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
                None
            }
            outgoing = selected_broadcast(&mut selected) => {
                match outgoing {
                    Ok(frame) => {
                        let active = selected
                            .as_mut()
                            .expect("a selected-chat broadcast requires a selected chat");
                        if !sequence(&frame)
                            .is_some_and(|value| value <= active.delivered_sequence)
                        {
                            if let Some(value) = sequence(&frame) {
                                active.delivered_sequence = value;
                            }
                            write_frame(&mut writer, &frame).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        write_server_error(
                            &mut writer,
                            "client_lagged",
                            "the client fell behind the event stream; reconnect with the last sequence",
                            true,
                        )
                        .await?;
                        return Ok(());
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
                None
            }
        };
        let Some(incoming) = incoming else {
            continue;
        };
        let Some(frame) = incoming? else {
            return Ok(());
        };
        if let Err(error) = validate_version(frame.version) {
            write_server_error(&mut writer, "protocol_version", error.to_string(), true).await?;
            return Ok(());
        }
        let client = AuthenticatedClient {
            id: &client_id,
            connections: &client_connections,
            revocations: &client_revocations,
        };
        handle_message(
            frame.message,
            &auth,
            &host,
            &cron,
            &client,
            &mut selected,
            &mut writer,
        )
        .await?;
    }
}

struct SelectedChat {
    host: HostHandle,
    broadcasts: broadcast::Receiver<ServerFrame>,
    delivered_sequence: u64,
}

struct AuthenticatedClient<'a> {
    id: &'a str,
    connections: &'a ClientConnections,
    revocations: &'a broadcast::Sender<String>,
}

async fn selected_broadcast(
    selected: &mut Option<SelectedChat>,
) -> std::result::Result<ServerFrame, broadcast::error::RecvError> {
    match selected {
        Some(active) => active.broadcasts.recv().await,
        None => std::future::pending().await,
    }
}

async fn handle_message(
    message: ClientMessage,
    auth: &AuthStore,
    gateway: &GatewayHost,
    cron: &CronStore,
    client: &AuthenticatedClient<'_>,
    selected: &mut Option<SelectedChat>,
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
        ClientMessage::ListClients { request_id } => {
            write_client_inventory(writer, request_id, client.id, auth, client.connections).await
        }
        ClientMessage::UnpairClient {
            request_id,
            client_id,
        } => match auth.unpair_client(client.id, &client_id) {
            Ok(true) => {
                let _ = client.revocations.send(client_id);
                write_client_inventory(writer, request_id, client.id, auth, client.connections)
                    .await
            }
            Ok(false) => {
                write_rejection(
                    writer,
                    request_id,
                    Rejection {
                        code: "unpair_rejected",
                        message: "that paired device cannot be unpaired from this connection"
                            .into(),
                        fatal: false,
                    },
                )
                .await
            }
            Err(_) => {
                write_rejection(
                    writer,
                    request_id,
                    internal_rejection("failed to update paired devices".into()),
                )
                .await
            }
        },
        ClientMessage::ListSessions { request_id } => match gateway.sessions().await {
            Ok(sessions) => {
                write_frame(
                    writer,
                    &ServerFrame::new(ServerMessage::Sessions {
                        request_id: Some(request_id),
                        sessions,
                    }),
                )
                .await
            }
            Err(rejection) => write_rejection(writer, request_id, rejection).await,
        },
        ClientMessage::CreateSession {
            request_id,
            workspace,
        } => match gateway.create_session(&workspace).await {
            Ok(host) => open_selected(writer, selected, request_id, host, None).await,
            Err(rejection) => write_rejection(writer, request_id, rejection).await,
        },
        ClientMessage::OpenSession {
            request_id,
            session_id,
            last_sequence,
        } => match gateway.open_session(&session_id).await {
            Ok(host) => open_selected(writer, selected, request_id, host, last_sequence).await,
            Err(rejection) => write_rejection(writer, request_id, rejection).await,
        },
        ClientMessage::RenameSession {
            request_id,
            session_id,
            title,
        } => {
            let host = match require_selected(selected, &session_id) {
                Ok(host) => host,
                Err(rejection) => return write_rejection(writer, request_id, rejection).await,
            };
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
            let host = match require_selected(selected, &session_id) {
                Ok(host) => host,
                Err(rejection) => return write_rejection(writer, request_id, rejection).await,
            };
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
        } => {
            let host = match require_selected(selected, &session_id) {
                Ok(host) => host,
                Err(rejection) => return write_rejection(writer, request_id, rejection).await,
            };
            write_result(writer, request_id, host.delete_session(session_id).await).await
        }
        ClientMessage::Submit {
            session_id,
            submission,
        } => {
            let request_id = submission.id.clone();
            let host = match require_selected(selected, &session_id) {
                Ok(host) => host,
                Err(rejection) => return write_rejection(writer, request_id, rejection).await,
            };
            write_result(writer, request_id, host.submit(submission).await).await
        }
        ClientMessage::ConfigureSession {
            request_id,
            session_id,
            expected_revision,
            config,
        } => {
            let host = match require_selected(selected, &session_id) {
                Ok(host) => host,
                Err(rejection) => return write_rejection(writer, request_id, rejection).await,
            };
            write_result(
                writer,
                request_id,
                host.configure(expected_revision, config).await,
            )
            .await
        }
        ClientMessage::ConfigureDefaultAgent {
            request_id,
            expected_revision,
            config,
        } => match gateway
            .configure_default_agent(expected_revision, config)
            .await
        {
            Ok(payload) => {
                write_frame(
                    writer,
                    &ServerFrame::new(ServerMessage::GatewayConfigured {
                        request_id,
                        payload,
                    }),
                )
                .await
            }
            Err(rejection) => write_rejection(writer, request_id, rejection).await,
        },
        ClientMessage::GetGitDiff {
            request_id,
            session_id,
        } => match require_selected(selected, &session_id) {
            Err(rejection) => write_rejection(writer, request_id, rejection).await,
            Ok(host) => match host.git_diff().await {
                Ok(diff) => {
                    write_frame(
                        writer,
                        &ServerFrame::new(ServerMessage::GitDiff {
                            request_id,
                            session_id,
                            diff,
                        }),
                    )
                    .await
                }
                Err(rejection) => write_rejection(writer, request_id, rejection).await,
            },
        },
        ClientMessage::SwitchGitBranch {
            request_id,
            session_id,
            branch,
        } => {
            let host = match require_selected(selected, &session_id) {
                Ok(host) => host,
                Err(rejection) => return write_rejection(writer, request_id, rejection).await,
            };
            write_result(writer, request_id, host.switch_git_branch(branch).await).await
        }
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
        } => {
            match gateway
                .set_credential(provider.clone(), api_key, None)
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
            }
        }
        ClientMessage::SetProviderEndpointCredential {
            request_id,
            provider,
            base_url,
            api_key,
        } => {
            match gateway
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
            }
        }
        ClientMessage::RegisterProvider { request_id, config } => {
            match gateway.register_provider(config).await {
                Ok(payload) => {
                    write_frame(
                        writer,
                        &ServerFrame::new(ServerMessage::GatewayConfigured {
                            request_id,
                            payload,
                        }),
                    )
                    .await
                }
                Err(rejection) => write_rejection(writer, request_id, rejection).await,
            }
        }
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
                gateway.start_provider_login(request_id, provider).await,
            )
            .await
        }
        ClientMessage::GetProfile { request_id } => match gateway.profile().await {
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
        ClientMessage::ListArtifacts {
            request_id,
            session_id,
        } => match require_selected(selected, &session_id) {
            Err(rejection) => write_rejection(writer, request_id, rejection).await,
            Ok(host) => match host.artifacts().await {
                Ok(artifacts) => {
                    write_frame(
                        writer,
                        &ServerFrame::new(ServerMessage::Artifacts {
                            request_id,
                            session_id,
                            artifacts,
                        }),
                    )
                    .await
                }
                Err(rejection) => write_rejection(writer, request_id, rejection).await,
            },
        },
        ClientMessage::StartCronSetup {
            request_id,
            session_id,
            task,
        } => {
            let host = match require_selected(selected, &session_id) {
                Ok(host) => host,
                Err(rejection) => return write_rejection(writer, request_id, rejection).await,
            };
            write_result(writer, request_id, host.start_cron_setup(task).await).await
        }
        ClientMessage::ListCron {
            request_id,
            session_id,
        } => match cron.list(&session_id) {
            Ok(tasks) => {
                write_frame(
                    writer,
                    &ServerFrame::new(ServerMessage::CronTasks {
                        request_id,
                        session_id,
                        tasks,
                    }),
                )
                .await
            }
            Err(error) => write_rejection(writer, request_id, cron_rejection(error)).await,
        },
        ClientMessage::RescheduleCron {
            request_id,
            session_id,
            id,
            schedule,
        } => {
            let result = cron
                .reschedule(&session_id, &id, &schedule)
                .map(|_| ())
                .map_err(cron_rejection);
            write_result(writer, request_id, result).await
        }
        ClientMessage::DeleteCron {
            request_id,
            session_id,
            id,
        } => {
            let result = cron
                .delete(&session_id, &id)
                .map(|_| ())
                .map_err(cron_rejection);
            write_result(writer, request_id, result).await
        }
        ClientMessage::RunCron {
            request_id,
            session_id,
            id,
        } => write_result(writer, request_id, gateway.run_cron(session_id, id).await).await,
        ClientMessage::ListCronHistory {
            request_id,
            session_id,
            id,
        } => match cron.history(&session_id, id.as_deref()) {
            Ok(runs) => {
                write_frame(
                    writer,
                    &ServerFrame::new(ServerMessage::CronHistory {
                        request_id,
                        session_id,
                        runs,
                    }),
                )
                .await
            }
            Err(error) => write_rejection(writer, request_id, cron_rejection(error)).await,
        },
    }
}

async fn write_client_inventory(
    writer: &mut (impl AsyncWrite + Unpin),
    request_id: String,
    current_client_id: &str,
    auth: &AuthStore,
    client_connections: &ClientConnections,
) -> Result<()> {
    write_frame(
        writer,
        &ServerFrame::new(ServerMessage::Clients {
            request_id,
            current_client_id: current_client_id.into(),
            clients: client_connections.snapshot(&auth.clients()?)?,
        }),
    )
    .await
}

async fn open_selected(
    writer: &mut (impl AsyncWrite + Unpin),
    selected: &mut Option<SelectedChat>,
    request_id: String,
    host: HostHandle,
    last_sequence: Option<u64>,
) -> Result<()> {
    let broadcasts = host.subscribe();
    let snapshot = match host.snapshot(last_sequence).await {
        Ok(snapshot) => snapshot,
        Err(rejection) => return write_rejection(writer, request_id, rejection).await,
    };
    let delivered_sequence = snapshot.ready.latest_sequence;
    write_frame(
        writer,
        &ServerFrame::new(ServerMessage::SessionOpened {
            request_id,
            payload: snapshot.ready,
        }),
    )
    .await?;
    for frame in snapshot.replay {
        write_frame(writer, &frame).await?;
    }
    *selected = Some(SelectedChat {
        host,
        broadcasts,
        delivered_sequence,
    });
    Ok(())
}

fn require_selected<'a>(
    selected: &'a Option<SelectedChat>,
    session_id: &str,
) -> std::result::Result<&'a HostHandle, Rejection> {
    let host = require_any_selected(selected)?;
    if host.session_id() != session_id {
        return Err(Rejection {
            code: "session_not_selected",
            message: "open this chat on the connection before controlling it".into(),
            fatal: false,
        });
    }
    Ok(host)
}

fn require_any_selected(
    selected: &Option<SelectedChat>,
) -> std::result::Result<&HostHandle, Rejection> {
    selected
        .as_ref()
        .map(|selected| &selected.host)
        .ok_or_else(|| Rejection {
            code: "session_required",
            message: "create or open a chat first".into(),
            fatal: false,
        })
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
    use futures_util::SinkExt as _;
    use horus::protocol::{Op, Submission};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::protocol::Message;
    use tokio_tungstenite::tungstenite::protocol::Role;
    use uuid::Uuid;

    use crate::client::{Endpoint, GatewayClient, GatewayEvents, GatewaySender};
    use crate::wire::{SessionActivity, SessionActivityState};

    use super::*;

    #[test]
    fn client_inventory_aggregates_connections_and_keeps_inactive_devices() {
        let clients = Arc::new(ClientConnections::default());
        let identity = ClientIdentity {
            id: "client-a".into(),
            label: "Mac".into(),
        };
        let paired = [identity.clone()];
        let first = clients
            .register(identity.id.clone(), ClientKind::Macos)
            .expect("first connection");
        let _dashboard = clients
            .register(identity.id.clone(), ClientKind::GatewayDashboard)
            .expect("dashboard connection");
        let second = clients
            .register(identity.id, ClientKind::Macos)
            .expect("second connection");

        let two = clients.snapshot(&paired).expect("two connections")[0].connections;
        drop(first);
        let one = clients.snapshot(&paired).expect("one connection")[0].connections;
        drop(second);
        let inactive = clients.snapshot(&paired).expect("inactive client")[0].clone();

        assert_eq!(
            (two, one, inactive.connections, inactive.kinds),
            (2, 1, 0, Vec::new())
        );
    }

    #[test]
    fn websocket_upgrade_rejects_non_root_targets() {
        let request = Request::builder().uri("/other").body(()).expect("request");

        let rejection = WebSocketUpgradePolicy {
            expected_host: None,
        }
        .on_request(&request, Response::new(()))
        .expect_err("non-root path must fail");

        assert_eq!(rejection.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn websocket_upgrade_rejects_browser_origins() {
        let request = Request::builder()
            .uri("/")
            .header(ORIGIN, "https://attacker.example")
            .body(())
            .expect("request");

        let rejection = WebSocketUpgradePolicy {
            expected_host: None,
        }
        .on_request(&request, Response::new(()))
        .expect_err("Origin header must fail");

        assert_eq!(rejection.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn websocket_upgrade_rejects_the_wrong_cloudflare_host() {
        let request = Request::builder()
            .uri("/")
            .header(HOST, "other.example")
            .body(())
            .expect("request");

        let rejection = WebSocketUpgradePolicy {
            expected_host: Some("gateway.example".into()),
        }
        .on_request(&request, Response::new(()))
        .expect_err("wrong Host must fail");

        assert_eq!(rejection.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn websocket_upgrade_accepts_the_cloudflare_host_with_standard_port() {
        let request = Request::builder()
            .uri("/")
            .header(HOST, "gateway.example:443")
            .body(())
            .expect("request");

        let accepted = WebSocketUpgradePolicy {
            expected_host: Some("gateway.example".into()),
        }
        .on_request(&request, Response::new(()));

        assert!(accepted.is_ok());
    }

    #[tokio::test]
    async fn websocket_binary_messages_use_unprefixed_json_frames() {
        let root = tempfile::tempdir().expect("temporary directory");
        let (server, grant) = GatewayServer::bootstrap(
            root.path().join("state"),
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        )
        .await
        .expect("bootstrap gateway");
        let listen = server.listen_addr();
        let (shutdown, signal) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(server.serve_until(async move {
            let _ = signal.await;
        }));
        let stream = TcpStream::connect(listen).await.expect("connect gateway");
        let (mut websocket, _) = tokio_tungstenite::client_async(format!("ws://{listen}/"), stream)
            .await
            .expect("upgrade WebSocket");
        let payload = serde_json::to_vec(&ClientFrame::new(ClientMessage::Pair {
            code: grant.code,
            client_label: "WebSocket test".into(),
            client_kind: ClientKind::Ios,
        }))
        .expect("encode pair frame");
        websocket
            .send(Message::Binary(payload.into()))
            .await
            .expect("send pair frame");
        let Message::Binary(payload) = websocket
            .next()
            .await
            .expect("pairing response")
            .expect("read pairing response")
        else {
            panic!("gateway response must be binary");
        };
        let frame = serde_json::from_slice::<ServerFrame>(&payload).expect("decode pairing frame");

        assert!(matches!(frame.message, ServerMessage::Paired { .. }));
        drop(websocket);
        shutdown.send(()).expect("stop gateway");
        serving.await.expect("gateway task").expect("gateway stop");
    }

    #[tokio::test(start_paused = true)]
    async fn websocket_upgrade_and_authentication_share_one_deadline() {
        let root = tempfile::tempdir().expect("temporary directory");
        let (server, _) = GatewayServer::bootstrap(
            root.path().join("state"),
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        )
        .await
        .expect("bootstrap gateway");
        let listen = server.listen_addr();
        let GatewayServer {
            listener,
            auth,
            host,
            cron,
            ..
        } = server;
        let client_connections = Arc::new(ClientConnections::default());
        let (client_revocations, _) = broadcast::channel(MAX_CONNECTIONS);
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept connection");
            let auth_deadline = Instant::now() + AUTH_TIMEOUT;
            accepted_tx.send(()).expect("report accepted connection");
            serve_plaintext_connection(
                stream,
                auth,
                host,
                cron,
                client_connections,
                client_revocations,
                PlaintextHandshake {
                    expected_websocket_host: None,
                    auth_deadline,
                },
            )
            .await
        });
        let mut stream = TcpStream::connect(listen).await.expect("connect gateway");
        accepted_rx.await.expect("connection accepted");

        tokio::time::advance(Duration::from_secs(5)).await;
        stream.write_all(b"G").await.expect("start upgrade");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        stream
            .write_all(
                format!(
                    "ET / HTTP/1.1\r\nHost: {listen}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .expect("finish upgrade");
        let mut response = Vec::new();
        while !response.ends_with(b"\r\n\r\n") {
            let mut byte = [0_u8; 1];
            let read = stream.read(&mut byte).await.expect("read upgrade response");
            assert_eq!(read, 1, "upgrade response ended early");
            response.push(byte[0]);
        }
        assert!(response.starts_with(b"HTTP/1.1 101"));
        let mut websocket = WebSocketStream::from_raw_socket(stream, Role::Client, None).await;

        tokio::time::advance(Duration::from_secs(6)).await;
        tokio::time::resume();
        let closed = tokio::time::timeout(Duration::from_secs(1), websocket.next())
            .await
            .expect("authentication deadline must close the socket");

        assert!(matches!(
            closed,
            None | Some(Ok(Message::Close(_))) | Some(Err(_))
        ));
        assert!(matches!(
            serving.await.expect("gateway task"),
            Err(Error::Unauthorized)
        ));
    }

    async fn wait_gateway_ready(events: &mut GatewayEvents) {
        loop {
            let frame = events
                .next()
                .await
                .expect("gateway frame")
                .expect("gateway open");
            if matches!(frame.message, ServerMessage::Ready { .. }) {
                return;
            }
        }
    }

    async fn create_chat(
        sender: &GatewaySender,
        events: &mut GatewayEvents,
        workspace: &Path,
    ) -> String {
        let request_id = Uuid::new_v4().to_string();
        sender
            .send(ClientMessage::CreateSession {
                request_id: request_id.clone(),
                workspace: workspace.into(),
            })
            .await
            .expect("create chat");
        loop {
            let frame = events
                .next()
                .await
                .expect("chat frame")
                .expect("gateway open");
            if let ServerMessage::SessionOpened {
                request_id: actual,
                payload,
            } = frame.message
                && actual == request_id
            {
                return payload.session.session_id;
            }
        }
    }

    async fn open_chat(sender: &GatewaySender, events: &mut GatewayEvents, session_id: &str) {
        let request_id = Uuid::new_v4().to_string();
        sender
            .send(ClientMessage::OpenSession {
                request_id: request_id.clone(),
                session_id: session_id.into(),
                last_sequence: None,
            })
            .await
            .expect("open chat");
        loop {
            let frame = events
                .next()
                .await
                .expect("chat frame")
                .expect("gateway open");
            if matches!(
                frame.message,
                ServerMessage::SessionOpened { request_id: actual, .. } if actual == request_id
            ) {
                return;
            }
        }
    }

    async fn wait_submission(events: &mut GatewayEvents, submission_id: &str) {
        loop {
            let frame = events
                .next()
                .await
                .expect("agent frame")
                .expect("gateway open");
            if matches!(
                frame.message,
                ServerMessage::AgentEvent { event, .. }
                    if event.submission_id.as_deref() == Some(submission_id)
            ) {
                return;
            }
        }
    }

    async fn wait_session_activity(
        events: &mut GatewayEvents,
        session_id: &str,
        state: SessionActivityState,
    ) -> SessionActivity {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(2), events.next())
                .await
                .expect("session activity timeout")
                .expect("gateway frame")
                .expect("gateway open");
            match frame.message {
                ServerMessage::AgentEvent {
                    session_id: actual, ..
                } if actual == session_id => {
                    panic!("a nonselected chat event crossed the gateway-wide stream")
                }
                ServerMessage::Sessions { sessions, .. } => {
                    if let Some(activity) = sessions
                        .into_iter()
                        .find(|session| session.summary.session_id == session_id)
                        .map(|session| session.activity)
                        .filter(|activity| activity.state == state)
                    {
                        return activity;
                    }
                }
                _ => {}
            }
        }
    }

    async fn drain_ready_replay(events: &mut GatewayEvents) {
        while matches!(
            tokio::time::timeout(Duration::from_millis(10), events.next()).await,
            Ok(Ok(Some(_)))
        ) {}
    }

    fn run_git(workspace: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .env("LC_ALL", "C")
            .current_dir(workspace)
            .output()
            .expect("run Git");
        assert!(
            output.status.success(),
            "Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn bootstrap_owns_the_listener_before_creating_state() {
        let root = tempfile::tempdir().expect("temporary directory");
        let occupied = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("occupied listener");
        let listen = occupied.local_addr().expect("listen address");
        let state = root.path().join("state");

        let result = GatewayServer::bootstrap(state.clone(), listen).await;

        assert!(matches!(result, Err(Error::Io(_))));
        assert!(!state.exists());
    }

    #[tokio::test]
    async fn connected_client_pauses_and_resets_inactivity_shutdown() {
        let root = tempfile::tempdir().expect("temporary directory");
        let (server, grant) = GatewayServer::bootstrap(
            root.path().join("state"),
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        )
        .await
        .expect("bootstrap gateway");
        let listen = server.config.listen;
        let serving = tokio::spawn(
            server.serve_until_inactive(std::future::pending(), Duration::from_millis(200)),
        );
        let endpoint = format!("tcp://{listen}")
            .parse::<Endpoint>()
            .expect("endpoint");
        let (connection, _) =
            GatewayClient::pair(&endpoint, grant.code, "inactivity test", ClientKind::Cli)
                .await
                .expect("connect client");

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!serving.is_finished());
        drop(connection);
        tokio::time::sleep(Duration::from_millis(75)).await;
        assert!(!serving.is_finished());

        tokio::time::timeout(Duration::from_secs(2), serving)
            .await
            .expect("inactivity shutdown timeout")
            .expect("gateway task")
            .expect("gateway shutdown");
    }

    #[tokio::test]
    async fn unpairing_disconnects_the_client_and_rejects_its_token() {
        let root = tempfile::tempdir().expect("temporary directory");
        let (server, grant) = GatewayServer::bootstrap(
            root.path().join("state"),
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        )
        .await
        .expect("bootstrap gateway");
        let listen = server.config.listen;
        let auth = Arc::clone(&server.auth);
        let (shutdown, signal) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(server.serve_until(async move {
            let _ = signal.await;
        }));
        let endpoint = format!("tcp://{listen}")
            .parse::<Endpoint>()
            .expect("endpoint");
        let (dashboard, dashboard_identity) = GatewayClient::pair(
            &endpoint,
            grant.code,
            "dashboard",
            ClientKind::GatewayDashboard,
        )
        .await
        .expect("pair dashboard");
        let (dashboard_sender, mut dashboard_events) = dashboard.into_parts();
        wait_gateway_ready(&mut dashboard_events).await;
        let device_grant = auth.create_pairing_code().expect("device pairing code");
        let (device, device_identity) =
            GatewayClient::pair(&endpoint, device_grant.code, "iPhone", ClientKind::Ios)
                .await
                .expect("pair device");
        let (_device_sender, mut device_events) = device.into_parts();
        wait_gateway_ready(&mut device_events).await;

        let request_id = Uuid::new_v4().to_string();
        dashboard_sender
            .send(ClientMessage::UnpairClient {
                request_id: request_id.clone(),
                client_id: device_identity.client_id.clone(),
            })
            .await
            .expect("unpair device");
        let (current_client_id, clients) = loop {
            let frame = dashboard_events
                .next()
                .await
                .expect("inventory frame")
                .expect("dashboard open");
            if let ServerMessage::Clients {
                request_id: actual,
                current_client_id,
                clients,
            } = frame.message
                && actual == request_id
            {
                break (current_client_id, clients);
            }
        };
        while tokio::time::timeout(Duration::from_secs(2), device_events.next())
            .await
            .expect("device disconnect timeout")
            .expect("device frame")
            .is_some()
        {}
        let reconnect =
            GatewayClient::connect(&endpoint, device_identity.token, ClientKind::Ios).await;

        assert_eq!(
            (
                current_client_id == dashboard_identity.client_id,
                clients
                    .iter()
                    .all(|client| client.client_id != device_identity.client_id),
                matches!(reconnect, Err(Error::Unauthorized)),
            ),
            (true, true, true)
        );
        shutdown.send(()).expect("send shutdown");
        serving
            .await
            .expect("gateway task")
            .expect("gateway shutdown");
    }

    #[tokio::test(start_paused = true)]
    async fn scheduled_task_disables_inactivity_shutdown() {
        let root = tempfile::tempdir().expect("temporary directory");
        let (server, _) = GatewayServer::bootstrap(
            root.path().join("state"),
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        )
        .await
        .expect("bootstrap gateway");
        let cron = Arc::clone(&server.cron);
        let (shutdown, signal) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(server.serve_until_inactive(
            async move {
                let _ = signal.await;
            },
            Duration::from_millis(50),
        ));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(25)).await;
        cron.add_for_test("source-chat", "do work", "0 9 * * *")
            .expect("schedule task");

        tokio::time::advance(Duration::from_millis(75)).await;
        assert!(
            !serving.is_finished(),
            "scheduled task must keep gateway alive"
        );
        shutdown.send(()).expect("send shutdown");
        serving
            .await
            .expect("gateway task")
            .expect("gateway shutdown");
    }

    #[tokio::test]
    async fn frontends_select_independent_chats_and_can_share_one_chat() {
        let root = tempfile::tempdir().expect("temporary directory");
        let first_workspace = root.path().join("first");
        let second_workspace = root.path().join("second");
        fs::create_dir(&first_workspace).expect("first workspace");
        fs::create_dir(&second_workspace).expect("second workspace");
        let (server, grant) = GatewayServer::bootstrap(
            root.path().join("state"),
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        )
        .await
        .expect("bootstrap gateway");
        let listen = server.config.listen;
        let (shutdown, signal) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(server.serve_until(async move {
            let _ = signal.await;
        }));
        let endpoint = format!("tcp://{listen}")
            .parse::<Endpoint>()
            .expect("endpoint");
        let (first, paired) = GatewayClient::pair(&endpoint, grant.code, "first", ClientKind::Cli)
            .await
            .expect("pair first frontend");
        let second = GatewayClient::connect(&endpoint, paired.token, ClientKind::Macos)
            .await
            .expect("connect second frontend");
        let (first_sender, mut first_events) = first.into_parts();
        let (second_sender, mut second_events) = second.into_parts();
        wait_gateway_ready(&mut first_events).await;
        wait_gateway_ready(&mut second_events).await;
        let clients_request = Uuid::new_v4().to_string();
        first_sender
            .send(ClientMessage::ListClients {
                request_id: clients_request.clone(),
            })
            .await
            .expect("list clients");
        let clients = loop {
            let frame = first_events
                .next()
                .await
                .expect("client-inventory frame")
                .expect("gateway open");
            if let ServerMessage::Clients {
                request_id,
                current_client_id: _,
                clients,
            } = frame.message
                && request_id == clients_request
            {
                break clients;
            }
        };
        assert_eq!(
            (clients[0].kinds.as_slice(), clients[0].connections),
            ([ClientKind::Cli, ClientKind::Macos].as_slice(), 2)
        );
        let first_session = create_chat(&first_sender, &mut first_events, &first_workspace).await;
        let second_session =
            create_chat(&second_sender, &mut second_events, &second_workspace).await;
        assert_ne!(first_session, second_session);
        drain_ready_replay(&mut first_events).await;
        drain_ready_replay(&mut second_events).await;

        let first_submission = Uuid::new_v4().to_string();
        first_sender
            .send(ClientMessage::Submit {
                session_id: first_session.clone(),
                submission: Submission {
                    id: first_submission.clone(),
                    op: Op::UserInput {
                        text: "hello".into(),
                    },
                },
            })
            .await
            .expect("submit first chat");
        wait_submission(&mut first_events, &first_submission).await;
        let running = wait_session_activity(
            &mut second_events,
            &first_session,
            SessionActivityState::Running,
        )
        .await;
        let finished = wait_session_activity(
            &mut second_events,
            &first_session,
            SessionActivityState::Idle,
        )
        .await;
        assert!(running.turn_id.is_some());
        assert!(finished.last_outcome.is_some());

        open_chat(&second_sender, &mut second_events, &first_session).await;
        drain_ready_replay(&mut second_events).await;
        let shared_submission = Uuid::new_v4().to_string();
        first_sender
            .send(ClientMessage::Submit {
                session_id: first_session,
                submission: Submission {
                    id: shared_submission.clone(),
                    op: Op::UserInput {
                        text: "shared".into(),
                    },
                },
            })
            .await
            .expect("submit shared chat");
        wait_submission(&mut first_events, &shared_submission).await;
        wait_submission(&mut second_events, &shared_submission).await;

        shutdown.send(()).expect("stop gateway");
        serving.await.expect("gateway task").expect("gateway stop");
    }

    #[tokio::test]
    async fn branch_switch_is_acknowledged_and_broadcasts_fresh_status() {
        let root = tempfile::tempdir().expect("temporary directory");
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).expect("workspace");
        run_git(&workspace, &["init", "--quiet", "--initial-branch", "main"]);
        run_git(
            &workspace,
            &["config", "user.email", "horus@example.invalid"],
        );
        run_git(&workspace, &["config", "user.name", "Horus Test"]);
        fs::write(workspace.join("tracked.txt"), b"main").expect("tracked file");
        run_git(&workspace, &["add", "--", "tracked.txt"]);
        run_git(&workspace, &["commit", "--quiet", "-m", "initial"]);
        run_git(&workspace, &["branch", "feature"]);
        let (server, grant) = GatewayServer::bootstrap(
            root.path().join("state"),
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        )
        .await
        .expect("bootstrap gateway");
        let listen = server.config.listen;
        let (shutdown, signal) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(server.serve_until(async move {
            let _ = signal.await;
        }));
        let endpoint = format!("tcp://{listen}")
            .parse::<Endpoint>()
            .expect("endpoint");
        let (connection, _) =
            GatewayClient::pair(&endpoint, grant.code, "branch test", ClientKind::Cli)
                .await
                .expect("pair frontend");
        let (sender, mut events) = connection.into_parts();
        wait_gateway_ready(&mut events).await;
        let session_id = create_chat(&sender, &mut events, &workspace).await;
        drain_ready_replay(&mut events).await;
        let request_id = Uuid::new_v4().to_string();

        sender
            .send(ClientMessage::SwitchGitBranch {
                request_id: request_id.clone(),
                session_id,
                branch: "feature".into(),
            })
            .await
            .expect("switch branch");
        let mut accepted = false;
        let mut changed = false;
        while !accepted || !changed {
            let frame = tokio::time::timeout(Duration::from_secs(5), events.next())
                .await
                .expect("branch response timeout")
                .expect("gateway frame")
                .expect("gateway open");
            match frame.message {
                ServerMessage::Accepted { request_id: actual } if actual == request_id => {
                    accepted = true;
                }
                ServerMessage::SessionChanged { payload } => {
                    changed = payload.git.is_some_and(|git| {
                        git.current_branch == "feature"
                            && git.branches == ["feature".to_string(), "main".to_string()]
                    });
                }
                _ => {}
            }
        }

        shutdown.send(()).expect("stop gateway");
        serving.await.expect("gateway task").expect("gateway stop");
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
        let (mut writer, reader) = tokio::io::duplex(1024);
        let mut reader = FrameReader::new(reader);
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
