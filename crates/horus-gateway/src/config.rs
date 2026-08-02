//! Validated gateway configuration and owner-only persistence.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use horus::backend::model::provider::{ProviderAuth, provider};
use horus::protocol::TokenUsage;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::wire::{
    AgentComposition, DailyUsage, MiddlewareConfig, ProfileSnapshot, ProviderConfig,
    VersionedAgentConfig, WorkspaceInfo,
};
use crate::{Error, Result};

const CONFIG_VERSION: u32 = 1;
const CONFIG_FILE: &str = "gateway.json";
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_SYSTEM_PROMPT_BYTES: usize = 64 * 1024;
const MAX_API_KEY_BYTES: usize = 16 * 1024;
const SECONDS_PER_DAY: u64 = 86_400;
const USAGE_HISTORY_DAYS: u64 = 52 * 7;

/// Default loopback listener used by a local gateway.
pub const DEFAULT_LISTEN: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8741);

/// Default system prompt installed by `horus-gateway init`.
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are Horus, a concise coding agent. Inspect the workspace before editing, make focused changes, preserve unrelated work, and verify the result.";

/// Certificate paths required by a TLS listener.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    pub certificate: PathBuf,
    pub private_key: PathBuf,
}

/// Durable settings for one gateway process and workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    version: u32,
    pub listen: SocketAddr,
    pub workspace: PathBuf,
    pub tls: Option<TlsConfig>,
    pub agent: VersionedAgentConfig,
    #[serde(default)]
    usage: UsageHistory,
}

/// File owner for gateway configuration and aggregate usage.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    state_dir: PathBuf,
    path: PathBuf,
}

/// Owner-only API-key storage kept outside frontend-readable configuration.
pub struct CredentialStore {
    path: PathBuf,
    values: Mutex<BTreeMap<String, StoredCredential>>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCredential {
    api_key: String,
    base_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageHistory {
    days: BTreeMap<u64, TokenUsage>,
    sessions: BTreeMap<String, TokenUsage>,
}

impl Default for AgentComposition {
    fn default() -> Self {
        Self {
            provider: ProviderConfig {
                provider: "openai_codex".into(),
                model: "gpt-5.6-sol".into(),
                base_url: None,
                api_key_env: None,
                reasoning_effort: Some("medium".into()),
                web_search: horus::backend::model::provider::HostedWebSearch::Off,
            },
            middleware: MiddlewareConfig {
                tools: true,
                skills: true,
                subagents: true,
                steering: true,
                compaction: true,
                sessions: true,
            },
            approval: horus::backend::sandbox::ApprovalPolicy::On,
            system_prompt: DEFAULT_SYSTEM_PROMPT.into(),
        }
    }
}

impl GatewayConfig {
    /// Builds the initial validated settings for one workspace.
    pub fn new(listen: SocketAddr, workspace: PathBuf, tls: Option<TlsConfig>) -> Result<Self> {
        let config = Self {
            version: CONFIG_VERSION,
            listen,
            workspace,
            tls,
            agent: VersionedAgentConfig {
                revision: 1,
                config: AgentComposition::default(),
            },
            usage: UsageHistory::default(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Returns the workspace's frontend-safe identity.
    #[must_use]
    pub fn workspace_info(&self) -> WorkspaceInfo {
        WorkspaceInfo {
            id: workspace_id(&self.workspace),
            label: self.workspace.display().to_string(),
        }
    }

    /// Builds a revision-checked replacement without mutating the current value.
    pub fn replacing_agent(
        &self,
        expected_revision: u64,
        composition: AgentComposition,
    ) -> Result<Self> {
        if expected_revision != self.agent.revision {
            return Err(Error::Config(format!(
                "configuration revision changed from {expected_revision} to {}",
                self.agent.revision
            )));
        }
        validate_agent_composition(&composition)?;
        let mut next = self.clone();
        next.agent = VersionedAgentConfig {
            revision: self
                .agent
                .revision
                .checked_add(1)
                .ok_or_else(|| Error::Config("configuration revision overflow".into()))?,
            config: composition,
        };
        Ok(next)
    }

    /// Records a cumulative token counter and reports whether daily usage changed.
    pub fn observe_usage(
        &mut self,
        session_id: &str,
        total: &TokenUsage,
        live: bool,
    ) -> Result<bool> {
        self.usage
            .observe(session_id, total, live, SystemTime::now())
    }

    /// Returns frontend-safe local identity and daily aggregate usage.
    #[must_use]
    pub fn profile(&self) -> ProfileSnapshot {
        ProfileSnapshot {
            user_name: local_user_name(),
            workspace: self.workspace_info(),
            daily_usage: self
                .usage
                .days
                .iter()
                .map(|(unix_day, usage)| DailyUsage {
                    unix_day: *unix_day,
                    usage: usage.clone(),
                })
                .collect(),
        }
    }

    /// Validates every persisted trust-boundary field.
    pub fn validate(&self) -> Result<()> {
        if self.version != CONFIG_VERSION {
            return Err(Error::Config(format!(
                "unsupported gateway config version {}",
                self.version
            )));
        }
        if self.agent.revision == 0 {
            return Err(Error::Config(
                "configuration revision must be positive".into(),
            ));
        }
        if self.listen.port() == 0 {
            return Err(Error::Config(
                "gateway listen port must be greater than zero".into(),
            ));
        }
        if !self.workspace.is_absolute()
            || !self.workspace.is_dir()
            || self.workspace.parent().is_none()
        {
            return Err(Error::Config(
                "workspace must be an existing absolute non-root directory".into(),
            ));
        }
        match (&self.tls, self.listen.ip().is_loopback()) {
            (None, false) => {
                return Err(Error::Config(
                    "non-loopback gateway listeners require a TLS certificate and private key"
                        .into(),
                ));
            }
            (Some(tls), _) => {
                tls.validate()?;
                if fs::canonicalize(&tls.private_key)?
                    .starts_with(fs::canonicalize(&self.workspace)?)
                {
                    return Err(Error::Config(
                        "TLS private key must be stored outside the agent workspace".into(),
                    ));
                }
            }
            (None, true) => {}
        }
        validate_agent_composition(&self.agent.config)?;
        for usage in self.usage.days.values().chain(self.usage.sessions.values()) {
            validate_usage(usage)?;
        }
        Ok(())
    }
}

impl TlsConfig {
    fn validate(&self) -> Result<()> {
        for (name, path) in [
            ("TLS certificate", &self.certificate),
            ("TLS private key", &self.private_key),
        ] {
            if !path.is_absolute() || !path.is_file() {
                return Err(Error::Config(format!(
                    "{name} must be an existing absolute file"
                )));
            }
        }
        Ok(())
    }
}

impl ConfigStore {
    /// Initializes an owner-only state directory and new config file.
    pub fn initialize(
        state_dir: PathBuf,
        workspace: PathBuf,
        listen: SocketAddr,
        tls: Option<TlsConfig>,
    ) -> Result<(Self, GatewayConfig)> {
        let workspace = canonical_workspace(&workspace)?;
        let config = GatewayConfig::new(listen, workspace.clone(), tls)?;
        let state_dir = prepare_state_dir(state_dir, &workspace)?;
        let store = Self::at(state_dir);
        store.save_with_mode(&config, true)?;
        Ok((store, config))
    }

    /// Opens and validates persisted gateway configuration.
    pub fn open(state_dir: PathBuf) -> Result<(Self, GatewayConfig)> {
        let state_dir = fs::canonicalize(state_dir)?;
        let store = Self::at(state_dir);
        let mut file = fs::File::open(&store.path)?;
        let mut contents = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut contents)?;
        if u64::try_from(contents.len()).unwrap_or(u64::MAX) > MAX_CONFIG_BYTES {
            return Err(Error::Config("gateway configuration is too large".into()));
        }
        let config: GatewayConfig = serde_json::from_slice(&contents)?;
        store.validate_config(&config)?;
        Ok((store, config))
    }

    pub(crate) fn replacing_workspace(
        &self,
        current: &GatewayConfig,
        workspace: &Path,
    ) -> Result<GatewayConfig> {
        let mut next = current.clone();
        next.workspace = canonical_workspace(workspace)?;
        self.validate_config(&next)?;
        Ok(next)
    }

    /// Atomically replaces validated persistent configuration.
    pub fn save(&self, config: &GatewayConfig) -> Result<()> {
        self.save_with_mode(config, false)
    }

    /// Returns the protected state directory.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Returns the provider credential file path.
    #[must_use]
    pub fn credentials_path(&self) -> PathBuf {
        self.state_dir.join("credentials.json")
    }

    /// Returns the provider browser-auth file path.
    #[must_use]
    pub fn provider_auth_path(&self) -> PathBuf {
        self.state_dir.join("provider-auth.json")
    }

    /// Returns the checkpoint database path.
    #[must_use]
    pub fn checkpoints_path(&self) -> PathBuf {
        self.state_dir.join("checkpoints.sqlite3")
    }

    /// Returns the authentication state path.
    #[must_use]
    pub fn auth_path(&self) -> PathBuf {
        self.state_dir.join("auth.json")
    }

    fn at(state_dir: PathBuf) -> Self {
        let path = state_dir.join(CONFIG_FILE);
        Self { state_dir, path }
    }

    fn save_with_mode(&self, config: &GatewayConfig, create_new: bool) -> Result<()> {
        self.validate_config(config)?;
        let contents = serde_json::to_vec_pretty(config)?;
        if u64::try_from(contents.len()).unwrap_or(u64::MAX) > MAX_CONFIG_BYTES {
            return Err(Error::Config("gateway configuration is too large".into()));
        }
        let mut file = tempfile::NamedTempFile::new_in(&self.state_dir)?;
        #[cfg(unix)]
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(&contents)?;
        file.as_file().sync_all()?;
        if create_new {
            file.persist_noclobber(&self.path)
                .map_err(|error| error.error)?;
        } else {
            file.persist(&self.path).map_err(|error| error.error)?;
        }
        Ok(())
    }

    fn validate_config(&self, config: &GatewayConfig) -> Result<()> {
        config.validate()?;
        if self.state_dir.starts_with(&config.workspace)
            || config.workspace.starts_with(&self.state_dir)
        {
            return Err(Error::Config(
                "gateway state directory and workspace must not overlap".into(),
            ));
        }
        Ok(())
    }
}

impl CredentialStore {
    /// Opens credential state, treating a missing file as an empty store.
    pub fn open(path: PathBuf) -> Result<Self> {
        let values = match fs::read(&path) {
            Ok(contents) => {
                if contents.len() > 256 * 1024 {
                    return Err(Error::Config(
                        "provider credential state is too large".into(),
                    ));
                }
                serde_json::from_slice(&contents)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            path,
            values: Mutex::new(values),
        })
    }

    /// Atomically replaces one API key after provider and size validation.
    pub fn set(&self, provider_id: &str, api_key: &str, base_url: Option<&str>) -> Result<()> {
        let definition = provider(provider_id)?;
        if !matches!(definition.auth(), ProviderAuth::ApiKey(_)) {
            return Err(Error::Config(format!(
                "provider `{provider_id}` does not accept an API key"
            )));
        }
        if api_key.trim().is_empty() || api_key.len() > MAX_API_KEY_BYTES {
            return Err(Error::Config(format!(
                "API key must be 1–{MAX_API_KEY_BYTES} bytes"
            )));
        }
        if definition.configurable_base_url() != base_url.is_some() {
            return Err(Error::Config(
                "credential endpoint does not match the provider".into(),
            ));
        }
        let mut values = self
            .values
            .lock()
            .map_err(|_| Error::Config("provider credential lock is poisoned".into()))?;
        let mut next = values.clone();
        next.insert(
            provider_id.into(),
            StoredCredential {
                api_key: api_key.into(),
                base_url: base_url.map(str::to_owned),
            },
        );
        save_private_map(&self.path, &next)?;
        *values = next;
        Ok(())
    }

    /// Resolves a credential for model assembly without exposing it to clients.
    pub fn get(&self, provider_id: &str, base_url: Option<&str>) -> Result<Option<String>> {
        let values = self
            .values
            .lock()
            .map_err(|_| Error::Config("provider credential lock is poisoned".into()))?;
        Ok(values
            .get(provider_id)
            .filter(|credential| credential.base_url.as_deref() == base_url)
            .map(|credential| credential.api_key.clone()))
    }

    /// Reports whether an API key is stored for one provider.
    pub fn configured(&self, provider_id: &str) -> Result<bool> {
        let values = self
            .values
            .lock()
            .map_err(|_| Error::Config("provider credential lock is poisoned".into()))?;
        Ok(values.contains_key(provider_id))
    }
}

/// Resolves the gateway state directory from the environment or home directory.
pub fn state_dir() -> Result<PathBuf> {
    if let Some(path) = env::var_os("HORUS_GATEWAY_STATE_DIR") {
        if path.is_empty() {
            return Err(Error::Config("HORUS_GATEWAY_STATE_DIR is empty".into()));
        }
        return Ok(path.into());
    }
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join(".horus").join("gateway"))
        .ok_or_else(|| {
            Error::Config("cannot determine the home directory; set HORUS_GATEWAY_STATE_DIR".into())
        })
}

/// Validates the complete frontend-writable agent composition.
pub fn validate_agent_composition(config: &AgentComposition) -> Result<()> {
    if config.system_prompt.trim().is_empty()
        || config.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES
    {
        return Err(Error::Config(format!(
            "system prompt must be 1–{MAX_SYSTEM_PROMPT_BYTES} bytes"
        )));
    }
    if config.provider.provider.trim().is_empty() || config.provider.provider.len() > 256 {
        return Err(Error::Config("provider ID must be 1–256 bytes".into()));
    }
    if config.provider.model.trim().is_empty() || config.provider.model.len() > 1024 {
        return Err(Error::Config("model must be 1–1024 bytes".into()));
    }
    if let Some(name) = config.provider.api_key_env.as_deref()
        && !valid_env_name(name)
    {
        return Err(Error::Config(
            "API-key environment variable name is invalid".into(),
        ));
    }
    let definition = provider(&config.provider.provider)?;
    if matches!(definition.auth(), ProviderAuth::Browser(_))
        && config.provider.api_key_env.is_some()
    {
        return Err(Error::Config(
            "browser-auth providers cannot configure an API-key environment variable".into(),
        ));
    }
    if definition.configurable_base_url() && config.provider.api_key_env.is_some() {
        return Err(Error::Config(
            "custom-endpoint credentials must be stored through the gateway".into(),
        ));
    }
    definition.build_config_is_valid(
        &config.provider.model,
        config.provider.base_url.as_deref(),
        config.provider.reasoning_effort.as_deref(),
        config.provider.web_search,
    )?;
    Ok(())
}

fn canonical_workspace(path: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(path)?;
    if !path.is_dir() || path.parent().is_none() {
        return Err(Error::Config(
            "workspace must be an existing non-root directory".into(),
        ));
    }
    Ok(path)
}

fn prepare_state_dir(path: PathBuf, workspace: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| Error::Config("gateway state directory must have a name".into()))?
        .to_owned();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let path = fs::canonicalize(parent)?.join(name);
    if path.starts_with(workspace) || workspace.starts_with(&path) {
        return Err(Error::Config(
            "gateway state directory and workspace must not overlap".into(),
        ));
    }
    match fs::create_dir(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(Error::Config(
                "gateway state directory already exists".into(),
            ));
        }
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

fn save_private_map<T: Serialize>(path: &Path, values: &BTreeMap<String, T>) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Config("provider credential path has no parent".into()))?;
    let contents = serde_json::to_vec(values)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(&contents)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn workspace_id(path: &Path) -> String {
    let digest = sha2::Sha256::digest(path.as_os_str().as_encoded_bytes());
    let mut id = String::from("path-v1:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut id, "{byte:02x}").expect("writing to a string cannot fail");
    }
    id
}

pub(crate) fn local_user_name() -> Option<String> {
    ["USER", "USERNAME"]
        .into_iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.trim().is_empty()))
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

impl UsageHistory {
    fn observe(
        &mut self,
        session_id: &str,
        total: &TokenUsage,
        live: bool,
        now: SystemTime,
    ) -> Result<bool> {
        validate_usage(total)?;
        let previous = self.sessions.get(session_id).cloned().unwrap_or_default();
        let Some(delta) = usage_delta(total, &previous) else {
            self.set_baseline(session_id, total);
            return Ok(false);
        };
        if !live || delta == TokenUsage::default() {
            self.set_baseline(session_id, total);
            return Ok(false);
        }
        let day = unix_day(now)?;
        let mut bucket = self.days.get(&day).cloned().unwrap_or_default();
        bucket
            .checked_add(&delta)
            .ok_or_else(|| Error::Config("daily token usage overflow".into()))?;
        self.days.insert(day, bucket);
        let first_day = day.saturating_sub(USAGE_HISTORY_DAYS - 1);
        self.days.retain(|stored, _| *stored >= first_day);
        self.set_baseline(session_id, total);
        Ok(true)
    }

    fn set_baseline(&mut self, session_id: &str, total: &TokenUsage) {
        self.sessions.clear();
        self.sessions.insert(session_id.into(), total.clone());
    }
}

fn unix_day(now: SystemTime) -> Result<u64> {
    Ok(now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Config("system clock is before the Unix epoch".into()))?
        .as_secs()
        / SECONDS_PER_DAY)
}

fn usage_delta(current: &TokenUsage, previous: &TokenUsage) -> Option<TokenUsage> {
    Some(TokenUsage {
        input_tokens: current.input_tokens.checked_sub(previous.input_tokens)?,
        cached_input_tokens: current
            .cached_input_tokens
            .checked_sub(previous.cached_input_tokens)?,
        cache_write_input_tokens: current
            .cache_write_input_tokens
            .checked_sub(previous.cache_write_input_tokens)?,
        output_tokens: current.output_tokens.checked_sub(previous.output_tokens)?,
        reasoning_output_tokens: current
            .reasoning_output_tokens
            .checked_sub(previous.reasoning_output_tokens)?,
        total_tokens: current.total_tokens.checked_sub(previous.total_tokens)?,
    })
    .filter(usage_nonnegative)
}

fn validate_usage(usage: &TokenUsage) -> Result<()> {
    if !usage_nonnegative(usage) {
        return Err(Error::Config("token usage cannot be negative".into()));
    }
    Ok(())
}

fn usage_nonnegative(usage: &TokenUsage) -> bool {
    usage.input_tokens >= 0
        && usage.cached_input_tokens >= 0
        && usage.cache_write_input_tokens >= 0
        && usage.output_tokens >= 0
        && usage.reasoning_output_tokens >= 0
        && usage.total_tokens >= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_loopback_listener_requires_tls() {
        let workspace = tempfile::tempdir().expect("workspace");
        let listen = "0.0.0.0:8741".parse().expect("listen address");

        let error = GatewayConfig::new(listen, workspace.path().to_path_buf(), None)
            .expect_err("remote plaintext must fail");

        assert!(error.to_string().contains("require a TLS certificate"));
    }

    #[test]
    fn listener_rejects_port_zero() {
        let workspace = tempfile::tempdir().expect("workspace");
        let listen = "127.0.0.1:0".parse().expect("listen address");

        let error = GatewayConfig::new(listen, workspace.path().to_path_buf(), None)
            .expect_err("port zero must fail");

        assert!(error.to_string().contains("greater than zero"));
    }

    #[test]
    fn invalid_configuration_does_not_create_gateway_state() {
        let root = tempfile::tempdir().expect("temporary directory");
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).expect("workspace");
        let state = root.path().join("state");
        let listen = "127.0.0.1:0".parse().expect("listen address");

        let error = ConfigStore::initialize(state.clone(), workspace, listen, None)
            .expect_err("invalid config must fail");

        assert!(error.to_string().contains("greater than zero"));
        assert!(!state.exists());
    }

    #[test]
    fn replacement_workspace_must_be_an_existing_directory() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        let file = root.path().join("file");
        fs::create_dir(&workspace).expect("workspace");
        fs::write(&file, "not a directory").expect("file");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) =
            ConfigStore::initialize(state, workspace, listen, None).expect("initialize config");

        let error = store
            .replacing_workspace(&config, &file)
            .expect_err("file workspace must fail");

        assert!(error.to_string().contains("existing non-root directory"));
    }

    #[test]
    fn replacement_workspace_cannot_expose_gateway_state() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        fs::create_dir(&workspace).expect("workspace");
        let listen = "127.0.0.1:8741".parse().expect("listen address");
        let (store, config) =
            ConfigStore::initialize(state, workspace, listen, None).expect("initialize config");

        let error = store
            .replacing_workspace(&config, root.path())
            .expect_err("state-containing workspace must fail");

        assert!(error.to_string().contains("must not overlap"));
    }

    #[test]
    fn rejected_workspace_overlap_does_not_create_gateway_state() {
        let workspace = tempfile::tempdir().expect("workspace");
        let state = workspace.path().join(".horus").join("gateway");
        let listen = "127.0.0.1:8741".parse().expect("listen address");

        let error =
            ConfigStore::initialize(state.clone(), workspace.path().to_path_buf(), listen, None)
                .expect_err("overlapping state must fail");

        assert!(error.to_string().contains("must not overlap"));
        assert!(!state.exists());
    }

    #[test]
    fn tls_private_key_cannot_be_exposed_inside_the_agent_workspace() {
        let workspace = tempfile::tempdir().expect("workspace");
        let certificate = workspace.path().join("certificate.pem");
        let private_key = workspace.path().join("private-key.pem");
        fs::write(&certificate, "certificate").expect("certificate");
        fs::write(&private_key, "private key").expect("private key");
        let listen = "127.0.0.1:8741".parse().expect("listen address");

        let error = GatewayConfig::new(
            listen,
            workspace.path().to_path_buf(),
            Some(TlsConfig {
                certificate,
                private_key,
            }),
        )
        .expect_err("workspace TLS key must fail");

        assert!(error.to_string().contains("outside the agent workspace"));
    }

    #[test]
    fn config_rejects_an_empty_system_prompt() {
        let mut config = AgentComposition::default();
        config.system_prompt.clear();

        let error = validate_agent_composition(&config).expect_err("empty prompt must fail");

        assert!(error.to_string().contains("system prompt"));
    }

    #[test]
    fn custom_endpoint_rejects_host_environment_credentials() {
        let config = AgentComposition {
            provider: ProviderConfig {
                provider: "responses".into(),
                model: "custom-model".into(),
                base_url: Some("https://example.com/v1".into()),
                api_key_env: Some("OPENAI_API_KEY".into()),
                reasoning_effort: None,
                web_search: horus::backend::model::provider::HostedWebSearch::Off,
            },
            ..AgentComposition::default()
        };

        let error = validate_agent_composition(&config).expect_err("host credential redirect");

        assert!(error.to_string().contains("stored through the gateway"));
    }

    #[cfg(unix)]
    #[test]
    fn provider_credentials_are_owner_only_and_absent_from_agent_snapshots() {
        let directory = tempfile::tempdir().expect("state directory");
        let path = directory.path().join("credentials.json");
        let credentials = CredentialStore::open(path.clone()).expect("credential store");

        credentials
            .set("openrouter", "write-only-secret", None)
            .expect("store credential");

        let mode = fs::metadata(path)
            .expect("credential metadata")
            .permissions()
            .mode()
            & 0o777;
        let snapshot = serde_json::to_string(&AgentComposition::default()).expect("snapshot");
        assert_eq!(mode, 0o600);
        assert!(!snapshot.contains("write-only-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn initialized_state_and_config_are_owner_only() {
        let workspace = tempfile::tempdir().expect("workspace");
        let state_parent = tempfile::tempdir().expect("state parent");
        let state = state_parent.path().join("gateway");
        let listen = "127.0.0.1:8741".parse().expect("listen address");

        let (store, _) =
            ConfigStore::initialize(state.clone(), workspace.path().to_path_buf(), listen, None)
                .expect("initialize config");

        let directory_mode = fs::metadata(store.state_dir())
            .expect("state metadata")
            .permissions()
            .mode()
            & 0o777;
        let file_mode = fs::metadata(state.join(CONFIG_FILE))
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!((directory_mode, file_mode), (0o700, 0o600));
    }

    #[cfg(unix)]
    #[test]
    fn initialization_does_not_repermission_an_existing_directory() {
        let workspace = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("existing state directory");
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o755))
            .expect("state permissions");
        let listen = "127.0.0.1:8741".parse().expect("listen address");

        let error = ConfigStore::initialize(
            state.path().to_path_buf(),
            workspace.path().to_path_buf(),
            listen,
            None,
        )
        .expect_err("existing state directory must fail");
        let mode = fs::metadata(state.path())
            .expect("state metadata")
            .permissions()
            .mode()
            & 0o777;

        assert!(error.to_string().contains("already exists"));
        assert_eq!(mode, 0o755);
    }
}
