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

use crate::config::{ChatSpec, ConfigStore, CredentialStore, GatewayConfig, local_user_name};
use crate::cron::{ConversationalCron, CronStore};
use crate::sandbox::GatewaySandbox;
use crate::wire::{ProviderAuthKind, ProviderConfig, ProviderStatus};
use crate::{Error, Result};

const DEFAULT_CONTEXT_WINDOW: i64 = 272_000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_MODEL_ROUTE_BYTES: usize = 4 * 1024;

pub(crate) struct BuiltAgent {
    pub(crate) agent: Agent,
    pub(crate) gateway_sandbox: Arc<GatewaySandbox>,
    pub(crate) subagent_template: Option<Arc<OnceLock<AgentConfig>>>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "the headless composition root keeps its runtime dependencies explicit"
)]
pub(crate) async fn assemble(
    gateway: &GatewayConfig,
    chat: &ChatSpec,
    store: &ConfigStore,
    credentials: Arc<CredentialStore>,
    cron: Arc<CronStore>,
    checkpoints: Arc<dyn CheckpointStore>,
    session_id: Option<String>,
    origin_label: &str,
    override_saved_model_route: bool,
) -> Result<BuiltAgent> {
    let (models, context_window) =
        if credential_is_configured(&chat.agent.config.provider, store, &credentials)? {
            build_models(&chat.agent.config.provider, store, &credentials)?
        } else {
            unavailable_models(&chat.agent.config.provider)?
        };
    let gateway_sandbox = Arc::new(GatewaySandbox::new(
        &chat.workspace,
        store.state_dir(),
        gateway.tls.as_ref().map(|tls| tls.private_key.as_path()),
        COMMAND_TIMEOUT,
    )?);
    let backend: Arc<dyn SandboxBackend> = gateway_sandbox.clone();
    let sandbox = Arc::new(Sandbox::new(backend, chat.agent.config.approval));
    let template = chat
        .agent
        .config
        .middleware
        .subagents
        .then(|| Arc::new(OnceLock::<AgentConfig>::new()));
    let launcher = template.as_ref().map(subagent_launcher);
    let middleware = build_middleware(
        &chat.agent.config.middleware,
        &chat.workspace,
        launcher,
        cron,
    )?;
    let mut metadata = match session_id.as_deref() {
        Some(session_id) => checkpoints
            .load(session_id)
            .await?
            .map(|checkpoint| checkpoint.metadata)
            .unwrap_or_default(),
        None => Default::default(),
    };
    metadata.extend(chat.metadata()?);
    let workspace = chat.workspace_info();
    let mut agent_config = AgentConfig::new(
        models,
        sandbox,
        checkpoints,
        middleware,
        chat.agent.config.system_prompt.clone(),
    )
    .context_window(context_window)
    .metadata(metadata)
    .session_context(SessionContext {
        user_name: local_user_name(),
        workspace_id: Some(workspace.id),
        workspace_label: Some(workspace.path.display().to_string()),
        origin_label: Some(origin_label.into()),
        ..SessionContext::default()
    });
    if let Some(session_id) = session_id {
        if session_id.trim().is_empty() || session_id.len() > 4 * 1024 {
            return Err(Error::Config("session ID must be 1–4096 bytes".into()));
        }
        agent_config = agent_config.session_id(session_id);
    }
    if override_saved_model_route {
        agent_config = agent_config.override_saved_model_route();
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

pub(crate) fn configured_model_choices(
    gateway: &GatewayConfig,
    store: &ConfigStore,
    credentials: &CredentialStore,
) -> Result<Vec<ModelChoice>> {
    Ok(configured_model_routes(gateway, store, credentials)?
        .into_iter()
        .map(|route| route.choice)
        .collect())
}

pub(crate) fn configured_provider_for_route(
    gateway: &GatewayConfig,
    store: &ConfigStore,
    credentials: &CredentialStore,
    route: &str,
) -> Result<ProviderConfig> {
    if route.trim().is_empty() || route.len() > MAX_MODEL_ROUTE_BYTES {
        return Err(Error::Config(format!(
            "model route must be 1–{MAX_MODEL_ROUTE_BYTES} bytes"
        )));
    }
    configured_model_routes(gateway, store, credentials)?
        .into_iter()
        .find(|candidate| candidate.choice.route == route)
        .map(|candidate| candidate.provider)
        .ok_or_else(|| Error::Config("model route is not in the configured gateway catalog".into()))
}

fn configured_model_routes(
    gateway: &GatewayConfig,
    store: &ConfigStore,
    credentials: &CredentialStore,
) -> Result<Vec<CatalogRoute>> {
    let mut routes = Vec::new();
    let default_provider = gateway
        .default_agent
        .as_ref()
        .map(|default| default.config.provider.provider.as_str());
    let mut definitions = providers().iter().collect::<Vec<_>>();
    definitions.sort_by_key(|definition| Some(definition.id()) != default_provider);
    for definition in definitions {
        let Some(selection) = gateway.configured_providers.get(definition.id()) else {
            continue;
        };
        if credential_is_configured(selection, store, credentials)? {
            routes.extend(catalog_routes(definition, selection));
        }
    }
    Ok(routes)
}

fn catalog_routes(
    definition: &ProviderDefinition,
    selection: &ProviderConfig,
) -> Vec<CatalogRoute> {
    let mut models = definition
        .models()
        .iter()
        .map(|preset| (preset.id, Some(preset)))
        .collect::<Vec<_>>();
    if models.iter().all(|(model, _)| *model != selection.model) {
        models.insert(0, (selection.model.as_str(), None));
    } else {
        models.sort_by_key(|(model, _)| *model != selection.model);
    }

    let mut routes = Vec::new();
    for (model, preset) in models {
        let preferred = if model == selection.model {
            selection
                .reasoning_effort
                .as_deref()
                .or_else(|| preset.and_then(|preset| preset.default_reasoning))
        } else {
            preset.and_then(|preset| preset.default_reasoning)
        };
        let mut efforts = vec![preferred];
        for reasoning in preset.into_iter().flat_map(|preset| preset.reasoning) {
            let effort = Some(reasoning.id);
            if !efforts.contains(&effort) {
                efforts.push(effort);
            }
        }
        for effort in efforts {
            let mut provider = selection.clone();
            provider.model = model.into();
            provider.reasoning_effort = effort.map(str::to_string);
            let route = route_id(definition.id(), model, effort);
            routes.push(CatalogRoute {
                choice: ModelChoice {
                    route,
                    group: format!(
                        "{} · {}",
                        definition.label(),
                        preset.map_or(model, |preset| preset.label)
                    ),
                    model: model.into(),
                    reasoning_effort: effort.map(str::to_string),
                    context_window: Some(
                        preset.map_or(DEFAULT_CONTEXT_WINDOW, |preset| preset.context_window),
                    ),
                },
                provider,
            });
        }
    }
    routes
}

struct CatalogRoute {
    choice: ModelChoice,
    provider: ProviderConfig,
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
    let credential = resolve_credential(definition, base_url.as_deref(), store, credentials)?;
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
            let value = std::env::var(default_env).map_err(|_| {
                Error::Config(format!("set a credential for `{}`", definition.id()))
            })?;
            if value.trim().is_empty() {
                return Err(Error::Config(format!(
                    "credential environment variable {default_env} is empty"
                )));
            }
            Ok(ProviderCredential::ApiKey(value))
        }
        ProviderAuth::Browser(auth) => auth.load(&store.provider_auth_path()).map_err(Error::from),
    }
}

pub(crate) fn credential_is_configured(
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
            Ok(std::env::var(default_env).is_ok_and(|value| !value.trim().is_empty()))
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
    let id = route_id(spec.definition.id(), spec.model, spec.effort);
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

fn route_id(provider: &str, model: &str, effort: Option<&str>) -> String {
    format!("{provider}::{model}::{}", effort.unwrap_or("default"))
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
    cron: Arc<CronStore>,
) -> Result<MiddlewareStack> {
    let mut entries: Vec<Arc<dyn Middleware>> = Vec::new();
    if settings.tools {
        entries.push(Arc::new(Tools::coding()));
    }
    entries.push(Arc::new(ConversationalCron::new(cron)));
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
    entries.push(Arc::new(Sessions::default()));
    Ok(MiddlewareStack::new(entries)?)
}

#[cfg(test)]
mod tests {
    use horus::backend::checkpoint::{Checkpoint, sqlite::SqliteCheckpoint};

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
    fn configured_catalog_resolves_manifest_and_opaque_custom_routes() {
        let root = tempfile::tempdir().expect("root");
        let state = root.path().join("state");
        let (store, config) = ConfigStore::initialize(
            state,
            "127.0.0.1:8741".parse().expect("listen address"),
            None,
        )
        .expect("config");
        let credentials =
            CredentialStore::open(store.credentials_path()).expect("credential store");
        credentials
            .set("kimi", "kimi-secret", None)
            .expect("Kimi credential");
        credentials
            .set("responses", "custom-secret", Some("https://example.com/v1"))
            .expect("custom credential");
        let kimi = ProviderConfig {
            provider: "kimi".into(),
            model: "kimi-k3".into(),
            base_url: None,
            reasoning_effort: Some("max".into()),
            web_search: HostedWebSearch::Off,
        };
        let custom = ProviderConfig {
            provider: "responses".into(),
            model: "vendor/model::opaque".into(),
            base_url: Some("https://example.com/v1".into()),
            reasoning_effort: Some("provider-defined".into()),
            web_search: HostedWebSearch::Off,
        };
        let config = config
            .registering_provider(kimi)
            .and_then(|config| config.registering_provider(custom.clone()))
            .expect("register providers");

        let choices = configured_model_choices(&config, &store, &credentials).expect("catalog");
        let custom_route = choices
            .iter()
            .find(|choice| choice.model == custom.model)
            .expect("custom choice");
        let resolved =
            configured_provider_for_route(&config, &store, &credentials, &custom_route.route)
                .expect("resolve custom route");

        assert!(
            choices
                .first()
                .is_some_and(|choice| choice.route.starts_with("kimi::"))
        );
        assert_eq!(resolved, custom);
        assert_eq!(
            custom_route.group,
            format!(
                "{} · {}",
                provider("responses").expect("provider").label(),
                custom.model
            )
        );
    }

    #[test]
    fn custom_responses_requires_an_endpoint_bound_stored_credential() {
        let root = tempfile::tempdir().expect("root");
        let state = root.path().join("state");
        let (store, _) = ConfigStore::initialize(
            state,
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
            reasoning_effort: None,
            web_search: HostedWebSearch::Off,
        };

        let error = resolve_credential(
            provider("responses").expect("provider"),
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
                selection.base_url.as_deref(),
                &store,
                &credentials,
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn updating_the_chat_recipe_preserves_capability_metadata() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let (store, gateway) = ConfigStore::initialize(
            root.path().join("state"),
            "127.0.0.1:8741".parse().expect("listen address"),
            None,
        )
        .expect("config");
        let credentials =
            Arc::new(CredentialStore::open(store.credentials_path()).expect("credentials"));
        let cron = Arc::new(CronStore::open(store.state_dir()).expect("cron"));
        let checkpoints: Arc<dyn CheckpointStore> =
            Arc::new(SqliteCheckpoint::new(store.checkpoints_path()).expect("checkpoints"));
        let original = ChatSpec::new(
            &workspace,
            crate::wire::VersionedAgentConfig {
                revision: 1,
                config: crate::wire::AgentComposition::default(),
            },
            store.state_dir(),
            None,
        )
        .expect("chat spec");
        let mut checkpoint = Checkpoint::empty("chat");
        checkpoint.metadata = original.metadata().expect("chat metadata");
        checkpoint.metadata.insert(
            "capability.test".into(),
            serde_json::json!({"identity": "preserved"}),
        );
        checkpoints
            .save(&checkpoint, &[])
            .await
            .expect("seed checkpoint");
        let mut composition = original.agent.config.clone();
        composition.middleware.tools = false;
        let updated = original
            .replacing_agent(1, composition, store.state_dir(), None)
            .expect("updated chat spec");

        let built = assemble(
            &gateway,
            &updated,
            &store,
            credentials,
            cron,
            Arc::clone(&checkpoints),
            Some("chat".into()),
            "test",
            true,
        )
        .await
        .expect("assemble chat");
        let (sender, mut events) = built.agent.into_parts();
        drop(sender);
        while events.recv().await.is_some() {}
        let checkpoint = checkpoints
            .load("chat")
            .await
            .expect("load checkpoint")
            .expect("saved checkpoint");
        let saved = ChatSpec::from_metadata(&checkpoint.metadata, store.state_dir(), None)
            .expect("saved chat spec");

        assert_eq!(
            checkpoint.metadata["capability.test"],
            serde_json::json!({"identity": "preserved"})
        );
        assert_eq!(saved.agent.revision, 2);
        assert!(!saved.agent.config.middleware.tools);
    }
}
