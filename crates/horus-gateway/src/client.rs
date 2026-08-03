//! Reusable async client for CLI and native frontends.

use std::collections::VecDeque;
use std::env;
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use rustls::ClientConfig;
use rustls::RootCertStore;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_rustls::TlsConnector;

use crate::wire::{
    ClientFrame, ClientMessage, FrameReader, ServerFrame, ServerMessage, read_frame,
    validate_version, write_frame,
};
use crate::{Error, Result};

const DEFAULT_ENDPOINT: &str = "tcp://127.0.0.1:8741";
/// Maximum number of frames a focused client flow may temporarily defer.
pub const MAX_PENDING_FRAMES: usize = 1024;

trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> Transport for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

type BoxedTransport = Box<dyn Transport>;

/// Validated plaintext-loopback or authenticated-root TLS endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    security: Security,
    host: String,
    port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Security {
    Plaintext,
    Tls,
}

/// Token returned while pairing a new client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedClient {
    pub client_id: String,
    pub token: String,
}

/// Connected client before its command and event halves are separated.
pub struct GatewayClient {
    sender: GatewaySender,
    events: GatewayEvents,
}

/// Cloneable framed command writer.
#[derive(Clone)]
pub struct GatewaySender {
    writer: Arc<Mutex<WriteHalf<BoxedTransport>>>,
}

/// Single-owner framed event reader.
pub struct GatewayEvents {
    reader: FrameReader<ReadHalf<BoxedTransport>>,
    pending: VecDeque<ServerFrame>,
}

impl Endpoint {
    /// Resolves `HORUS_GATEWAY_ENDPOINT`, defaulting to local plaintext.
    pub fn from_env() -> Result<Self> {
        env::var("HORUS_GATEWAY_ENDPOINT")
            .unwrap_or_else(|_| DEFAULT_ENDPOINT.into())
            .parse()
    }

    /// Returns whether this endpoint uses loopback-only plaintext transport.
    #[must_use]
    pub const fn is_plaintext(&self) -> bool {
        matches!(self.security, Security::Plaintext)
    }

    async fn connect(&self) -> Result<BoxedTransport> {
        let address = format_address(&self.host, self.port);
        let stream = TcpStream::connect(&address).await?;
        if self.security == Security::Plaintext {
            let peer = stream.peer_addr()?;
            if !peer.ip().is_loopback() {
                return Err(Error::Config(
                    "plaintext gateway connections are restricted to loopback".into(),
                ));
            }
            return Ok(Box::new(stream));
        }

        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = ServerName::try_from(self.host.clone())
            .map_err(|_| Error::Config("TLS endpoint has an invalid server name".into()))?;
        let stream = TlsConnector::from(Arc::new(config))
            .connect(name, stream)
            .await
            .map_err(|error| Error::Protocol(format!("TLS handshake failed: {error}")))?;
        Ok(Box::new(stream))
    }
}

impl FromStr for Endpoint {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let (security, authority) = if let Some(authority) = value.strip_prefix("tcp://") {
            (Security::Plaintext, authority)
        } else if let Some(authority) = value.strip_prefix("tls://") {
            (Security::Tls, authority)
        } else {
            return Err(Error::Config(
                "gateway endpoint must use tcp:// or tls://".into(),
            ));
        };
        if authority.contains(['/', '?', '#', '@']) {
            return Err(Error::Config(
                "gateway endpoint must contain only a host and port".into(),
            ));
        }
        let (host, port) = authority
            .rsplit_once(':')
            .ok_or_else(|| Error::Config("gateway endpoint requires a port".into()))?;
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        if host.is_empty() {
            return Err(Error::Config("gateway endpoint requires a host".into()));
        }
        let port = port
            .parse::<u16>()
            .map_err(|_| Error::Config("gateway endpoint has an invalid port".into()))?;
        if port == 0 {
            return Err(Error::Config(
                "gateway endpoint port must be greater than zero".into(),
            ));
        }
        if security == Security::Plaintext && !plaintext_host_is_loopback(host) {
            return Err(Error::Config(
                "tcp:// endpoints are restricted to loopback; use tls:// remotely".into(),
            ));
        }
        Ok(Self {
            security,
            host: host.into(),
            port,
        })
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = match self.security {
            Security::Plaintext => "tcp",
            Security::Tls => "tls",
        };
        write!(
            formatter,
            "{scheme}://{}",
            format_address(&self.host, self.port)
        )
    }
}

impl GatewayClient {
    /// Authenticates an existing client and leaves the gateway Ready frame for `events`.
    pub async fn connect(endpoint: &Endpoint, token: impl Into<String>) -> Result<Self> {
        let transport = endpoint.connect().await?;
        let (reader, writer) = tokio::io::split(transport);
        let client = Self::from_parts(reader, writer);
        client
            .sender
            .write(ClientMessage::Authenticate {
                token: token.into(),
            })
            .await?;
        client.expect_authenticated().await
    }

    /// Consumes a pending pairing code and returns a connected independent client.
    pub async fn pair(
        endpoint: &Endpoint,
        code: impl Into<String>,
        client_label: impl Into<String>,
    ) -> Result<(Self, PairedClient)> {
        let transport = endpoint.connect().await?;
        let (reader, writer) = tokio::io::split(transport);
        let mut client = Self::from_parts(reader, writer);
        client
            .sender
            .write(ClientMessage::Pair {
                code: code.into(),
                client_label: client_label.into(),
            })
            .await?;
        let frame = client
            .events
            .next()
            .await?
            .ok_or_else(|| Error::Protocol("gateway closed during pairing".into()))?;
        let paired = match frame.message {
            ServerMessage::Paired { client_id, token } => PairedClient { client_id, token },
            ServerMessage::Error { code, message, .. } => {
                return Err(connection_error(&code, message));
            }
            _ => {
                return Err(Error::Protocol(
                    "gateway did not return a paired response".into(),
                ));
            }
        };
        client = client.expect_authenticated().await?;
        Ok((client, paired))
    }

    /// Separates the clonable command writer from the single event reader.
    #[must_use]
    pub fn into_parts(self) -> (GatewaySender, GatewayEvents) {
        (self.sender, self.events)
    }

    fn from_parts(reader: ReadHalf<BoxedTransport>, writer: WriteHalf<BoxedTransport>) -> Self {
        Self {
            sender: GatewaySender {
                writer: Arc::new(Mutex::new(writer)),
            },
            events: GatewayEvents {
                reader: FrameReader::new(reader),
                pending: VecDeque::new(),
            },
        }
    }

    async fn expect_authenticated(mut self) -> Result<Self> {
        let frame = self
            .events
            .next()
            .await?
            .ok_or_else(|| Error::Protocol("gateway closed during authentication".into()))?;
        match frame.message {
            ServerMessage::Authenticated => Ok(self),
            ServerMessage::Error { code, message, .. } => Err(connection_error(&code, message)),
            _ => Err(Error::Protocol(
                "gateway did not acknowledge authentication".into(),
            )),
        }
    }
}

fn connection_error(code: &str, message: String) -> Error {
    if code == "unauthorized" {
        Error::Unauthorized
    } else {
        Error::Protocol(message)
    }
}

impl GatewaySender {
    /// Sends one authenticated operation.
    pub async fn send(&self, message: ClientMessage) -> Result<()> {
        if matches!(
            message,
            ClientMessage::Pair { .. } | ClientMessage::Authenticate { .. }
        ) {
            return Err(Error::Protocol(
                "authentication messages are valid only during connection setup".into(),
            ));
        }
        self.write(message).await
    }

    async fn write(&self, message: ClientMessage) -> Result<()> {
        let mut writer = self.writer.lock().await;
        write_frame(&mut *writer, &ClientFrame::new(message)).await
    }
}

impl GatewayEvents {
    /// Receives the next version-checked server frame.
    pub async fn next(&mut self) -> Result<Option<ServerFrame>> {
        if let Some(frame) = self.pending.pop_front() {
            return Ok(Some(frame));
        }
        let Some(frame) = read_frame::<ServerFrame>(&mut self.reader).await? else {
            return Ok(None);
        };
        validate_version(frame.version)?;
        Ok(Some(frame))
    }

    /// Restores temporarily consumed frames ahead of unread transport data.
    pub fn prepend(&mut self, frames: Vec<ServerFrame>) -> Result<()> {
        if self.pending.len() + frames.len() > MAX_PENDING_FRAMES {
            return Err(Error::Protocol(format!(
                "gateway event backlog exceeds {MAX_PENDING_FRAMES} frames"
            )));
        }
        for frame in &frames {
            validate_version(frame.version)?;
        }
        for frame in frames.into_iter().rev() {
            self.pending.push_front(frame);
        }
        Ok(())
    }
}

/// Resolves the bearer token expected by the reusable CLI client.
pub fn token_from_env() -> Result<String> {
    env::var("HORUS_GATEWAY_TOKEN")
        .ok()
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| Error::Config("set HORUS_GATEWAY_TOKEN before connecting".into()))
}

fn plaintext_host_is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn format_address(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_authenticates_without_a_session_cursor() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind gateway");
        let endpoint = format!("tcp://{}", listener.local_addr().expect("gateway address"))
            .parse::<Endpoint>()
            .expect("gateway endpoint");
        let gateway = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept client");
            let (reader, mut writer) = tokio::io::split(stream);
            let mut reader = FrameReader::new(reader);
            let frame = read_frame::<ClientFrame>(&mut reader)
                .await
                .expect("read authentication")
                .expect("authentication frame");
            write_frame(&mut writer, &ServerFrame::new(ServerMessage::Authenticated))
                .await
                .expect("acknowledge authentication");
            frame
        });

        let _client = GatewayClient::connect(&endpoint, "secret")
            .await
            .expect("connect client");
        let frame = gateway.await.expect("gateway task");

        assert_eq!(
            frame.message,
            ClientMessage::Authenticate {
                token: "secret".into()
            }
        );
    }

    #[test]
    fn endpoint_rejects_remote_plaintext() {
        let error = "tcp://example.com:8741"
            .parse::<Endpoint>()
            .expect_err("remote plaintext must fail");

        assert!(error.to_string().contains("use tls://"));
        assert!("tcp://127.0.0.1:0".parse::<Endpoint>().is_err());
    }

    #[test]
    fn endpoint_accepts_loopback_plaintext_and_remote_tls() {
        let loopback = "tcp://127.0.0.1:8741"
            .parse::<Endpoint>()
            .expect("loopback endpoint");
        let remote = "tls://gateway.example:443"
            .parse::<Endpoint>()
            .expect("TLS endpoint");

        assert_eq!(loopback.to_string(), "tcp://127.0.0.1:8741");
        assert_eq!(remote.to_string(), "tls://gateway.example:443");
        assert!(loopback.is_plaintext());
        assert!(!remote.is_plaintext());
    }

    #[test]
    fn authentication_errors_preserve_unauthorized_semantics() {
        assert!(matches!(
            connection_error("unauthorized", "authentication failed".into()),
            Error::Unauthorized
        ));
    }

    #[tokio::test]
    async fn prepended_frames_are_returned_in_order() {
        let (transport, _peer) = tokio::io::duplex(64);
        let (reader, _writer) = tokio::io::split(Box::new(transport) as BoxedTransport);
        let mut events = GatewayEvents {
            reader: FrameReader::new(reader),
            pending: VecDeque::new(),
        };
        events
            .prepend(vec![
                ServerFrame::new(ServerMessage::Accepted {
                    request_id: "first".into(),
                }),
                ServerFrame::new(ServerMessage::Accepted {
                    request_id: "second".into(),
                }),
            ])
            .expect("defer frames");

        for expected in ["first", "second"] {
            let frame = events.next().await.expect("next frame").expect("frame");
            assert!(matches!(
                frame.message,
                ServerMessage::Accepted { request_id } if request_id == expected
            ));
        }
        let mut invalid = ServerFrame::new(ServerMessage::Accepted {
            request_id: "invalid".into(),
        });
        invalid.version = 0;
        assert!(events.prepend(vec![invalid]).is_err());
        let frame = ServerFrame::new(ServerMessage::Accepted {
            request_id: "overflow".into(),
        });
        assert!(events.prepend(vec![frame; MAX_PENDING_FRAMES + 1]).is_err());
    }
}
