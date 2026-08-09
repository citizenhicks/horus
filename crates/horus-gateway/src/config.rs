//! Validated gateway configuration and owner-only persistence.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use horus::agent::DEFAULT_MAX_MODEL_STEPS;
use horus::backend::model::provider::{
    ProviderAuth, ProviderDefinition, default_provider, provider,
};
use horus::protocol::TokenUsage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest as _;

use crate::wire::{
    AgentComposition, DailyUsage, ProfileSnapshot, ProviderConfig, VersionedAgentConfig,
    WorkspaceInfo,
};
use crate::{Error, Result};

const CONFIG_VERSION: u32 = 13;
const CHAT_SPEC_VERSION: u32 = 6;
pub(crate) const CHAT_SPEC_METADATA_KEY: &str = "horus_gateway.chat";
const CONFIG_FILE: &str = "gateway.toml";
const CLOUDFLARE_TOKEN_FILE: &str = "cloudflare-token";
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_SYSTEM_PROMPT_BYTES: usize = 64 * 1024;
const MAX_API_KEY_BYTES: usize = 16 * 1024;
const MAX_PROVIDER_CATALOG_ENTRIES: usize = 64;
const MAX_PROVIDER_CATALOG_ENTRY_BYTES: usize = 1024;
const MAX_PROVIDER_CATALOG_BYTES: usize = 16 * 1024;
const MAX_CUSTOM_MODEL_ROUTES: usize = 64;
const MAX_CLOUDFLARE_TOKEN_BYTES: usize = 16 * 1024;
const SECONDS_PER_DAY: u64 = 86_400;
const USAGE_HISTORY_DAYS: u64 = 52 * 7;

mod defaults {
    include!(concat!(env!("OUT_DIR"), "/defaults.rs"));
}

/// Default loopback listener used by a local gateway.
pub const DEFAULT_LISTEN: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8741);

/// Default system prompt installed by `horus-gateway init`.
pub const DEFAULT_SYSTEM_PROMPT: &str = defaults::DEFAULT_SYSTEM_PROMPT;

/// Context window used for custom models without an advertised preset.
pub const DEFAULT_CONTEXT_WINDOW: i64 = defaults::DEFAULT_CONTEXT_WINDOW;

/// Certificate paths required by a TLS listener.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    pub certificate: PathBuf,
    pub private_key: PathBuf,
}

/// Cloudflare Tunnel exposure selected for this gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudflareConfig {
    /// Account-free tunnel with an address assigned at process startup.
    Quick,
    /// User-owned tunnel with a stable published hostname.
    Named { hostname: String },
}

/// Durable machine-wide settings and defaults for one gateway process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    version: u32,
    pub listen: SocketAddr,
    pub tls: Option<TlsConfig>,
    pub cloudflare: Option<CloudflareConfig>,
    pub default_agent: Option<VersionedAgentConfig>,
    pub(crate) configured_providers: BTreeMap<String, ConfiguredProvider>,
    usage: UsageHistory,
}

/// One durable provider selection and its gateway model and reasoning catalogs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfiguredProvider {
    pub(crate) selection: ProviderConfig,
    pub(crate) model_ids: Vec<String>,
    pub(crate) reasoning_efforts: Vec<String>,
}

/// Durable runtime recipe copied into one chat checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChatSpec {
    version: u32,
    pub(crate) workspace: PathBuf,
    pub(crate) agent: VersionedAgentConfig,
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
}

impl Default for AgentComposition {
    fn default() -> Self {
        let provider = default_provider();
        let model = provider
            .default_model()
            .and_then(|id| provider.model(id))
            .expect("default model manifest");
        Self {
            provider: ProviderConfig {
                provider: provider.id().into(),
                model: model.id.into(),
                base_url: provider.default_base_url().map(str::to_string),
                reasoning_effort: model.default_reasoning.map(str::to_string),
                web_search: *provider
                    .web_search()
                    .first()
                    .expect("default provider web-search manifest"),
            },
            middleware: crate::middleware_manifest::default_config(),
            system_prompt: DEFAULT_SYSTEM_PROMPT.into(),
            max_model_steps: DEFAULT_MAX_MODEL_STEPS as u64,
        }
    }
}

impl GatewayConfig {
    /// Builds validated machine-wide settings and new-chat defaults.
    pub fn new(listen: SocketAddr, tls: Option<TlsConfig>) -> Result<Self> {
        let config = Self {
            version: CONFIG_VERSION,
            listen,
            tls,
            cloudflare: None,
            default_agent: None,
            configured_providers: BTreeMap::new(),
            usage: UsageHistory::default(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Builds a loopback gateway exposed through Cloudflare Tunnel.
    pub fn new_cloudflare(listen: SocketAddr, cloudflare: CloudflareConfig) -> Result<Self> {
        let mut config = Self::new(listen, None)?;
        config.cloudflare = Some(cloudflare);
        config.validate()?;
        Ok(config)
    }

    /// Registers one configured provider and establishes the first as the new-chat default.
    pub(crate) fn registering_provider(
        &self,
        selection: ProviderConfig,
        model_ids: Vec<String>,
        reasoning_efforts: Vec<String>,
    ) -> Result<Self> {
        let configured = ConfiguredProvider {
            selection: selection.clone(),
            model_ids,
            reasoning_efforts,
        };
        let mut next = self.clone();
        next.configured_providers
            .insert(selection.provider.clone(), configured);
        if self.default_agent.is_none() {
            let config = AgentComposition {
                provider: selection,
                ..AgentComposition::default()
            };
            next.default_agent = Some(VersionedAgentConfig {
                revision: 1,
                config,
            });
        }
        next.validate()?;
        Ok(next)
    }

    /// Replaces only the defaults copied into future chats.
    pub(crate) fn replacing_default_agent(
        &self,
        expected_revision: u64,
        composition: AgentComposition,
    ) -> Result<Self> {
        let current = self
            .default_agent
            .as_ref()
            .ok_or_else(|| Error::Config("configure a provider before saving defaults".into()))?;
        if current.revision != expected_revision {
            return Err(Error::Config(format!(
                "configuration revision changed from {expected_revision} to {}",
                current.revision
            )));
        }
        let mut next = self.clone();
        next.default_agent = Some(VersionedAgentConfig {
            revision: current
                .revision
                .checked_add(1)
                .ok_or_else(|| Error::Config("configuration revision overflow".into()))?,
            config: composition,
        });
        next.validate()?;
        Ok(next)
    }

    pub(crate) fn validate_provider_selection(&self, selection: &ProviderConfig) -> Result<()> {
        validate_provider_config(selection)?;
        let configured = self
            .configured_providers
            .get(&selection.provider)
            .ok_or_else(|| {
                Error::Config("provider selection must use a configured provider entry".into())
            })?;
        validate_configured_provider_selection(configured, selection)
    }

    /// Records one live token-usage increment and reports whether daily usage changed.
    pub fn observe_usage(&mut self, usage: &TokenUsage) -> Result<bool> {
        self.usage.observe(usage, SystemTime::now())
    }

    /// Returns frontend-safe local identity and daily aggregate usage.
    #[must_use]
    pub fn profile(&self) -> ProfileSnapshot {
        ProfileSnapshot {
            user_name: local_user_name(),
            daily_usage: self
                .usage
                .days
                .iter()
                .map(|(unix_day, usage)| DailyUsage {
                    unix_day: *unix_day,
                    usage: usage.clone(),
                })
                .collect(),
            run_stats: crate::wire::RunStats::default(),
            recent_run_groups: Vec::new(),
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
        if self.listen.port() == 0 {
            return Err(Error::Config(
                "gateway listen port must be greater than zero".into(),
            ));
        }
        match (&self.tls, self.listen.ip().is_loopback()) {
            (None, false) => {
                return Err(Error::Config(
                    "non-loopback gateway listeners require a TLS certificate and private key"
                        .into(),
                ));
            }
            (Some(tls), _) => tls.validate()?,
            (None, true) => {}
        }
        if self.cloudflare.is_some() && (!self.listen.ip().is_loopback() || self.tls.is_some()) {
            return Err(Error::Config(
                "Cloudflare gateways require a plaintext loopback listener".into(),
            ));
        }
        if let Some(cloudflare) = &self.cloudflare {
            cloudflare.validate()?;
        }
        if self.configured_providers.is_empty() != self.default_agent.is_none() {
            return Err(Error::Config(
                "the gateway default must exist exactly when a provider is configured".into(),
            ));
        }
        for (provider_id, configured) in &self.configured_providers {
            if provider_id != &configured.selection.provider {
                return Err(Error::Config(format!(
                    "configured provider key `{provider_id}` does not match `{}`",
                    configured.selection.provider
                )));
            }
            validate_configured_provider(configured)?;
        }
        validate_custom_model_route_count(&self.configured_providers)?;
        if let Some(default) = &self.default_agent {
            if default.revision == 0 {
                return Err(Error::Config(
                    "configuration revision must be positive".into(),
                ));
            }
            validate_agent_composition(&default.config)?;
            self.validate_provider_selection(&default.config.provider)?;
            for (middleware, setting, route) in
                crate::middleware_manifest::configured_model_routes(&default.config.middleware)
            {
                if !crate::assembly::configured_route_exists(self, route)? {
                    return Err(Error::Config(format!(
                        "gateway default middleware setting `{middleware}.{setting}` is not a configured model route"
                    )));
                }
            }
        }
        for usage in self.usage.days.values() {
            validate_usage(usage)?;
        }
        Ok(())
    }
}

impl ChatSpec {
    pub(crate) fn new(
        workspace: &Path,
        agent: VersionedAgentConfig,
        state_dir: &Path,
        tls: Option<&TlsConfig>,
    ) -> Result<Self> {
        let spec = Self {
            version: CHAT_SPEC_VERSION,
            workspace: validate_chat_workspace(workspace, state_dir, tls)?,
            agent,
        };
        spec.validate(state_dir, tls)?;
        Ok(spec)
    }

    pub(crate) fn from_metadata(
        metadata: &BTreeMap<String, Value>,
        state_dir: &Path,
        tls: Option<&TlsConfig>,
    ) -> Result<Self> {
        let value = metadata.get(CHAT_SPEC_METADATA_KEY).ok_or_else(|| {
            Error::Config("chat checkpoint has no gateway runtime configuration".into())
        })?;
        let spec: Self = serde_json::from_value(value.clone())?;
        spec.validate(state_dir, tls)?;
        Ok(spec)
    }

    pub(crate) fn metadata(&self) -> Result<BTreeMap<String, Value>> {
        Ok(BTreeMap::from([(
            CHAT_SPEC_METADATA_KEY.into(),
            serde_json::to_value(self)?,
        )]))
    }

    #[must_use]
    pub(crate) fn workspace_info(&self) -> WorkspaceInfo {
        WorkspaceInfo {
            id: workspace_id(&self.workspace),
            path: self.workspace.clone(),
        }
    }

    pub(crate) fn replacing_agent(
        &self,
        expected_revision: u64,
        composition: AgentComposition,
        gateway: &GatewayConfig,
        state_dir: &Path,
        tls: Option<&TlsConfig>,
    ) -> Result<Self> {
        if expected_revision != self.agent.revision {
            return Err(Error::Config(format!(
                "configuration revision changed from {expected_revision} to {}",
                self.agent.revision
            )));
        }
        let mut next = self.clone();
        next.agent = VersionedAgentConfig {
            revision: self
                .agent
                .revision
                .checked_add(1)
                .ok_or_else(|| Error::Config("configuration revision overflow".into()))?,
            config: composition,
        };
        next.validate(state_dir, tls)?;
        gateway.validate_provider_selection(&next.agent.config.provider)?;
        Ok(next)
    }

    fn validate(&self, state_dir: &Path, tls: Option<&TlsConfig>) -> Result<()> {
        if self.version != CHAT_SPEC_VERSION {
            return Err(Error::Config(format!(
                "unsupported chat configuration version {}",
                self.version
            )));
        }
        if self.agent.revision == 0 {
            return Err(Error::Config(
                "chat configuration revision must be positive".into(),
            ));
        }
        let workspace = validate_chat_workspace(&self.workspace, state_dir, tls)?;
        if workspace != self.workspace {
            return Err(Error::Config(
                "chat workspace must use its canonical path".into(),
            ));
        }
        validate_agent_composition(&self.agent.config)
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

impl CloudflareConfig {
    /// Validates and normalizes a stable public hostname.
    pub fn named(hostname: &str) -> Result<Self> {
        let hostname = hostname.trim().to_ascii_lowercase();
        let config = Self::Named { hostname };
        config.validate()?;
        Ok(config)
    }

    /// Returns the stable endpoint when one exists before startup.
    #[must_use]
    pub fn endpoint(&self) -> Option<String> {
        self.hostname().map(|hostname| format!("wss://{hostname}"))
    }

    /// Returns the stable hostname when this is a named tunnel.
    #[must_use]
    pub fn hostname(&self) -> Option<&str> {
        match self {
            Self::Quick => None,
            Self::Named { hostname } => Some(hostname),
        }
    }

    /// Validates one tunnel-scoped connector token without retaining it.
    pub fn validate_token(token: &str) -> Result<()> {
        validate_cloudflare_token(token).map(|_| ())
    }

    fn validate(&self) -> Result<()> {
        if let Self::Named { hostname } = self
            && (hostname.len() > 253
                || !hostname.is_ascii()
                || hostname != &hostname.to_ascii_lowercase()
                || !hostname.contains('.')
                || !hostname.split('.').all(valid_hostname_label))
        {
            return Err(invalid_cloudflare_hostname());
        }
        Ok(())
    }
}

impl ConfigStore {
    /// Initializes an owner-only state directory and new config file.
    pub fn initialize(
        state_dir: PathBuf,
        listen: SocketAddr,
        tls: Option<TlsConfig>,
    ) -> Result<(Self, GatewayConfig)> {
        let config = GatewayConfig::new(listen, tls)?;
        let state_dir = prepare_state_dir(state_dir)?;
        let store = Self::at(state_dir);
        store.save_with_mode(&config, true)?;
        Ok((store, config))
    }

    /// Initializes state for an account-free Cloudflare Quick Tunnel.
    pub fn initialize_quick_cloudflare(
        state_dir: PathBuf,
        listen: SocketAddr,
    ) -> Result<(Self, GatewayConfig)> {
        Self::initialize_cloudflare(state_dir, listen, CloudflareConfig::Quick, None)
    }

    /// Initializes state for one user-owned Cloudflare Tunnel.
    pub fn initialize_named_cloudflare(
        state_dir: PathBuf,
        listen: SocketAddr,
        hostname: &str,
        token: &str,
    ) -> Result<(Self, GatewayConfig)> {
        Self::initialize_cloudflare(
            state_dir,
            listen,
            CloudflareConfig::named(hostname)?,
            Some(validate_cloudflare_token(token)?),
        )
    }

    fn initialize_cloudflare(
        state_dir: PathBuf,
        listen: SocketAddr,
        cloudflare: CloudflareConfig,
        token: Option<&str>,
    ) -> Result<(Self, GatewayConfig)> {
        let config = GatewayConfig::new_cloudflare(listen, cloudflare)?;
        let state_dir = prepare_state_dir(state_dir)?;
        let store = Self::at(state_dir);
        let result = token
            .map_or(Ok(()), |token| store.save_cloudflare_token(token))
            .and_then(|()| store.save_with_mode(&config, true));
        if let Err(error) = result {
            fs::remove_dir_all(&store.state_dir).map_err(|cleanup| {
                Error::Config(format!(
                    "{error}; failed to remove incomplete gateway state at {}: {cleanup}",
                    store.state_dir.display()
                ))
            })?;
            return Err(error);
        }
        Ok((store, config))
    }

    /// Opens and validates persisted gateway configuration.
    pub fn open(state_dir: PathBuf) -> Result<(Self, GatewayConfig)> {
        let state_dir = fs::canonicalize(state_dir)?;
        validate_private_state_dir(&state_dir)?;
        let store = Self::at(state_dir);
        let mut file = fs::File::open(&store.path)?;
        let mut contents = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut contents)?;
        if u64::try_from(contents.len()).unwrap_or(u64::MAX) > MAX_CONFIG_BYTES {
            return Err(Error::Config("gateway configuration is too large".into()));
        }
        let config: GatewayConfig = toml::from_slice(&contents).map_err(|error| {
            Error::Config(format!(
                "gateway state at {} is incompatible with this release; remove that directory and run `horus` again: {error}",
                store.state_dir.display()
            ))
        })?;
        store.validate_config(&config)?;
        Ok((store, config))
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

    /// Returns the owner-only Cloudflare connector-token path.
    #[must_use]
    pub fn cloudflare_token_path(&self) -> PathBuf {
        self.state_dir.join(CLOUDFLARE_TOKEN_FILE)
    }

    fn at(state_dir: PathBuf) -> Self {
        let path = state_dir.join(CONFIG_FILE);
        Self { state_dir, path }
    }

    fn save_with_mode(&self, config: &GatewayConfig, create_new: bool) -> Result<()> {
        self.validate_config(config)?;
        let config = toml::to_string_pretty(config).map_err(|error| {
            Error::Config(format!("cannot encode gateway configuration: {error}"))
        })?;
        let contents = config;
        if u64::try_from(contents.len()).unwrap_or(u64::MAX) > MAX_CONFIG_BYTES {
            return Err(Error::Config("gateway configuration is too large".into()));
        }
        let mut file = tempfile::NamedTempFile::new_in(&self.state_dir)?;
        #[cfg(unix)]
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(contents.as_bytes())?;
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
        if matches!(
            config.cloudflare.as_ref(),
            Some(CloudflareConfig::Named { .. })
        ) {
            load_cloudflare_token(&self.cloudflare_token_path())?;
        }
        Ok(())
    }

    fn save_cloudflare_token(&self, token: &str) -> Result<()> {
        let token = validate_cloudflare_token(token)?;
        let mut file = tempfile::NamedTempFile::new_in(&self.state_dir)?;
        #[cfg(unix)]
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(token.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist_noclobber(self.cloudflare_token_path())
            .map_err(|error| error.error)?;
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

/// Loads a connector token from an owner-only regular file without exposing its contents.
pub fn load_cloudflare_token(path: &Path) -> Result<String> {
    #[cfg(unix)]
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| -> Error {
            if error.raw_os_error() == Some(nix::libc::ELOOP) {
                invalid_cloudflare_token_file()
            } else {
                error.into()
            }
        })?;
    #[cfg(not(unix))]
    let file = {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file() {
            return Err(invalid_cloudflare_token_file());
        }
        fs::File::open(path)?
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(invalid_cloudflare_token_file());
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::Config(
            "Cloudflare tunnel token file must not be accessible by group or others (use mode 0600)"
                .into(),
        ));
    }
    if metadata.len() > MAX_CLOUDFLARE_TOKEN_BYTES as u64 {
        return Err(invalid_cloudflare_token());
    }
    let mut contents = String::new();
    file.take(MAX_CLOUDFLARE_TOKEN_BYTES as u64 + 1)
        .read_to_string(&mut contents)?;
    let token = validate_cloudflare_token(&contents)?;
    Ok(token.to_owned())
}

/// Validates the complete frontend-writable agent composition.
pub fn validate_agent_composition(config: &AgentComposition) -> Result<()> {
    if config.max_model_steps == 0 {
        return Err(Error::Config("maximum model steps must be positive".into()));
    }
    if config.system_prompt.trim().is_empty()
        || config.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES
    {
        return Err(Error::Config(format!(
            "system prompt must be 1–{MAX_SYSTEM_PROMPT_BYTES} bytes"
        )));
    }
    validate_provider_config(&config.provider)?;
    crate::middleware_manifest::validate(&config.middleware)
}

fn validate_provider_config(config: &ProviderConfig) -> Result<()> {
    if config.provider.trim().is_empty() || config.provider.len() > 256 {
        return Err(Error::Config("provider ID must be 1–256 bytes".into()));
    }
    if config.model.trim().is_empty() || config.model.len() > 1024 {
        return Err(Error::Config("model must be 1–1024 bytes".into()));
    }
    let definition = provider(&config.provider)?;
    definition.build_config_is_valid(
        &config.model,
        config.base_url.as_deref(),
        config.reasoning_effort.as_deref(),
        config.web_search,
    )?;
    Ok(())
}

fn validate_configured_provider(configured: &ConfiguredProvider) -> Result<()> {
    validate_provider_config(&configured.selection)?;
    let definition = provider(&configured.selection.provider)?;
    if definition.models().is_empty() {
        validate_model_ids(&configured.model_ids)?;
        validate_reasoning_efforts(&configured.reasoning_efforts)?;
    } else if !configured.model_ids.is_empty() || !configured.reasoning_efforts.is_empty() {
        return Err(Error::Config(format!(
            "provider `{}` uses its advertised model and reasoning catalogs",
            configured.selection.provider
        )));
    }
    validate_configured_provider_selection(configured, &configured.selection)
}

fn validate_configured_provider_selection(
    configured: &ConfiguredProvider,
    selection: &ProviderConfig,
) -> Result<()> {
    if selection.provider != configured.selection.provider {
        return Err(Error::Config(
            "provider selection does not match its configured provider entry".into(),
        ));
    }
    let definition = provider(&selection.provider)?;
    if !definition.models().is_empty() {
        return Ok(());
    }
    if !configured.model_ids.contains(&selection.model) {
        return Err(Error::Config(format!(
            "provider `{}` selection model is not in its configured model catalog",
            selection.provider
        )));
    }
    let effort = effective_reasoning_effort(definition, configured, selection);
    if !effort.is_none_or(|effort| {
        configured
            .reasoning_efforts
            .iter()
            .any(|item| item == effort)
    }) {
        return Err(Error::Config(format!(
            "provider `{}` selection reasoning effort is not in its configured reasoning catalog",
            selection.provider
        )));
    }
    Ok(())
}

fn validate_custom_model_route_count(
    configured_providers: &BTreeMap<String, ConfiguredProvider>,
) -> Result<()> {
    let mut routes = BTreeSet::new();
    for configured in configured_providers.values() {
        if !provider(&configured.selection.provider)?
            .models()
            .is_empty()
        {
            continue;
        }
        for model in &configured.model_ids {
            if configured.reasoning_efforts.is_empty() {
                routes.insert(model_route_id(&configured.selection.provider, model, None));
                continue;
            }
            for effort in &configured.reasoning_efforts {
                if !routes.insert(model_route_id(
                    &configured.selection.provider,
                    model,
                    Some(effort),
                )) {
                    return Err(Error::Config(
                        "custom model and reasoning catalogs generate an ambiguous route".into(),
                    ));
                }
            }
        }
    }
    if routes.len() > MAX_CUSTOM_MODEL_ROUTES {
        return Err(Error::Config(format!(
            "custom provider catalogs may generate at most {MAX_CUSTOM_MODEL_ROUTES} model routes"
        )));
    }
    Ok(())
}

fn validate_model_ids(model_ids: &[String]) -> Result<()> {
    validate_catalog_entries(model_ids, "model IDs", "model ID")
}

fn validate_reasoning_efforts(reasoning_efforts: &[String]) -> Result<()> {
    if reasoning_efforts.is_empty() {
        return Ok(());
    }
    validate_catalog_entries(reasoning_efforts, "reasoning efforts", "reasoning effort")
}

fn validate_catalog_entries(
    entries: &[String],
    plural_name: &str,
    singular_name: &str,
) -> Result<()> {
    if entries.is_empty() || entries.len() > MAX_PROVIDER_CATALOG_ENTRIES {
        return Err(Error::Config(format!(
            "{plural_name} must contain 1–{MAX_PROVIDER_CATALOG_ENTRIES} entries"
        )));
    }
    let mut seen = BTreeSet::new();
    let mut bytes = 0_usize;
    for entry in entries {
        if entry.is_empty()
            || entry.len() > MAX_PROVIDER_CATALOG_ENTRY_BYTES
            || entry != entry.trim()
        {
            return Err(Error::Config(format!(
                "each {singular_name} must be canonical and 1–{MAX_PROVIDER_CATALOG_ENTRY_BYTES} bytes"
            )));
        }
        if entry.chars().any(char::is_control) {
            return Err(Error::Config(format!(
                "each {singular_name} must not contain control characters"
            )));
        }
        if !seen.insert(entry.as_str()) {
            return Err(Error::Config(format!(
                "duplicate {singular_name} `{entry}`"
            )));
        }
        bytes = bytes
            .checked_add(entry.len())
            .ok_or_else(|| Error::Config(format!("{singular_name} catalog is too large")))?;
    }
    if bytes > MAX_PROVIDER_CATALOG_BYTES {
        return Err(Error::Config(format!(
            "{plural_name} are limited to {MAX_PROVIDER_CATALOG_BYTES} bytes in total"
        )));
    }
    Ok(())
}

pub(crate) fn model_route_id(provider: &str, model: &str, effort: Option<&str>) -> String {
    format!("{provider}::{model}::{}", effort.unwrap_or("default"))
}

pub(crate) fn effective_reasoning_effort<'a>(
    definition: &ProviderDefinition,
    configured: &'a ConfiguredProvider,
    selection: &'a ProviderConfig,
) -> Option<&'a str> {
    selection
        .reasoning_effort
        .as_deref()
        .or_else(|| {
            definition
                .model(&selection.model)
                .and_then(|model| model.default_reasoning)
        })
        .or_else(|| configured.reasoning_efforts.first().map(String::as_str))
}

fn valid_hostname_label(label: &str) -> bool {
    (1..=63).contains(&label.len())
        && label
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && label
            .bytes()
            .next_back()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn invalid_cloudflare_hostname() -> Error {
    Error::Config(
        "Cloudflare hostname must be a DNS name such as horus.example.com, without a scheme, path, or port"
            .into(),
    )
}

fn validate_cloudflare_token(token: &str) -> Result<&str> {
    let token = token.trim();
    if token.is_empty()
        || token.len() > MAX_CLOUDFLARE_TOKEN_BYTES
        || !token.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(invalid_cloudflare_token());
    }
    Ok(token)
}

fn invalid_cloudflare_token() -> Error {
    Error::Config(format!(
        "Cloudflare tunnel token must be 1–{MAX_CLOUDFLARE_TOKEN_BYTES} visible ASCII bytes"
    ))
}

fn invalid_cloudflare_token_file() -> Error {
    Error::Config("Cloudflare tunnel token must be stored in a regular file".into())
}

fn validate_chat_workspace(
    path: &Path,
    state_dir: &Path,
    tls: Option<&TlsConfig>,
) -> Result<PathBuf> {
    let path = fs::canonicalize(path)?;
    if !path.is_dir() || path.parent().is_none() {
        return Err(Error::Config(
            "workspace must be an existing non-root directory".into(),
        ));
    }
    let state_dir = fs::canonicalize(state_dir)?;
    if path.starts_with(&state_dir) || state_dir.starts_with(&path) {
        return Err(Error::Config(
            "gateway state directory and chat workspace must not overlap".into(),
        ));
    }
    if tls.is_some_and(|tls| {
        fs::canonicalize(&tls.private_key).is_ok_and(|key| key.starts_with(&path))
    }) {
        return Err(Error::Config(
            "TLS private key must be stored outside every chat workspace".into(),
        ));
    }
    Ok(path)
}

fn prepare_state_dir(path: PathBuf) -> Result<PathBuf> {
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

fn validate_private_state_dir(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_dir() {
        return Err(Error::Config(
            "gateway state path must be a directory".into(),
        ));
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::Config(
            "gateway state directory must not be accessible by group or others (use mode 0700)"
                .into(),
        ));
    }
    Ok(())
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

impl UsageHistory {
    fn observe(&mut self, usage: &TokenUsage, now: SystemTime) -> Result<bool> {
        validate_usage(usage)?;
        if usage == &TokenUsage::default() {
            return Ok(false);
        }
        let day = unix_day(now)?;
        let mut bucket = self.days.get(&day).cloned().unwrap_or_default();
        bucket
            .checked_add(usage)
            .ok_or_else(|| Error::Config("daily token usage overflow".into()))?;
        self.days.insert(day, bucket);
        let first_day = day.saturating_sub(USAGE_HISTORY_DAYS - 1);
        self.days.retain(|stored, _| *stored >= first_day);
        Ok(true)
    }
}

fn unix_day(now: SystemTime) -> Result<u64> {
    Ok(now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Config("system clock is before the Unix epoch".into()))?
        .as_secs()
        / SECONDS_PER_DAY)
}

pub(crate) fn usage_delta(
    current: &TokenUsage,
    previous: &TokenUsage,
) -> Result<Option<TokenUsage>> {
    validate_usage(current)?;
    Ok(checked_usage_delta(current, previous).filter(usage_nonnegative))
}

fn checked_usage_delta(current: &TokenUsage, previous: &TokenUsage) -> Option<TokenUsage> {
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

    fn test_agent() -> VersionedAgentConfig {
        VersionedAgentConfig {
            revision: 1,
            config: AgentComposition::default(),
        }
    }

    #[test]
    fn gateway_config_is_machine_scoped() {
        let config = GatewayConfig::new(DEFAULT_LISTEN, None).expect("gateway config");
        let serialized = serde_json::to_value(config).expect("serialize gateway config");

        assert!(serialized.get("workspace").is_none());
        assert!(serialized["cloudflare"].is_null());
        assert!(serialized["default_agent"].is_null());
        assert_eq!(serialized["configured_providers"], serde_json::json!({}));
        assert!(serialized["usage"].get("sessions").is_none());
    }

    #[test]
    fn cloudflare_config_normalizes_a_dns_hostname() {
        let config = CloudflareConfig::named("  Horus.Example.com ").expect("Cloudflare config");

        assert_eq!(
            config.endpoint().as_deref(),
            Some("wss://horus.example.com")
        );
    }

    #[test]
    fn cloudflare_config_rejects_a_url_instead_of_a_hostname() {
        let error = CloudflareConfig::named("wss://horus.example.com/path")
            .expect_err("URL must be rejected");

        assert!(
            error
                .to_string()
                .contains("without a scheme, path, or port")
        );
    }

    #[test]
    fn quick_cloudflare_config_round_trips_without_a_token() {
        let root = tempfile::tempdir().expect("temporary directory");
        let state = root.path().join("state");
        let (store, _) = ConfigStore::initialize_quick_cloudflare(state.clone(), DEFAULT_LISTEN)
            .expect("initialize quick tunnel");

        let contents = fs::read_to_string(state.join(CONFIG_FILE)).expect("gateway config");
        let (_, opened) = ConfigStore::open(state).expect("open quick tunnel config");

        assert!(
            contents.contains("mode = \"quick\"")
                && !store.cloudflare_token_path().exists()
                && opened.cloudflare == Some(CloudflareConfig::Quick)
        );
    }

    #[test]
    fn gateway_config_rejects_v12_without_migration() {
        let root = tempfile::tempdir().expect("temporary directory");
        let state = root.path().join("state");
        ConfigStore::initialize(state.clone(), DEFAULT_LISTEN, None).expect("initialize gateway");
        let path = state.join(CONFIG_FILE);
        let contents = fs::read_to_string(&path)
            .expect("read gateway config")
            .replacen("version = 13", "version = 12", 1);
        fs::write(&path, contents).expect("write v12 config");

        let error = ConfigStore::open(state).expect_err("v12 must be rejected");

        assert!(
            error
                .to_string()
                .contains("unsupported gateway config version 12")
        );
    }

    #[test]
    fn generated_toml_round_trips_manifest_settings() {
        let root = tempfile::tempdir().expect("temporary directory");
        let state = root.path().join("state");
        let (store, config) =
            ConfigStore::initialize(state.clone(), DEFAULT_LISTEN, None).expect("initialize state");
        let mut config = config
            .registering_provider(AgentComposition::default().provider, Vec::new(), Vec::new())
            .expect("register provider");
        let usage = TokenUsage {
            input_tokens: 7,
            total_tokens: 7,
            ..TokenUsage::default()
        };
        config
            .usage
            .observe(
                &usage,
                UNIX_EPOCH + std::time::Duration::from_secs(2 * SECONDS_PER_DAY),
            )
            .expect("record usage");
        store.save(&config).expect("save config");

        let contents = fs::read_to_string(state.join(CONFIG_FILE)).expect("read config");
        let (_, restored) = ConfigStore::open(state).expect("open config");

        assert!(contents.starts_with("version = 13"));
        assert!(contents.contains("max_model_steps = 256"));
        assert!(contents.contains("[default_agent.config.middleware.settings.context_offloading]"));
        assert!(contents.contains("[default_agent.config.middleware.settings.sessions]"));
        assert_eq!(restored, config);
    }

    #[test]
    fn provider_registration_never_silently_changes_existing_defaults() {
        let config = GatewayConfig::new(DEFAULT_LISTEN, None).expect("gateway config");
        let kimi = ProviderConfig {
            provider: "kimi".into(),
            model: "kimi-k3".into(),
            base_url: None,
            reasoning_effort: Some("max".into()),
            web_search: horus::backend::model::provider::HostedWebSearch::Off,
        };
        let first = config
            .registering_provider(kimi.clone(), Vec::new(), Vec::new())
            .expect("register Kimi");
        let openrouter = ProviderConfig {
            provider: "openrouter".into(),
            model: "openrouter/pareto-code".into(),
            base_url: None,
            reasoning_effort: None,
            web_search: horus::backend::model::provider::HostedWebSearch::Off,
        };
        let second = first
            .registering_provider(
                openrouter.clone(),
                vec![openrouter.model.clone(), "anthropic/claude-opus-4.1".into()],
                Vec::new(),
            )
            .expect("register OpenRouter");

        assert_eq!(second.configured_providers["kimi"].selection, kimi);
        assert_eq!(
            second.configured_providers["openrouter"].selection,
            openrouter
        );
        assert_eq!(second.configured_providers["openrouter"].model_ids.len(), 2);
        assert_eq!(
            second
                .default_agent
                .as_ref()
                .expect("gateway default")
                .config
                .provider
                .provider,
            "kimi"
        );

        let mut updated = kimi.clone();
        updated.model = "kimi-k2.7-code".into();
        updated.reasoning_effort = None;
        let third = second
            .registering_provider(updated.clone(), Vec::new(), Vec::new())
            .expect("update registered provider");
        assert_eq!(third.configured_providers["kimi"].selection, updated);
        let default = third.default_agent.expect("preserved default");
        assert_eq!(default.revision, 1);
        assert_eq!(default.config.provider, kimi);
    }

    #[test]
    fn configured_custom_provider_keeps_its_endpoint_and_model() {
        let selection = ProviderConfig {
            provider: "responses".into(),
            model: "vendor/model-opaque".into(),
            base_url: Some("https://example.com/v1".into()),
            reasoning_effort: Some("provider-defined".into()),
            web_search: horus::backend::model::provider::HostedWebSearch::Off,
        };
        let config = GatewayConfig::new(DEFAULT_LISTEN, None)
            .expect("gateway config")
            .registering_provider(
                selection.clone(),
                vec![selection.model.clone()],
                vec!["provider-defined".into()],
            )
            .expect("register custom provider");

        assert_eq!(
            config.configured_providers["responses"].selection,
            selection
        );
        assert_eq!(
            config.configured_providers["responses"].reasoning_efforts,
            ["provider-defined"]
        );
        assert_eq!(
            config
                .default_agent
                .expect("gateway default")
                .config
                .provider,
            selection
        );
    }

    #[test]
    fn custom_provider_registration_validates_its_model_catalog() {
        let selection = ProviderConfig {
            provider: "openrouter".into(),
            model: "anthropic/claude-sonnet-4".into(),
            base_url: None,
            reasoning_effort: None,
            web_search: horus::backend::model::provider::HostedWebSearch::Off,
        };
        let config = GatewayConfig::new(DEFAULT_LISTEN, None).expect("gateway config");

        let missing = config
            .registering_provider(selection.clone(), Vec::new(), Vec::new())
            .expect_err("custom catalog must not be empty");
        let duplicate = config
            .registering_provider(
                selection.clone(),
                vec![selection.model.clone(), selection.model.clone()],
                Vec::new(),
            )
            .expect_err("custom catalog IDs must be unique");
        let padded = config
            .registering_provider(
                selection.clone(),
                vec![" anthropic/claude-sonnet-4".into()],
                Vec::new(),
            )
            .expect_err("custom catalog IDs must be canonical");
        let duplicate_reasoning = config
            .registering_provider(
                selection.clone(),
                vec![selection.model.clone()],
                vec!["high".into(), "high".into()],
            )
            .expect_err("custom reasoning efforts must be unique");
        let mut missing_reasoning = selection;
        missing_reasoning.reasoning_effort = Some("high".into());
        let missing_reasoning = config
            .registering_provider(
                missing_reasoning,
                vec!["anthropic/claude-sonnet-4".into()],
                vec!["medium".into()],
            )
            .expect_err("selected custom reasoning must be configured");

        assert!(missing.to_string().contains("1–64 entries"));
        assert!(duplicate.to_string().contains("duplicate model ID"));
        assert!(padded.to_string().contains("must be canonical"));
        assert!(
            duplicate_reasoning
                .to_string()
                .contains("duplicate reasoning effort")
        );
        assert!(
            missing_reasoning
                .to_string()
                .contains("configured reasoning catalog")
        );
    }

    #[test]
    fn custom_provider_catalogs_accept_opaque_ids_but_reject_ambiguous_routes() {
        let config = GatewayConfig::new(DEFAULT_LISTEN, None).expect("gateway config");
        config
            .registering_provider(
                ProviderConfig {
                    provider: "openrouter".into(),
                    model: "vendor::model".into(),
                    base_url: None,
                    reasoning_effort: None,
                    web_search: horus::backend::model::provider::HostedWebSearch::Off,
                },
                vec!["vendor::model".into()],
                Vec::new(),
            )
            .expect("opaque model ID");
        let collision = config
            .registering_provider(
                ProviderConfig {
                    provider: "openrouter".into(),
                    model: "vendor:".into(),
                    base_url: None,
                    reasoning_effort: Some("high".into()),
                    web_search: horus::backend::model::provider::HostedWebSearch::Off,
                },
                vec!["vendor:".into(), "vendor".into()],
                vec!["high".into(), ":high".into()],
            )
            .expect_err("distinct catalog pairs must not share a route");

        assert!(collision.to_string().contains("ambiguous route"));
    }

    #[test]
    fn custom_provider_catalogs_bound_the_total_generated_routes() {
        let models = (0..8)
            .map(|index| format!("vendor/model-{index}"))
            .collect::<Vec<_>>();
        let efforts = (0..8)
            .map(|index| format!("effort-{index}"))
            .collect::<Vec<_>>();
        let config = GatewayConfig::new(DEFAULT_LISTEN, None)
            .expect("gateway config")
            .registering_provider(
                ProviderConfig {
                    provider: "openrouter".into(),
                    model: models[0].clone(),
                    base_url: None,
                    reasoning_effort: Some(efforts[0].clone()),
                    web_search: horus::backend::model::provider::HostedWebSearch::Off,
                },
                models,
                efforts,
            )
            .expect("64 custom routes");

        let error = config
            .registering_provider(
                ProviderConfig {
                    provider: "responses".into(),
                    model: "local-model".into(),
                    base_url: Some("http://127.0.0.1:11434/v1".into()),
                    reasoning_effort: None,
                    web_search: horus::backend::model::provider::HostedWebSearch::Off,
                },
                vec!["local-model".into()],
                Vec::new(),
            )
            .expect_err("65 total custom routes must fail");

        assert!(error.to_string().contains("at most 64 model routes"));
    }

    #[test]
    fn provider_registration_rejects_a_catalog_that_invalidates_the_current_default() {
        let model = "vendor/model".to_string();
        let config = GatewayConfig::new(DEFAULT_LISTEN, None)
            .expect("gateway config")
            .registering_provider(
                ProviderConfig {
                    provider: "openrouter".into(),
                    model: model.clone(),
                    base_url: None,
                    reasoning_effort: Some("high".into()),
                    web_search: horus::backend::model::provider::HostedWebSearch::Off,
                },
                vec![model.clone()],
                vec!["high".into(), "medium".into()],
            )
            .expect("register provider");

        let error = config
            .registering_provider(
                ProviderConfig {
                    provider: "openrouter".into(),
                    model: model.clone(),
                    base_url: None,
                    reasoning_effort: Some("medium".into()),
                    web_search: horus::backend::model::provider::HostedWebSearch::Off,
                },
                vec![model],
                vec!["medium".into()],
            )
            .expect_err("updated catalog must preserve current default membership");

        assert!(
            error
                .to_string()
                .contains("selection reasoning effort is not in its configured reasoning catalog")
        );
    }

    #[test]
    fn default_and_persisted_config_validate_custom_reasoning_membership() {
        let model = "vendor/model".to_string();
        let config = GatewayConfig::new(DEFAULT_LISTEN, None)
            .expect("gateway config")
            .registering_provider(
                ProviderConfig {
                    provider: "openrouter".into(),
                    model: model.clone(),
                    base_url: None,
                    reasoning_effort: Some("high".into()),
                    web_search: horus::backend::model::provider::HostedWebSearch::Off,
                },
                vec![model],
                vec!["high".into(), "medium".into()],
            )
            .expect("register provider");
        let mut replacement = config
            .default_agent
            .as_ref()
            .expect("default")
            .config
            .clone();
        replacement.provider.reasoning_effort = Some("low".into());

        let replace_error = config
            .replacing_default_agent(1, replacement)
            .expect_err("default reasoning must be in the catalog");
        let mut persisted = config;
        persisted
            .default_agent
            .as_mut()
            .expect("default")
            .config
            .provider
            .reasoning_effort = Some("low".into());
        let persisted_error = persisted
            .validate()
            .expect_err("persisted default reasoning must be in the catalog");

        assert!(replace_error.to_string().contains("reasoning effort"));
        assert!(persisted_error.to_string().contains("reasoning effort"));
    }

    #[test]
    fn chat_replacement_rejects_out_of_catalog_model_and_reasoning() {
        let model = "vendor/model".to_string();
        let gateway = GatewayConfig::new(DEFAULT_LISTEN, None)
            .expect("gateway config")
            .registering_provider(
                ProviderConfig {
                    provider: "openrouter".into(),
                    model: model.clone(),
                    base_url: None,
                    reasoning_effort: Some("high".into()),
                    web_search: horus::backend::model::provider::HostedWebSearch::Off,
                },
                vec![model],
                vec!["high".into(), "medium".into()],
            )
            .expect("register provider");
        let workspace = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        let chat = ChatSpec::new(
            workspace.path(),
            gateway.default_agent.clone().expect("default"),
            state.path(),
            None,
        )
        .expect("chat spec");
        let mut invalid_model = chat.agent.config.clone();
        invalid_model.provider.model = "vendor/unknown".into();
        let mut invalid_reasoning = chat.agent.config.clone();
        invalid_reasoning.provider.reasoning_effort = Some("low".into());

        let model_error = chat
            .replacing_agent(1, invalid_model, &gateway, state.path(), None)
            .expect_err("chat model must be in the catalog");
        let reasoning_error = chat
            .replacing_agent(1, invalid_reasoning, &gateway, state.path(), None)
            .expect_err("chat reasoning must be in the catalog");

        assert!(model_error.to_string().contains("selection model"));
        assert!(reasoning_error.to_string().contains("reasoning effort"));
    }

    #[test]
    fn saving_defaults_is_revisioned_and_does_not_change_existing_chat_specs() {
        let registered = GatewayConfig::new(DEFAULT_LISTEN, None)
            .expect("gateway config")
            .registering_provider(AgentComposition::default().provider, Vec::new(), Vec::new())
            .expect("register provider");
        let workspace = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        let chat = ChatSpec::new(
            workspace.path(),
            registered.default_agent.clone().expect("default"),
            state.path(),
            None,
        )
        .expect("chat spec");
        let mut replacement = registered
            .default_agent
            .as_ref()
            .expect("default")
            .config
            .clone();
        replacement.middleware.set_enabled("tasks", true);

        let updated = registered
            .replacing_default_agent(1, replacement.clone())
            .expect("replace defaults");

        assert_eq!(updated.default_agent.as_ref().expect("default").revision, 2);
        assert_eq!(
            updated.default_agent.as_ref().expect("default").config,
            replacement
        );
        assert_eq!(chat.agent.revision, 1);
        assert!(
            registered
                .replacing_default_agent(2, AgentComposition::default())
                .expect_err("stale revision")
                .to_string()
                .contains("revision changed")
        );
    }

    #[test]
    fn non_loopback_listener_requires_tls() {
        let listen = "0.0.0.0:8741".parse().expect("listen address");

        let error = GatewayConfig::new(listen, None).expect_err("remote plaintext must fail");

        assert!(error.to_string().contains("require a TLS certificate"));
    }

    #[test]
    fn listener_rejects_port_zero() {
        let listen = "127.0.0.1:0".parse().expect("listen address");

        let error = GatewayConfig::new(listen, None).expect_err("port zero must fail");

        assert!(error.to_string().contains("greater than zero"));
    }

    #[test]
    fn invalid_configuration_does_not_create_gateway_state() {
        let root = tempfile::tempdir().expect("temporary directory");
        let state = root.path().join("state");
        let listen = "127.0.0.1:0".parse().expect("listen address");

        let error = ConfigStore::initialize(state.clone(), listen, None)
            .expect_err("invalid config must fail");

        assert!(error.to_string().contains("greater than zero"));
        assert!(!state.exists());
    }

    #[test]
    fn incompatible_state_explains_the_required_reset() {
        let root = tempfile::tempdir().expect("temporary directory");
        let state = root.path().join("state");
        let (_, config) =
            ConfigStore::initialize(state.clone(), DEFAULT_LISTEN, None).expect("initialize state");
        let mut legacy = serde_json::to_value(config).expect("serialize config");
        legacy
            .as_object_mut()
            .expect("config object")
            .insert("workspace".into(), serde_json::json!(root.path()));
        fs::write(
            state.join(CONFIG_FILE),
            serde_json::to_vec(&legacy).expect("encode legacy config"),
        )
        .expect("write legacy config");

        let error = ConfigStore::open(state.clone()).expect_err("legacy state must fail");

        assert!(error.to_string().contains("incompatible with this release"));
        assert!(error.to_string().contains(&state.display().to_string()));
    }

    #[test]
    fn chats_keep_canonical_specs_for_different_worktrees() {
        let root = tempfile::tempdir().expect("root");
        let state = root.path().join("state");
        let worktrees = root.path().join("worktrees");
        let first = worktrees.join("first");
        let second = worktrees.join("second");
        fs::create_dir(&state).expect("state");
        fs::create_dir_all(&first).expect("first worktree");
        fs::create_dir(&second).expect("second worktree");
        let agent = test_agent();

        let first_spec =
            ChatSpec::new(&first.join("..").join("first"), agent.clone(), &state, None)
                .expect("first chat spec");
        let second_spec = ChatSpec::new(&second, agent, &state, None).expect("second chat spec");

        assert_eq!(
            first_spec.workspace,
            fs::canonicalize(first).expect("first")
        );
        assert_eq!(
            second_spec.workspace,
            fs::canonicalize(second).expect("second")
        );
        assert_ne!(first_spec.workspace_info(), second_spec.workspace_info());
    }

    #[test]
    fn chat_specs_reject_both_state_overlap_directions() {
        let root = tempfile::tempdir().expect("root");
        let workspace_parent = root.path().join("workspace-parent");
        let state_inside = workspace_parent.join("state");
        let state_parent = root.path().join("state-parent");
        let workspace_inside = state_parent.join("workspace");
        fs::create_dir_all(&state_inside).expect("nested state");
        fs::create_dir_all(&workspace_inside).expect("nested workspace");
        let agent = test_agent();

        let state_inside_error =
            ChatSpec::new(&workspace_parent, agent.clone(), &state_inside, None)
                .expect_err("state inside workspace must fail");
        let workspace_inside_error = ChatSpec::new(&workspace_inside, agent, &state_parent, None)
            .expect_err("workspace inside state must fail");

        assert!(state_inside_error.to_string().contains("must not overlap"));
        assert!(
            workspace_inside_error
                .to_string()
                .contains("must not overlap")
        );
    }

    #[test]
    fn chat_spec_rejects_a_tls_private_key_inside_its_workspace() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        fs::create_dir(&workspace).expect("workspace");
        fs::create_dir(&state).expect("state");
        let certificate = root.path().join("certificate.pem");
        let private_key = workspace.join("private-key.pem");
        fs::write(&certificate, "certificate").expect("certificate");
        fs::write(&private_key, "private key").expect("private key");
        let tls = TlsConfig {
            certificate,
            private_key,
        };
        let agent = test_agent();

        let error = ChatSpec::new(&workspace, agent, &state, Some(&tls))
            .expect_err("workspace TLS key must fail");

        assert!(error.to_string().contains("outside every chat workspace"));
    }

    #[test]
    fn chat_spec_metadata_round_trips_and_revalidates_tampering() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        fs::create_dir(&workspace).expect("workspace");
        fs::create_dir(&state).expect("state");
        let agent = test_agent();
        let spec = ChatSpec::new(&workspace, agent, &state, None).expect("chat spec");
        let mut metadata = spec.metadata().expect("chat metadata");

        assert_eq!(
            ChatSpec::from_metadata(&metadata, &state, None).expect("restore chat spec"),
            spec
        );
        metadata
            .get_mut(CHAT_SPEC_METADATA_KEY)
            .and_then(Value::as_object_mut)
            .expect("chat metadata object")
            .insert(
                "workspace".into(),
                serde_json::to_value(fs::canonicalize(&state).expect("canonical state"))
                    .expect("state path value"),
            );

        let error = ChatSpec::from_metadata(&metadata, &state, None)
            .expect_err("tampered workspace must be revalidated");

        assert!(error.to_string().contains("must not overlap"));
    }

    #[test]
    fn usage_history_aggregates_live_increments() {
        let now = UNIX_EPOCH + std::time::Duration::from_secs(2 * SECONDS_PER_DAY);
        let usage = |tokens| TokenUsage {
            input_tokens: tokens,
            total_tokens: tokens,
            ..TokenUsage::default()
        };
        let mut history = UsageHistory::default();

        assert!(history.observe(&usage(30), now).expect("observe first"));
        assert!(history.observe(&usage(40), now).expect("observe second"));

        assert_eq!(history.days.get(&2), Some(&usage(70)));
    }

    #[test]
    fn config_rejects_an_empty_system_prompt() {
        let mut config = AgentComposition::default();
        config.system_prompt.clear();

        let error = validate_agent_composition(&config).expect_err("empty prompt must fail");

        assert!(error.to_string().contains("system prompt"));
    }

    #[test]
    fn agent_composition_requires_a_positive_model_step_limit() {
        let config = AgentComposition {
            max_model_steps: 0,
            ..AgentComposition::default()
        };

        let error = validate_agent_composition(&config).expect_err("zero limit must fail");

        assert!(error.to_string().contains("maximum model steps"));
    }

    #[test]
    fn agent_composition_has_no_policy_upper_model_step_limit() {
        let config = AgentComposition {
            max_model_steps: u64::MAX,
            ..AgentComposition::default()
        };

        validate_agent_composition(&config).expect("platform maximum must be accepted");
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
        let state_parent = tempfile::tempdir().expect("state parent");
        let state = state_parent.path().join("gateway");
        let listen = "127.0.0.1:8741".parse().expect("listen address");

        let (store, _) =
            ConfigStore::initialize(state.clone(), listen, None).expect("initialize config");

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
    fn cloudflare_token_is_owner_only_and_absent_from_gateway_config() {
        let state_parent = tempfile::tempdir().expect("state parent");
        let state = state_parent.path().join("gateway");
        let (store, _) = ConfigStore::initialize_named_cloudflare(
            state.clone(),
            DEFAULT_LISTEN,
            "horus.example.com",
            "secret-tunnel-token",
        )
        .expect("initialize Cloudflare config");

        let mode = fs::metadata(store.cloudflare_token_path())
            .expect("token metadata")
            .permissions()
            .mode()
            & 0o777;
        let config = fs::read_to_string(state.join(CONFIG_FILE)).expect("gateway config");

        assert_eq!(mode, 0o600);
        assert!(!config.contains("secret-tunnel-token"));
    }

    #[cfg(unix)]
    #[test]
    fn cloudflare_token_loader_rejects_a_symlink() {
        let directory = tempfile::tempdir().expect("token directory");
        let target = directory.path().join("target");
        let link = directory.path().join("token");
        fs::write(&target, "secret-tunnel-token").expect("token");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("token permissions");
        std::os::unix::fs::symlink(target, &link).expect("token symlink");

        let error = load_cloudflare_token(&link).expect_err("symlink must fail");

        assert!(error.to_string().contains("regular file"));
    }

    #[test]
    fn cloudflare_token_loader_rejects_a_nonregular_file() {
        let directory = tempfile::tempdir().expect("token directory");

        let error = load_cloudflare_token(directory.path()).expect_err("directory must fail");

        assert!(error.to_string().contains("regular file"));
    }

    #[cfg(unix)]
    #[test]
    fn opening_cloudflare_state_rejects_a_public_token_file() {
        let state_parent = tempfile::tempdir().expect("state parent");
        let state = state_parent.path().join("gateway");
        let (store, _) = ConfigStore::initialize_named_cloudflare(
            state.clone(),
            DEFAULT_LISTEN,
            "horus.example.com",
            "secret-tunnel-token",
        )
        .expect("initialize Cloudflare config");
        fs::set_permissions(
            store.cloudflare_token_path(),
            fs::Permissions::from_mode(0o644),
        )
        .expect("loosen token permissions");

        let error = ConfigStore::open(state).expect_err("public token file must fail");

        assert!(error.to_string().contains("mode 0600"));
    }

    #[cfg(unix)]
    #[test]
    fn opening_state_rejects_a_public_state_directory() {
        let state_parent = tempfile::tempdir().expect("state parent");
        let state = state_parent.path().join("gateway");
        ConfigStore::initialize(state.clone(), DEFAULT_LISTEN, None).expect("initialize state");
        fs::set_permissions(&state, fs::Permissions::from_mode(0o755))
            .expect("loosen state permissions");

        let error = ConfigStore::open(state).expect_err("public state directory must fail");

        assert!(error.to_string().contains("mode 0700"));
    }

    #[cfg(unix)]
    #[test]
    fn initialization_does_not_repermission_an_existing_directory() {
        let state = tempfile::tempdir().expect("existing state directory");
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o755))
            .expect("state permissions");
        let listen = "127.0.0.1:8741".parse().expect("listen address");

        let error = ConfigStore::initialize(state.path().to_path_buf(), listen, None)
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
