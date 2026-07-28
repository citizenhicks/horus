use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::env;
use std::path::PathBuf;

use horus::backend::model::provider::ModelPreset;
use horus::backend::model::provider::ProviderAuth;
use horus::backend::model::provider::ProviderDefinition;
use horus::backend::model::provider::providers;
use horus::backend::sandbox::ApprovalPolicy;
use horus::middleware::compaction::DEFAULT_COMPACTION_TOKENS;

use crate::config::AgentSettings;
use crate::config::CheckpointSettings;
use crate::config::DEFAULT_COMMAND_TIMEOUT_SECONDS;
use crate::config::DEFAULT_CONTEXT_WINDOW;
use crate::config::DEFAULT_SUBAGENT_CONCURRENCY;
use crate::config::DEFAULT_SUBAGENT_MAX_AGENTS;
use crate::config::DEFAULT_SYSTEM_PROMPT;
use crate::config::FileConfig;
use crate::config::MiddlewareSettings;
use crate::config::ModelSettings;
use crate::config::SandboxSettings;

pub(super) const FEATURES: [SetupFeature; 5] = [
    SetupFeature::Skills,
    SetupFeature::Subagents,
    SetupFeature::Steering,
    SetupFeature::Compaction,
    SetupFeature::Sessions,
];

pub(super) struct ApprovalChoice {
    pub(super) label: &'static str,
    pub(super) description: &'static str,
    pub(super) policy: ApprovalPolicy,
}

pub(super) const APPROVALS: [ApprovalChoice; 3] = [
    ApprovalChoice {
        label: "On",
        description: "Pause before approval-required tools",
        policy: ApprovalPolicy::On,
    },
    ApprovalChoice {
        label: "Allow (no network)",
        description: "Run sandboxed tools without prompting or network",
        policy: ApprovalPolicy::Allow,
    },
    ApprovalChoice {
        label: "Allow (network)",
        description: "Run sandboxed tools without prompting, with network access",
        policy: ApprovalPolicy::AllowNetwork,
    },
];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum SetupFeature {
    Skills,
    Subagents,
    Steering,
    Compaction,
    Sessions,
}

impl SetupFeature {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Skills => "Skills",
            Self::Subagents => "Subagents",
            Self::Steering => "Steering",
            Self::Compaction => "Compaction",
            Self::Sessions => "Sessions",
        }
    }

    pub(super) fn description(self) -> &'static str {
        match self {
            Self::Skills => "Discover local SKILL.md capabilities",
            Self::Subagents => "Run independent work asynchronously",
            Self::Steering => "Accept guidance during an active turn",
            Self::Compaction => "Compact long conversations as context fills",
            Self::Sessions => "Resume and fork saved sessions",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Provider,
    Credential,
    Endpoint,
    Model,
    CustomModel,
    CustomContext,
    Reasoning,
    Search,
    Features,
    Approvals,
    Review,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SetupMode {
    Full,
    Provider,
    Credential,
}

pub(super) enum Flow {
    Continue,
    Finish,
    Cancel,
    Authenticate,
}

pub(super) struct SetupState {
    pub(super) step: Step,
    pub(super) provider: usize,
    pub(super) credential: String,
    pub(super) endpoint: String,
    pub(super) model: usize,
    pub(super) custom_model: String,
    pub(super) custom_context: String,
    pub(super) reasoning: usize,
    pub(super) search: usize,
    pub(super) features: BTreeSet<SetupFeature>,
    pub(super) feature: usize,
    pub(super) approvals: usize,
    pub(super) error: Option<String>,
    pub(super) repair_message: Option<String>,
    pub(super) mode: SetupMode,
    pub(super) configured_credentials: BTreeSet<String>,
    pub(super) auth_path: PathBuf,
    pub(super) oauth_url: Option<String>,
}

impl SetupState {
    pub(super) fn new(repair_message: Option<&str>, auth_path: PathBuf, mode: SetupMode) -> Self {
        Self {
            step: if mode == SetupMode::Credential {
                Step::Credential
            } else {
                Step::Provider
            },
            provider: 0,
            credential: String::new(),
            endpoint: String::new(),
            model: 0,
            custom_model: String::new(),
            custom_context: DEFAULT_CONTEXT_WINDOW.to_string(),
            reasoning: 0,
            search: 0,
            features: FEATURES.into_iter().collect(),
            feature: 0,
            approvals: 0,
            error: None,
            repair_message: repair_message.map(str::to_owned),
            mode,
            configured_credentials: BTreeSet::new(),
            auth_path,
            oauth_url: None,
        }
    }

    pub(super) fn provider(&self) -> &'static ProviderDefinition {
        &providers()[self.provider]
    }

    pub(super) fn model_preset(&self) -> Option<&'static ModelPreset> {
        self.provider().models().get(self.model)
    }

    fn model_id(&self) -> &str {
        self.model_preset()
            .map_or(self.custom_model.trim(), |model| model.id)
    }

    fn has_credential(&self) -> bool {
        self.configured_credentials.contains(self.provider().id())
            || match self.provider().auth() {
                ProviderAuth::ApiKey(name) => {
                    env::var(name).is_ok_and(|value| !value.trim().is_empty())
                }
                ProviderAuth::Browser(_) => false,
            }
    }

    pub(super) fn steps(&self) -> Vec<Step> {
        if self.mode == SetupMode::Credential {
            return vec![Step::Credential];
        }
        let mut steps = vec![Step::Provider];
        if !self.has_credential() {
            steps.push(Step::Credential);
        }
        if self.provider().configurable_base_url() {
            steps.push(Step::Endpoint);
        }
        steps.push(Step::Model);
        if self.model_preset().is_none() {
            steps.extend([Step::CustomModel, Step::CustomContext]);
        } else if self
            .model_preset()
            .is_some_and(|model| !model.reasoning.is_empty())
        {
            steps.push(Step::Reasoning);
        }
        if self.provider().web_search().len() > 1 {
            steps.push(Step::Search);
        }
        if self.mode == SetupMode::Full {
            steps.extend([Step::Features, Step::Approvals]);
        }
        steps.push(Step::Review);
        steps
    }

    pub(super) fn is_text_entry(&self) -> bool {
        matches!(
            self.step,
            Step::Endpoint | Step::CustomModel | Step::CustomContext
        ) || self.step == Step::Credential
            && matches!(self.provider().auth(), ProviderAuth::ApiKey(_))
    }

    pub(super) fn text_mut(&mut self) -> &mut String {
        match self.step {
            Step::Credential => &mut self.credential,
            Step::Endpoint => &mut self.endpoint,
            Step::CustomModel => &mut self.custom_model,
            Step::CustomContext => &mut self.custom_context,
            Step::Provider
            | Step::Model
            | Step::Reasoning
            | Step::Search
            | Step::Features
            | Step::Approvals
            | Step::Review => unreachable!("only text-entry steps request mutable text"),
        }
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        let count = self.selection_count();
        if count == 0 {
            return;
        }
        let selected = (self.selection() as isize + delta).rem_euclid(count as isize) as usize;
        self.select(selected);
        if self.step == Step::Model {
            self.reasoning = 0;
        }
        self.error = None;
    }

    pub(super) fn choose_number(&mut self, index: usize) -> Flow {
        if index >= self.selection_count() {
            return Flow::Continue;
        }
        self.select(index);
        if self.step == Step::Features {
            self.toggle_feature(index);
            return Flow::Continue;
        }
        if self.step == Step::Model {
            self.reasoning = 0;
        }
        self.confirm()
    }

    pub(super) fn toggle_feature(&mut self, index: usize) {
        let feature = FEATURES[index];
        if !self.features.remove(&feature) {
            self.features.insert(feature);
        }
    }

    pub(super) fn selection_count(&self) -> usize {
        match self.step {
            Step::Provider => providers().len(),
            Step::Model => self.provider().models().len() + 1,
            Step::Reasoning => self
                .model_preset()
                .map_or(1, |model| model.reasoning.len() + 1),
            Step::Search => self.provider().web_search().len(),
            Step::Features => FEATURES.len(),
            Step::Approvals => APPROVALS.len(),
            Step::Credential
            | Step::Endpoint
            | Step::CustomModel
            | Step::CustomContext
            | Step::Review => 0,
        }
    }

    pub(super) fn selection(&self) -> usize {
        match self.step {
            Step::Provider => self.provider,
            Step::Model => self.model,
            Step::Reasoning => self.reasoning,
            Step::Search => self.search,
            Step::Features => self.feature,
            Step::Approvals => self.approvals,
            Step::Credential
            | Step::Endpoint
            | Step::CustomModel
            | Step::CustomContext
            | Step::Review => 0,
        }
    }

    pub(super) fn select(&mut self, index: usize) {
        match self.step {
            Step::Provider => self.provider = index,
            Step::Model => self.model = index,
            Step::Reasoning => self.reasoning = index,
            Step::Search => self.search = index,
            Step::Features => self.feature = index,
            Step::Approvals => self.approvals = index,
            Step::Credential
            | Step::Endpoint
            | Step::CustomModel
            | Step::CustomContext
            | Step::Review => {}
        }
    }

    pub(super) fn confirm(&mut self) -> Flow {
        match self.step {
            Step::Provider => {
                self.credential.clear();
                self.endpoint = self
                    .provider()
                    .default_base_url()
                    .unwrap_or_default()
                    .into();
                self.model = 0;
                self.custom_model.clear();
                self.custom_context = DEFAULT_CONTEXT_WINDOW.to_string();
                self.reasoning = 0;
                self.search = 0;
            }
            Step::Credential => match self.provider().auth() {
                ProviderAuth::ApiKey(_) if self.credential.trim().is_empty() => {
                    self.error = Some("API key cannot be empty".into());
                    return Flow::Continue;
                }
                ProviderAuth::ApiKey(_) if self.mode == SetupMode::Credential => {
                    return Flow::Finish;
                }
                ProviderAuth::ApiKey(_) => {}
                ProviderAuth::Browser(_) => return Flow::Authenticate,
            },
            Step::Endpoint => {
                if let Err(error) = self
                    .provider()
                    .validate_base_url(Some(self.endpoint.trim()))
                {
                    self.error = Some(error.to_string());
                    return Flow::Continue;
                }
            }
            Step::CustomModel => {
                if self.custom_model.trim().is_empty() {
                    self.error = Some("model ID cannot be empty".into());
                    return Flow::Continue;
                }
            }
            Step::CustomContext => {
                if self
                    .custom_context
                    .trim()
                    .parse::<i64>()
                    .ok()
                    .is_none_or(|value| value <= 0)
                {
                    self.error = Some("context window must be a positive token count".into());
                    return Flow::Continue;
                }
            }
            Step::Review => return Flow::Finish,
            Step::Model | Step::Reasoning | Step::Search | Step::Features | Step::Approvals => {}
        }
        self.advance();
        Flow::Continue
    }

    pub(super) fn advance(&mut self) {
        let steps = self.steps();
        let index = steps
            .iter()
            .position(|step| *step == self.step)
            .unwrap_or(0);
        if let Some(next) = steps.get(index + 1) {
            self.step = *next;
        }
        self.error = None;
    }

    pub(super) fn back(&mut self) -> Flow {
        let steps = self.steps();
        let index = steps
            .iter()
            .position(|step| *step == self.step)
            .unwrap_or(0);
        let Some(previous) = index.checked_sub(1).and_then(|index| steps.get(index)) else {
            return Flow::Cancel;
        };
        self.step = *previous;
        self.error = None;
        Flow::Continue
    }

    pub(super) fn model_settings(&self) -> ModelSettings {
        let provider = self.provider();
        let reasoning_effort = self.model_preset().and_then(|model| {
            self.reasoning
                .checked_sub(1)
                .and_then(|index| model.reasoning.get(index))
                .map(|preset| preset.id.to_string())
        });
        let web_search = provider.web_search()[self.search];
        let (api_key, api_key_env) = match provider.auth() {
            ProviderAuth::ApiKey(name) => (
                (!self.credential.trim().is_empty()).then(|| self.credential.trim().to_string()),
                provider.configurable_base_url().then(|| name.to_string()),
            ),
            ProviderAuth::Browser(_) => (None, None),
        };
        ModelSettings {
            provider: provider.id().to_string(),
            model: self.model_id().to_string(),
            base_url: provider
                .configurable_base_url()
                .then(|| self.endpoint.trim().to_string()),
            api_key,
            api_key_env,
            context_window: self.model_preset().is_none().then(|| self.context_window()),
            reasoning_effort,
            web_search,
        }
    }

    pub(super) fn config(&self) -> FileConfig {
        let context_window = self.context_window();
        FileConfig {
            agent: AgentSettings {
                model: "default".into(),
                system_prompt: DEFAULT_SYSTEM_PROMPT.into(),
                context_window,
            },
            models: BTreeMap::from([("default".into(), self.model_settings())]),
            middleware: middleware_settings(&self.features),
            sandbox: SandboxSettings {
                command_timeout_seconds: DEFAULT_COMMAND_TIMEOUT_SECONDS,
                approval: APPROVALS[self.approvals].policy,
            },
            checkpoint: CheckpointSettings {
                path: PathBuf::from("horus.sqlite3"),
            },
        }
    }

    fn context_window(&self) -> i64 {
        self.model_preset()
            .map(|model| model.context_window)
            .unwrap_or_else(|| {
                self.custom_context
                    .trim()
                    .parse()
                    .expect("custom context was validated")
            })
    }
}

fn middleware_settings(features: &BTreeSet<SetupFeature>) -> Vec<MiddlewareSettings> {
    let mut middleware = vec![MiddlewareSettings::Tools];
    if features.contains(&SetupFeature::Skills) {
        middleware.push(MiddlewareSettings::Skills {
            roots: vec![
                PathBuf::from(".agents/skills"),
                PathBuf::from(".codex/skills"),
            ],
            prompt: None,
        });
    }
    if features.contains(&SetupFeature::Subagents) {
        middleware.push(MiddlewareSettings::Subagents {
            max_depth: 1,
            max_concurrency: DEFAULT_SUBAGENT_CONCURRENCY,
            max_agents: DEFAULT_SUBAGENT_MAX_AGENTS,
            default_model: None,
            default_reasoning: None,
            prompt: None,
        });
    }
    if features.contains(&SetupFeature::Steering) {
        middleware.push(MiddlewareSettings::Steering);
    }
    if features.contains(&SetupFeature::Compaction) {
        middleware.push(MiddlewareSettings::Compaction {
            at_tokens: DEFAULT_COMPACTION_TOKENS,
        });
    }
    if features.contains(&SetupFeature::Sessions) {
        middleware.push(MiddlewareSettings::Sessions);
    }
    middleware
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
