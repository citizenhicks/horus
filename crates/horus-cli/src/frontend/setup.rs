//! Gateway-native provider login and agent setup wizard.

use std::collections::BTreeSet;
use std::io;

use horus::backend::model::provider::{ProviderAuth, ProviderDefinition};
use horus::backend::sandbox::ApprovalPolicy;
use horus::{Error, Result};
use horus_gateway::client::{GatewayEvents, GatewaySender, MAX_PENDING_FRAMES};
use horus_gateway::wire::{
    AgentComposition, ClientMessage, MiddlewareConfig, MiddlewareFeature, ProviderConfig,
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

use super::gateway_actions::validated_provider;
use super::terminal::{INPUT_POLL, MAX_INPUT_BATCH, poll_event};
use super::terminal_text;
use super::theme::{Role, current};

const MAX_API_KEY_BYTES: usize = 16 * 1024;
const MAX_ENDPOINT_BYTES: usize = 4 * 1024;
const MAX_MODEL_BYTES: usize = 1024;

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
    let mut state = SetupState::new(mode, preferred_provider, gateway, session)?;
    terminal.clear()?;

    if !edit(terminal, &mut state, sender, events, gateway).await? {
        return Ok(());
    }
    apply(terminal, &mut state, sender, events, gateway, session).await?;
    Ok(())
}

struct ProviderEntry {
    status: ProviderStatus,
    definition: &'static ProviderDefinition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Provider,
    Authentication,
    Models,
    Agent,
}

struct ApprovalChoice {
    label: &'static str,
    description: &'static str,
    policy: ApprovalPolicy,
}

const APPROVALS: [ApprovalChoice; 3] = [
    ApprovalChoice {
        label: "Ask",
        description: "Pause before approval-required tools",
        policy: ApprovalPolicy::On,
    },
    ApprovalChoice {
        label: "Allow · no network",
        description: "Run sandboxed tools without prompting or network",
        policy: ApprovalPolicy::Allow,
    },
    ApprovalChoice {
        label: "Allow · network",
        description: "Run sandboxed tools without prompting, with network access",
        policy: ApprovalPolicy::AllowNetwork,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Authenticate,
    Finish,
    Cancel,
}

struct Progress {
    title: &'static str,
    detail: String,
    verification: Option<(String, String)>,
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
    features: Vec<MiddlewareFeature>,
    middleware: MiddlewareConfig,
    approval: usize,
    row: usize,
    error: Option<String>,
    progress: Option<Progress>,
}

impl SetupState {
    fn new(
        mode: SetupMode,
        preferred_provider: Option<&str>,
        gateway: &ReadyPayload,
        session: &SessionReadyPayload,
    ) -> Result<Self> {
        let mut state = Self::from_parts(
            mode,
            validated_providers(&gateway.providers)?,
            gateway.middleware_features.clone(),
            session.config.config.clone(),
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
    ) -> Result<Self> {
        if providers.is_empty() {
            return Err(Error::Config(
                "the gateway did not advertise any providers".into(),
            ));
        }
        let provider = providers
            .iter()
            .position(|entry| entry.definition.id() == original.provider.provider)
            .or_else(|| (mode == SetupMode::Login).then_some(0))
            .ok_or_else(|| {
                Error::Config(format!(
                    "the gateway did not advertise the active provider `{}`",
                    original.provider.provider
                ))
            })?;
        let middleware = original.middleware.clone();
        let approval = APPROVALS
            .iter()
            .position(|choice| choice.policy == original.approval)
            .unwrap_or_default();
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
            features,
            middleware,
            approval,
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

    fn definition(&self) -> &'static ProviderDefinition {
        self.entry().definition
    }

    fn select_provider(&mut self, provider: &str) -> Result<()> {
        self.provider = self
            .providers
            .iter()
            .position(|entry| entry.definition.id() == provider)
            .ok_or_else(|| {
                Error::Config(format!(
                    "provider `{provider}` is not advertised by this gateway; run `/login` to choose an available provider"
                ))
            })?;
        self.reset_provider_fields();
        Ok(())
    }

    fn model_choice_count(&self) -> usize {
        self.definition().models().len() + 1
    }

    fn reasoning_choice_count(&self) -> usize {
        self.definition()
            .models()
            .get(self.model)
            .map_or(1, |model| model.reasoning.len() + 1)
    }

    fn row_count(&self) -> usize {
        match self.page {
            Page::Provider => self.providers.len(),
            Page::Authentication => 0,
            Page::Models => self.model_choice_count() + self.reasoning_choice_count(),
            Page::Agent => self.features.len() + 1,
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
                    && matches!(self.definition().auth(), ProviderAuth::Browser(_));
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
                    && matches!(self.definition().auth(), ProviderAuth::ApiKey(_)) =>
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
        let custom_row = self.definition().models().len();
        match key.code {
            KeyCode::Esc => {
                self.page = Page::Authentication;
                self.error = None;
            }
            KeyCode::Backspace if self.row == custom_row => {
                self.model = custom_row;
                self.custom_model.pop();
                self.error = None;
            }
            KeyCode::Char(character)
                if self.row == custom_row
                    && character != ' '
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.model = custom_row;
                self.push_text(&character.to_string());
            }
            KeyCode::Up | KeyCode::BackTab => self.move_selection(-1),
            KeyCode::Down | KeyCode::Tab => self.move_selection(1),
            KeyCode::Char(' ') => self.select_model_row(),
            KeyCode::Enter => return self.finish(),
            _ => {}
        }
        Flow::Continue
    }

    fn handle_agent_key(&mut self, key: KeyEvent) -> Flow {
        match key.code {
            KeyCode::Esc => return Flow::Cancel,
            KeyCode::Up | KeyCode::BackTab => self.move_selection(-1),
            KeyCode::Down | KeyCode::Tab => self.move_selection(1),
            KeyCode::Char(' ') if self.row < self.features.len() => {
                let feature = &self.features[self.row];
                if !feature.required {
                    self.middleware
                        .set_enabled(&feature.id, !self.middleware.enabled(&feature.id));
                }
            }
            KeyCode::Char(' ') | KeyCode::Right if self.row == self.features.len() => {
                self.approval = (self.approval + 1) % APPROVALS.len();
            }
            KeyCode::Left if self.row == self.features.len() => {
                self.approval = (self.approval + APPROVALS.len() - 1) % APPROVALS.len();
            }
            KeyCode::Enter => return self.finish(),
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
        } else {
            self.reasoning = self.row - models;
        }
        self.error = None;
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
        self.endpoint_focused || matches!(self.definition().auth(), ProviderAuth::ApiKey(_))
    }

    fn paste(&mut self, text: &str) {
        let custom_row = self.definition().models().len();
        if self.page == Page::Authentication && self.authentication_is_editable() {
            self.push_text(text.trim());
        } else if self.page == Page::Models && self.row == custom_row {
            self.model = custom_row;
            self.push_text(text.trim());
        }
    }

    fn push_text(&mut self, text: &str) {
        let custom = self.page == Page::Models;
        let endpoint = self.page == Page::Authentication && self.endpoint_focused;
        let (target, limit) = if custom {
            (&mut self.custom_model, MAX_MODEL_BYTES)
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
        let definition = self.definition();
        let status = self.entry().status.clone();
        let current = &self.original.provider;
        let same_provider = current.provider == definition.id();
        self.endpoint = if same_provider {
            current
                .base_url
                .as_deref()
                .or_else(|| definition.default_base_url())
        } else {
            status
                .default_base_url
                .as_deref()
                .or_else(|| definition.default_base_url())
        }
        .unwrap_or_default()
        .into();
        self.model = if same_provider {
            definition
                .models()
                .iter()
                .position(|model| model.id == current.model)
                .unwrap_or(definition.models().len())
        } else {
            status
                .default_model
                .as_deref()
                .and_then(|id| definition.models().iter().position(|model| model.id == id))
                .unwrap_or(0)
        };
        self.custom_model = if same_provider && self.model == definition.models().len() {
            current.model.clone()
        } else {
            String::new()
        };
        let reasoning = if same_provider {
            current.reasoning_effort.as_deref()
        } else {
            status.default_reasoning_effort.as_deref()
        };
        self.reasoning = definition
            .models()
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
        self.endpoint_focused = false;
        self.row = self.model;
        self.error = None;
    }

    fn selected_model(&self) -> &str {
        self.definition()
            .models()
            .get(self.model)
            .map_or_else(|| self.custom_model.trim(), |model| model.id)
    }

    fn selected_base_url(&self) -> Option<String> {
        self.definition()
            .configurable_base_url()
            .then(|| self.endpoint.trim().to_string())
    }

    fn authentication_target(&self) -> (String, Option<String>) {
        (self.definition().id().into(), self.selected_base_url())
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
        self.definition()
            .validate_base_url(self.selected_base_url().as_deref())?;
        match self.definition().auth() {
            ProviderAuth::ApiKey(environment)
                if self.credential.trim().is_empty()
                    && environment_api_key(environment).is_none()
                    && !self.has_matching_credential() =>
            {
                Err(Error::Config(format!(
                    "Paste an API key or set `{environment}`"
                )))
            }
            ProviderAuth::ApiKey(_) | ProviderAuth::Browser(_) => Ok(()),
        }
    }

    fn take_authentication(&mut self) -> Result<Authentication> {
        self.authentication_ready()?;
        if self.mode == SetupMode::Agent {
            return Ok(Authentication::Reuse);
        }
        match self.definition().auth() {
            ProviderAuth::ApiKey(environment) => {
                let credential = take_trimmed(&mut self.credential);
                if !credential.is_empty() {
                    Ok(Authentication::ApiKey(credential))
                } else if let Some(credential) = environment_api_key(environment) {
                    Ok(Authentication::ApiKey(credential))
                } else {
                    Ok(Authentication::Reuse)
                }
            }
            ProviderAuth::Browser(_) if self.has_matching_credential() => Ok(Authentication::Reuse),
            ProviderAuth::Browser(_) => Ok(Authentication::DeviceCode),
        }
    }

    fn agent_composition(&self, current: &AgentComposition) -> Result<AgentComposition> {
        let mut config = current.clone();
        if self.mode == SetupMode::Agent {
            config.middleware = self.middleware.clone();
            config.approval = APPROVALS[self.approval].policy;
            return Ok(config);
        }
        let definition = self.definition();
        let model = self.selected_model();
        let reasoning_effort = if let Some(model) = definition.models().get(self.model) {
            self.reasoning
                .checked_sub(1)
                .and_then(|index| model.reasoning.get(index))
                .map(|preset| preset.id.to_string())
        } else if current.provider.provider == definition.id() && current.provider.model == model {
            current.provider.reasoning_effort.clone()
        } else {
            None
        };
        let web_search = (current.provider.provider == definition.id())
            .then_some(current.provider.web_search)
            .filter(|search| definition.web_search().contains(search))
            .or_else(|| {
                definition
                    .web_search()
                    .contains(&self.entry().status.default_web_search)
                    .then_some(self.entry().status.default_web_search)
            })
            .or_else(|| definition.web_search().first().copied())
            .unwrap_or_default();
        let base_url = self.selected_base_url();
        definition.build_config_is_valid(
            model,
            base_url.as_deref(),
            reasoning_effort.as_deref(),
            web_search,
        )?;
        config.provider = ProviderConfig {
            provider: definition.id().into(),
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
            if !seen.insert(status.provider.as_str()) {
                return Err(Error::Config(format!(
                    "gateway advertised provider `{}` more than once",
                    status.provider
                )));
            }
            let definition = validated_provider(&status.provider, statuses)?;
            if let ProviderAuth::Browser(auth) = definition.auth()
                && !auth.supports_device_login()
            {
                return Err(Error::Config(format!(
                    "provider `{}` does not support gateway device login",
                    definition.id()
                )));
            }
            Ok(ProviderEntry {
                status: status.clone(),
                definition,
            })
        })
        .collect()
}

fn take_trimmed(value: &mut String) -> String {
    let mut value = std::mem::take(value);
    value.truncate(value.trim_end().len());
    let start = value.len() - value.trim_start().len();
    value.drain(..start);
    value
}

fn environment_api_key(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
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
    let provider = state.definition().id().to_string();
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
    let expected_revision = session.config.revision;
    let config = state.agent_composition(&session.config.config)?;
    if state.mode == SetupMode::Login {
        state.set_progress(
            "Registering provider",
            "Updating the gateway model catalog…",
        );
        draw(terminal, state)?;
        *gateway =
            register_provider(terminal, state, sender, events, config.provider.clone()).await?;
    }
    if config == session.config.config {
        return Ok(());
    }
    state.set_progress(
        "Applying agent configuration",
        "The gateway is restarting the agent while preserving this session…",
    );
    draw(terminal, state)?;
    let session_id = session.session.session_id.clone();
    let payload = configure_session(
        terminal,
        state,
        sender,
        events,
        &session_id,
        expected_revision,
        config,
    )
    .await?;
    *session = payload;
    Ok(())
}

async fn register_provider(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    config: ProviderConfig,
) -> Result<ReadyPayload> {
    let request_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::RegisterProvider {
            request_id: request_id.clone(),
            config,
        })
        .await
        .map_err(gateway_error)?;
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
                    "gateway event backlog exceeds {MAX_PENDING_FRAMES} frames while registering a provider: {message:?}"
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
        render_editing(&mut lines, state);
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.style(Role::Canvas))
            .wrap(Wrap { trim: false }),
        area,
    );
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

fn render_editing(lines: &mut Vec<Line<'static>>, state: &SetupState) {
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
    render_page(lines, state);
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
            "Only providers validated against this CLI are shown.".into(),
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
            "Toggle capabilities and choose the execution approval policy.".into(),
        ),
    }
}

fn render_page(lines: &mut Vec<Line<'static>>, state: &SetupState) {
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
                    entry.definition.label(),
                    &format!("{} · {configured}", entry.definition.description()),
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
            match state.definition().auth() {
                ProviderAuth::ApiKey(environment) => {
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
                            format!(
                                "    Paste a key, or leave empty to use {environment} when set."
                            )
                        },
                        theme.style(Role::Muted),
                    ));
                }
                ProviderAuth::Browser(auth) => {
                    lines.push(Line::styled(
                        format!("  Press Enter to start {} device login.", auth.label()),
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
            lines.push(Line::styled("  Model", theme.style(Role::Muted)));
            for (index, model) in state.definition().models().iter().enumerate() {
                choice(
                    lines,
                    model.label,
                    model.description,
                    state.row == index,
                    if state.model == index { "●" } else { "○" },
                );
            }
            let custom = state.definition().models().len();
            choice(
                lines,
                "Custom model",
                if state.custom_model.is_empty() {
                    "Type or paste an exact model ID"
                } else {
                    &state.custom_model
                },
                state.row == custom,
                if state.model == custom { "●" } else { "○" },
            );
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
                .models()
                .get(state.model)
                .into_iter()
                .flat_map(|model| model.reasoning)
                .enumerate()
            {
                choice(
                    lines,
                    preset.label,
                    preset.description,
                    state.row == reasoning_start + index + 1,
                    if state.reasoning == index + 1 {
                        "●"
                    } else {
                        "○"
                    },
                );
            }
        }
        Page::Agent => {
            for (index, feature) in state.features.iter().enumerate() {
                choice(
                    lines,
                    &feature.label,
                    &feature.description,
                    state.row == index,
                    if feature.required || state.middleware.enabled(&feature.id) {
                        "[x]"
                    } else {
                        "[ ]"
                    },
                );
            }
            lines.push(Line::from(""));
            choice(
                lines,
                &format!("Approval  ‹ {} ›", APPROVALS[state.approval].label),
                APPROVALS[state.approval].description,
                state.row == state.features.len(),
                "",
            );
        }
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
    lines.push(Line::styled(
        format!(
            "{} {:3} {}",
            if focused { "›" } else { " " },
            marker,
            terminal_text(label)
        ),
        theme.style(role),
    ));
    lines.push(Line::styled(
        format!("     {}", terminal_text(description)),
        theme.style(if focused {
            Role::Selection
        } else {
            Role::Muted
        }),
    ));
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
                && matches!(state.definition().auth(), ProviderAuth::ApiKey(_)) =>
        {
            "  type/paste · tab switch field · enter continue · esc back"
        }
        Page::Authentication if matches!(state.definition().auth(), ProviderAuth::ApiKey(_)) => {
            "  type/paste · enter continue · esc back"
        }
        Page::Authentication if state.definition().configurable_base_url() => {
            "  type/paste endpoint · enter continue · esc back"
        }
        Page::Authentication => "  enter continue · esc back",
        Page::Models => "  ↑↓ move · space select · type custom ID · enter apply · esc back",
        Page::Agent => "  ↑↓ move · space toggle · ←→ approval · enter apply · esc cancel",
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
    use horus::backend::model::provider::{HostedWebSearch, ProviderAuth};
    use horus_gateway::wire::{ProviderAuthKind, ProviderStatus};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    fn status(provider: &str, configured: bool) -> ProviderStatus {
        let definition = horus::backend::model::provider::provider(provider).expect("provider");
        ProviderStatus {
            provider: provider.into(),
            label: "untrusted label".into(),
            configured,
            auth: match definition.auth() {
                ProviderAuth::ApiKey(_) => ProviderAuthKind::ApiKey,
                ProviderAuth::Browser(_) => ProviderAuthKind::DeviceCode,
            },
            default_model: None,
            default_base_url: None,
            default_api_key_env: None,
            default_reasoning_effort: None,
            default_web_search: HostedWebSearch::Off,
        }
    }

    fn state(mode: SetupMode, provider: &str, configured: bool) -> SetupState {
        let statuses = vec![status(provider, configured)];
        let providers = validated_providers(&statuses).expect("validated providers");
        let mut original = AgentComposition::default();
        original.provider.provider = provider.into();
        if let Some(model) = providers[0].definition.models().first() {
            original.provider.model = model.id.into();
        }
        if providers[0].definition.configurable_base_url() {
            original.provider.base_url = providers[0]
                .definition
                .default_base_url()
                .map(str::to_string);
        }
        SetupState::from_parts(mode, providers, features(), original).expect("setup state")
    }

    fn features() -> Vec<MiddlewareFeature> {
        [
            ("tools", "Tools", false),
            ("sessions", "Sessions", true),
            ("skills", "Skills", false),
        ]
        .into_iter()
        .map(|(id, label, required)| MiddlewareFeature {
            id: id.into(),
            label: label.into(),
            description: label.into(),
            required,
        })
        .collect()
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
        state.row = state.definition().models().len();
        state.paste("custom-model");
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
        original.provider.base_url = providers[0]
            .definition
            .default_base_url()
            .map(str::to_string);
        let mut state = SetupState::from_parts(SetupMode::Login, providers, features(), original)
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
    fn agent_is_one_page_and_only_changes_features_and_approval() {
        let mut state = state(SetupMode::Agent, "openai_socket", true);
        state.original.provider.web_search = HostedWebSearch::Live;
        state.original.system_prompt = "Keep this system prompt".into();
        state.row = state
            .features
            .iter()
            .position(|feature| feature.id == "skills")
            .expect("skills feature");
        state.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        state.approval = APPROVALS
            .iter()
            .position(|choice| choice.policy == ApprovalPolicy::AllowNetwork)
            .expect("network approval choice");
        let original = state.original.clone();

        assert_eq!(state.page, Page::Agent);
        assert_eq!(
            state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Flow::Finish
        );
        let configured = state
            .agent_composition(&original)
            .expect("agent composition");

        assert_eq!(configured.provider, original.provider);
        assert!(!configured.middleware.enabled("skills"));
        assert_eq!(configured.approval, ApprovalPolicy::AllowNetwork);
        assert_eq!(configured.system_prompt, "Keep this system prompt");
    }

    #[test]
    fn required_features_are_visible_but_cannot_be_toggled() {
        let mut state = state(SetupMode::Agent, "openai_socket", true);
        state.row = state
            .features
            .iter()
            .position(|feature| feature.id == "sessions")
            .expect("sessions feature");

        state.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

        assert!(!state.middleware.enabled("sessions"));
        let mut lines = Vec::new();
        render_page(&mut lines, &state);
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().contains("[x] Sessions"))
        );
    }

    #[test]
    fn provider_validation_rejects_an_authentication_mismatch() {
        let mut advertised = status("openai_socket", false);
        advertised.auth = ProviderAuthKind::DeviceCode;

        let error = match validated_providers(&[advertised]) {
            Ok(_) => panic!("mismatched provider authentication must fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("authentication does not match"));
    }

    #[test]
    fn provider_validation_rejects_duplicate_ids() {
        let advertised = status("openai_socket", false);

        let error = match validated_providers(&[advertised.clone(), advertised]) {
            Ok(_) => panic!("duplicate providers must fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("more than once"));
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
