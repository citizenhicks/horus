//! Remote Streamable HTTP MCP connections declared by Agent Plugins.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use http::{HeaderName, HeaderValue};
use mobius::backend::model::ToolDefinition;
use mobius::middleware::tools::{ApprovalRequirement, ExecutionMode, Tool, ToolContext};
use rmcp::model::{CallToolRequestParams, Tool as McpTool};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::auth::{
    AuthClient, AuthError, AuthorizationManager, AuthorizationRequest, AuthorizationSession,
    CredentialStore, StateStore, StoredAuthorizationState, StoredCredentials,
};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpClientTransportConfig,
};
use rmcp::{RoleClient, ServiceExt as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tokio::sync::Mutex;
use url::Url;

use crate::extensions::ResolvedMcpServer;
use crate::wire::{ExtensionConnectionKind, ExtensionConnectionRecord, ExtensionConnectionState};
use crate::{Error, Result};

const REDIRECT_URI: &str = "mobius://extension-auth";
const MAX_AUTH_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SECRET_BYTES: usize = 64 * 1024;
const MAX_PENDING_STATES: usize = 16;
const STATE_TTL: Duration = Duration::from_secs(10 * 60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const TOOL_TIMEOUT: Duration = Duration::from_secs(120);
static AUTH_FILE_LOCK: LazyLock<Arc<StdMutex<()>>> = LazyLock::new(|| Arc::new(StdMutex::new(())));

pub(crate) struct RemoteMcp {
    state_dir: PathBuf,
    pending: Mutex<BTreeMap<String, AuthorizationSession>>,
}

impl RemoteMcp {
    pub(crate) fn new(state_dir: &Path) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
            pending: Mutex::new(BTreeMap::new()),
        }
    }

    pub(crate) async fn start_oauth(
        &self,
        extension_id: &str,
        server: &crate::extensions::InstalledMcpServer,
        redirect_uri: &str,
    ) -> Result<String> {
        if redirect_uri != REDIRECT_URI {
            return Err(Error::Config(format!(
                "extension OAuth redirect must be `{REDIRECT_URI}`"
            )));
        }
        validate_remote_url(&server.url, "MCP server")?;
        let store = self.store(extension_id, server)?;
        let mut manager = AuthorizationManager::new(&server.url)
            .await
            .map_err(auth_error)?;
        manager.set_credential_store(store.clone());
        manager.set_state_store(store);
        let resolution = manager.resolve_metadata().await.map_err(auth_error)?;
        manager.set_metadata(resolution.metadata);
        let request = AuthorizationRequest::new(REDIRECT_URI).with_client_name("Mobius");
        let mut pending = self.pending.lock().await;
        let session = AuthorizationSession::new(manager, request)
            .await
            .map_err(|(_, error)| auth_error(error))?;
        let authorization_url = session.get_authorization_url().to_owned();
        validate_remote_url(&authorization_url, "OAuth authorization")?;
        pending.insert(extension_id.to_owned(), session);
        Ok(authorization_url)
    }

    pub(crate) async fn finish_oauth(&self, extension_id: &str, callback_url: &str) -> Result<()> {
        validate_callback(callback_url)?;
        let mut pending = self.pending.lock().await;
        let session = pending
            .remove(extension_id)
            .ok_or_else(|| Error::Config("extension OAuth setup is not pending".into()))?;
        session
            .handle_callback_url(callback_url)
            .await
            .map_err(auth_error)?;
        Ok(())
    }

    pub(crate) fn set_secret(
        &self,
        extension_id: &str,
        server: &crate::extensions::InstalledMcpServer,
        secret: &str,
    ) -> Result<()> {
        if secret.is_empty() || secret.len() > MAX_SECRET_BYTES {
            return Err(Error::Config("extension API key is invalid".into()));
        }
        HeaderValue::from_str(secret).map_err(|_| {
            Error::Config("extension API key is not a valid HTTP header value".into())
        })?;
        let store = self.store(extension_id, server)?;
        store.update(|file| file.secret = Some(secret.to_owned()))
    }

    pub(crate) async fn disconnect(&self, extension_id: &str) -> Result<()> {
        self.pending.lock().await.remove(extension_id);
        AuthFileStore::unbound(auth_path(&self.state_dir, extension_id), auth_file_lock())
            .clear_file()
    }

    fn store(
        &self,
        extension_id: &str,
        server: &crate::extensions::InstalledMcpServer,
    ) -> Result<AuthFileStore> {
        AuthFileStore::for_server(
            auth_path(&self.state_dir, extension_id),
            auth_file_lock(),
            server,
        )
    }
}

pub(crate) fn connection_record(
    state_dir: &Path,
    extension_id: &str,
    server: &crate::extensions::InstalledMcpServer,
) -> ExtensionConnectionRecord {
    let connection = server
        .connection
        .as_ref()
        .expect("connection records require connection metadata");
    let store = AuthFileStore::for_active_server(
        auth_path(state_dir, extension_id),
        auth_file_lock(),
        server,
    );
    let (state, message) = match store.and_then(|store| store.read()) {
        Ok(file) => {
            let connected = match connection.kind {
                ExtensionConnectionKind::OAuth => file
                    .credentials
                    .as_ref()
                    .is_some_and(|credentials| credentials.token_response.is_some()),
                ExtensionConnectionKind::ApiKey => file.secret.is_some(),
            };
            (
                if connected {
                    ExtensionConnectionState::Connected
                } else {
                    ExtensionConnectionState::Disconnected
                },
                None,
            )
        }
        Err(_) => (
            ExtensionConnectionState::NeedsAttention,
            Some("Saved connection data is unavailable. Disconnect and reconnect.".into()),
        ),
    };
    ExtensionConnectionRecord {
        kind: connection.kind,
        state,
        label: connection.label.clone(),
        message,
    }
}

pub(crate) async fn tools_for(
    servers: &[ResolvedMcpServer],
    state_dir: &Path,
) -> Vec<Arc<dyn Tool>> {
    let mut tools = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for server in servers {
        if let Ok(mut server_tools) = tools_for_server(server, state_dir).await {
            server_tools.retain(|tool| names.insert(tool.definition().name));
            tools.append(&mut server_tools);
        }
    }
    tools
}

async fn tools_for_server(
    server: &ResolvedMcpServer,
    state_dir: &Path,
) -> Result<Vec<Arc<dyn Tool>>> {
    validate_remote_url(&server.config.url, "MCP server")?;
    let mut headers = headers(&server.config.headers)?;
    let config = StreamableHttpClientTransportConfig::with_uri(server.config.url.clone());
    let service = match server.config.connection.as_ref() {
        Some(connection) if connection.kind == ExtensionConnectionKind::ApiKey => {
            let store = AuthFileStore::for_active_server(
                auth_path(state_dir, &server.extension_id),
                auth_file_lock(),
                &server.config,
            )?;
            let header = connection
                .secret_header
                .as_deref()
                .ok_or_else(|| Error::Config("extension API key header is missing".into()))?;
            let secret = store
                .read()?
                .secret
                .ok_or_else(|| Error::Config("extension API key is not configured".into()))?;
            headers.insert(parse_header_name(header)?, parse_header_value(&secret)?);
            connect(http_client()?, config.custom_headers(headers)).await?
        }
        Some(connection) if connection.kind == ExtensionConnectionKind::OAuth => {
            let store = AuthFileStore::for_active_server(
                auth_path(state_dir, &server.extension_id),
                auth_file_lock(),
                &server.config,
            )?;
            let mut manager = AuthorizationManager::new(&server.config.url)
                .await
                .map_err(auth_error)?;
            manager.set_credential_store(store.clone());
            manager.set_state_store(store);
            if !manager.initialize_from_store().await.map_err(auth_error)? {
                return Err(Error::Config("extension OAuth is not configured".into()));
            }
            let client = AuthClient::new(http_client()?, manager);
            connect(client, config.custom_headers(headers)).await?
        }
        Some(_) => {
            return Err(Error::Config(
                "unsupported extension connection type".into(),
            ));
        }
        None => connect(http_client()?, config.custom_headers(headers)).await?,
    };
    let service = Arc::new(service);
    let remote_tools = tokio::time::timeout(CONNECT_TIMEOUT, service.list_all_tools())
        .await
        .map_err(|_| Error::Config("remote MCP tool discovery timed out".into()))?
        .map_err(|error| Error::Config(format!("remote MCP tool discovery failed: {error}")))?;
    Ok(remote_tools
        .into_iter()
        .map(|tool| {
            Arc::new(RemoteTool::new(
                &server.plugin_name,
                &server.config.name,
                tool,
                Arc::clone(&service),
            )) as Arc<dyn Tool>
        })
        .collect())
}

async fn connect<C>(
    client: C,
    config: StreamableHttpClientTransportConfig,
) -> Result<RunningService<RoleClient, ()>>
where
    C: StreamableHttpClient + Send + Sync + 'static,
{
    let transport = StreamableHttpClientTransport::with_client(client, config);
    tokio::time::timeout(CONNECT_TIMEOUT, ().serve(transport))
        .await
        .map_err(|_| Error::Config("remote MCP connection timed out".into()))?
        .map_err(|error| Error::Config(format!("remote MCP connection failed: {error}")))
}

struct RemoteTool {
    definition: ToolDefinition,
    remote_name: String,
    approval: ApprovalRequirement,
    service: Arc<RunningService<RoleClient, ()>>,
}

impl RemoteTool {
    fn new(
        plugin_name: &str,
        server_name: &str,
        tool: McpTool,
        service: Arc<RunningService<RoleClient, ()>>,
    ) -> Self {
        let approval = remote_tool_approval(tool.annotations.as_ref());
        let remote_name = tool.name.into_owned();
        Self {
            definition: ToolDefinition {
                name: format!(
                    "mcp__{}__{}__{}",
                    identifier(plugin_name),
                    identifier(server_name),
                    identifier(&remote_name)
                ),
                description: tool.description.map_or_else(
                    || format!("Remote MCP tool `{remote_name}`"),
                    |value| value.into_owned(),
                ),
                parameters: Value::Object((*tool.input_schema).clone()),
            },
            remote_name,
            approval,
            service,
        }
    }
}

fn remote_tool_approval(
    _annotations: Option<&rmcp::model::ToolAnnotations>,
) -> ApprovalRequirement {
    // MCP annotations are untrusted hints, not authority to bypass host approval.
    ApprovalRequirement::Always
}

impl Tool for RemoteTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }

    fn approval(&self) -> ApprovalRequirement {
        self.approval
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> mobius::BoxFuture<'a, mobius::Result<String>> {
        Box::pin(async move {
            let arguments = arguments.as_object().cloned().ok_or_else(|| {
                mobius::Error::Tool("remote MCP arguments must be an object".into())
            })?;
            let result = tokio::time::timeout(
                TOOL_TIMEOUT,
                self.service.call_tool(
                    CallToolRequestParams::new(self.remote_name.clone()).with_arguments(arguments),
                ),
            )
            .await
            .map_err(|_| mobius::Error::Tool("remote MCP tool call timed out".into()))?
            .map_err(|error| {
                mobius::Error::Tool(format!("remote MCP tool call failed: {error}"))
            })?;
            let output = serde_json::to_string(&result)?;
            if result.is_error == Some(true) {
                return Err(mobius::Error::Tool(output));
            }
            Ok(output)
        })
    }
}

#[derive(Clone)]
struct AuthFileStore {
    path: PathBuf,
    lock: Arc<StdMutex<()>>,
    binding: Option<String>,
    allow_initialize: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthFile {
    #[serde(default)]
    binding: Option<String>,
    secret: Option<String>,
    credentials: Option<StoredCredentials>,
    states: BTreeMap<String, StoredAuthorizationState>,
}

impl AuthFileStore {
    fn for_server(
        path: PathBuf,
        lock: Arc<StdMutex<()>>,
        server: &crate::extensions::InstalledMcpServer,
    ) -> Result<Self> {
        Self::bound(path, lock, server, true)
    }

    fn for_active_server(
        path: PathBuf,
        lock: Arc<StdMutex<()>>,
        server: &crate::extensions::InstalledMcpServer,
    ) -> Result<Self> {
        Self::bound(path, lock, server, false)
    }

    fn bound(
        path: PathBuf,
        lock: Arc<StdMutex<()>>,
        server: &crate::extensions::InstalledMcpServer,
        allow_initialize: bool,
    ) -> Result<Self> {
        Ok(Self {
            path,
            lock,
            binding: Some(connection_binding(server)?),
            allow_initialize,
        })
    }

    fn unbound(path: PathBuf, lock: Arc<StdMutex<()>>) -> Self {
        Self {
            path,
            lock,
            binding: None,
            allow_initialize: false,
        }
    }

    fn read(&self) -> Result<AuthFile> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::Config("extension auth lock is poisoned".into()))?;
        let file = read_auth_file(&self.path)?.unwrap_or_default();
        if file.has_material() && file.binding != self.binding {
            return Err(Error::Config(
                "saved extension connection belongs to different server metadata".into(),
            ));
        }
        Ok(file)
    }

    fn update(&self, update: impl FnOnce(&mut AuthFile)) -> Result<()> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::Config("extension auth lock is poisoned".into()))?;
        let mut file = match read_auth_file(&self.path)? {
            Some(file) => file,
            None if self.allow_initialize => AuthFile::default(),
            None => {
                return Err(Error::Config(
                    "extension connection was disconnected".into(),
                ));
            }
        };
        if file.binding != self.binding {
            if !self.allow_initialize {
                return Err(Error::Config(
                    "saved extension connection belongs to different server metadata".into(),
                ));
            }
            file = AuthFile {
                binding: self.binding.clone(),
                ..AuthFile::default()
            };
        }
        update(&mut file);
        save_auth_file(&self.path, &file)
    }

    fn clear_file(&self) -> Result<()> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::Config("extension auth lock is poisoned".into()))?;
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
                Error::Config("extension auth path is not a regular file".into()),
            ),
            Ok(_) => {
                fs::remove_file(&self.path)?;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

impl AuthFile {
    fn has_material(&self) -> bool {
        self.secret.is_some() || self.credentials.is_some() || !self.states.is_empty()
    }
}

#[async_trait]
impl CredentialStore for AuthFileStore {
    async fn load(&self) -> std::result::Result<Option<StoredCredentials>, AuthError> {
        self.read()
            .map(|file| file.credentials)
            .map_err(store_error)
    }

    async fn save(&self, credentials: StoredCredentials) -> std::result::Result<(), AuthError> {
        self.update(|file| file.credentials = Some(credentials))
            .map_err(store_error)
    }

    async fn clear(&self) -> std::result::Result<(), AuthError> {
        self.update(|file| file.credentials = None)
            .map_err(store_error)
    }
}

#[async_trait]
impl StateStore for AuthFileStore {
    async fn save(
        &self,
        csrf_token: &str,
        state: StoredAuthorizationState,
    ) -> std::result::Result<(), AuthError> {
        self.update(|file| {
            purge_states(&mut file.states);
            if file.states.len() >= MAX_PENDING_STATES
                && let Some(oldest) = file
                    .states
                    .iter()
                    .min_by_key(|(_, state)| state.created_at)
                    .map(|(key, _)| key.clone())
            {
                file.states.remove(&oldest);
            }
            file.states.insert(csrf_token.to_owned(), state);
        })
        .map_err(store_error)
    }

    async fn load(
        &self,
        csrf_token: &str,
    ) -> std::result::Result<Option<StoredAuthorizationState>, AuthError> {
        let mut found = None;
        self.update(|file| {
            purge_states(&mut file.states);
            found = file.states.get(csrf_token).cloned();
        })
        .map_err(store_error)?;
        Ok(found)
    }

    async fn delete(&self, csrf_token: &str) -> std::result::Result<(), AuthError> {
        self.update(|file| {
            file.states.remove(csrf_token);
        })
        .map_err(store_error)
    }
}

fn read_auth_file(path: &Path) -> Result<Option<AuthFile>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_AUTH_FILE_BYTES
    {
        return Err(Error::Config("extension auth file is invalid".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::Config(
                "extension auth file must use owner-only permissions".into(),
            ));
        }
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_AUTH_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_AUTH_FILE_BYTES {
        return Err(Error::Config("extension auth file is too large".into()));
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn auth_file_lock() -> Arc<StdMutex<()>> {
    Arc::clone(&AUTH_FILE_LOCK)
}

fn save_auth_file(path: &Path, file: &AuthFile) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Config("extension auth path has no parent".into()))?;
    prepare_auth_directory(parent)?;
    let contents = serde_json::to_vec(file)?;
    if contents.len() as u64 > MAX_AUTH_FILE_BYTES {
        return Err(Error::Config("extension auth file is too large".into()));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(&contents)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn prepare_auth_directory(path: &Path) -> Result<()> {
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(Error::Config(
            "extension auth directory cannot be a symlink".into(),
        ));
    }
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn auth_path(state_dir: &Path, extension_id: &str) -> PathBuf {
    let mut hash = Sha256::new();
    hash.update(extension_id.as_bytes());
    state_dir
        .join("extension-auth")
        .join(format!("{:x}.json", hash.finalize()))
}

fn connection_binding(server: &crate::extensions::InstalledMcpServer) -> Result<String> {
    let connection = server
        .connection
        .as_ref()
        .ok_or_else(|| Error::Config("extension connection metadata is missing".into()))?;
    let kind = match connection.kind {
        ExtensionConnectionKind::OAuth => "oauth",
        ExtensionConnectionKind::ApiKey => "api_key",
    };
    let mut hash = Sha256::new();
    for value in [
        kind,
        server.name.as_str(),
        server.url.as_str(),
        connection.secret_header.as_deref().unwrap_or_default(),
    ] {
        hash.update(value.as_bytes());
        hash.update([0]);
    }
    Ok(format!("v1:{:x}", hash.finalize()))
}

fn validate_callback(callback_url: &str) -> Result<()> {
    if callback_url.len() > 8 * 1024 {
        return Err(Error::Config("extension OAuth callback is invalid".into()));
    }
    let callback = Url::parse(callback_url)
        .map_err(|_| Error::Config("extension OAuth callback is invalid".into()))?;
    if callback.scheme() != "mobius"
        || callback.host_str() != Some("extension-auth")
        || callback.path() != ""
        || callback.fragment().is_some()
        || !callback.username().is_empty()
        || callback.password().is_some()
        || callback.port().is_some()
    {
        return Err(Error::Config("extension OAuth callback is invalid".into()));
    }
    Ok(())
}

fn validate_remote_url(value: &str, label: &str) -> Result<()> {
    let url = Url::parse(value).map_err(|_| Error::Config(format!("invalid {label} URL")))?;
    if value.len() > 8 * 1024
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Config(format!("invalid {label} URL")));
    }
    Ok(())
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TOOL_TIMEOUT)
        .build()
        .map_err(Into::into)
}

fn headers(values: &BTreeMap<String, String>) -> Result<HashMap<HeaderName, HeaderValue>> {
    values
        .iter()
        .map(|(name, value)| Ok((parse_header_name(name)?, parse_header_value(value)?)))
        .collect()
}

fn parse_header_name(value: &str) -> Result<HeaderName> {
    HeaderName::from_bytes(value.as_bytes())
        .map_err(|_| Error::Config("extension MCP header name is invalid".into()))
}

fn parse_header_value(value: &str) -> Result<HeaderValue> {
    HeaderValue::from_str(value)
        .map_err(|_| Error::Config("extension MCP header value is invalid".into()))
}

fn identifier(value: &str) -> String {
    let mut output = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    if output.is_empty() || output.starts_with(|character: char| character.is_ascii_digit()) {
        output.insert(0, '_');
    }
    output
}

fn purge_states(states: &mut BTreeMap<String, StoredAuthorizationState>) {
    let cutoff = now().saturating_sub(STATE_TTL.as_secs());
    states.retain(|_, state| state.created_at >= cutoff);
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn auth_error(error: AuthError) -> Error {
    Error::Config(format!("extension OAuth failed: {error}"))
}

fn store_error(error: Error) -> AuthError {
    AuthError::InternalError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::{InstalledConnection, InstalledMcpServer, ResolvedMcpServer};

    fn maps_server(url: &str) -> InstalledMcpServer {
        InstalledMcpServer {
            name: "google-maps".into(),
            url: url.into(),
            headers: BTreeMap::new(),
            connection: Some(InstalledConnection {
                kind: ExtensionConnectionKind::ApiKey,
                label: "Google Maps API key".into(),
                secret_header: Some("X-Goog-Api-Key".into()),
            }),
        }
    }

    #[test]
    fn secret_state_is_owner_only_and_frontend_safe() {
        let temporary = tempfile::tempdir().expect("temporary state");
        let manager = RemoteMcp::new(temporary.path());
        let server = maps_server("https://mapstools.googleapis.com/mcp");
        manager
            .set_secret("plugin:maps", &server, "secret-value")
            .expect("save secret");
        let path = auth_path(temporary.path(), "plugin:maps");
        let record = connection_record(temporary.path(), "plugin:maps", &server);

        assert_eq!(record.state, ExtensionConnectionState::Connected);
        assert!(
            !serde_json::to_string(&record)
                .expect("record")
                .contains("secret-value")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(path).expect("auth file").permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn disconnect_removes_saved_connection_material() {
        let temporary = tempfile::tempdir().expect("temporary state");
        let manager = RemoteMcp::new(temporary.path());
        let server = maps_server("https://mapstools.googleapis.com/mcp");
        manager
            .set_secret("plugin:maps", &server, "secret-value")
            .expect("save secret");

        manager.disconnect("plugin:maps").await.expect("disconnect");

        assert!(!auth_path(temporary.path(), "plugin:maps").exists());
    }

    #[tokio::test]
    async fn active_store_cannot_restore_credentials_after_disconnect() {
        let temporary = tempfile::tempdir().expect("temporary state");
        let manager = RemoteMcp::new(temporary.path());
        let server = maps_server("https://mapstools.googleapis.com/mcp");
        manager
            .set_secret("plugin:maps", &server, "secret-value")
            .expect("save secret");
        let path = auth_path(temporary.path(), "plugin:maps");
        let active_store =
            AuthFileStore::for_active_server(path.clone(), auth_file_lock(), &server)
                .expect("active store");
        let credentials = serde_json::from_value(serde_json::json!({
            "client_id": "client-id"
        }))
        .expect("stored credentials");

        manager.disconnect("plugin:maps").await.expect("disconnect");

        assert!(
            CredentialStore::save(&active_store, credentials)
                .await
                .is_err()
        );
        assert!(!path.exists());
    }

    #[test]
    fn saved_secret_is_bound_to_exact_server_metadata() {
        let temporary = tempfile::tempdir().expect("temporary state");
        let manager = RemoteMcp::new(temporary.path());
        let original = maps_server("https://mapstools.googleapis.com/mcp");
        manager
            .set_secret("plugin:maps", &original, "secret-value")
            .expect("save secret");

        let changed = maps_server("https://example.com/mcp");
        let record = connection_record(temporary.path(), "plugin:maps", &changed);

        assert_eq!(record.state, ExtensionConnectionState::NeedsAttention);
        assert!(
            AuthFileStore::for_server(
                auth_path(temporary.path(), "plugin:maps"),
                Arc::new(StdMutex::new(())),
                &changed,
            )
            .expect("changed store")
            .read()
            .is_err()
        );
    }

    #[test]
    fn callback_is_fixed_to_the_native_redirect() {
        assert!(validate_callback("mobius://extension-auth?code=a&state=b").is_ok());
        assert!(validate_callback("https://example.com?code=a&state=b").is_err());
        assert!(validate_callback("mobius://extension-auth/path?code=a&state=b").is_err());
    }

    #[test]
    fn remote_urls_require_credential_free_https() {
        assert!(validate_remote_url("https://mcp.notion.com/mcp", "MCP server").is_ok());
        assert!(validate_remote_url("http://mcp.notion.com/mcp", "MCP server").is_err());
        assert!(validate_remote_url("https://token@example.com/mcp", "MCP server").is_err());
        assert!(validate_remote_url("https://example.com/mcp#token", "MCP server").is_err());
    }

    #[test]
    fn untrusted_read_only_hint_does_not_bypass_approval() {
        let mut annotations = rmcp::model::ToolAnnotations::default();
        annotations.read_only_hint = Some(true);

        assert_eq!(
            remote_tool_approval(Some(&annotations)),
            ApprovalRequirement::Always
        );
    }

    #[tokio::test]
    async fn disconnected_server_is_component_local() {
        let temporary = tempfile::tempdir().expect("temporary state");
        let server = ResolvedMcpServer {
            extension_id: "plugin:google-maps".into(),
            plugin_name: "google-maps".into(),
            config: maps_server("https://mapstools.googleapis.com/mcp"),
        };

        assert!(tools_for(&[server], temporary.path()).await.is_empty());
    }
}
