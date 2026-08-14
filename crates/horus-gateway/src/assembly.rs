use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use horus::Error as HorusError;
use horus::agent::{Agent, AgentConfig, create_agent};
use horus::backend::checkpoint::CheckpointStore;
use horus::backend::model::provider::{
    HttpClient, ProviderAuth, ProviderBuildConfig, ProviderCredential, ProviderDefinition,
    provider, providers, streaming_client,
};
use horus::backend::model::{
    Model, ModelChoice, ModelEventSink, ModelInfo, ModelOutput, ModelRequest, ModelRouter,
};
use horus::backend::sandbox::{
    ApprovalPolicy, ApprovalReviewerConfig, ApprovalStrictness, Sandbox, SandboxBackend,
};
use horus::middleware::artifacts::Artifacts;
use horus::middleware::attachments::Attachments;
use horus::middleware::compaction::Compaction;
use horus::middleware::context_offloading::ContextOffloading;
use horus::middleware::cron::Cron;
use horus::middleware::instructions::Instructions;
use horus::middleware::scratchpad::{Scratchpad, ScratchpadStore};
use horus::middleware::session_files::SessionFileStore;
use horus::middleware::sessions::Sessions;
use horus::middleware::skills::Skills;
use horus::middleware::steering::Steering;
use horus::middleware::subagents::{SubagentLaunch, SubagentLauncher, Subagents};
use horus::middleware::tasks::Tasks;
use horus::middleware::tools::Tools;
use horus::middleware::{Middleware, MiddlewareStack};
use horus::protocol::{SessionContext, TokenUsage};

use crate::config::{
    ChatSpec, ConfigStore, ConfiguredProvider, CredentialStore, DEFAULT_CONTEXT_WINDOW,
    GatewayConfig, effective_reasoning_effort, local_user_name, model_route_id,
};
use crate::cron::CronStore;
use crate::middleware_manifest::{BuiltinMiddleware, MIDDLEWARE};
use crate::sandbox::GatewaySandbox;
use crate::wire::{
    MiddlewareConfig, ProviderAuthKind, ProviderConfig, ProviderModel, ProviderStatus,
    ReasoningChoice, validate_session_id,
};
use crate::{Error, Result};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_MODEL_ROUTE_BYTES: usize = 4 * 1024;

pub(crate) struct BuiltAgent {
    pub(crate) agent: Agent,
    pub(crate) model_router: Arc<ModelRouter>,
    pub(crate) gateway_sandbox: Arc<GatewaySandbox>,
    pub(crate) subagent_template: Option<Arc<OnceLock<AgentConfig>>>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "the headless composition root keeps its runtime dependencies explicit"
)]
pub(crate) async fn assemble(
    gateway: Arc<Mutex<GatewayConfig>>,
    chat: &ChatSpec,
    store: &ConfigStore,
    credentials: Arc<CredentialStore>,
    cron: Arc<CronStore>,
    checkpoints: Arc<dyn CheckpointStore>,
    scratchpad: ScratchpadStore,
    session_files: SessionFileStore,
    session_id: Option<String>,
    origin_label: &str,
    override_saved_model_route: bool,
    reusable_model_router: Option<Arc<ModelRouter>>,
) -> Result<BuiltAgent> {
    if let Some(session_id) = session_id.as_deref() {
        validate_session_id(session_id)?;
    }
    let gateway_config = gateway
        .lock()
        .map_err(|_| Error::Config("gateway configuration lock is poisoned".into()))?
        .clone();
    let model_providers = configured_model_providers(&gateway_config, store, &credentials)?;
    let (models, context_window) = if let Some(models) = reusable_model_router {
        let context_window = models
            .choices()
            .find(|choice| choice.route == models.default_provider())
            .and_then(|choice| choice.context_window)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        (models, context_window)
    } else if credential_is_configured(&chat.agent.config.provider, store, &credentials)? {
        build_models(
            &gateway_config,
            &chat.agent.config.provider,
            store,
            &credentials,
        )?
    } else {
        unavailable_models(&gateway_config, &chat.agent.config.provider)?
    };
    let gateway_sandbox = Arc::new(GatewaySandbox::new(
        &chat.workspace,
        store.state_dir(),
        gateway_config
            .tls
            .as_ref()
            .map(|tls| tls.private_key.as_path()),
        COMMAND_TIMEOUT,
    )?);
    let backend: Arc<dyn SandboxBackend> = gateway_sandbox.clone();
    let model_choices = models.choices().cloned().collect::<Vec<_>>();
    crate::middleware_manifest::validate_choices(&chat.agent.config.middleware, &model_choices)?;
    let approval_policy = crate::middleware_manifest::string_setting(
        &chat.agent.config.middleware,
        "sandbox",
        "approval_policy",
    )?
    .ok_or_else(|| Error::Config("missing middleware setting `sandbox.approval_policy`".into()))?
    .parse::<ApprovalPolicy>()?;
    let reviewer_strictness = crate::middleware_manifest::string_setting(
        &chat.agent.config.middleware,
        "sandbox",
        "reviewer_strictness",
    )?
    .ok_or_else(|| {
        Error::Config("missing middleware setting `sandbox.reviewer_strictness`".into())
    })?
    .parse::<ApprovalStrictness>()?;
    let mut reviewer = ApprovalReviewerConfig::default().strictness(reviewer_strictness);
    if let Some(route) = crate::middleware_manifest::string_setting(
        &chat.agent.config.middleware,
        "sandbox",
        "reviewer_model_route",
    )? {
        reviewer = reviewer.model_route(route)?;
    }
    let sandbox = Arc::new(Sandbox::new(backend, approval_policy).approval_reviewer(reviewer));
    let (middleware, template) = build_middleware(
        &chat.agent.config.middleware,
        &chat.workspace,
        cron,
        scratchpad,
        session_files,
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
    let usage_store = store.clone();
    let max_model_steps = usize::try_from(chat.agent.config.max_model_steps).map_err(|_| {
        Error::Config("maximum model steps exceed this platform's supported range".into())
    })?;
    let mut agent_config = AgentConfig::new(
        models,
        sandbox,
        checkpoints,
        middleware,
        chat.agent.config.system_prompt.clone(),
    )
    .context_window(context_window)
    .initial_replay_batches(0)
    .max_model_steps(max_model_steps)
    .metadata(metadata)
    .usage_observer(move |route, usage| {
        persist_usage(&gateway, &usage_store, &model_providers, route, usage)
    })
    .session_context(SessionContext {
        user_name: local_user_name(),
        workspace_id: Some(workspace.id),
        workspace_label: Some(workspace.path.display().to_string()),
        origin_label: Some(origin_label.into()),
        ..SessionContext::default()
    });
    if let Some(session_id) = session_id {
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
    let agent = create_agent(agent_config).await?;
    let model_router = agent.model_router();
    Ok(BuiltAgent {
        agent,
        model_router,
        gateway_sandbox,
        subagent_template: template,
    })
}

fn persist_usage(
    gateway: &Mutex<GatewayConfig>,
    store: &ConfigStore,
    model_providers: &BTreeMap<String, String>,
    route: &str,
    usage: &TokenUsage,
) -> horus::Result<()> {
    let provider = model_providers.get(route).ok_or_else(|| {
        HorusError::Config("model route is not in the configured gateway usage catalog".into())
    })?;
    let mut gateway = gateway
        .lock()
        .map_err(|_| HorusError::Config("gateway configuration lock is poisoned".into()))?;
    let mut next = gateway.clone();
    if next
        .observe_usage(provider, usage)
        .map_err(|error| HorusError::Config(error.to_string()))?
    {
        store
            .save(&next)
            .map_err(|error| HorusError::Config(error.to_string()))?;
        *gateway = next;
    }
    Ok(())
}

pub(crate) fn provider_statuses(
    gateway: &GatewayConfig,
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
            Ok(provider_status(
                definition,
                configured,
                gateway.configured_providers.get(definition.id()).cloned(),
            ))
        })
        .collect()
}

pub(crate) fn configured_model_choices(
    gateway: &GatewayConfig,
    store: &ConfigStore,
    credentials: &CredentialStore,
) -> Result<Vec<ModelChoice>> {
    Ok(instantiate_routes(
        configured_model_routes(gateway, store, credentials)?,
        store,
        credentials,
    )?
    .into_iter()
    .map(|route| route.choice)
    .collect())
}

pub(crate) fn configured_model_providers(
    gateway: &GatewayConfig,
    store: &ConfigStore,
    credentials: &CredentialStore,
) -> Result<BTreeMap<String, String>> {
    Ok(configured_model_routes(gateway, store, credentials)?
        .into_iter()
        .map(|route| (route.choice.route, route.provider.provider))
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

pub(crate) fn configured_route_exists(gateway: &GatewayConfig, route: &str) -> Result<bool> {
    for configured in gateway.configured_providers.values() {
        let definition = provider(&configured.selection.provider)?;
        if catalog_routes(definition, configured, &configured.selection)
            .iter()
            .any(|candidate| candidate.choice.route == route)
        {
            return Ok(true);
        }
    }
    Ok(false)
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
        let Some(configured) = gateway.configured_providers.get(definition.id()) else {
            continue;
        };
        if credential_is_configured(&configured.selection, store, credentials)? {
            routes.extend(catalog_routes(
                definition,
                configured,
                &configured.selection,
            ));
        }
    }
    Ok(routes)
}

fn catalog_routes(
    definition: &ProviderDefinition,
    configured: &ConfiguredProvider,
    selection: &ProviderConfig,
) -> Vec<CatalogRoute> {
    let mut models = definition
        .models()
        .iter()
        .map(|preset| (preset.id, Some(preset)))
        .collect::<Vec<_>>();
    for model in &configured.model_ids {
        if models.iter().all(|(candidate, _)| *candidate != model) {
            models.push((model, None));
        }
    }
    models.sort_by_key(|(model, _)| *model != selection.model);

    let mut routes = Vec::new();
    for (model, preset) in models {
        let catalog_default = preset
            .and_then(|preset| preset.default_reasoning)
            .or_else(|| configured.reasoning_efforts.first().map(String::as_str));
        let preferred = if model == selection.model {
            effective_reasoning_effort(definition, configured, selection)
        } else {
            catalog_default
        };
        let mut efforts = vec![preferred];
        for reasoning in preset.into_iter().flat_map(|preset| preset.reasoning) {
            let effort = Some(reasoning.id);
            if !efforts.contains(&effort) {
                efforts.push(effort);
            }
        }
        if preset.is_none() {
            for reasoning in &configured.reasoning_efforts {
                let effort = Some(reasoning.as_str());
                if !efforts.contains(&effort) {
                    efforts.push(effort);
                }
            }
        }
        for effort in efforts {
            let mut provider = selection.clone();
            provider.model = model.into();
            provider.reasoning_effort = effort.map(str::to_string);
            let route = model_route_id(definition.id(), model, effort);
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
                    supports_image_input: definition.supports_image_input(),
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

fn provider_status(
    definition: &ProviderDefinition,
    configured: bool,
    configured_provider: Option<ConfiguredProvider>,
) -> ProviderStatus {
    let (auth, default_api_key_env) = match definition.auth() {
        ProviderAuth::ApiKey(default_env) => (
            ProviderAuthKind::ApiKey,
            (!definition.configurable_base_url()).then(|| default_env.to_string()),
        ),
        ProviderAuth::Browser(_) => (ProviderAuthKind::DeviceCode, None),
    };
    let (selection, model_ids, reasoning_efforts) = configured_provider.map_or_else(
        || (None, Vec::new(), Vec::new()),
        |configured| {
            (
                Some(configured.selection),
                configured.model_ids,
                configured.reasoning_efforts,
            )
        },
    );
    ProviderStatus {
        provider: definition.id().into(),
        label: definition.label().into(),
        symbol: definition.symbol().clone(),
        description: definition.description().into(),
        configured,
        selection,
        model_ids,
        reasoning_efforts,
        model_ids_configurable: definition.models().is_empty(),
        auth,
        default_base_url: definition.default_base_url().map(str::to_string),
        default_api_key_env,
        models: definition
            .models()
            .iter()
            .map(|model| ProviderModel {
                id: model.id.into(),
                label: model.label.into(),
                description: model.description.into(),
                context_window: model.context_window,
                reasoning: model
                    .reasoning
                    .iter()
                    .map(|reasoning| ReasoningChoice {
                        id: reasoning.id.into(),
                        label: reasoning.label.into(),
                        description: reasoning.description.into(),
                    })
                    .collect(),
                default_reasoning: model.default_reasoning.map(str::to_string),
            })
            .collect(),
        web_search: definition.web_search().to_vec(),
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
    gateway: &GatewayConfig,
    selection: &ProviderConfig,
    store: &ConfigStore,
    credentials: &CredentialStore,
) -> Result<(Arc<ModelRouter>, i64)> {
    gateway.validate_provider_selection(selection)?;
    let definition = provider(&selection.provider)?;
    let configured = gateway
        .configured_providers
        .get(&selection.provider)
        .ok_or_else(|| Error::Config("active provider is not in the configured catalog".into()))?;
    let effort = effective_reasoning_effort(definition, configured, selection);
    let selected_route = model_route_id(&selection.provider, &selection.model, effort);
    let mut catalog = catalog_routes(definition, configured, selection);
    catalog.extend(
        configured_model_routes(gateway, store, credentials)?
            .into_iter()
            .filter(|route| route.provider.provider != selection.provider),
    );
    catalog.sort_by_key(|route| route.choice.route != selected_route);
    if catalog.first().map(|route| route.choice.route.as_str()) != Some(selected_route.as_str()) {
        return Err(Error::Config(
            "active model route is not in the configured gateway catalog".into(),
        ));
    }
    let routes = instantiate_routes(catalog, store, credentials)?;
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

fn instantiate_routes(
    catalog: Vec<CatalogRoute>,
    store: &ConfigStore,
    credentials: &CredentialStore,
) -> Result<Vec<RouteValue>> {
    let http = streaming_client()?;
    let mut provider_credentials = BTreeMap::<String, ProviderCredential>::new();
    let mut routes = Vec::with_capacity(catalog.len());
    for route in catalog {
        let definition = provider(&route.provider.provider)?;
        let base_url = definition
            .configurable_base_url()
            .then(|| {
                route
                    .provider
                    .base_url
                    .clone()
                    .or_else(|| definition.default_base_url().map(str::to_string))
            })
            .flatten();
        let credential = match provider_credentials.get(definition.id()) {
            Some(credential) => credential.clone(),
            None => {
                let credential =
                    resolve_credential(definition, base_url.as_deref(), store, credentials)?;
                provider_credentials.insert(definition.id().into(), credential.clone());
                credential
            }
        };
        routes.push(build_route(route, definition, credential, base_url, &http)?);
    }
    Ok(routes)
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

fn build_route(
    route: CatalogRoute,
    definition: &'static ProviderDefinition,
    credential: ProviderCredential,
    base_url: Option<String>,
    http: &HttpClient,
) -> Result<RouteValue> {
    let model = definition.build(ProviderBuildConfig {
        credential,
        model: route.provider.model,
        base_url,
        reasoning_effort: route.provider.reasoning_effort,
        web_search: route.provider.web_search,
        http: http.clone(),
    })?;
    let mut choice = route.choice;
    choice.supports_image_input = model.supports_image_input();
    let id = choice.route.clone();
    Ok(RouteValue { choice, id, model })
}

struct RouteValue {
    id: String,
    choice: ModelChoice,
    model: Arc<dyn Model>,
}

struct UnavailableModel {
    info: ModelInfo,
    supports_image_input: bool,
}

impl Model for UnavailableModel {
    fn info(&self) -> ModelInfo {
        self.info.clone()
    }

    fn supports_image_input(&self) -> bool {
        self.supports_image_input
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

fn unavailable_models(
    gateway: &GatewayConfig,
    selection: &ProviderConfig,
) -> Result<(Arc<ModelRouter>, i64)> {
    let definition = provider(&selection.provider)?;
    let context_window = definition
        .model(&selection.model)
        .map_or(DEFAULT_CONTEXT_WINDOW, |preset| preset.context_window);
    let effort = match gateway.configured_providers.get(&selection.provider) {
        Some(configured) => {
            gateway.validate_provider_selection(selection)?;
            effective_reasoning_effort(definition, configured, selection).map(str::to_string)
        }
        None => selection.reasoning_effort.clone().or_else(|| {
            definition
                .model(&selection.model)
                .and_then(|preset| preset.default_reasoning.map(str::to_string))
        }),
    };
    let route = model_route_id(&selection.provider, &selection.model, effort.as_deref());
    let model: Arc<dyn Model> = Arc::new(UnavailableModel {
        info: ModelInfo {
            model: selection.model.clone(),
            reasoning_effort: effort.clone(),
        },
        supports_image_input: definition.supports_image_input(),
    });
    let mut router = ModelRouter::new(&route, model);
    router.configure_choice(ModelChoice {
        route,
        group: selection.model.clone(),
        model: selection.model.clone(),
        reasoning_effort: effort,
        context_window: Some(context_window),
        supports_image_input: definition.supports_image_input(),
    })?;
    Ok((Arc::new(router), context_window))
}

fn build_middleware(
    settings: &MiddlewareConfig,
    workspace: &std::path::Path,
    cron: Arc<CronStore>,
    scratchpad: ScratchpadStore,
    session_files: SessionFileStore,
) -> Result<(MiddlewareStack, Option<Arc<OnceLock<AgentConfig>>>)> {
    let mut entries: Vec<Arc<dyn Middleware>> = Vec::new();
    let mut subagent_template = None;
    for feature in MIDDLEWARE.iter().filter(|feature| {
        feature.manifest.required
            || settings.enabled(feature.manifest.id)
            || matches!(feature.kind, BuiltinMiddleware::Scratchpad)
    }) {
        let middleware: Arc<dyn Middleware> = match feature.kind {
            BuiltinMiddleware::Sandbox => continue,
            BuiltinMiddleware::Attachments => Arc::new(Attachments::new(session_files.clone())),
            BuiltinMiddleware::Artifacts => Arc::new(Artifacts::new(session_files.clone())),
            BuiltinMiddleware::Tools => Arc::new(Tools::coding()),
            BuiltinMiddleware::Instructions => Arc::new(Instructions::discover(workspace)?),
            BuiltinMiddleware::Cron => {
                let cron = Arc::clone(&cron);
                Arc::new(Cron::new(move |session_id, task, schedule| {
                    cron.add_managed(session_id, task, schedule)
                        .map(|task| task.id)
                        .map_err(|error| HorusError::Tool(error.to_string()))
                }))
            }
            BuiltinMiddleware::Scratchpad => Arc::new(
                Scratchpad::new(scratchpad.clone()).agent_enabled(settings.enabled("scratchpad")),
            ),
            BuiltinMiddleware::Skills => Arc::new(Skills::discover_installed([
                workspace.join(".agents/skills"),
                workspace.join(".codex/skills"),
            ])?),
            BuiltinMiddleware::Tasks => Arc::new(Tasks),
            BuiltinMiddleware::Subagents => {
                let template = Arc::new(OnceLock::<AgentConfig>::new());
                let max_depth = u8::try_from(crate::middleware_manifest::integer_setting(
                    settings,
                    "subagents",
                    "max_depth",
                )?)
                .map_err(|_| {
                    Error::Config("subagent max depth must fit an unsigned byte".into())
                })?;
                let middleware = Subagents::new(
                    max_depth,
                    crate::middleware_manifest::usize_setting(
                        settings,
                        "subagents",
                        "max_concurrency",
                    )?,
                    crate::middleware_manifest::usize_setting(settings, "subagents", "max_agents")?,
                    subagent_launcher(&template),
                )?;
                let middleware = match crate::middleware_manifest::string_setting(
                    settings,
                    "subagents",
                    "model_route",
                )? {
                    Some(route) => middleware.default_model(route),
                    None => middleware,
                };
                subagent_template = Some(template);
                Arc::new(middleware)
            }
            BuiltinMiddleware::Steering => Arc::new(Steering::new(
                crate::middleware_manifest::usize_setting(settings, "steering", "max_pending")?,
            )?),
            BuiltinMiddleware::ContextOffloading => Arc::new(ContextOffloading::new(
                crate::middleware_manifest::integer_setting(
                    settings,
                    "context_offloading",
                    "stale_after_tokens",
                )?,
            )?),
            BuiltinMiddleware::Compaction => Arc::new(Compaction::new(
                crate::middleware_manifest::integer_setting(settings, "compaction", "at_tokens")?,
            )?),
            BuiltinMiddleware::Sessions => Arc::new(Sessions::new(
                crate::middleware_manifest::usize_setting(settings, "sessions", "page_size")?,
            )?),
        };
        entries.push(middleware);
    }
    Ok((MiddlewareStack::new(entries)?, subagent_template))
}

#[cfg(test)]
mod tests {
    use horus::backend::checkpoint::{Checkpoint, sqlite::SqliteCheckpoint};
    use horus::backend::model::provider::HostedWebSearch;
    use horus::protocol::FrontendSymbol;

    use super::*;

    #[test]
    fn provider_status_uses_manifest_defaults() {
        let status = provider_status(provider("openai_socket").expect("provider"), false, None);

        assert_eq!(status.provider, "openai_socket");
        assert_eq!(status.label, "OpenAI");
        assert_eq!(status.symbol, FrontendSymbol::ChatGpt);
        assert_eq!(status.models[0].id, "gpt-5.6-sol");
        assert_eq!(
            status.default_api_key_env.as_deref(),
            Some("OPENAI_API_KEY")
        );
        assert_eq!(
            status.models[0].default_reasoning.as_deref(),
            Some("medium")
        );
        assert_eq!(status.web_search[0], HostedWebSearch::Off);

        let custom = provider_status(provider("responses").expect("provider"), false, None);
        assert!(custom.models.is_empty());
        assert!(custom.model_ids_configurable);
        assert!(custom.model_ids.is_empty());
        assert!(custom.reasoning_efforts.is_empty());
        assert_eq!(
            custom.default_base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(custom.default_api_key_env, None);

        let openrouter = provider_status(provider("openrouter").expect("provider"), false, None);
        assert!(openrouter.models.is_empty());
        assert!(openrouter.model_ids_configurable);
        assert_eq!(openrouter.default_base_url, None);
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
            model: "vendor/model-opaque".into(),
            base_url: Some("https://example.com/v1".into()),
            reasoning_effort: Some("provider-defined".into()),
            web_search: HostedWebSearch::Off,
        };
        let alternate_model = "vendor/model-alternate".to_string();
        let config = config
            .registering_provider(kimi, Vec::new(), Vec::new())
            .and_then(|config| {
                config.registering_provider(
                    custom.clone(),
                    vec![custom.model.clone(), alternate_model.clone()],
                    vec!["provider-defined".into(), "minimal".into()],
                )
            })
            .expect("register providers");

        let choices = configured_model_choices(&config, &store, &credentials).expect("catalog");
        let custom_route = choices
            .iter()
            .find(|choice| choice.model == custom.model)
            .expect("custom choice");
        let resolved =
            configured_provider_for_route(&config, &store, &credentials, &custom_route.route)
                .expect("resolve custom route");
        let model_providers =
            configured_model_providers(&config, &store, &credentials).expect("provider IDs");

        assert!(
            choices
                .first()
                .is_some_and(|choice| choice.route.starts_with("kimi::"))
        );
        assert_eq!(resolved, custom);
        assert_eq!(model_providers[&custom_route.route], "responses");
        assert!(choices.iter().any(|choice| choice.model == alternate_model));
        assert!(choices.iter().any(|choice| {
            choice.model == alternate_model && choice.reasoning_effort.as_deref() == Some("minimal")
        }));
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
    fn usage_sink_attributes_a_model_route_to_its_provider() {
        let root = tempfile::tempdir().expect("root");
        let state = root.path().join("state");
        let (store, config) = ConfigStore::initialize(
            state.clone(),
            "127.0.0.1:8741".parse().expect("listen address"),
            None,
        )
        .expect("config");
        let gateway = Mutex::new(config);
        let model_providers = BTreeMap::from([("primary".into(), "openai_socket".into())]);
        let usage = TokenUsage {
            input_tokens: 11,
            total_tokens: 11,
            ..TokenUsage::default()
        };

        persist_usage(&gateway, &store, &model_providers, "primary", &usage)
            .expect("persist usage");

        let (_, restored) = ConfigStore::open(state).expect("reopen config");
        let daily_usage = restored.profile().daily_usage;
        assert_eq!(daily_usage.len(), 1);
        assert_eq!(daily_usage[0].provider, "openai_socket");
        assert_eq!(daily_usage[0].usage, usage);
    }

    #[test]
    fn custom_selection_without_reasoning_uses_the_first_configured_effort() {
        let root = tempfile::tempdir().expect("root");
        let (store, config) = ConfigStore::initialize(
            root.path().join("state"),
            "127.0.0.1:8741".parse().expect("listen address"),
            None,
        )
        .expect("config");
        let credentials =
            CredentialStore::open(store.credentials_path()).expect("credential store");
        credentials
            .set(
                "responses",
                "custom-secret",
                Some("http://127.0.0.1:11434/v1"),
            )
            .expect("custom credential");
        let selection = ProviderConfig {
            provider: "responses".into(),
            model: "local-model".into(),
            base_url: Some("http://127.0.0.1:11434/v1".into()),
            reasoning_effort: None,
            web_search: HostedWebSearch::Off,
        };
        let config = config
            .registering_provider(
                selection.clone(),
                vec![selection.model.clone()],
                vec!["high".into(), "medium".into()],
            )
            .expect("register provider");

        let choices = configured_model_choices(&config, &store, &credentials).expect("catalog");
        let (router, _) =
            build_models(&config, &selection, &store, &credentials).expect("build selected model");
        let selected = router.choices().next().expect("selected route");

        assert_eq!(choices[0].reasoning_effort.as_deref(), Some("high"));
        assert_eq!(selected.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(router.default_provider(), choices[0].route);
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
        let gateway = gateway
            .registering_provider(
                crate::wire::AgentComposition::default().provider,
                Vec::new(),
                Vec::new(),
            )
            .expect("register provider");
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
            .save(&checkpoint, &[], None)
            .await
            .expect("seed checkpoint");
        let (reusable_router, _) = unavailable_models(&gateway, &original.agent.config.provider)
            .expect("unavailable model router");
        let mut composition = original.agent.config.clone();
        composition.middleware.set_enabled("cron", false);
        composition.middleware.set_enabled("scratchpad", false);
        composition.system_prompt = "updated instructions".into();
        let updated = original
            .replacing_agent(1, composition, &gateway, store.state_dir(), None)
            .expect("updated chat spec");
        let gateway = Arc::new(Mutex::new(gateway));

        let built = assemble(
            gateway,
            &updated,
            &store,
            credentials,
            cron,
            Arc::clone(&checkpoints),
            ScratchpadStore::new(Arc::clone(&checkpoints)),
            SessionFileStore::new(store.state_dir()),
            Some("chat".into()),
            "test",
            true,
            Some(Arc::clone(&reusable_router)),
        )
        .await
        .expect("assemble chat");
        assert!(Arc::ptr_eq(&reusable_router, &built.model_router));
        let scratchpad = built
            .agent
            .frontend()
            .contributions()
            .iter()
            .find(|contribution| contribution.capability == "scratchpad")
            .expect("disabled scratchpad management surface");
        assert_eq!(scratchpad.commands.len(), 1);
        assert_eq!(scratchpad.commands[0].name, "scratchpad");
        assert_eq!(scratchpad.widgets.len(), 2);
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
        assert!(!saved.agent.config.middleware.enabled("cron"));
        assert_eq!(saved.agent.config.system_prompt, "updated instructions");
    }
}
