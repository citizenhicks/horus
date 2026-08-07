//! Gateway-native provider login and agent setup wizard.

use std::collections::BTreeSet;
use std::io;

use horus::backend::model::provider::HostedWebSearch;
use horus::protocol::{
    FrontendSetting, FrontendSettingKind, FrontendSettingValue, MiddlewareFeature,
};
use horus::{Error, Result};
use horus_gateway::client::{GatewayEvents, GatewaySender, MAX_PENDING_FRAMES};
use horus_gateway::wire::{
    AgentComposition, ClientMessage, MiddlewareConfig, ProviderAuthKind, ProviderConfig,
    ProviderStatus, ReadyPayload, ServerFrame, ServerMessage, SessionReadyPayload,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

use super::terminal::{INPUT_POLL, MAX_INPUT_BATCH, poll_event};
use super::terminal_text;
use super::theme::{Role, current};

const MAX_API_KEY_BYTES: usize = 16 * 1024;
const MAX_ENDPOINT_BYTES: usize = 4 * 1024;
const MAX_MODEL_IDS_BYTES: usize = 16 * 1024;
const MIN_INLINE_DESCRIPTION_WIDTH: usize = 20;

const CHANGE_CHAT_LABEL: &str = "Change for this chat only";
const CHANGE_CHAT_DESCRIPTION: &str = "Restart the active chat without changing future chats";
const SAVE_DEFAULT_LABEL: &str = "Save as default";
const SAVE_DEFAULT_DESCRIPTION: &str = "Use these settings for future chats only";

type SetupTerminal = Terminal<CrosstermBackend<io::Stdout>>;

/// The focused setup flow requested by the CLI shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetupMode {
    Login,
    Agent,
}

/// Runs one gateway-backed setup flow and updates its machine and chat snapshots.
pub(crate) async fn run(
    terminal: &mut SetupTerminal,
    mode: SetupMode,
    preferred_provider: Option<&str>,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    gateway: &mut ReadyPayload,
    session: &mut SessionReadyPayload,
) -> Result<()> {
    let mut state = SetupState::new(
        mode,
        preferred_provider,
        gateway,
        session.config.config.clone(),
        false,
    )?;
    terminal.clear()?;

    if !edit(terminal, &mut state, sender, events, gateway).await? {
        return Ok(());
    }
    apply(terminal, &mut state, sender, events, gateway, session).await?;
    Ok(())
}

/// Runs provider or default-agent setup without creating or changing a chat.
pub(crate) async fn run_gateway(
    terminal: &mut SetupTerminal,
    mode: SetupMode,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    gateway: &mut ReadyPayload,
) -> Result<()> {
    let original = gateway
        .default_config
        .as_ref()
        .map(|default| default.config.clone())
        .unwrap_or_default();
    if mode == SetupMode::Agent && gateway.default_config.is_none() {
        return Err(Error::Config(
            "configure a provider before changing gateway defaults".into(),
        ));
    }
    let mut state = SetupState::new(mode, None, gateway, original, true)?;
    terminal.clear()?;
    if !edit(terminal, &mut state, sender, events, gateway).await? {
        return Ok(());
    }
    apply_gateway(terminal, &mut state, sender, events, gateway).await
}

struct ProviderEntry {
    status: ProviderStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Provider,
    Authentication,
    Models,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Authenticate,
    Finish,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyTarget {
    Session,
    Default,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MiddlewareRow {
    Feature(usize),
    Setting { feature: usize, setting: usize },
}

struct Progress {
    title: &'static str,
    detail: String,
    verification: Option<(String, String)>,
}

#[derive(Clone, Copy)]
struct AgentLayout {
    width: usize,
    value_column: usize,
    description_column: Option<usize>,
}

struct SetupState {
    mode: SetupMode,
    providers: Vec<ProviderEntry>,
    original: AgentComposition,
    page: Page,
    provider: usize,
    credential: String,
    endpoint: String,
    endpoint_focused: bool,
    authenticated: Option<(String, Option<String>)>,
    model: usize,
    custom_model: String,
    reasoning: usize,
    web_search: usize,
    features: Vec<MiddlewareFeature>,
    middleware: MiddlewareConfig,
    target: ApplyTarget,
    default_only: bool,
    row: usize,
    error: Option<String>,
    progress: Option<Progress>,
}

impl SetupState {
    fn new(
        mode: SetupMode,
        preferred_provider: Option<&str>,
        gateway: &ReadyPayload,
        original: AgentComposition,
        default_only: bool,
    ) -> Result<Self> {
        let mut state = Self::from_parts(
            mode,
            validated_providers(&gateway.providers)?,
            gateway.middleware_features.clone(),
            original,
            default_only,
        )?;
        if let Some(provider) = preferred_provider {
            state.select_provider(provider)?;
        }
        Ok(state)
    }

    fn from_parts(
        mode: SetupMode,
        providers: Vec<ProviderEntry>,
        features: Vec<MiddlewareFeature>,
        original: AgentComposition,
        default_only: bool,
    ) -> Result<Self> {
        if providers.is_empty() {
            return Err(Error::Config(
                "the gateway did not advertise any providers".into(),
            ));
        }
        let provider = providers
            .iter()
            .position(|entry| entry.status.provider == original.provider.provider)
            .ok_or_else(|| {
                Error::Config(format!(
                    "the gateway did not advertise the active provider `{}`",
                    original.provider.provider
                ))
            })?;
        validate_active_provider(&providers[provider].status, &original.provider)?;
        let middleware = original.middleware.clone();
        let mut state = Self {
            mode,
            providers,
            original,
            page: match mode {
                SetupMode::Login => Page::Provider,
                SetupMode::Agent => Page::Agent,
            },
            provider,
            credential: String::new(),
            endpoint: String::new(),
            endpoint_focused: false,
            authenticated: None,
            model: 0,
            custom_model: String::new(),
            reasoning: 0,
            web_search: 0,
            features,
            middleware,
            target: if default_only {
                ApplyTarget::Default
            } else {
                ApplyTarget::Session
            },
            default_only,
            row: 0,
            error: None,
            progress: None,
        };
        state.reset_provider_fields();
        Ok(state)
    }

    fn entry(&self) -> &ProviderEntry {
        &self.providers[self.provider]
    }

    fn definition(&self) -> &ProviderStatus {
        &self.entry().status
    }

    fn select_provider(&mut self, provider: &str) -> Result<()> {
        self.provider = self
            .providers
            .iter()
            .position(|entry| entry.status.provider == provider)
            .ok_or_else(|| {
                Error::Config(format!(
                    "provider `{provider}` is not advertised by this gateway; run `/login` to choose an available provider"
                ))
            })?;
        self.reset_provider_fields();
        Ok(())
    }

    fn model_choice_count(&self) -> usize {
        self.definition().models.len().max(1)
    }

    fn reasoning_choice_count(&self) -> usize {
        self.definition()
            .models
            .get(self.model)
            .map_or(1, |model| model.reasoning.len() + 1)
    }

    fn search_choice_count(&self) -> usize {
        let count = self.definition().web_search.len();
        if count > 1 { count } else { 0 }
    }

    fn models_action_start(&self) -> usize {
        self.model_choice_count() + self.reasoning_choice_count() + self.search_choice_count()
    }

    fn agent_action_start(&self) -> usize {
        self.middleware_row_count()
    }

    fn middleware_row_count(&self) -> usize {
        self.features
            .iter()
            .map(|feature| feature.settings.len() + 1)
            .sum()
    }

    fn middleware_row(&self, row: usize) -> Option<MiddlewareRow> {
        let mut start = 0;
        for (feature, definition) in self.features.iter().enumerate() {
            if row == start {
                return Some(MiddlewareRow::Feature(feature));
            }
            let settings = start + 1..start + 1 + definition.settings.len();
            if settings.contains(&row) {
                return Some(MiddlewareRow::Setting {
                    feature,
                    setting: row - start - 1,
                });
            }
            start = settings.end;
        }
        None
    }

    fn apply_target_for_row(&self) -> Option<ApplyTarget> {
        let start = match self.page {
            Page::Models => self.models_action_start(),
            Page::Agent => self.agent_action_start(),
            Page::Provider | Page::Authentication => return None,
        };
        match (self.default_only, self.row.checked_sub(start)) {
            (true, Some(0)) => Some(ApplyTarget::Default),
            (false, Some(0)) => Some(ApplyTarget::Session),
            (false, Some(1)) => Some(ApplyTarget::Default),
            _ => None,
        }
    }

    fn row_count(&self) -> usize {
        match self.page {
            Page::Provider => self.providers.len(),
            Page::Authentication => 0,
            Page::Models => {
                self.model_choice_count()
                    + self.reasoning_choice_count()
                    + self.search_choice_count()
                    + if self.default_only { 1 } else { 2 }
            }
            Page::Agent => self.agent_action_start() + if self.default_only { 1 } else { 2 },
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.page {
            Page::Provider => {
                self.provider = (self.provider as isize + delta)
                    .rem_euclid(self.providers.len() as isize)
                    as usize;
                self.reset_provider_fields();
            }
            Page::Models | Page::Agent => {
                self.row =
                    (self.row as isize + delta).rem_euclid(self.row_count() as isize) as usize;
            }
            Page::Authentication => {}
        }
        self.error = None;
    }

    fn handle_key(&mut self, key: KeyEvent) -> Flow {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Flow::Continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'd'))
        {
            return Flow::Cancel;
        }
        match self.page {
            Page::Provider => self.handle_provider_key(key),
            Page::Authentication => self.handle_authentication_key(key),
            Page::Models => self.handle_models_key(key),
            Page::Agent => self.handle_agent_key(key),
        }
    }

    fn handle_provider_key(&mut self, key: KeyEvent) -> Flow {
        match key.code {
            KeyCode::Esc => return Flow::Cancel,
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Enter => {
                self.endpoint_focused = self.definition().configurable_base_url()
                    && self.definition().auth == ProviderAuthKind::DeviceCode;
                self.page = Page::Authentication;
                self.error = None;
            }
            _ => {}
        }
        Flow::Continue
    }

    fn handle_authentication_key(&mut self, key: KeyEvent) -> Flow {
        match key.code {
            KeyCode::Esc => {
                self.page = Page::Provider;
                self.error = None;
            }
            KeyCode::Tab | KeyCode::BackTab
                if self.definition().configurable_base_url()
                    && self.definition().auth == ProviderAuthKind::ApiKey =>
            {
                self.endpoint_focused = !self.endpoint_focused;
                self.error = None;
            }
            KeyCode::Enter => {
                if let Err(error) = self.authentication_ready() {
                    self.error = Some(error.to_string());
                    return Flow::Continue;
                }
                self.error = None;
                return Flow::Authenticate;
            }
            KeyCode::Backspace if self.authentication_is_editable() => {
                if self.endpoint_focused {
                    self.endpoint.pop();
                } else {
                    self.credential.pop();
                }
                self.error = None;
            }
            KeyCode::Char(character)
                if self.authentication_is_editable()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.push_text(&character.to_string());
            }
            _ => {}
        }
        Flow::Continue
    }

    fn handle_models_key(&mut self, key: KeyEvent) -> Flow {
        let custom_row = self.definition().model_ids_configurable.then_some(0);
        match key.code {
            KeyCode::Esc => {
                self.page = Page::Authentication;
                self.error = None;
            }
            KeyCode::Backspace if Some(self.row) == custom_row => {
                self.model = 0;
                self.custom_model.pop();
                self.error = None;
            }
            KeyCode::Char(character)
                if Some(self.row) == custom_row
                    && character != ' '
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.model = 0;
                self.push_text(&character.to_string());
            }
            KeyCode::Up | KeyCode::BackTab => self.move_selection(-1),
            KeyCode::Down | KeyCode::Tab => self.move_selection(1),
            KeyCode::Char(' ') => {
                if let Some(target) = self.apply_target_for_row() {
                    self.target = target;
                    return self.finish();
                }
                self.select_model_row();
            }
            KeyCode::Enter => {
                if let Some(target) = self.apply_target_for_row() {
                    self.target = target;
                    return self.finish();
                }
                self.select_model_row();
            }
            _ => {}
        }
        Flow::Continue
    }

    fn handle_agent_key(&mut self, key: KeyEvent) -> Flow {
        let middleware_row = self.middleware_row(self.row);
        match key.code {
            KeyCode::Esc => return Flow::Cancel,
            KeyCode::Up | KeyCode::BackTab => self.move_selection(-1),
            KeyCode::Down | KeyCode::Tab => self.move_selection(1),
            KeyCode::Char(' ') if matches!(middleware_row, Some(MiddlewareRow::Feature(_))) => {
                let MiddlewareRow::Feature(index) = middleware_row.expect("guarded feature row")
                else {
                    unreachable!()
                };
                let feature = &self.features[index];
                if !feature.required {
                    self.middleware
                        .set_enabled(&feature.id, !self.middleware.enabled(&feature.id));
                }
            }
            KeyCode::Char(' ') | KeyCode::Right
                if matches!(middleware_row, Some(MiddlewareRow::Setting { .. })) =>
            {
                self.adjust_middleware_setting(middleware_row.expect("guarded setting row"), 1);
            }
            KeyCode::Left if matches!(middleware_row, Some(MiddlewareRow::Setting { .. })) => {
                self.adjust_middleware_setting(middleware_row.expect("guarded setting row"), -1);
            }
            KeyCode::Enter | KeyCode::Char(' ') if self.apply_target_for_row().is_some() => {
                self.target = self
                    .apply_target_for_row()
                    .expect("guard requires an apply target");
                return self.finish();
            }
            _ => {}
        }
        Flow::Continue
    }

    fn select_model_row(&mut self) {
        let models = self.model_choice_count();
        if self.row < models {
            if self.model != self.row {
                self.model = self.row;
                self.reasoning = 0;
            }
        } else if self.row < models + self.reasoning_choice_count() {
            self.reasoning = self.row - models;
        } else if self.row < self.models_action_start() {
            self.web_search = self.row - models - self.reasoning_choice_count();
        }
        self.error = None;
    }

    fn adjust_middleware_setting(&mut self, row: MiddlewareRow, delta: isize) {
        let MiddlewareRow::Setting { feature, setting } = row else {
            return;
        };
        self.error = self
            .adjust_setting(feature, setting, delta)
            .err()
            .map(|error| error.to_string());
    }

    fn adjust_setting(&mut self, feature: usize, setting: usize, delta: isize) -> Result<()> {
        let feature = &self.features[feature];
        let setting = &feature.settings[setting];
        if !feature.required && !self.middleware.enabled(&feature.id) {
            return Ok(());
        }
        let value = match &setting.kind {
            FrontendSettingKind::Integer { min, max, step } => {
                let Some(FrontendSettingValue::Integer(current)) =
                    self.middleware.setting(&feature.id, &setting.id)
                else {
                    return Err(Error::Config(format!(
                        "{} requires an integer value",
                        setting.label
                    )));
                };
                let step = (*step).max(1);
                let next = if delta.is_positive() {
                    current.saturating_add(step)
                } else {
                    current.saturating_sub(step)
                };
                FrontendSettingValue::Integer(
                    max.map_or(next.max(*min), |max| next.max(*min).min(max)),
                )
            }
            FrontendSettingKind::Select {
                options,
                unset_label,
            } => {
                let offset = usize::from(unset_label.is_some());
                let count = options.len() + offset;
                if count == 0 {
                    return Err(Error::Config(format!(
                        "{} has no advertised choices",
                        setting.label
                    )));
                }
                let current = match self.middleware.setting(&feature.id, &setting.id) {
                    Some(FrontendSettingValue::String(value)) => options
                        .iter()
                        .position(|option| option.value == *value)
                        .map(|index| index + offset)
                        .ok_or_else(|| {
                            Error::Config(format!(
                                "{} is not in the gateway catalog",
                                setting.label
                            ))
                        })?,
                    None if unset_label.is_some() => 0,
                    Some(FrontendSettingValue::Integer(_)) | None => {
                        return Err(Error::Config(format!(
                            "{} requires a selected value",
                            setting.label
                        )));
                    }
                };
                let next = (current as isize + delta).rem_euclid(count as isize) as usize;
                if next < offset {
                    self.middleware.set_setting(&feature.id, &setting.id, None);
                    return Ok(());
                }
                FrontendSettingValue::String(options[next - offset].value.clone())
            }
        };
        self.middleware
            .set_setting(&feature.id, &setting.id, Some(value));
        Ok(())
    }

    fn finish(&mut self) -> Flow {
        if let Err(error) = self.authentication_ready() {
            self.error = Some(error.to_string());
            return Flow::Continue;
        }
        if let Err(error) = self.agent_composition(&self.original) {
            self.error = Some(error.to_string());
            return Flow::Continue;
        }
        Flow::Finish
    }

    fn authentication_is_editable(&self) -> bool {
        self.endpoint_focused || self.definition().auth == ProviderAuthKind::ApiKey
    }

    fn paste(&mut self, text: &str) {
        if self.page == Page::Authentication && self.authentication_is_editable() {
            self.push_text(text.trim());
        } else if self.page == Page::Models
            && self.definition().model_ids_configurable
            && self.row == 0
        {
            self.model = 0;
            self.push_text(text.trim());
        }
    }

    fn push_text(&mut self, text: &str) {
        let custom = self.page == Page::Models;
        let endpoint = self.page == Page::Authentication && self.endpoint_focused;
        let (target, limit) = if custom {
            (&mut self.custom_model, MAX_MODEL_IDS_BYTES)
        } else if endpoint {
            (&mut self.endpoint, MAX_ENDPOINT_BYTES)
        } else {
            (&mut self.credential, MAX_API_KEY_BYTES)
        };
        let mut rejected = false;
        for character in text.chars().filter(|character| !character.is_control()) {
            if target.len() + character.len_utf8() > limit {
                rejected = true;
                break;
            }
            target.push(character);
        }
        self.error = rejected.then(|| format!("input is limited to {limit} bytes"));
    }

    fn reset_provider_fields(&mut self) {
        self.credential.clear();
        self.authenticated = None;
        let definition = self.entry().status.clone();
        let current = &self.original.provider;
        let same_provider = current.provider == definition.provider;
        self.endpoint = if same_provider {
            current
                .base_url
                .as_deref()
                .or(definition.default_base_url.as_deref())
        } else {
            definition.default_base_url.as_deref()
        }
        .unwrap_or_default()
        .into();
        self.model = if definition.model_ids_configurable {
            0
        } else if same_provider {
            definition
                .models
                .iter()
                .position(|model| model.id == current.model)
                .expect("active provider model was validated")
        } else {
            0
        };
        self.custom_model = if definition.model_ids_configurable {
            let mut model_ids = definition.model_ids.clone();
            if same_provider && !model_ids.contains(&current.model) {
                model_ids.insert(0, current.model.clone());
            }
            model_ids.join(", ")
        } else {
            String::new()
        };
        let reasoning = if same_provider {
            current.reasoning_effort.as_deref()
        } else {
            definition
                .models
                .get(self.model)
                .and_then(|model| model.default_reasoning.as_deref())
        };
        self.reasoning = definition
            .models
            .get(self.model)
            .and_then(|model| {
                reasoning.and_then(|effort| {
                    model
                        .reasoning
                        .iter()
                        .position(|preset| preset.id == effort)
                })
            })
            .map_or(0, |index| index + 1);
        self.web_search = if same_provider {
            definition
                .web_search
                .iter()
                .position(|search| *search == current.web_search)
                .expect("active provider search mode was validated")
        } else {
            0
        };
        self.endpoint_focused = false;
        self.row = self.model;
        self.error = None;
    }

    fn configured_model_ids(&self) -> Result<Vec<String>> {
        if !self.definition().model_ids_configurable {
            return Ok(Vec::new());
        }
        let model_ids = self
            .custom_model
            .split(',')
            .map(str::trim)
            .map(str::to_string)
            .collect::<Vec<_>>();
        if model_ids.iter().any(String::is_empty) {
            return Err(Error::Config(
                "Enter one or more model IDs separated by commas".into(),
            ));
        }
        if model_ids.iter().collect::<BTreeSet<_>>().len() != model_ids.len() {
            return Err(Error::Config("Model IDs must be unique".into()));
        }
        Ok(model_ids)
    }

    fn selected_base_url(&self) -> Option<String> {
        self.definition()
            .configurable_base_url()
            .then(|| self.endpoint.trim().to_string())
    }

    fn authentication_target(&self) -> (String, Option<String>) {
        (self.definition().provider.clone(), self.selected_base_url())
    }

    fn authentication_succeeded(&mut self) {
        self.authenticated = Some(self.authentication_target());
        self.progress = None;
        self.page = Page::Models;
        self.row = self.model;
    }

    fn has_matching_credential(&self) -> bool {
        let target = self.authentication_target();
        self.authenticated.as_ref() == Some(&target)
            || !self.definition().configurable_base_url() && self.entry().status.configured
    }

    fn authentication_ready(&self) -> Result<()> {
        if self.mode == SetupMode::Agent {
            return Ok(());
        }
        if self.definition().configurable_base_url()
            && self
                .selected_base_url()
                .is_none_or(|url| url.trim().is_empty())
        {
            return Err(Error::Config("Base URL is required".into()));
        }
        match self.definition().auth {
            ProviderAuthKind::ApiKey
                if self.credential.trim().is_empty() && !self.has_matching_credential() =>
            {
                Err(Error::Config(
                    "Paste an API key or configure this provider on the gateway".into(),
                ))
            }
            ProviderAuthKind::ApiKey | ProviderAuthKind::DeviceCode => Ok(()),
        }
    }

    fn take_authentication(&mut self) -> Result<Authentication> {
        self.authentication_ready()?;
        if self.mode == SetupMode::Agent {
            return Ok(Authentication::Reuse);
        }
        match self.definition().auth {
            ProviderAuthKind::ApiKey => {
                let credential = take_trimmed(&mut self.credential);
                if !credential.is_empty() {
                    Ok(Authentication::ApiKey(credential))
                } else {
                    Ok(Authentication::Reuse)
                }
            }
            ProviderAuthKind::DeviceCode if self.has_matching_credential() => {
                Ok(Authentication::Reuse)
            }
            ProviderAuthKind::DeviceCode => Ok(Authentication::DeviceCode),
        }
    }

    fn agent_composition(&self, current: &AgentComposition) -> Result<AgentComposition> {
        let mut config = current.clone();
        if self.mode == SetupMode::Agent {
            config.middleware = self.middleware.clone();
            return Ok(config);
        }
        let definition = self.definition();
        let model_ids = self.configured_model_ids()?;
        let model = definition.models.get(self.model).map_or_else(
            || model_ids.first().map_or("", String::as_str),
            |model| model.id.as_str(),
        );
        let reasoning_effort = if let Some(model) = definition.models.get(self.model) {
            self.reasoning
                .checked_sub(1)
                .and_then(|index| model.reasoning.get(index))
                .map(|preset| preset.id.to_string())
        } else if current.provider.provider == definition.provider
            && current.provider.model == model
        {
            current.provider.reasoning_effort.clone()
        } else {
            None
        };
        let web_search = definition
            .web_search
            .get(self.web_search)
            .copied()
            .ok_or_else(|| Error::Config("Hosted web-search selection is invalid".into()))?;
        let base_url = self.selected_base_url();
        if model.is_empty() {
            return Err(Error::Config("Model is required".into()));
        }
        config.provider = ProviderConfig {
            provider: definition.provider.clone(),
            model: model.into(),
            base_url,
            reasoning_effort,
            web_search,
        };
        Ok(config)
    }

    fn set_progress(&mut self, title: &'static str, detail: impl Into<String>) {
        self.progress = Some(Progress {
            title,
            detail: detail.into(),
            verification: None,
        });
    }

    fn show_device_code(&mut self, verification_url: String, user_code: String) {
        self.progress = Some(Progress {
            title: "Complete device login",
            detail: "Open the verification URL and enter this one-time code.".into(),
            verification: Some((verification_url, user_code)),
        });
    }
}

enum Authentication {
    Reuse,
    ApiKey(String),
    DeviceCode,
}

fn validated_providers(statuses: &[ProviderStatus]) -> Result<Vec<ProviderEntry>> {
    let mut seen = BTreeSet::new();
    statuses
        .iter()
        .map(|status| {
            if status.provider.trim().is_empty() || !seen.insert(status.provider.as_str()) {
                return Err(Error::Config(format!(
                    "gateway advertised invalid or duplicate provider `{}`",
                    status.provider
                )));
            }
            if status.label.trim().is_empty()
                || status.description.trim().is_empty()
                || status.web_search.first() != Some(&HostedWebSearch::Off)
                || status.model_ids_configurable != status.models.is_empty()
                || !status.model_ids_configurable && !status.model_ids.is_empty()
            {
                return Err(Error::Config(format!(
                    "gateway advertised an incomplete manifest for `{}`",
                    status.provider
                )));
            }
            Ok(ProviderEntry {
                status: status.clone(),
            })
        })
        .collect()
}

fn validate_active_provider(status: &ProviderStatus, config: &ProviderConfig) -> Result<()> {
    if !status.web_search.contains(&config.web_search) {
        return Err(Error::Config(format!(
            "gateway active provider `{}` has an unadvertised web-search mode",
            status.provider
        )));
    }
    if status.configurable_base_url() != config.base_url.is_some() {
        return Err(Error::Config(format!(
            "gateway active provider `{}` has invalid endpoint settings",
            status.provider
        )));
    }
    if status.model_ids_configurable {
        return if status.model_ids.iter().any(|model| model == &config.model) {
            Ok(())
        } else {
            Err(Error::Config(format!(
                "gateway active provider `{}` has unconfigured model `{}`",
                status.provider, config.model
            )))
        };
    }
    let model = status
        .models
        .iter()
        .find(|model| model.id == config.model)
        .ok_or_else(|| {
            Error::Config(format!(
                "gateway active provider `{}` has unadvertised model `{}`",
                status.provider, config.model
            ))
        })?;
    if let Some(effort) = config.reasoning_effort.as_deref()
        && !model.reasoning.iter().any(|choice| choice.id == effort)
    {
        return Err(Error::Config(format!(
            "gateway active model `{}` has unadvertised reasoning `{effort}`",
            model.id
        )));
    }
    Ok(())
}

fn take_trimmed(value: &mut String) -> String {
    let mut value = std::mem::take(value);
    value.truncate(value.trim_end().len());
    let start = value.len() - value.trim_start().len();
    value.drain(..start);
    value
}

async fn edit(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    gateway: &mut ReadyPayload,
) -> Result<bool> {
    let mut tick = tokio::time::interval(INPUT_POLL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut dirty = true;
    loop {
        if dirty {
            draw(terminal, state)?;
            dirty = false;
        }
        tick.tick().await;
        for _ in 0..MAX_INPUT_BATCH {
            let Some(event) = poll_event()? else {
                break;
            };
            dirty = true;
            let flow = match event {
                Event::Key(key) => state.handle_key(key),
                Event::Paste(text) => {
                    state.paste(&text);
                    Flow::Continue
                }
                Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Mouse(_) => {
                    Flow::Continue
                }
            };
            match flow {
                Flow::Continue => {}
                Flow::Authenticate => {
                    authenticate(terminal, state, sender, events, gateway).await?;
                    state.authentication_succeeded();
                    break;
                }
                Flow::Finish => return Ok(true),
                Flow::Cancel => return Ok(false),
            }
        }
    }
}

async fn authenticate(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    gateway: &mut ReadyPayload,
) -> Result<()> {
    let provider = state.definition().provider.clone();
    let base_url = state.selected_base_url();
    match state.take_authentication()? {
        Authentication::Reuse => {}
        Authentication::ApiKey(api_key) => {
            state.set_progress(
                "Saving credential",
                "Sending the key securely to the gateway…",
            );
            draw(terminal, state)?;
            set_credential(
                terminal,
                state,
                sender,
                events,
                provider.clone(),
                base_url,
                api_key,
            )
            .await?;
            mark_provider_configured(gateway, &provider);
        }
        Authentication::DeviceCode => {
            state.set_progress("Starting device login", "Requesting a one-time login code…");
            draw(terminal, state)?;
            device_login(terminal, state, sender, events, provider.clone()).await?;
            mark_provider_configured(gateway, &provider);
        }
    }
    Ok(())
}

async fn apply(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    gateway: &mut ReadyPayload,
    session: &mut SessionReadyPayload,
) -> Result<()> {
    let config = state.agent_composition(&session.config.config)?;
    if state.mode == SetupMode::Login {
        state.set_progress(
            "Registering provider",
            "Updating the gateway model catalog…",
        );
        draw(terminal, state)?;
        let model_ids = state.configured_model_ids()?;
        *gateway = register_provider(
            terminal,
            state,
            sender,
            events,
            config.provider.clone(),
            model_ids,
        )
        .await?;
    }
    match state.target {
        ApplyTarget::Session => {
            if config == session.config.config {
                return Ok(());
            }
            state.set_progress(
                "Applying agent configuration",
                "The gateway is restarting the agent while preserving this session…",
            );
            draw(terminal, state)?;
            let session_id = session.session.session_id.clone();
            *session = configure_session(
                terminal,
                state,
                sender,
                events,
                &session_id,
                session.config.revision,
                config,
            )
            .await?;
        }
        ApplyTarget::Default => {
            let default = gateway.default_config.as_ref().ok_or_else(|| {
                Error::Config("configure a provider before saving defaults".into())
            })?;
            if config == default.config {
                return Ok(());
            }
            state.set_progress(
                "Saving gateway defaults",
                "Future chats will use this agent configuration…",
            );
            draw(terminal, state)?;
            *gateway =
                configure_default_agent(terminal, state, sender, events, default.revision, config)
                    .await?;
        }
    }
    Ok(())
}

async fn apply_gateway(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    gateway: &mut ReadyPayload,
) -> Result<()> {
    let config = state.agent_composition(&state.original)?;
    if state.mode == SetupMode::Login {
        state.set_progress(
            "Registering provider",
            "Updating the gateway model catalog…",
        );
        draw(terminal, state)?;
        let model_ids = state.configured_model_ids()?;
        *gateway = register_provider(
            terminal,
            state,
            sender,
            events,
            config.provider.clone(),
            model_ids,
        )
        .await?;
    }
    let default = gateway
        .default_config
        .as_ref()
        .ok_or_else(|| Error::Config("configure a provider before saving defaults".into()))?;
    if config == default.config {
        return Ok(());
    }
    state.set_progress(
        "Saving gateway defaults",
        "Future chats will use this agent configuration…",
    );
    draw(terminal, state)?;
    *gateway =
        configure_default_agent(terminal, state, sender, events, default.revision, config).await?;
    Ok(())
}

async fn register_provider(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    config: ProviderConfig,
    model_ids: Vec<String>,
) -> Result<ReadyPayload> {
    let request_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::RegisterProvider {
            request_id: request_id.clone(),
            config,
            model_ids,
        })
        .await
        .map_err(gateway_error)?;
    wait_gateway_configured(
        terminal,
        state,
        events,
        &request_id,
        "registering a provider",
    )
    .await
}

async fn configure_default_agent(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    expected_revision: u64,
    config: AgentComposition,
) -> Result<ReadyPayload> {
    let request_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::ConfigureDefaultAgent {
            request_id: request_id.clone(),
            expected_revision,
            config,
        })
        .await
        .map_err(gateway_error)?;
    wait_gateway_configured(
        terminal,
        state,
        events,
        &request_id,
        "saving gateway defaults",
    )
    .await
}

async fn wait_gateway_configured(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    events: &mut GatewayEvents,
    request_id: &str,
    operation: &str,
) -> Result<ReadyPayload> {
    let mut deferred = Vec::new();
    let result = loop {
        let frame = match next_frame(terminal, state, events, false).await {
            Ok(frame) => frame,
            Err(error) => break Err(error),
        };
        match frame.message {
            ServerMessage::GatewayConfigured {
                request_id: actual,
                payload,
            } if actual == request_id => break Ok(payload),
            ServerMessage::Rejected {
                request_id: actual,
                message,
                ..
            } if actual == request_id => break Err(Error::Stopped(message)),
            ServerMessage::Error { message, .. } => break Err(Error::Stopped(message)),
            message if deferred.len() == MAX_PENDING_FRAMES => {
                break Err(Error::Stopped(format!(
                    "gateway event backlog exceeds {MAX_PENDING_FRAMES} frames while {operation}: {message:?}"
                )));
            }
            message => deferred.push(ServerFrame::new(message)),
        }
    };
    events.prepend(deferred).map_err(gateway_error)?;
    result
}

async fn set_credential(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    provider: String,
    base_url: Option<String>,
    api_key: String,
) -> Result<()> {
    let request_id = Uuid::new_v4().to_string();
    let message = match base_url {
        None => ClientMessage::SetProviderCredential {
            request_id: request_id.clone(),
            provider: provider.clone(),
            api_key,
        },
        Some(base_url) => ClientMessage::SetProviderEndpointCredential {
            request_id: request_id.clone(),
            provider: provider.clone(),
            base_url,
            api_key,
        },
    };
    sender.send(message).await.map_err(gateway_error)?;
    let _ = wait_for_response(
        terminal,
        state,
        events,
        &request_id,
        ExpectedResponse::Credential(&provider),
    )
    .await?;
    Ok(())
}

async fn device_login(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    provider: String,
) -> Result<()> {
    let request_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::StartProviderLogin {
            request_id: request_id.clone(),
            provider: provider.clone(),
        })
        .await
        .map_err(gateway_error)?;
    let _ = wait_for_response(
        terminal,
        state,
        events,
        &request_id,
        ExpectedResponse::Login(&provider),
    )
    .await?;
    Ok(())
}

async fn configure_session(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    session_id: &str,
    expected_revision: u64,
    config: AgentComposition,
) -> Result<SessionReadyPayload> {
    let request_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::ConfigureSession {
            request_id: request_id.clone(),
            session_id: session_id.into(),
            expected_revision,
            config,
        })
        .await
        .map_err(gateway_error)?;
    wait_for_response(
        terminal,
        state,
        events,
        &request_id,
        ExpectedResponse::Configure {
            session_id,
            revision: expected_revision,
        },
    )
    .await?
    .ok_or_else(|| Error::Stopped("gateway did not return the configured chat".into()))
}

#[derive(Clone, Copy)]
enum ExpectedResponse<'a> {
    Credential(&'a str),
    Login(&'a str),
    Configure { session_id: &'a str, revision: u64 },
}

async fn wait_for_response(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    events: &mut GatewayEvents,
    request_id: &str,
    expected: ExpectedResponse<'_>,
) -> Result<Option<SessionReadyPayload>> {
    let mut accepted = matches!(expected, ExpectedResponse::Credential(_));
    let mut completed = matches!(expected, ExpectedResponse::Configure { .. });
    let mut deferred = Vec::new();
    let mut snapshot = None;
    let result = loop {
        let frame = match next_frame(
            terminal,
            state,
            events,
            matches!(expected, ExpectedResponse::Login(_)),
        )
        .await
        {
            Ok(frame) => frame,
            Err(error) => break Err(error),
        };
        let defer = match &frame.message {
            ServerMessage::SessionChanged { payload }
                if matches!(
                    expected,
                    ExpectedResponse::Configure {
                        session_id,
                        revision,
                    } if payload.session.session_id == session_id
                        && payload.config.revision > revision
                ) =>
            {
                snapshot = Some((deferred.len(), payload.clone()));
                false
            }
            ServerMessage::Accepted { request_id: actual } if actual == request_id => {
                accepted = true;
                false
            }
            ServerMessage::ProviderCredentialStatus {
                request_id: actual,
                provider,
                configured: true,
            } if actual == request_id
                && matches!(expected, ExpectedResponse::Credential(expected) if provider == expected) =>
            {
                completed = true;
                false
            }
            ServerMessage::ProviderLoginStarted {
                request_id: actual,
                provider,
                verification_url,
                user_code,
                ..
            } if actual == request_id
                && matches!(expected, ExpectedResponse::Login(expected) if provider == expected) =>
            {
                state.show_device_code(verification_url.clone(), user_code.clone());
                if let Err(error) = draw(terminal, state) {
                    break Err(error);
                }
                false
            }
            ServerMessage::ProviderLoginFinished {
                request_id: actual,
                provider,
                ..
            } if actual == request_id
                && matches!(expected, ExpectedResponse::Login(expected) if provider == expected) =>
            {
                completed = true;
                false
            }
            ServerMessage::ProviderCredentialStatus {
                request_id: actual, ..
            }
            | ServerMessage::ProviderLoginStarted {
                request_id: actual, ..
            }
            | ServerMessage::ProviderLoginFinished {
                request_id: actual, ..
            } if actual == request_id => {
                break Err(Error::Stopped(
                    "gateway returned an invalid setup response".into(),
                ));
            }
            ServerMessage::Rejected {
                request_id: actual,
                message,
                ..
            } if actual == request_id => break Err(Error::Stopped(message.clone())),
            ServerMessage::Error { message, .. } => break Err(Error::Stopped(message.clone())),
            _ => true,
        };
        if accepted && completed {
            if matches!(expected, ExpectedResponse::Configure { .. }) {
                if let Some((_, payload)) = snapshot.take() {
                    break Ok(Some(payload));
                }
            } else {
                break Ok(None);
            }
        }
        if defer && deferred.len() + usize::from(snapshot.is_some()) == MAX_PENDING_FRAMES {
            break Err(Error::Stopped(format!(
                "gateway event backlog exceeds {MAX_PENDING_FRAMES} frames"
            )));
        }
        if defer {
            deferred.push(frame);
        }
    };
    if result.is_err()
        && let Some((index, snapshot)) = snapshot
    {
        deferred.insert(
            index,
            ServerFrame::new(ServerMessage::SessionChanged { payload: snapshot }),
        );
    }
    events.prepend(deferred).map_err(gateway_error)?;
    result
}

fn mark_provider_configured(gateway: &mut ReadyPayload, provider: &str) {
    if let Some(status) = gateway
        .providers
        .iter_mut()
        .find(|status| status.provider == provider)
    {
        status.configured = true;
    }
}

async fn next_frame(
    terminal: &mut SetupTerminal,
    state: &SetupState,
    events: &mut GatewayEvents,
    cancellable: bool,
) -> Result<ServerFrame> {
    loop {
        tokio::select! {
            frame = events.next() => {
                return frame
                    .map_err(gateway_error)?
                    .ok_or_else(|| Error::Stopped("gateway disconnected during setup".into()));
            }
            _ = tokio::time::sleep(INPUT_POLL) => {
                for _ in 0..MAX_INPUT_BATCH {
                    let Some(event) = poll_event()? else {
                        break;
                    };
                    match event {
                        Event::Key(key)
                            if cancellable
                                && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                                && (key.code == KeyCode::Esc
                                    || key.modifiers.contains(KeyModifiers::CONTROL)
                                        && matches!(key.code, KeyCode::Char('c' | 'd'))) =>
                        {
                            return Err(Error::Config(
                                "setup cancelled; gateway login will stop when its code expires"
                                    .into(),
                            ));
                        }
                        Event::Resize(_, _) => draw(terminal, state)?,
                        Event::Key(_)
                        | Event::Paste(_)
                        | Event::FocusGained
                        | Event::FocusLost
                        | Event::Mouse(_) => {}
                    }
                }
            }
        }
    }
}

fn gateway_error(error: horus_gateway::Error) -> Error {
    Error::Stopped(error.to_string())
}

fn draw(terminal: &mut SetupTerminal, state: &SetupState) -> Result<()> {
    terminal.draw(|frame| render(frame, state))?;
    Ok(())
}

fn render(frame: &mut ratatui::Frame<'_>, state: &SetupState) {
    let theme = current();
    frame.render_widget(
        Block::default().style(theme.style(Role::Canvas)),
        frame.area(),
    );
    let area = content_area(frame.area());
    let mut lines = header(state);
    lines.push(Line::from(""));
    if let Some(progress) = &state.progress {
        if let Some(page) = login_page(state) {
            lines.push(Line::styled(
                format!("Page {page} of 3"),
                theme.style(Role::Muted),
            ));
            lines.push(Line::from(""));
        }
        render_progress(&mut lines, progress);
    } else {
        render_editing(&mut lines, state, area.width);
    }
    let scroll = selection_scroll(&lines, area);
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.style(Role::Canvas))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
}

fn selection_scroll(lines: &[Line<'_>], area: Rect) -> u16 {
    if area.width == 0 || area.height == 0 {
        return 0;
    }
    let selection = current().style(Role::Selection);
    let Some(start) = lines.iter().position(|line| line.style == selection) else {
        return 0;
    };
    let end = lines[start..]
        .iter()
        .position(|line| line.style != selection)
        .map_or(lines.len(), |length| start + length);
    let selected_end = Paragraph::new(lines[..end].to_vec())
        .wrap(Wrap { trim: false })
        .line_count(area.width);
    selected_end
        .saturating_sub(usize::from(area.height))
        .min(usize::from(u16::MAX)) as u16
}

fn content_area(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).min(82);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y.saturating_add(1),
        width,
        area.height.saturating_sub(2),
    )
}

fn header(state: &SetupState) -> Vec<Line<'static>> {
    let theme = current();
    vec![Line::from(vec![
        Span::styled("◉ ", theme.style(Role::AccentStrong)),
        Span::styled(
            "HORUS",
            theme.style(Role::Accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            match state.mode {
                SetupMode::Login => " provider login",
                SetupMode::Agent => " agent setup",
            },
            theme.style(Role::Muted),
        ),
    ])]
}

fn render_editing(lines: &mut Vec<Line<'static>>, state: &SetupState, width: u16) {
    let theme = current();
    if let Some(page) = login_page(state) {
        lines.push(Line::styled(
            format!("Page {page} of 3"),
            theme.style(Role::Muted),
        ));
    }
    lines.push(Line::from(""));
    let (title, context) = page_prompt(state);
    lines.push(Line::styled(
        format!("  {title}"),
        theme.style(Role::Text).add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::styled(
        format!("  {context}"),
        theme.style(Role::Muted),
    ));
    lines.push(Line::from(""));
    render_page(lines, state, width);
    if let Some(error) = &state.error {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("  {}", terminal_text(error)),
            theme.style(Role::Error),
        ));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(footer(state), theme.style(Role::Muted)));
}

fn login_page(state: &SetupState) -> Option<u8> {
    (state.mode == SetupMode::Login).then(|| match state.page {
        Page::Provider => 1,
        Page::Authentication => 2,
        Page::Models => 3,
        Page::Agent => unreachable!("agent page is not part of provider login"),
    })
}

fn page_prompt(state: &SetupState) -> (&'static str, String) {
    match state.page {
        Page::Provider => (
            "Choose a model provider",
            "Providers are loaded from the connected gateway.".into(),
        ),
        Page::Authentication => (
            "Set up provider access",
            "Credentials are write-only and sent securely to the gateway.".into(),
        ),
        Page::Models => (
            "Models & reasoning",
            "Review the manifest, select defaults, or enter a custom model ID.".into(),
        ),
        Page::Agent => (
            "Agent settings",
            "Toggle capabilities and adjust their advertised settings.".into(),
        ),
    }
}

fn render_page(lines: &mut Vec<Line<'static>>, state: &SetupState, width: u16) {
    let theme = current();
    match state.page {
        Page::Provider => {
            for (index, entry) in state.providers.iter().enumerate() {
                let configured = if entry.status.configured {
                    "configured"
                } else {
                    "login required"
                };
                choice(
                    lines,
                    &entry.status.label,
                    &format!("{} · {configured}", entry.status.description),
                    index == state.provider,
                    if index == state.provider {
                        "●"
                    } else {
                        "○"
                    },
                );
            }
        }
        Page::Authentication => {
            match state.definition().auth {
                ProviderAuthKind::ApiKey => {
                    let focused = !state.endpoint_focused;
                    lines.push(Line::styled(
                        format!(
                            "{} API key  {}▏",
                            if focused { "›" } else { " " },
                            masked_credential(&state.credential)
                        ),
                        theme.style(if focused { Role::Selection } else { Role::Text }),
                    ));
                    lines.push(Line::styled(
                        if state.has_matching_credential() {
                            "    Paste a new key, or leave empty to reuse the gateway credential."
                                .into()
                        } else {
                            state
                                .definition()
                                .default_api_key_env
                                .as_deref()
                                .map_or_else(
                                    || "    Paste a key configured for this gateway endpoint.".into(),
                                    |environment| format!(
                                        "    Paste a key, or leave empty to use {environment} when set."
                                    ),
                                )
                        },
                        theme.style(Role::Muted),
                    ));
                }
                ProviderAuthKind::DeviceCode => {
                    lines.push(Line::styled(
                        format!(
                            "  Press Enter to start {} device login.",
                            state.definition().label
                        ),
                        theme.style(Role::Info),
                    ));
                }
            }
            if state.definition().configurable_base_url() {
                let focused = state.endpoint_focused;
                lines.push(Line::from(""));
                lines.push(Line::styled(
                    format!(
                        "{} Base URL  {}▏",
                        if focused { "›" } else { " " },
                        terminal_text(&state.endpoint)
                    ),
                    theme.style(if focused { Role::Selection } else { Role::Text }),
                ));
                lines.push(Line::styled(
                    "    Credential storage is bound to this exact endpoint.",
                    theme.style(Role::Muted),
                ));
            }
        }
        Page::Models => {
            lines.push(Line::styled(
                if state.definition().model_ids_configurable {
                    "  Model IDs"
                } else {
                    "  Model"
                },
                theme.style(Role::Muted),
            ));
            for (index, model) in state.definition().models.iter().enumerate() {
                choice(
                    lines,
                    &model.label,
                    &model.description,
                    state.row == index,
                    if state.model == index { "●" } else { "○" },
                );
            }
            if state.definition().model_ids_configurable {
                choice(
                    lines,
                    "Model IDs",
                    if state.custom_model.is_empty() {
                        "Comma-separated; the first model is selected"
                    } else {
                        &state.custom_model
                    },
                    state.row == 0,
                    "●",
                );
            }
            lines.push(Line::from(""));
            lines.push(Line::styled("  Reasoning", theme.style(Role::Muted)));
            let reasoning_start = state.model_choice_count();
            choice(
                lines,
                "Provider default",
                "Use the selected model's default reasoning",
                state.row == reasoning_start,
                if state.reasoning == 0 { "●" } else { "○" },
            );
            for (index, preset) in state
                .definition()
                .models
                .get(state.model)
                .into_iter()
                .flat_map(|model| &model.reasoning)
                .enumerate()
            {
                choice(
                    lines,
                    &preset.label,
                    &preset.description,
                    state.row == reasoning_start + index + 1,
                    if state.reasoning == index + 1 {
                        "●"
                    } else {
                        "○"
                    },
                );
            }
            lines.push(Line::from(""));
            lines.push(Line::styled(
                "  Hosted web search",
                theme.style(Role::Muted),
            ));
            if state.definition().web_search.len() == 1 {
                choice(
                    lines,
                    state.definition().web_search[0].label(),
                    "This provider does not expose another hosted-search mode",
                    false,
                    "[fixed]",
                );
            } else {
                let search_start = state.model_choice_count() + state.reasoning_choice_count();
                for (index, search) in state.definition().web_search.iter().enumerate() {
                    choice(
                        lines,
                        search.label(),
                        match search {
                            HostedWebSearch::Off => "Do not use provider-hosted web search",
                            HostedWebSearch::Cached => "Allow cached provider-hosted search",
                            HostedWebSearch::Live => "Allow live provider-hosted search",
                        },
                        state.row == search_start + index,
                        if state.web_search == index {
                            "●"
                        } else {
                            "○"
                        },
                    );
                }
            }
            render_apply_actions(lines, state, state.models_action_start(), None);
        }
        Page::Agent => {
            let layout = agent_layout(state, usize::from(width));
            let mut row = 0;
            for feature in &state.features {
                agent_choice(
                    lines,
                    &feature.label,
                    &feature.description,
                    state.row == row,
                    if feature.required || state.middleware.enabled(&feature.id) {
                        "[x]"
                    } else {
                        "[ ]"
                    },
                    layout,
                );
                row += 1;
                for setting in &feature.settings {
                    let (value, role) =
                        middleware_setting_value(&state.middleware, &feature.id, setting);
                    setting_choice(
                        lines,
                        &setting.label,
                        &value,
                        role,
                        &setting.description,
                        state.row == row,
                        layout,
                    );
                    row += 1;
                }
            }
            render_apply_actions(lines, state, state.agent_action_start(), Some(layout));
        }
    }
}

fn agent_layout(state: &SetupState, width: usize) -> AgentLayout {
    let value_column = state
        .features
        .iter()
        .flat_map(|feature| &feature.settings)
        .map(|setting| 8 + display_width(&terminal_text(&setting.label)) + 2)
        .max()
        .unwrap_or(8);
    let setting_end = state
        .features
        .iter()
        .flat_map(|feature| {
            feature.settings.iter().map(|setting| {
                let (value, _) = middleware_setting_value(&state.middleware, &feature.id, setting);
                value_column + display_width(&format!("‹ {} ›", terminal_text(&value)))
            })
        })
        .max()
        .unwrap_or(0);
    let feature_end = state
        .features
        .iter()
        .map(|feature| 6 + display_width(&terminal_text(&feature.label)))
        .max()
        .unwrap_or(0);
    let action_end = if state.default_only {
        6 + display_width(SAVE_DEFAULT_LABEL)
    } else {
        6 + display_width(CHANGE_CHAT_LABEL).max(display_width(SAVE_DEFAULT_LABEL))
    };
    let description_column = setting_end.max(feature_end).max(action_end) + 2;

    AgentLayout {
        width,
        value_column,
        description_column: (description_column + MIN_INLINE_DESCRIPTION_WIDTH <= width)
            .then_some(description_column),
    }
}

fn middleware_setting_value(
    config: &MiddlewareConfig,
    middleware: &str,
    setting: &FrontendSetting,
) -> (String, Role) {
    match (&setting.kind, config.setting(middleware, &setting.id)) {
        (FrontendSettingKind::Integer { .. }, Some(FrontendSettingValue::Integer(value))) => {
            (value.to_string(), Role::Accent)
        }
        (
            FrontendSettingKind::Select { options, .. },
            Some(FrontendSettingValue::String(value)),
        ) => (
            options
                .iter()
                .find(|option| option.value == *value)
                .map_or_else(|| value.clone(), |option| option.label.clone()),
            Role::Accent,
        ),
        (FrontendSettingKind::Select { unset_label, .. }, None) => (
            unset_label.clone().unwrap_or_else(|| "Not selected".into()),
            Role::Info,
        ),
        _ => ("Invalid value".into(), Role::Error),
    }
}

fn setting_choice(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    value: &str,
    value_role: Role,
    description: &str,
    focused: bool,
    layout: AgentLayout,
) {
    let theme = current();
    let row_role = if focused { Role::Selection } else { Role::Text };
    let value_role = if focused { Role::Selection } else { value_role };
    let mut label = format!(
        "{} {:3}   {}",
        if focused { "›" } else { " " },
        "",
        terminal_text(label)
    );
    label.push_str(&" ".repeat(layout.value_column.saturating_sub(display_width(&label))));
    push_described_row(
        lines,
        vec![
            Span::styled(label, theme.style(row_role)),
            Span::styled(
                format!("‹ {} ›", terminal_text(value)),
                theme.style(value_role),
            ),
        ],
        description,
        focused,
        layout,
    );
}

fn agent_choice(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    description: &str,
    focused: bool,
    marker: &str,
    layout: AgentLayout,
) {
    let role = if focused { Role::Selection } else { Role::Text };
    push_described_row(
        lines,
        vec![Span::styled(
            format!(
                "{} {:3} {}",
                if focused { "›" } else { " " },
                marker,
                terminal_text(label)
            ),
            current().style(role),
        )],
        description,
        focused,
        layout,
    );
}

fn push_described_row(
    lines: &mut Vec<Line<'static>>,
    mut content: Vec<Span<'static>>,
    description: &str,
    focused: bool,
    layout: AgentLayout,
) {
    let theme = current();
    let row_style = theme.style(if focused { Role::Selection } else { Role::Text });
    let description_style = theme.style(if focused {
        Role::Selection
    } else {
        Role::Muted
    });
    let content_width = Line::from(content.clone()).width();

    if let Some(column) = layout
        .description_column
        .filter(|column| *column >= content_width)
    {
        let mut wrapped = wrap_description(description, layout.width - column).into_iter();
        content.push(Span::styled(
            format!(
                "{}{}",
                " ".repeat(column - content_width),
                wrapped.next().unwrap_or_default()
            ),
            description_style,
        ));
        lines.push(Line::from(content).style(row_style));
        lines.extend(wrapped.map(|line| {
            Line::from(Span::styled(
                format!("{}{}", " ".repeat(column), line),
                description_style,
            ))
            .style(row_style)
        }));
        return;
    }

    lines.push(Line::from(content).style(row_style));
    let column = 6.min(layout.width.saturating_sub(1));
    lines.extend(
        wrap_description(description, layout.width.saturating_sub(column))
            .into_iter()
            .map(|line| {
                Line::from(Span::styled(
                    format!("{}{}", " ".repeat(column), line),
                    description_style,
                ))
                .style(row_style)
            }),
    );
}

fn wrap_description(value: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![terminal_text(value)];
    }
    let value = terminal_text(value);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;

    for word in value.split_whitespace() {
        let word_width = display_width(word);
        if current_width > 0 && current_width + 1 + word_width <= width {
            current.push(' ');
            current.push_str(word);
            current_width += 1 + word_width;
            continue;
        }
        if current_width > 0 {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        for character in word.chars() {
            let character_width = display_width(&character.to_string());
            if current_width > 0 && current_width + character_width > width {
                lines.push(std::mem::take(&mut current));
                current_width = 0;
            }
            current.push(character);
            current_width += character_width;
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

fn display_width(value: &str) -> usize {
    Span::raw(value).width()
}

fn render_apply_actions(
    lines: &mut Vec<Line<'static>>,
    state: &SetupState,
    start: usize,
    layout: Option<AgentLayout>,
) {
    lines.push(Line::from(""));
    if state.default_only {
        apply_choice(
            lines,
            SAVE_DEFAULT_LABEL,
            "Use these settings for future chats",
            state.row == start,
            layout,
        );
        return;
    }
    apply_choice(
        lines,
        CHANGE_CHAT_LABEL,
        CHANGE_CHAT_DESCRIPTION,
        state.row == start,
        layout,
    );
    apply_choice(
        lines,
        SAVE_DEFAULT_LABEL,
        SAVE_DEFAULT_DESCRIPTION,
        state.row == start + 1,
        layout,
    );
}

fn apply_choice(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    description: &str,
    focused: bool,
    layout: Option<AgentLayout>,
) {
    if let Some(layout) = layout {
        agent_choice(lines, label, description, focused, "→", layout);
    } else {
        choice(lines, label, description, focused, "→");
    }
}

fn choice(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    description: &str,
    focused: bool,
    marker: &str,
) {
    let theme = current();
    let role = if focused { Role::Selection } else { Role::Text };
    lines.push(
        Line::from(vec![
            Span::styled(
                format!(
                    "{} {:3} {}  ",
                    if focused { "›" } else { " " },
                    marker,
                    terminal_text(label)
                ),
                theme.style(role),
            ),
            Span::styled(
                terminal_text(description),
                theme.style(if focused {
                    Role::Selection
                } else {
                    Role::Muted
                }),
            ),
        ])
        .style(theme.style(role)),
    );
}

fn masked_credential(credential: &str) -> String {
    let count = credential.chars().count();
    let mut masked = "•".repeat(count.min(32));
    if count > 32 {
        masked.push('…');
    }
    masked
}

fn footer(state: &SetupState) -> &'static str {
    match state.page {
        Page::Provider => "  ↑↓ select · enter continue · esc cancel",
        Page::Authentication
            if state.definition().configurable_base_url()
                && state.definition().auth == ProviderAuthKind::ApiKey =>
        {
            "  type/paste · tab switch field · enter continue · esc back"
        }
        Page::Authentication if state.definition().auth == ProviderAuthKind::ApiKey => {
            "  type/paste · enter continue · esc back"
        }
        Page::Authentication if state.definition().configurable_base_url() => {
            "  type/paste endpoint · enter continue · esc back"
        }
        Page::Authentication => "  enter continue · esc back",
        Page::Models => "  ↑↓ move · space select · enter activate · esc back",
        Page::Agent => "  ↑↓ move · space/←→ change · enter activate · esc cancel",
    }
}

fn render_progress(lines: &mut Vec<Line<'static>>, progress: &Progress) {
    let theme = current();
    lines.push(Line::styled(
        format!("  {}", progress.title),
        theme.style(Role::Text).add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::styled(
        format!("  {}", terminal_text(&progress.detail)),
        theme.style(Role::Muted),
    ));
    if let Some((verification_url, user_code)) = &progress.verification {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("  {}", terminal_text(verification_url)),
            theme.style(Role::Info),
        ));
        lines.push(Line::styled(
            format!("  Code: {}", terminal_text(user_code)),
            theme.style(Role::AccentStrong).add_modifier(Modifier::BOLD),
        ));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        if progress.verification.is_some() {
            "  waiting for the gateway… · esc return to Horus"
        } else {
            "  waiting for the gateway…"
        },
        theme.style(Role::Muted),
    ));
}

#[cfg(test)]
mod tests {
    use horus::protocol::{FrontendSettingOption, FrontendSymbol};
    use horus_gateway::wire::{ProviderAuthKind, ProviderModel, ProviderStatus, ReasoningChoice};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    fn status(provider: &str, configured: bool) -> ProviderStatus {
        let (auth, default_base_url, default_api_key_env, models, web_search) = match provider {
            "responses" => (
                ProviderAuthKind::ApiKey,
                Some("https://api.openai.com/v1".into()),
                None,
                Vec::new(),
                vec![HostedWebSearch::Off],
            ),
            "kimi" => (
                ProviderAuthKind::ApiKey,
                None,
                Some("MOONSHOT_API_KEY".into()),
                vec![model("kimi-k3", "Kimi K3", Some("max"))],
                vec![HostedWebSearch::Off],
            ),
            "openai_socket" => (
                ProviderAuthKind::ApiKey,
                None,
                Some("OPENAI_API_KEY".into()),
                vec![model("gpt-5.6-sol", "Sol", Some("medium"))],
                vec![
                    HostedWebSearch::Off,
                    HostedWebSearch::Cached,
                    HostedWebSearch::Live,
                ],
            ),
            _ => panic!("unknown fixture provider"),
        };
        let model_ids_configurable = models.is_empty();
        let model_ids = if model_ids_configurable {
            vec![AgentComposition::default().provider.model]
        } else {
            Vec::new()
        };
        ProviderStatus {
            provider: provider.into(),
            label: provider.into(),
            symbol: FrontendSymbol::Storage,
            description: format!("{provider} provider"),
            configured,
            selection: None,
            model_ids,
            model_ids_configurable,
            auth,
            default_base_url,
            default_api_key_env,
            models,
            web_search,
        }
    }

    fn model(id: &str, label: &str, default_reasoning: Option<&str>) -> ProviderModel {
        ProviderModel {
            id: id.into(),
            label: label.into(),
            description: format!("{label} capabilities"),
            context_window: 1_000_000,
            reasoning: default_reasoning
                .into_iter()
                .map(|id| ReasoningChoice {
                    id: id.into(),
                    label: id.into(),
                    description: format!("{id} reasoning"),
                })
                .collect(),
            default_reasoning: default_reasoning.map(str::to_string),
        }
    }

    fn state(mode: SetupMode, provider: &str, configured: bool) -> SetupState {
        let statuses = vec![status(provider, configured)];
        let providers = validated_providers(&statuses).expect("validated providers");
        let mut original = AgentComposition::default();
        original.provider.provider = provider.into();
        if let Some(model) = providers[0].status.models.first() {
            original.provider.model.clone_from(&model.id);
            original
                .provider
                .reasoning_effort
                .clone_from(&model.default_reasoning);
        }
        if providers[0].status.configurable_base_url() {
            original.provider.base_url = providers[0].status.default_base_url.clone();
        }
        original.middleware.set_enabled("plain", true);
        original.middleware.set_enabled("configured", true);
        SetupState::from_parts(mode, providers, features(), original, false).expect("setup state")
    }

    fn features() -> Vec<MiddlewareFeature> {
        vec![
            MiddlewareFeature {
                id: "plain".into(),
                label: "Plain".into(),
                description: "Plain optional capability".into(),
                required: false,
                settings: Vec::new(),
            },
            MiddlewareFeature {
                id: "configured".into(),
                label: "Configured".into(),
                description: "Capability with advertised settings".into(),
                required: false,
                settings: vec![
                    FrontendSetting {
                        id: "limit".into(),
                        label: "Limit".into(),
                        description: "An advertised integer".into(),
                        kind: FrontendSettingKind::Integer {
                            min: 1,
                            max: Some(100),
                            step: 10,
                        },
                    },
                    FrontendSetting {
                        id: "route".into(),
                        label: "Route".into(),
                        description: "An advertised selection".into(),
                        kind: FrontendSettingKind::Select {
                            options: vec![FrontendSettingOption {
                                value: "route-a".into(),
                                label: "Route A".into(),
                                description: "First route".into(),
                            }],
                            unset_label: Some("Inherit".into()),
                        },
                    },
                ],
            },
            MiddlewareFeature {
                id: "required".into(),
                label: "Required".into(),
                description: "Required capability".into(),
                required: true,
                settings: Vec::new(),
            },
        ]
    }

    fn feature_row(state: &SetupState, id: &str) -> usize {
        (0..state.middleware_row_count())
            .find(|row| {
                matches!(
                    state.middleware_row(*row),
                    Some(MiddlewareRow::Feature(index)) if state.features[index].id == id
                )
            })
            .expect("feature row")
    }

    fn setting_row(state: &SetupState, feature_id: &str, setting_id: &str) -> usize {
        (0..state.middleware_row_count())
            .find(|row| {
                matches!(
                    state.middleware_row(*row),
                    Some(MiddlewareRow::Setting { feature, setting })
                        if state.features[feature].id == feature_id
                            && state.features[feature].settings[setting].id == setting_id
                )
            })
            .expect("setting row")
    }

    #[test]
    fn login_is_three_pages_with_endpoint_and_custom_model_inline() {
        let mut state = state(SetupMode::Login, "responses", false);

        assert_eq!(state.page, Page::Provider);
        assert_eq!(
            state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Flow::Continue
        );
        assert_eq!(state.page, Page::Authentication);
        state.credential = "secret".into();
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(state.endpoint_focused);
        assert_eq!(state.page, Page::Authentication);
        assert_eq!(
            state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Flow::Authenticate
        );
        assert_eq!(state.page, Page::Authentication);
        state.authentication_succeeded();
        assert_eq!(state.page, Page::Models);
        state.row = 0;
        state.custom_model.clear();
        state.paste("custom-model, alternate-model");
        assert_eq!(
            state.configured_model_ids().expect("model IDs"),
            ["custom-model", "alternate-model"]
        );
        state.row = state.models_action_start();
        assert_eq!(
            state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Flow::Finish
        );
    }

    #[test]
    fn configurable_provider_requires_an_exact_authenticated_endpoint() {
        let mut custom = state(SetupMode::Login, "responses", true);

        assert!(!custom.has_matching_credential());
        custom.authentication_succeeded();
        assert!(custom.has_matching_credential());

        let fixed = state(SetupMode::Login, "kimi", true);
        assert!(fixed.has_matching_credential());
    }

    #[test]
    fn configured_fixed_provider_can_be_selected_from_another_provider() {
        let providers = validated_providers(&[status("responses", false), status("kimi", true)])
            .expect("validated providers");
        let mut original = AgentComposition::default();
        original.provider.provider = "responses".into();
        original.provider.base_url = providers[0].status.default_base_url.clone();
        let mut state =
            SetupState::from_parts(SetupMode::Login, providers, features(), original, false)
                .expect("setup state");

        state.select_provider("kimi").expect("select Kimi");

        assert!(state.has_matching_credential());
    }

    #[test]
    fn preferred_provider_must_be_advertised() {
        let mut state = state(SetupMode::Login, "responses", false);

        let error = state
            .select_provider("missing")
            .expect_err("unknown provider must fail");

        assert!(error.to_string().contains("run `/login`"));
    }

    #[test]
    fn unchanged_custom_model_keeps_its_reasoning_effort() {
        let state = state(SetupMode::Login, "responses", true);
        let mut current = state.original.clone();
        current.provider.reasoning_effort = Some("provider-defined".into());

        let configured = state.agent_composition(&current).expect("configuration");

        assert_eq!(
            configured.provider.reasoning_effort.as_deref(),
            Some("provider-defined")
        );
    }

    #[test]
    fn hosted_search_is_selected_only_from_the_gateway_manifest() {
        let mut selectable = state(SetupMode::Login, "openai_socket", true);
        let search_start = selectable.model_choice_count() + selectable.reasoning_choice_count();
        selectable.row = search_start + 2;
        selectable.select_model_row();
        let configured = selectable
            .agent_composition(&selectable.original)
            .expect("select live search");
        assert_eq!(configured.provider.web_search, HostedWebSearch::Live);

        let fixed = state(SetupMode::Login, "kimi", true);
        assert_eq!(fixed.definition().web_search, [HostedWebSearch::Off]);
        assert_eq!(
            fixed
                .agent_composition(&fixed.original)
                .expect("fixed search")
                .provider
                .web_search,
            HostedWebSearch::Off
        );
    }

    #[test]
    fn agent_is_one_page_and_preserves_unedited_provider_settings() {
        let mut state = state(SetupMode::Agent, "openai_socket", true);
        state.original.provider.web_search = HostedWebSearch::Live;
        state.original.system_prompt = "Keep this system prompt".into();
        state.middleware.set_enabled("plain", true);
        state.row = feature_row(&state, "plain");
        state.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        let original = state.original.clone();
        state.row = state.agent_action_start();

        assert_eq!(state.page, Page::Agent);
        assert_eq!(
            state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Flow::Finish
        );
        let configured = state
            .agent_composition(&original)
            .expect("agent composition");

        assert_eq!(configured.provider, original.provider);
        assert!(!configured.middleware.enabled("plain"));
        assert_eq!(configured.system_prompt, "Keep this system prompt");
    }

    #[test]
    fn agent_edits_an_advertised_select_without_knowing_the_middleware() {
        let mut state = state(SetupMode::Agent, "openai_socket", true);
        state.row = setting_row(&state, "configured", "route");

        state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));

        let configured = state
            .agent_composition(&state.original)
            .expect("agent composition");

        assert_eq!(
            configured.middleware.setting("configured", "route"),
            Some(&FrontendSettingValue::String("route-a".into()))
        );
    }

    #[test]
    fn agent_edits_an_advertised_integer_without_knowing_the_middleware() {
        let mut state = state(SetupMode::Agent, "openai_socket", true);
        state.middleware.set_setting(
            "configured",
            "limit",
            Some(FrontendSettingValue::Integer(50)),
        );
        state.row = setting_row(&state, "configured", "limit");

        state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));

        assert_eq!(
            state
                .agent_composition(&state.original)
                .expect("agent composition")
                .middleware
                .setting("configured", "limit"),
            Some(&FrontendSettingValue::Integer(60))
        );
    }

    #[test]
    fn agent_setting_rows_style_inherited_explicit_and_focused_values() {
        let inherited = state(SetupMode::Agent, "openai_socket", true);
        let mut inherited_lines = Vec::new();
        render_page(&mut inherited_lines, &inherited, 82);
        let inherited_route = inherited_lines
            .iter()
            .find(|line| line.to_string().contains("Route  ‹ Inherit ›"))
            .expect("inherited route row");

        let mut explicit = state(SetupMode::Agent, "openai_socket", true);
        explicit.middleware.set_setting(
            "configured",
            "route",
            Some(FrontendSettingValue::String("route-a".into())),
        );
        explicit.middleware.set_setting(
            "configured",
            "limit",
            Some(FrontendSettingValue::Integer(50)),
        );
        let mut explicit_lines = Vec::new();
        render_page(&mut explicit_lines, &explicit, 82);
        let explicit_route = explicit_lines
            .iter()
            .find(|line| line.to_string().contains("Route  ‹ Route A ›"))
            .expect("explicit route row");
        let explicit_limit = explicit_lines
            .iter()
            .find(|line| line.to_string().contains("Limit  ‹ 50 ›"))
            .expect("explicit integer row");

        explicit.row = setting_row(&explicit, "configured", "route");
        let mut focused_lines = Vec::new();
        render_page(&mut focused_lines, &explicit, 82);
        let focused_route = focused_lines
            .iter()
            .find(|line| line.to_string().contains("Route  ‹ Route A ›"))
            .expect("focused route row");
        let theme = current();

        assert_eq!(
            (
                inherited_route.spans[0].style.fg,
                inherited_route.spans[1].style.fg,
                inherited_route.spans[2].style.fg,
                inherited_route
                    .to_string()
                    .contains("An advertised selection"),
                explicit_route.spans[1].style.fg,
                explicit_limit.spans[1].style.fg,
                focused_route.style,
                focused_route.spans[0].style.fg,
                focused_route.spans[1].style.fg,
                focused_route.spans[2].style.fg,
            ),
            (
                Some(theme.color(Role::Text)),
                Some(theme.color(Role::Info)),
                Some(theme.color(Role::Muted)),
                true,
                Some(theme.color(Role::Accent)),
                Some(theme.color(Role::Accent)),
                theme.style(Role::Selection),
                Some(theme.color(Role::Selection)),
                Some(theme.color(Role::Selection)),
                Some(theme.color(Role::Selection)),
            )
        );
    }

    #[test]
    fn agent_descriptions_share_a_column_and_wrap_under_it() {
        fn column_of(line: &str, value: &str) -> Option<usize> {
            line.find(value).map(|index| display_width(&line[..index]))
        }

        let mut state = state(SetupMode::Agent, "openai_socket", true);
        let layout = agent_layout(&state, 70);
        let column = layout.description_column.expect("wide inline layout");
        state.features[0].description =
            format!("{} wrapped-marker", "x".repeat(layout.width - column));
        state.row = feature_row(&state, "plain");
        let mut lines = Vec::new();

        render_page(&mut lines, &state, layout.width as u16);

        let feature = lines
            .iter()
            .find(|line| line.to_string().contains("[x] Plain"))
            .expect("feature row")
            .to_string();
        let setting = lines
            .iter()
            .find(|line| line.to_string().contains("An advertised integer"))
            .expect("setting row")
            .to_string();
        let action_description = "Restart the active chat";
        let action = lines
            .iter()
            .find(|line| line.to_string().contains(action_description))
            .expect("apply row")
            .to_string();
        let continuation = lines
            .iter()
            .find(|line| line.to_string().contains("wrapped-marker"))
            .expect("wrapped feature description");

        assert_eq!(column_of(&feature, "xxxxx"), Some(column));
        assert_eq!(column_of(&setting, "An advertised integer"), Some(column));
        assert_eq!(column_of(&action, action_description), Some(column));
        assert_eq!(
            column_of(&continuation.to_string(), "wrapped-marker"),
            Some(column)
        );
        assert_eq!(continuation.style, current().style(Role::Selection));
    }

    #[test]
    fn selected_agent_row_stays_visible_in_a_short_viewport() {
        let mut state = state(SetupMode::Agent, "openai_socket", true);
        state.row = state.agent_action_start() + 1;
        let mut terminal = Terminal::new(TestBackend::new(90, 10)).expect("terminal");

        terminal
            .draw(|frame| render(frame, &state))
            .expect("agent setup draw");

        assert!(terminal.backend().to_string().contains("Save as default"));
    }

    #[test]
    fn save_as_default_row_selects_the_default_target() {
        let mut state = state(SetupMode::Agent, "openai_socket", true);
        state.row = state.agent_action_start() + 1;

        let flow = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(flow, Flow::Finish);
        assert_eq!(state.target, ApplyTarget::Default);
    }

    #[test]
    fn required_features_are_visible_but_cannot_be_toggled() {
        let mut state = state(SetupMode::Agent, "openai_socket", true);
        state.row = feature_row(&state, "required");

        state.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

        assert!(!state.middleware.enabled("required"));
        let mut lines = Vec::new();
        render_page(&mut lines, &state, 82);
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().contains("[x] Required"))
        );
    }

    #[test]
    fn provider_validation_rejects_an_incomplete_manifest() {
        let mut advertised = status("openai_socket", false);
        advertised.web_search.clear();

        let error = match validated_providers(&[advertised]) {
            Ok(_) => panic!("incomplete provider manifest must fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("incomplete manifest"));
    }

    #[test]
    fn provider_validation_rejects_duplicate_ids() {
        let advertised = status("openai_socket", false);

        let error = match validated_providers(&[advertised.clone(), advertised]) {
            Ok(_) => panic!("duplicate providers must fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("duplicate provider"));
    }

    #[test]
    fn setup_rejects_active_provider_values_outside_the_manifest() {
        let reject = |status: ProviderStatus, config: ProviderConfig| {
            let original = AgentComposition {
                provider: config,
                ..AgentComposition::default()
            };
            match SetupState::from_parts(
                SetupMode::Login,
                validated_providers(&[status]).expect("provider manifest"),
                features(),
                original,
                false,
            ) {
                Ok(_) => panic!("invalid active provider state must fail"),
                Err(error) => error.to_string(),
            }
        };

        let missing_provider = reject(
            status("kimi", true),
            ProviderConfig {
                provider: "missing".into(),
                model: "model".into(),
                base_url: None,
                reasoning_effort: None,
                web_search: HostedWebSearch::Off,
            },
        );
        assert!(missing_provider.contains("active provider"));

        let missing_model = reject(
            status("openai_socket", true),
            ProviderConfig {
                provider: "openai_socket".into(),
                model: "missing".into(),
                base_url: None,
                reasoning_effort: None,
                web_search: HostedWebSearch::Off,
            },
        );
        assert!(missing_model.contains("unadvertised model"));

        let missing_search = reject(
            status("kimi", true),
            ProviderConfig {
                provider: "kimi".into(),
                model: "kimi-k3".into(),
                base_url: None,
                reasoning_effort: Some("max".into()),
                web_search: HostedWebSearch::Live,
            },
        );
        assert!(missing_search.contains("unadvertised web-search"));

        let missing_reasoning = reject(
            status("openai_socket", true),
            ProviderConfig {
                provider: "openai_socket".into(),
                model: "gpt-5.6-sol".into(),
                base_url: None,
                reasoning_effort: Some("missing".into()),
                web_search: HostedWebSearch::Off,
            },
        );
        assert!(missing_reasoning.contains("unadvertised reasoning"));
    }

    #[test]
    fn agent_reuses_authentication_without_provider_controls() {
        let mut state = state(SetupMode::Agent, "responses", true);

        assert_eq!(state.page, Page::Agent);
        assert!(matches!(
            state.take_authentication().expect("reuse authentication"),
            Authentication::Reuse
        ));
    }

    #[test]
    fn credential_entry_is_masked_and_supports_backspace() {
        let mut state = state(SetupMode::Login, "openai_socket", false);
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        state.paste("abc123\n");
        state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

        assert_eq!(masked_credential(&state.credential), "•••••");
    }
}
