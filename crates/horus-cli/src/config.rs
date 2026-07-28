//! Manifest-driven assembly for Horus's built-in application.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use horus::Error;
use horus::Result;
#[cfg(test)]
use horus::backend::checkpoint::CheckpointStore;
#[cfg(test)]
use horus::backend::checkpoint::sqlite::SqliteCheckpoint;
use horus::backend::model::provider::HostedWebSearch;
use horus::backend::model::provider::ProviderAuth;
use horus::backend::model::provider::provider;
use horus::backend::sandbox::ApprovalPolicy;
use serde::Deserialize;
use serde::Serialize;

pub(crate) const DEFAULT_CONTEXT_WINDOW: i64 = 272_000;
pub(crate) const DEFAULT_COMMAND_TIMEOUT_SECONDS: u64 = 120;
pub(crate) const DEFAULT_SUBAGENT_CONCURRENCY: usize = 21;
pub(crate) const DEFAULT_SUBAGENT_MAX_AGENTS: usize = 64;

mod assembly;
pub(crate) use assembly::BuiltAgentConfig;

pub(crate) const DEFAULT_SYSTEM_PROMPT: &str = r#"You are Horus, a general purpose agent working in the user’s current workspace.
Complete the request using the available tools. Inspect relevant files and repository instructions before editing. Fix root causes, keep changes focused and simple, and preserve unrelated work. Continue until resolved; ask only when a missing decision materially changes the result. For longer tasks, emit short updates from time to time to the user to keep them in the loop.
Respect tool constraints and approvals. Do not commit, publish, or perform destructive actions unless explicitly requested. Verify changes with the smallest relevant checks. Be concise and report outcomes, assumptions, blockers, and unfinished work."#;

#[cfg(test)]
use self::assembly::{build_middleware, build_models};

#[derive(PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileConfig {
    pub(crate) agent: AgentSettings,
    pub(crate) models: BTreeMap<String, ModelSettings>,
    pub(crate) middleware: Vec<MiddlewareSettings>,
    pub(crate) sandbox: SandboxSettings,
    pub(crate) checkpoint: CheckpointSettings,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentSettings {
    pub(crate) model: String,
    pub(crate) system_prompt: String,
    pub(crate) context_window: i64,
}

#[derive(PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelSettings {
    pub(crate) provider: String,
    pub(crate) model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) context_window: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<String>,
    #[serde(default)]
    pub(crate) web_search: HostedWebSearch,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum MiddlewareSettings {
    Tools,
    Skills {
        roots: Vec<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    Subagents {
        max_depth: u8,
        max_concurrency: usize,
        max_agents: usize,
        default_model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_reasoning: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    Steering,
    Compaction {
        at_tokens: i64,
    },
    Sessions,
}

impl MiddlewareSettings {
    fn name(&self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Skills { .. } => "skills",
            Self::Subagents { .. } => "subagents",
            Self::Steering => "steering",
            Self::Compaction { .. } => "compaction",
            Self::Sessions => "sessions",
        }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SandboxSettings {
    pub(crate) command_timeout_seconds: u64,
    pub(crate) approval: ApprovalPolicy,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckpointSettings {
    pub(crate) path: PathBuf,
}

impl FileConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.agent.model.trim().is_empty() || !self.models.contains_key(&self.agent.model) {
            return Err(Error::Config(format!(
                "agent model route `{}` is not registered",
                self.agent.model
            )));
        }
        if self.agent.system_prompt.trim().is_empty() {
            return Err(Error::Config("system prompt cannot be empty".into()));
        }
        if self.agent.context_window <= 0 {
            return Err(Error::Config("context window must be positive".into()));
        }
        for (id, model) in &self.models {
            validate_route(id, model)?;
        }
        let mut names = BTreeSet::new();
        for middleware in &self.middleware {
            if !names.insert(middleware.name()) {
                return Err(Error::Duplicate(format!(
                    "middleware `{}`",
                    middleware.name()
                )));
            }
            match middleware {
                MiddlewareSettings::Subagents { default_model, .. } => {
                    if let Some(route) = default_model
                        && !self.models.contains_key(route)
                    {
                        return Err(Error::Config(format!(
                            "subagent model route `{route}` is not registered"
                        )));
                    }
                }
                MiddlewareSettings::Compaction { at_tokens } if *at_tokens <= 0 => {
                    return Err(Error::Config(
                        "compaction threshold must be positive".into(),
                    ));
                }
                MiddlewareSettings::Tools
                | MiddlewareSettings::Skills { .. }
                | MiddlewareSettings::Steering
                | MiddlewareSettings::Compaction { .. }
                | MiddlewareSettings::Sessions => {}
            }
        }
        if !names.contains("tools") {
            return Err(Error::Config(
                "required middleware `tools` is missing".into(),
            ));
        }
        if self.sandbox.command_timeout_seconds == 0 {
            return Err(Error::Config(
                "sandbox command timeout must be positive".into(),
            ));
        }
        state_path(Path::new("."), &self.checkpoint.path)?;
        Ok(())
    }
}

fn validate_route(id: &str, settings: &ModelSettings) -> Result<()> {
    if id.is_empty() || id.chars().any(char::is_whitespace) {
        return Err(Error::Config(format!("invalid model route `{id}`")));
    }
    let definition = provider(&settings.provider)?;
    if settings.model.trim().is_empty() {
        return Err(Error::Config(format!(
            "model route `{id}` has an empty model"
        )));
    }
    match definition.auth() {
        ProviderAuth::ApiKey(default_env) => {
            let api_key_env = settings.api_key_env.as_deref().unwrap_or(default_env);
            if !valid_env_name(api_key_env) {
                return Err(Error::Config(format!(
                    "model route `{id}` has an invalid API-key environment variable"
                )));
            }
            if settings
                .api_key
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                return Err(Error::Config(format!(
                    "model route `{id}` has an empty API key"
                )));
            }
        }
        ProviderAuth::Browser(_)
            if settings.api_key.is_some() || settings.api_key_env.is_some() =>
        {
            return Err(Error::Config(format!(
                "model route `{id}` cannot configure an API key for browser login"
            )));
        }
        ProviderAuth::Browser(_) => {}
    }
    if settings.context_window.is_some_and(|value| value <= 0) {
        return Err(Error::Config(format!(
            "model route `{id}` has a non-positive context window"
        )));
    }
    if settings
        .reasoning_effort
        .as_deref()
        .is_some_and(|effort| effort.trim().is_empty())
    {
        return Err(Error::Config(format!(
            "model route `{id}` has an empty reasoning effort"
        )));
    }
    definition.build_config_is_valid(
        &settings.model,
        settings.base_url.as_deref(),
        settings.reasoning_effort.as_deref(),
        settings.web_search,
    )?;
    Ok(())
}

fn api_key(configured: Option<&str>, name: &str) -> Result<String> {
    if let Some(value) = configured {
        return Ok(value.to_string());
    }
    let value =
        env::var(name).map_err(|_| Error::Config(format!("set {name} before starting Horus")))?;
    if value.trim().is_empty() {
        return Err(Error::Config(format!("{name} is empty")));
    }
    Ok(value)
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

pub(crate) fn parse_config(path: &Path, contents: &str) -> Result<FileConfig> {
    toml::from_str(contents)
        .map_err(|error| Error::Config(format!("{}: {}", path.display(), error.message())))
}

pub(crate) enum SaveMode {
    New,
    Replace,
}

pub(crate) fn save_config(path: &Path, config: &FileConfig, mode: SaveMode) -> Result<()> {
    config.validate()?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::Config("configuration path has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let contents = toml::to_string_pretty(config)
        .map_err(|error| Error::Config(format!("cannot encode config: {error}")))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(contents.as_bytes())?;
    file.as_file().sync_all()?;
    let result = match mode {
        SaveMode::New => file.persist_noclobber(path),
        SaveMode::Replace => file.persist(path),
    };
    result.map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn state_dir() -> Result<PathBuf> {
    resolve_state_dir(
        env::var_os("HORUS_STATE_DIR").map(PathBuf::from),
        env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from),
    )
}

fn resolve_state_dir(configured: Option<PathBuf>, home: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = configured {
        if path.as_os_str().is_empty() {
            return Err(Error::Config("HORUS_STATE_DIR is empty".into()));
        }
        return Ok(path);
    }
    home.filter(|path| !path.as_os_str().is_empty())
        .map(|path| path.join(".horus"))
        .ok_or_else(|| {
            Error::Config("cannot determine the home directory; set HORUS_STATE_DIR".into())
        })
}

pub(crate) fn auth_path(state_dir: &Path) -> PathBuf {
    state_dir.join("auth.json")
}

pub(crate) fn config_path(workspace: &Path, state_dir: &Path) -> PathBuf {
    env::var_os("HORUS_CONFIG")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                workspace.join(path)
            }
        })
        .unwrap_or_else(|| state_dir.join("config.toml"))
}

fn workspace_path(workspace: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    }
}

fn state_path(state_dir: &Path, path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(Error::Config(format!(
            "checkpoint path must stay inside the state directory: {}",
            path.display()
        )));
    }
    Ok(state_dir.join(path))
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
