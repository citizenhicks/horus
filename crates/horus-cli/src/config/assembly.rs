use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use horus::Error;
use horus::Result;
use horus::agent::AgentConfig;
use horus::agent::create_agent;
use horus::backend::checkpoint::CheckpointStore;
use horus::backend::checkpoint::sqlite::SqliteCheckpoint;
use horus::backend::model::Model;
use horus::backend::model::ModelChoice;
use horus::backend::model::ModelRouter;
use horus::backend::model::provider::HttpClient;
use horus::backend::model::provider::ProviderAuth;
use horus::backend::model::provider::ProviderBuildConfig;
use horus::backend::model::provider::ProviderCredential;
use horus::backend::model::provider::provider;
use horus::backend::model::provider::streaming_client;
use horus::backend::sandbox::Sandbox;
use horus::backend::sandbox::SandboxBackend;
use horus::backend::sandbox::local::LocalSandbox;
use horus::middleware::Middleware;
use horus::middleware::MiddlewareStack;
use horus::middleware::compaction::Compaction;
use horus::middleware::sessions::Sessions;
use horus::middleware::skills::Skills;
use horus::middleware::steering::Steering;
use horus::middleware::subagents::SubagentLaunch;
use horus::middleware::subagents::SubagentLauncher;
use horus::middleware::subagents::Subagents;
use horus::middleware::tools::Tools;
use horus::protocol::SessionContext;

use super::CheckpointSettings;
use super::FileConfig;
use super::MiddlewareSettings;
use super::ModelSettings;
use super::SandboxSettings;
use super::api_key;
use super::auth_path;
use super::state_path;
use super::workspace_path;

pub(crate) struct BuiltAgentConfig {
    pub(crate) config: AgentConfig,
    _subagent_template: Option<Arc<OnceLock<AgentConfig>>>,
}

impl FileConfig {
    pub(crate) fn build(
        self,
        workspace: &Path,
        state_dir: &Path,
        resume_session: Option<String>,
    ) -> Result<BuiltAgentConfig> {
        self.validate()?;
        let needs_subagents = self
            .middleware
            .iter()
            .any(|setting| matches!(setting, MiddlewareSettings::Subagents { .. }));
        let models = build_models(&self.models, &self.agent.model, state_dir)?;
        let SandboxSettings {
            command_timeout_seconds,
            approval,
        } = self.sandbox;
        let backend: Arc<dyn SandboxBackend> = Arc::new(
            LocalSandbox::new(workspace)?
                .command_timeout(Duration::from_secs(command_timeout_seconds))?,
        );
        let sandbox = Arc::new(Sandbox::new(backend, approval));
        std::fs::create_dir_all(state_dir)?;
        let CheckpointSettings { path } = self.checkpoint;
        let checkpoints: Arc<dyn CheckpointStore> =
            Arc::new(SqliteCheckpoint::new(state_path(state_dir, &path)?)?);
        // The launcher is middleware inside the config it must clone; fill that cycle once
        // after assembly so child agents reuse the exact root composition.
        let template = needs_subagents.then(|| Arc::new(OnceLock::<AgentConfig>::new()));
        let launch_agent = template.as_ref().map(|template| {
            let child_template = Arc::downgrade(template);
            let launcher: SubagentLauncher = Arc::new(move |launch: SubagentLaunch| {
                let child_template = child_template.clone();
                Box::pin(async move {
                    let config = child_template
                        .upgrade()
                        .ok_or_else(|| Error::Stopped("subagent launcher is unavailable".into()))?
                        .get()
                        .ok_or_else(|| {
                            Error::Config("subagent launcher is not initialized".into())
                        })?
                        .clone()
                        .session_id(launch.session_id)
                        .metadata(launch.metadata)
                        .model_route(&launch.model, launch.reasoning_effort.as_deref())?;
                    create_agent(config).await
                })
            });
            launcher
        });
        let middleware = build_middleware(&self.middleware, workspace, launch_agent)?;
        let mut config = AgentConfig::new(
            models,
            sandbox,
            checkpoints,
            middleware,
            self.agent.system_prompt,
        )
        .model_route(&self.agent.model, None)?
        .session_context(SessionContext {
            user_name: local_user_name(),
            ..SessionContext::default()
        })
        .context_window(self.agent.context_window);
        if let Some(session_id) = resume_session {
            if session_id.trim().is_empty() {
                return Err(Error::Config("session ID cannot be empty".into()));
            }
            config = config.session_id(session_id);
        }
        if let Some(template) = &template {
            template
                .set(config.clone())
                .map_err(|_| Error::Config("subagent launcher was initialized twice".into()))?;
        }
        Ok(BuiltAgentConfig {
            config,
            _subagent_template: template,
        })
    }
}

fn local_user_name() -> Option<String> {
    ["USER", "USERNAME"].into_iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}

pub(super) fn build_models(
    routes: &BTreeMap<String, ModelSettings>,
    default: &str,
    state_dir: &Path,
) -> Result<Arc<ModelRouter>> {
    // One client per assembly: route and reasoning variants clone it and share its
    // connection pool instead of building a pool per provider instance.
    let http = streaming_client()?;
    let default_model = routes
        .get(default)
        .ok_or_else(|| Error::Unknown(format!("model route `{default}`")))?;
    let mut router = ModelRouter::new(
        default,
        build_model(
            default_model,
            default_model.reasoning_effort.as_deref(),
            credential(default_model, state_dir)?,
            &http,
        )?,
    );
    for (id, settings) in routes {
        if id != default {
            router.register(
                id,
                build_model(
                    settings,
                    settings.reasoning_effort.as_deref(),
                    credential(settings, state_dir)?,
                    &http,
                )?,
            )?;
        }
    }

    let configured = std::iter::once((default, default_model)).chain(
        routes
            .iter()
            .filter(|(id, _)| id.as_str() != default)
            .map(|(id, settings)| (id.as_str(), settings)),
    );
    let mut choices = Vec::new();
    for (id, settings) in configured {
        let definition = provider(&settings.provider)?;
        let preset = definition.model(&settings.model);
        let effective_effort = settings
            .reasoning_effort
            .as_deref()
            .or_else(|| preset.and_then(|preset| preset.default_reasoning));
        let context_window = settings
            .context_window
            .or_else(|| preset.map(|preset| preset.context_window));
        choices.push(ModelChoice {
            route: id.to_string(),
            group: id.to_string(),
            model: settings.model.clone(),
            reasoning_effort: effective_effort.map(str::to_string),
            context_window,
        });
        for reasoning in preset
            .into_iter()
            .flat_map(|preset| preset.reasoning)
            .filter(|reasoning| Some(reasoning.id) != effective_effort)
        {
            let route = format!("__horus:{id}:reasoning:{}", reasoning.id);
            router.register(
                &route,
                build_model(
                    settings,
                    Some(reasoning.id),
                    credential(settings, state_dir)?,
                    &http,
                )?,
            )?;
            choices.push(ModelChoice {
                route,
                group: id.to_string(),
                model: settings.model.clone(),
                reasoning_effort: Some(reasoning.id.to_string()),
                context_window,
            });
        }
    }
    for choice in choices {
        router.configure_choice(choice)?;
    }
    Ok(Arc::new(router))
}

fn build_model(
    settings: &ModelSettings,
    reasoning_effort: Option<&str>,
    credential: ProviderCredential,
    http: &HttpClient,
) -> Result<Arc<dyn Model>> {
    let definition = provider(&settings.provider)?;
    definition.build(ProviderBuildConfig {
        credential,
        model: settings.model.clone(),
        base_url: settings.base_url.clone(),
        reasoning_effort: reasoning_effort.map(str::to_string),
        web_search: settings.web_search,
        http: http.clone(),
    })
}

fn credential(settings: &ModelSettings, state_dir: &Path) -> Result<ProviderCredential> {
    let definition = provider(&settings.provider)?;
    match definition.auth() {
        ProviderAuth::ApiKey(default_env) => {
            let name = settings.api_key_env.as_deref().unwrap_or(default_env);
            Ok(ProviderCredential::ApiKey(api_key(
                settings.api_key.as_deref(),
                name,
            )?))
        }
        ProviderAuth::Browser(auth) => auth.load(&auth_path(state_dir)),
    }
}

pub(super) fn build_middleware(
    settings: &[MiddlewareSettings],
    workspace: &Path,
    launch_agent: Option<SubagentLauncher>,
) -> Result<MiddlewareStack> {
    let mut entries: Vec<Arc<dyn Middleware>> = Vec::with_capacity(settings.len());
    for setting in settings {
        let entry: Arc<dyn Middleware> = match setting {
            MiddlewareSettings::Tools => Arc::new(Tools::coding()),
            MiddlewareSettings::Skills { roots, prompt } => {
                let middleware = Skills::discover_installed(
                    roots.iter().map(|root| workspace_path(workspace, root)),
                )?;
                Arc::new(match prompt {
                    Some(prompt) => middleware.prompt(prompt.as_str())?,
                    None => middleware,
                })
            }
            MiddlewareSettings::Subagents {
                max_depth,
                max_concurrency,
                max_agents,
                default_model,
                default_reasoning,
                prompt,
            } => {
                let launcher = launch_agent
                    .as_ref()
                    .ok_or_else(|| Error::Config("subagent launcher is required".into()))?;
                let mut middleware = Subagents::new(
                    *max_depth,
                    *max_concurrency,
                    *max_agents,
                    Arc::clone(launcher),
                )?;
                if let Some(prompt) = prompt {
                    middleware = middleware.prompt(prompt.as_str())?;
                }
                if let Some(reasoning) = default_reasoning {
                    middleware = middleware.default_reasoning(reasoning.as_str())?;
                }
                Arc::new(match default_model {
                    Some(route) => middleware.default_model(route),
                    None => middleware,
                })
            }
            MiddlewareSettings::Steering => Arc::new(Steering::default()),
            MiddlewareSettings::Compaction { at_tokens } => Arc::new(Compaction::new(*at_tokens)?),
            MiddlewareSettings::Sessions => Arc::new(Sessions::default()),
        };
        entries.push(entry);
    }
    MiddlewareStack::new(entries)
}
