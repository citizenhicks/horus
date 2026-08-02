use std::sync::{Arc, OnceLock};
use std::time::Duration;

use horus::Error as HorusError;
use horus::agent::{Agent, AgentConfig, create_agent};
use horus::backend::checkpoint::CheckpointStore;
use horus::backend::model::provider::{
    HostedWebSearch, HttpClient, ProviderAuth, ProviderBuildConfig, ProviderCredential,
    ProviderDefinition, provider, providers, streaming_client,
};
use horus::backend::model::{
    Model, ModelChoice, ModelEventSink, ModelInfo, ModelOutput, ModelRequest, ModelRouter,
};
use horus::backend::sandbox::{Sandbox, SandboxBackend};
use horus::middleware::compaction::Compaction;
use horus::middleware::sessions::Sessions;
use horus::middleware::skills::Skills;
use horus::middleware::steering::Steering;
use horus::middleware::subagents::{SubagentLaunch, SubagentLauncher, Subagents};
use horus::middleware::tools::Tools;
use horus::middleware::{Middleware, MiddlewareStack};
use horus::protocol::SessionContext;

use crate::config::{ConfigStore, CredentialStore, GatewayConfig, local_user_name};
use crate::sandbox::GatewaySandbox;
use crate::wire::{ProviderAuthKind, ProviderConfig, ProviderStatus};
use crate::{Error, Result};

const DEFAULT_CONTEXT_WINDOW: i64 = 272_000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) struct BuiltAgent {
    pub(crate) agent: Agent,
    pub(crate) gateway_sandbox: Arc<GatewaySandbox>,
    pub(crate) subagent_template: Option<Arc<OnceLock<AgentConfig>>>,
}

pub(crate) async fn assemble(
    config: &GatewayConfig,
    store: &ConfigStore,
    credentials: Arc<CredentialStore>,
    checkpoints: Arc<dyn CheckpointStore>,
    session_id: Option<String>,
    origin_label: &str,
) -> Result<BuiltAgent> {
    let (models, context_window) =
        if credential_is_configured(&config.agent.config.provider, store, &credentials)? {
            build_models(&config.agent.config.provider, store, &credentials)?
        } else {
            unavailable_models(&config.agent.config.provider)?
        };
    let gateway_sandbox = Arc::new(GatewaySandbox::new(
        &config.workspace,
        store.state_dir(),
        config.tls.as_ref().map(|tls| tls.private_key.as_path()),
        COMMAND_TIMEOUT,
    )?);
    let backend: Arc<dyn SandboxBackend> = gateway_sandbox.clone();
    let sandbox = Arc::new(Sandbox::new(backend, config.agent.config.approval));
    let template = config
        .agent
        .config
        .middleware
        .subagents
        .then(|| Arc::new(OnceLock::<AgentConfig>::new()));
    let launcher = template.as_ref().map(subagent_launcher);
    let middleware =
        build_middleware(&config.agent.config.middleware, &config.workspace, launcher)?;
    let workspace = config.workspace_info();
    let mut agent_config = AgentConfig::new(
        models,
        sandbox,
        checkpoints,
        middleware,
        config.agent.config.system_prompt.clone(),
    )
    .context_window(context_window)
    .session_context(SessionContext {
        user_name: local_user_name(),
        workspace_id: Some(workspace.id),
        workspace_label: Some(workspace.label),
        origin_label: Some(origin_label.into()),
        ..SessionContext::default()
    });
    if let Some(session_id) = session_id {
        if session_id.trim().is_empty() || session_id.len() > 4 * 1024 {
            return Err(Error::Config("session ID must be 1–4096 bytes".into()));
        }
        agent_config = agent_config.session_id(session_id);
    }
    if let Some(template) = &template {
        template
            .set(agent_config.clone())
            .map_err(|_| Error::Config("subagent launcher was initialized twice".into()))?;
    }
    Ok(BuiltAgent {
        agent: create_agent(agent_config).await?,
        gateway_sandbox,
        subagent_template: template,
    })
}

pub(crate) fn provider_statuses(
    store: &ConfigStore,
    credentials: &CredentialStore,
) -> Result<Vec<ProviderStatus>> {
    providers()
        .iter()
        .map(|definition| {
            let configured = match definition.auth() {
                ProviderAuth::ApiKey(default_env) => {
                    credentials.configured(definition.id())?
                        || (!definition.configurable_base_url()
                            && std::env::var(default_env)
                                .is_ok_and(|value| !value.trim().is_empty()))
                }
                ProviderAuth::Browser(auth) => auth.configured(&store.provider_auth_path())?,
            };
            Ok(provider_status(definition, configured))
        })
        .collect()
}

fn provider_status(definition: &ProviderDefinition, configured: bool) -> ProviderStatus {
    let (auth, default_api_key_env) = match definition.auth() {
        ProviderAuth::ApiKey(default_env) => (
            ProviderAuthKind::ApiKey,
            (!definition.configurable_base_url()).then(|| default_env.to_string()),
        ),
        ProviderAuth::Browser(_) => (ProviderAuthKind::DeviceCode, None),
    };
    let default_model = definition.models().first();
    ProviderStatus {
        provider: definition.id().into(),
        label: definition.label().into(),
        configured,
        auth,
        default_model: default_model.map(|model| model.id.into()),
        default_base_url: definition.default_base_url().map(str::to_string),
        default_api_key_env,
        default_reasoning_effort: default_model
            .and_then(|model| model.default_reasoning)
            .map(str::to_string),
        default_web_search: definition.web_search().first().copied().unwrap_or_default(),
    }
}

fn subagent_launcher(template: &Arc<OnceLock<AgentConfig>>) -> SubagentLauncher {
    let template = Arc::downgrade(template);
    Arc::new(move |launch: SubagentLaunch| {
        let template = template.clone();
        Box::pin(async move {
            let config = template
                .upgrade()
                .ok_or_else(|| HorusError::Stopped("subagent launcher stopped".into()))?
                .get()
                .ok_or_else(|| HorusError::Config("subagent launcher is not ready".into()))?
                .clone()
                .session_id(launch.session_id)
                .metadata(launch.metadata)
                .model_route(&launch.model, launch.reasoning_effort.as_deref())?;
            create_agent(config).await
        })
    })
}

fn build_models(
    selection: &ProviderConfig,
    store: &ConfigStore,
    credentials: &CredentialStore,
) -> Result<(Arc<ModelRouter>, i64)> {
    let definition = provider(&selection.provider)?;
    let base_url = if definition.configurable_base_url() {
        selection
            .base_url
            .clone()
            .or_else(|| definition.default_base_url().map(str::to_string))
    } else {
        None
    };
    let credential = resolve_credential(
        definition,
        selection,
        base_url.as_deref(),
        store,
        credentials,
    )?;
    let http = streaming_client()?;
    let mut models = definition.models().iter().collect::<Vec<_>>();
    models.sort_by_key(|model| model.id != selection.model);
    let custom_model = models
        .iter()
        .all(|preset| preset.id != selection.model)
        .then_some(selection.model.as_str());
    if models.is_empty() && custom_model.is_none() {
        return Err(Error::Config(format!(
            "provider `{}` has no model routes",
            definition.id()
        )));
    }

    let mut routes = Vec::new();
    if let Some(model) = custom_model {
        routes.push(build_route(RouteSpec {
            definition,
            credential: credential.clone(),
            http: &http,
            model,
            effort: selection.reasoning_effort.as_deref(),
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            base_url: base_url.clone(),
            web_search: selection.web_search,
        })?);
    }
    for preset in models {
        let preferred = (preset.id == selection.model)
            .then_some(selection.reasoning_effort.as_deref())
            .flatten()
            .or(preset.default_reasoning);
        let mut efforts = vec![preferred];
        for reasoning in preset.reasoning {
            let effort = Some(reasoning.id);
            if !efforts.contains(&effort) {
                efforts.push(effort);
            }
        }
        for effort in efforts {
            routes.push(build_route(RouteSpec {
                definition,
                credential: credential.clone(),
                http: &http,
                model: preset.id,
                effort,
                context_window: Some(preset.context_window),
                base_url: base_url.clone(),
                web_search: selection.web_search,
            })?);
        }
    }
    let first = routes
        .first()
        .ok_or_else(|| Error::Config("provider has no model routes".into()))?;
    let context_window = first
        .choice
        .context_window
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let mut router = ModelRouter::new(&first.id, Arc::clone(&first.model));
    for route in routes.iter().skip(1) {
        router.register(&route.id, Arc::clone(&route.model))?;
    }
    for route in routes {
        router.configure_choice(route.choice)?;
    }
    Ok((Arc::new(router), context_window))
}

fn resolve_credential(
    definition: &ProviderDefinition,
    selection: &ProviderConfig,
    base_url: Option<&str>,
    store: &ConfigStore,
    credentials: &CredentialStore,
) -> Result<ProviderCredential> {
    match definition.auth() {
        ProviderAuth::ApiKey(default_env) => {
            if let Some(value) = credentials.get(definition.id(), base_url)? {
                return Ok(ProviderCredential::ApiKey(value));
            }
            if definition.configurable_base_url() {
                return Err(Error::Config(format!(
                    "set a credential for `{}`",
                    definition.id()
                )));
            }
            let name = selection.api_key_env.as_deref().unwrap_or(default_env);
            let value = std::env::var(name).map_err(|_| {
                Error::Config(format!("set a credential for `{}`", definition.id()))
            })?;
            if value.trim().is_empty() {
                return Err(Error::Config(format!(
                    "credential environment variable {name} is empty"
                )));
            }
            Ok(ProviderCredential::ApiKey(value))
        }
        ProviderAuth::Browser(auth) => auth.load(&store.provider_auth_path()).map_err(Error::from),
    }
}

fn credential_is_configured(
    selection: &ProviderConfig,
    store: &ConfigStore,
    credentials: &CredentialStore,
) -> Result<bool> {
    let definition = provider(&selection.provider)?;
    let base_url = if definition.configurable_base_url() {
        selection
            .base_url
            .as_deref()
            .or_else(|| definition.default_base_url())
    } else {
        None
    };
    match definition.auth() {
        ProviderAuth::ApiKey(default_env) => {
            if credentials.get(definition.id(), base_url)?.is_some() {
                return Ok(true);
            }
            if definition.configurable_base_url() {
                return Ok(false);
            }
            let name = selection.api_key_env.as_deref().unwrap_or(default_env);
            Ok(std::env::var(name).is_ok_and(|value| !value.trim().is_empty()))
        }
        ProviderAuth::Browser(auth) => auth
            .configured(&store.provider_auth_path())
            .map_err(Error::from),
    }
}

struct RouteSpec<'a> {
    definition: &'static ProviderDefinition,
    credential: ProviderCredential,
    http: &'a HttpClient,
    model: &'a str,
    effort: Option<&'a str>,
    context_window: Option<i64>,
    base_url: Option<String>,
    web_search: HostedWebSearch,
}

fn build_route(spec: RouteSpec<'_>) -> Result<RouteValue> {
    let id = format!(
        "{}::{}::{}",
        spec.definition.id(),
        spec.model,
        spec.effort.unwrap_or("default")
    );
    let model = spec.definition.build(ProviderBuildConfig {
        credential: spec.credential,
        model: spec.model.into(),
        base_url: spec.base_url,
        reasoning_effort: spec.effort.map(str::to_string),
        web_search: spec.web_search,
        http: spec.http.clone(),
    })?;
    Ok(RouteValue {
        choice: ModelChoice {
            route: id.clone(),
            group: spec.model.into(),
            model: spec.model.into(),
            reasoning_effort: spec.effort.map(str::to_string),
            context_window: spec.context_window,
        },
        id,
        model,
    })
}

struct RouteValue {
    id: String,
    choice: ModelChoice,
    model: Arc<dyn Model>,
}

struct UnavailableModel {
    info: ModelInfo,
}

impl Model for UnavailableModel {
    fn info(&self) -> ModelInfo {
        self.info.clone()
    }

    fn respond<'a>(
        &'a self,
        _request: ModelRequest<'a>,
        _events: ModelEventSink,
    ) -> horus::BoxFuture<'a, horus::Result<ModelOutput>> {
        Box::pin(async {
            Err(HorusError::Auth(
                "the selected provider is not configured on this gateway".into(),
            ))
        })
    }
}

fn unavailable_models(selection: &ProviderConfig) -> Result<(Arc<ModelRouter>, i64)> {
    let definition = provider(&selection.provider)?;
    let context_window = definition
        .model(&selection.model)
        .map_or(DEFAULT_CONTEXT_WINDOW, |preset| preset.context_window);
    let effort = selection.reasoning_effort.clone().or_else(|| {
        definition
            .model(&selection.model)
            .and_then(|preset| preset.default_reasoning.map(str::to_string))
    });
    let route = format!(
        "{}::{}::{}",
        selection.provider,
        selection.model,
        effort.as_deref().unwrap_or("default")
    );
    let model: Arc<dyn Model> = Arc::new(UnavailableModel {
        info: ModelInfo {
            model: selection.model.clone(),
            reasoning_effort: effort.clone(),
        },
    });
    let mut router = ModelRouter::new(&route, model);
    router.configure_choice(ModelChoice {
        route,
        group: selection.model.clone(),
        model: selection.model.clone(),
        reasoning_effort: effort,
        context_window: Some(context_window),
    })?;
    Ok((Arc::new(router), context_window))
}

fn build_middleware(
    settings: &crate::wire::MiddlewareConfig,
    workspace: &std::path::Path,
    launcher: Option<SubagentLauncher>,
) -> Result<MiddlewareStack> {
    let mut entries: Vec<Arc<dyn Middleware>> = Vec::new();
    if settings.tools {
        entries.push(Arc::new(Tools::coding()));
    }
    if settings.skills {
        entries.push(Arc::new(Skills::discover_installed([
            workspace.join(".agents/skills"),
            workspace.join(".codex/skills"),
        ])?));
    }
    if settings.subagents {
        let launcher =
            launcher.ok_or_else(|| Error::Config("subagent launcher is missing".into()))?;
        entries.push(Arc::new(Subagents::new(4, 8, 32, launcher)?));
    }
    if settings.steering {
        entries.push(Arc::new(Steering::default()));
    }
    if settings.compaction {
        entries.push(Arc::new(Compaction::default()));
    }
    if settings.sessions {
        entries.push(Arc::new(Sessions::default()));
    }
    Ok(MiddlewareStack::new(entries)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_status_uses_manifest_defaults() {
        let status = provider_status(provider("openai_socket").expect("provider"), false);

        assert_eq!(status.provider, "openai_socket");
        assert_eq!(status.label, "OpenAI (API key)");
        assert_eq!(status.default_model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(
            status.default_api_key_env.as_deref(),
            Some("OPENAI_API_KEY")
        );
        assert_eq!(status.default_reasoning_effort.as_deref(), Some("medium"));
        assert_eq!(status.default_web_search, HostedWebSearch::Off);

        let custom = provider_status(provider("responses").expect("provider"), false);
        assert_eq!(custom.default_model, None);
        assert_eq!(
            custom.default_base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(custom.default_api_key_env, None);
    }

    #[test]
    fn custom_responses_does_not_load_host_environment_credentials() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        std::fs::create_dir(&workspace).expect("workspace");
        let (store, _) = ConfigStore::initialize(
            state,
            workspace,
            "127.0.0.1:8741".parse().expect("listen address"),
            None,
        )
        .expect("config");
        let credentials =
            CredentialStore::open(store.credentials_path()).expect("credential store");
        let selection = ProviderConfig {
            provider: "responses".into(),
            model: "custom-model".into(),
            base_url: Some("https://example.com/v1".into()),
            api_key_env: Some("PATH".into()),
            reasoning_effort: None,
            web_search: HostedWebSearch::Off,
        };

        let error = resolve_credential(
            provider("responses").expect("provider"),
            &selection,
            selection.base_url.as_deref(),
            &store,
            &credentials,
        )
        .err()
        .expect("custom provider must require stored credentials");

        assert!(
            error
                .to_string()
                .contains("set a credential for `responses`")
        );

        credentials
            .set(
                "responses",
                "official-secret",
                Some("https://api.openai.com/v1"),
            )
            .expect("store endpoint-bound credential");
        assert!(
            resolve_credential(
                provider("responses").expect("provider"),
                &selection,
                selection.base_url.as_deref(),
                &store,
                &credentials,
            )
            .is_err()
        );
    }
}
