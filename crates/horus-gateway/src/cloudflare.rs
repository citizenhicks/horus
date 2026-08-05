//! Lifecycle for one pre-provisioned Cloudflare Tunnel connector.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};
use tokio_tungstenite::connect_async;

use crate::config::{ConfigStore, GatewayConfig, load_cloudflare_token};
use crate::{Error, Result};

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const PROBE_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) struct CloudflareTunnel {
    child: Child,
}

impl CloudflareTunnel {
    pub(crate) fn start(store: &ConfigStore, config: &GatewayConfig) -> Result<Option<Self>> {
        if config.cloudflare.is_none() {
            return Ok(None);
        }
        let token_path = store.cloudflare_token_path();
        load_cloudflare_token(&token_path)?;
        let mut command = tunnel_command(cloudflared_executable(), &token_path);
        command.kill_on_drop(true);
        Ok(Some(Self {
            child: command.spawn().map_err(|error| {
                Error::Config(format!(
                    "failed to start bundled cloudflared; install it beside horus-gateway or on PATH: {error}"
                ))
            })?,
        }))
    }

    pub(crate) async fn wait(&mut self) -> Result<()> {
        let status = self.child.wait().await?;
        Err(Error::Config(format!(
            "cloudflared stopped unexpectedly with {status}"
        )))
    }

    pub(crate) async fn wait_ready(&mut self, endpoint: &str) -> Result<()> {
        timeout(READY_TIMEOUT, async {
            loop {
                if let Some(status) = self.child.try_wait()? {
                    return Err(Error::Config(format!(
                        "cloudflared stopped during startup with {status}"
                    )));
                }
                if timeout(PROBE_TIMEOUT, websocket_ready(endpoint))
                    .await
                    .unwrap_or(false)
                {
                    return Ok(());
                }
                sleep(PROBE_INTERVAL).await;
            }
        })
        .await
        .map_err(|_| {
            Error::Config(format!(
                "Cloudflare Tunnel did not expose {endpoint} within {} seconds; verify its hostname route and connector token",
                READY_TIMEOUT.as_secs()
            ))
        })?
    }
}

async fn websocket_ready(endpoint: &str) -> bool {
    let Ok((mut websocket, _)) = connect_async(endpoint).await else {
        return false;
    };
    let _ = websocket.close(None).await;
    true
}

fn tunnel_command(program: impl Into<OsString>, token_path: &Path) -> Command {
    let mut command = Command::new(program.into());
    command
        .arg("tunnel")
        .arg("--no-autoupdate")
        .arg("run")
        .arg("--token-file")
        .arg(token_path)
        .env_remove("TUNNEL_TOKEN")
        .env_remove("TUNNEL_TOKEN_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn cloudflared_executable() -> OsString {
    let name = if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    };
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join(name)))
        .filter(|path| path.is_file())
        .map_or_else(|| OsString::from(name), PathBuf::into_os_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn websocket_probe_requires_a_completed_upgrade() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("WebSocket listener");
        let endpoint = format!("ws://{}", listener.local_addr().expect("listener address"));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("probe connection");
            tokio_tungstenite::accept_async(stream)
                .await
                .expect("WebSocket upgrade");
        });

        assert!(websocket_ready(&endpoint).await);
        server.await.expect("probe server");
    }

    #[test]
    fn tunnel_command_passes_only_the_token_file_path() {
        let command = tunnel_command("cloudflared", Path::new("/private/cloudflare-token"));
        let arguments = command
            .as_std()
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            arguments,
            [
                "tunnel",
                "--no-autoupdate",
                "run",
                "--token-file",
                "/private/cloudflare-token"
            ]
        );
        assert_eq!(
            command
                .as_std()
                .get_envs()
                .filter(|(name, _)| {
                    *name == std::ffi::OsStr::new("TUNNEL_TOKEN")
                        || *name == std::ffi::OsStr::new("TUNNEL_TOKEN_FILE")
                })
                .count(),
            2
        );
    }
}
