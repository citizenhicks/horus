//! Provider-owned setup metadata and model construction.

use std::any::Any;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde::Serialize;

use super::Model;
use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::protocol::FrontendSymbol;

pub use super::transport::streaming_client;
pub use reqwest::Client as HttpClient;

/// A reasoning choice advertised for one model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReasoningPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

/// A model choice advertised by its backend provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub context_window: i64,
    pub reasoning: &'static [ReasoningPreset],
    pub default_reasoning: Option<&'static str>,
}

/// Hosted search modes a provider may expose.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedWebSearch {
    #[default]
    Off,
    Cached,
    Live,
}

impl HostedWebSearch {
    /// Returns the stable manifest value.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Cached => "cached",
            Self::Live => "live",
        }
    }

    /// Returns the user-facing setup label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Cached => "Cached",
            Self::Live => "Live",
        }
    }
}

/// Fully resolved settings passed to one provider constructor.
pub struct ProviderBuildConfig {
    pub credential: ProviderCredential,
    pub model: String,
    pub base_url: Option<String>,
    pub reasoning_effort: Option<String>,
    pub web_search: HostedWebSearch,
    /// Shared HTTP client; one per assembly keeps provider clones on one pool.
    pub http: HttpClient,
}

/// One provider-owned browser authentication flow.
pub trait BrowserLogin: Send {
    fn url(&self) -> &str;
    fn open_browser(&self);
    fn complete(self: Box<Self>, path: PathBuf) -> BoxFuture<'static, Result<()>>;
}

type BrowserLoginStart = fn() -> BoxFuture<'static, Result<Box<dyn BrowserLogin>>>;

/// One provider-owned device-code authentication flow.
pub trait DeviceLogin: Send {
    fn verification_url(&self) -> &str;
    fn user_code(&self) -> &str;
    fn complete(self: Box<Self>, path: PathBuf) -> BoxFuture<'static, Result<()>>;
}

type DeviceLoginStart = fn() -> BoxFuture<'static, Result<Box<dyn DeviceLogin>>>;

/// Provider-owned browser authentication hooks consumed generically by applications.
pub struct BrowserAuth {
    label: &'static str,
    configured: fn(&Path) -> Result<bool>,
    load: fn(&Path) -> Result<ProviderCredential>,
    start: BrowserLoginStart,
    start_device: Option<DeviceLoginStart>,
}

impl BrowserAuth {
    pub const fn new(
        label: &'static str,
        configured: fn(&Path) -> Result<bool>,
        load: fn(&Path) -> Result<ProviderCredential>,
        start: BrowserLoginStart,
    ) -> Self {
        Self {
            label,
            configured,
            load,
            start,
            start_device: None,
        }
    }

    /// Adds a cross-device login flow for headless provider hosts.
    #[must_use]
    pub const fn with_device_login(mut self, start: DeviceLoginStart) -> Self {
        self.start_device = Some(start);
        self
    }

    #[must_use]
    pub const fn label(&self) -> &'static str {
        self.label
    }

    pub fn configured(&self, path: &Path) -> Result<bool> {
        (self.configured)(path)
    }

    pub fn load(&self, path: &Path) -> Result<ProviderCredential> {
        (self.load)(path)
    }

    pub fn start(&self) -> BoxFuture<'static, Result<Box<dyn BrowserLogin>>> {
        (self.start)()
    }

    /// Reports whether the provider supports cross-device authentication.
    #[must_use]
    pub const fn supports_device_login(&self) -> bool {
        self.start_device.is_some()
    }

    /// Starts a cross-device login without binding a browser callback on the host.
    pub fn start_device(&self) -> BoxFuture<'static, Result<Box<dyn DeviceLogin>>> {
        match self.start_device {
            Some(start) => start(),
            None => Box::pin(async {
                Err(Error::Auth(
                    "provider does not support device-code login".into(),
                ))
            }),
        }
    }
}

/// Authentication required by a provider manifest.
#[derive(Clone, Copy)]
pub enum ProviderAuth {
    ApiKey(&'static str),
    Browser(&'static BrowserAuth),
}

/// Resolved credential passed to one provider constructor.
#[derive(Clone)]
pub enum ProviderCredential {
    ApiKey(String),
    Browser(Arc<dyn Any + Send + Sync>),
}

impl ProviderCredential {
    pub(super) fn into_api_key(self, provider: &str) -> Result<String> {
        match self {
            Self::ApiKey(api_key) => Ok(api_key),
            Self::Browser(_) => Err(Error::Config(format!(
                "provider `{provider}` requires an API key"
            ))),
        }
    }

    pub fn into_browser<T: Any + Send + Sync>(self, provider: &str) -> Result<Arc<T>> {
        match self {
            Self::Browser(credential) => Arc::downcast(credential)
                .map_err(|_| Error::Config(format!("provider `{provider}` received wrong login"))),
            Self::ApiKey(_) => Err(Error::Config(format!(
                "provider `{provider}` requires browser login"
            ))),
        }
    }
}

type ProviderBuilder = fn(ProviderBuildConfig) -> Result<Arc<dyn Model>>;

/// One backend provider's setup manifest and constructor.
pub struct ProviderDefinition {
    id: &'static str,
    label: &'static str,
    symbol: FrontendSymbol,
    description: &'static str,
    auth: ProviderAuth,
    models: &'static [ModelPreset],
    web_search: &'static [HostedWebSearch],
    default_base_url: Option<&'static str>,
    builder: ProviderBuilder,
}

impl ProviderDefinition {
    #[expect(
        clippy::too_many_arguments,
        reason = "a provider manifest keeps its required fields explicit at the registry entry"
    )]
    pub(crate) const fn new(
        id: &'static str,
        label: &'static str,
        symbol: FrontendSymbol,
        description: &'static str,
        auth: ProviderAuth,
        models: &'static [ModelPreset],
        web_search: &'static [HostedWebSearch],
        builder: ProviderBuilder,
    ) -> Self {
        Self {
            id,
            label,
            symbol,
            description,
            auth,
            models,
            web_search,
            default_base_url: None,
            builder,
        }
    }

    pub(crate) const fn with_base_url(mut self, default_base_url: &'static str) -> Self {
        self.default_base_url = Some(default_base_url);
        self
    }

    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    #[must_use]
    pub const fn label(&self) -> &'static str {
        self.label
    }

    #[must_use]
    pub const fn symbol(&self) -> &FrontendSymbol {
        &self.symbol
    }

    #[must_use]
    pub const fn description(&self) -> &'static str {
        self.description
    }

    #[must_use]
    pub const fn auth(&self) -> ProviderAuth {
        self.auth
    }

    #[must_use]
    pub const fn models(&self) -> &'static [ModelPreset] {
        self.models
    }

    #[must_use]
    pub const fn web_search(&self) -> &'static [HostedWebSearch] {
        self.web_search
    }

    #[must_use]
    pub const fn configurable_base_url(&self) -> bool {
        self.default_base_url.is_some()
    }

    #[must_use]
    pub const fn default_base_url(&self) -> Option<&'static str> {
        self.default_base_url
    }

    /// Returns a preset when the configured model is in this provider's picker.
    #[must_use]
    pub fn model(&self, id: &str) -> Option<&'static ModelPreset> {
        self.models.iter().find(|model| model.id == id)
    }

    /// Builds one runtime model after validating advertised capabilities.
    pub fn build(&self, mut config: ProviderBuildConfig) -> Result<Arc<dyn Model>> {
        if config.reasoning_effort.is_none() {
            config.reasoning_effort = self
                .model(&config.model)
                .and_then(|model| model.default_reasoning)
                .map(str::to_string);
        }
        self.build_config_is_valid(
            &config.model,
            config.base_url.as_deref(),
            config.reasoning_effort.as_deref(),
            config.web_search,
        )?;
        (self.builder)(config)
    }

    /// Validates provider-specific settings without resolving credentials.
    pub fn build_config_is_valid(
        &self,
        model: &str,
        base_url: Option<&str>,
        reasoning_effort: Option<&str>,
        web_search: HostedWebSearch,
    ) -> Result<()> {
        if model.trim().is_empty() {
            return Err(Error::Config(format!(
                "provider `{}` requires a model",
                self.id
            )));
        }
        let preset = self.model(model);
        if !self.models.is_empty() && preset.is_none() {
            return Err(Error::Config(format!(
                "provider `{}` does not advertise model `{model}`",
                self.id
            )));
        }
        if !self.web_search.contains(&web_search) {
            return Err(Error::Config(format!(
                "provider `{}` does not support web search mode `{}`",
                self.id,
                web_search.id()
            )));
        }
        self.validate_base_url(base_url)?;
        if let Some(effort) = reasoning_effort
            && let Some(preset) = preset
            && !preset.reasoning.iter().any(|preset| preset.id == effort)
        {
            return Err(Error::Config(format!(
                "model `{}` does not support reasoning effort `{effort}`",
                model
            )));
        }
        Ok(())
    }

    /// Validates this provider's base-URL boundary.
    pub fn validate_base_url(&self, base_url: Option<&str>) -> Result<()> {
        match (self.default_base_url, base_url) {
            (None, Some(_)) => Err(Error::Config(format!(
                "provider `{}` has a fixed API endpoint",
                self.id
            ))),
            (Some(_), None) => Err(Error::Config(format!(
                "provider `{}` requires a base URL",
                self.id
            ))),
            (Some(_), Some(base_url)) => validate_base_url(base_url),
            (None, None) => Ok(()),
        }
    }
}

pub(super) fn validate_base_url(base_url: &str) -> Result<()> {
    let url = reqwest::Url::parse(base_url)
        .map_err(|error| Error::Config(format!("invalid base URL: {error}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| Error::Config("base URL requires a host".into()))?;
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]");
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(Error::Config(
            "base URL must use HTTPS, except for loopback HTTP".into(),
        ));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Config(
            "base URL cannot contain credentials, a query, or a fragment".into(),
        ));
    }
    Ok(())
}

static PROVIDERS: &[ProviderDefinition] = &[
    super::openai_socket::provider(),
    super::openai_codex::provider(),
    super::deepseek::provider(),
    super::kimi::provider(),
    super::openrouter::provider(),
    super::anthropic::provider(),
    super::openai::generic_provider(),
];

/// Returns every built-in provider in setup-menu order.
#[must_use]
pub fn providers() -> &'static [ProviderDefinition] {
    PROVIDERS
}

/// Returns the provider used by an unconfigured composition.
#[must_use]
pub fn default_provider() -> &'static ProviderDefinition {
    &PROVIDERS[0]
}

/// Resolves a built-in provider by its stable manifest ID.
pub fn provider(id: &str) -> Result<&'static ProviderDefinition> {
    PROVIDERS
        .iter()
        .find(|provider| provider.id == id)
        .ok_or_else(|| Error::Unknown(format!("model provider `{id}`")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn provider_manifest_ids_are_unique() {
        let mut ids = BTreeSet::new();

        assert!(
            providers().iter().all(|provider| ids.insert(provider.id())),
            "provider manifest contains duplicate IDs"
        );
    }

    #[test]
    fn provider_manifests_are_complete_and_internally_consistent() {
        for provider in providers() {
            assert!(!provider.id().trim().is_empty());
            assert!(!provider.label().trim().is_empty());
            assert!(!provider.symbol().as_str().trim().is_empty());
            assert!(!provider.description().trim().is_empty());
            assert_eq!(provider.web_search().first(), Some(&HostedWebSearch::Off));

            let mut model_ids = BTreeSet::new();
            for model in provider.models() {
                assert!(model_ids.insert(model.id), "duplicate model `{}`", model.id);
                assert!(!model.label.trim().is_empty());
                assert!(!model.description.trim().is_empty());
                assert!(model.context_window > 0);

                let mut reasoning_ids = BTreeSet::new();
                for reasoning in model.reasoning {
                    assert!(reasoning_ids.insert(reasoning.id));
                    assert!(!reasoning.label.trim().is_empty());
                    assert!(!reasoning.description.trim().is_empty());
                }
                assert!(
                    model
                        .default_reasoning
                        .is_none_or(|default| reasoning_ids.contains(default))
                );
            }
        }
    }
}
