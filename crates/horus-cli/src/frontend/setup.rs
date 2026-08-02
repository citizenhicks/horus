//! Gateway-native provider login and agent setup wizard.

use std::collections::BTreeSet;
use std::io;

use horus::backend::model::provider::{ProviderAuth, ProviderDefinition};
use horus::{Error, Result};
use horus_gateway::client::{GatewayEvents, GatewaySender};
use horus_gateway::wire::{
    AgentComposition, ClientMessage, ProviderConfig, ProviderStatus, ReadyPayload, ServerMessage,
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

/// Runs one gateway-backed setup flow and returns the newest gateway snapshot.
pub(crate) async fn run(
    terminal: &mut SetupTerminal,
    mode: SetupMode,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    ready: &mut ReadyPayload,
) -> Result<()> {
    let mut state = SetupState::new(mode, ready)?;
    terminal.clear()?;

    if !edit(terminal, &mut state).await? {
        return Ok(());
    }
    apply(terminal, &mut state, sender, events, ready).await?;
    state.show_success();
    draw(terminal, &state)?;
    wait_for_acknowledgement(terminal, &state).await?;
    Ok(())
}

struct ProviderEntry {
    status: ProviderStatus,
    definition: &'static ProviderDefinition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Provider,
    Credential,
    Endpoint,
    Model,
    CustomModel,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Finish,
    Cancel,
}

struct Progress {
    title: &'static str,
    detail: String,
    verification: Option<(String, String)>,
    success: bool,
}

struct SetupState {
    mode: SetupMode,
    providers: Vec<ProviderEntry>,
    original: AgentComposition,
    step: Step,
    provider: usize,
    credential: String,
    endpoint: String,
    model: usize,
    custom_model: String,
    error: Option<String>,
    progress: Option<Progress>,
}

impl SetupState {
    fn new(mode: SetupMode, ready: &ReadyPayload) -> Result<Self> {
        Self::from_parts(
            mode,
            validated_providers(&ready.providers)?,
            ready.config.config.clone(),
        )
    }

    fn from_parts(
        mode: SetupMode,
        providers: Vec<ProviderEntry>,
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
            .unwrap_or_default();
        let mut state = Self {
            mode,
            providers,
            original,
            step: Step::Provider,
            provider,
            credential: String::new(),
            endpoint: String::new(),
            model: 0,
            custom_model: String::new(),
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

    fn steps(&self) -> Vec<Step> {
        let mut steps = vec![Step::Provider, Step::Credential];
        if self.definition().configurable_base_url() {
            steps.push(Step::Endpoint);
        }
        if self.mode == SetupMode::Agent {
            if !self.definition().models().is_empty() {
                steps.push(Step::Model);
            }
            if self.model == self.definition().models().len() {
                steps.push(Step::CustomModel);
            }
            steps.push(Step::Review);
        }
        steps
    }

    fn selection(&self) -> usize {
        match self.step {
            Step::Provider => self.provider,
            Step::Model => self.model,
            Step::Credential | Step::Endpoint | Step::CustomModel | Step::Review => 0,
        }
    }

    fn selection_count(&self) -> usize {
        match self.step {
            Step::Provider => self.providers.len(),
            Step::Model => self.definition().models().len() + 1,
            Step::Credential | Step::Endpoint | Step::CustomModel | Step::Review => 0,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let count = self.selection_count();
        if count == 0 {
            return;
        }
        let selected = (self.selection() as isize + delta).rem_euclid(count as isize) as usize;
        match self.step {
            Step::Provider => self.provider = selected,
            Step::Model => self.model = selected,
            Step::Credential | Step::Endpoint | Step::CustomModel | Step::Review => {}
        }
        self.error = None;
    }

    fn choose_number(&mut self, index: usize) -> Flow {
        if index >= self.selection_count() {
            return Flow::Continue;
        }
        match self.step {
            Step::Provider => self.provider = index,
            Step::Model => self.model = index,
            Step::Credential | Step::Endpoint | Step::CustomModel | Step::Review => {}
        }
        self.confirm()
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
        if self.is_text_entry() {
            return self.handle_text_key(key);
        }
        match key.code {
            KeyCode::Esc => self.back(),
            KeyCode::Char('q') => Flow::Cancel,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                Flow::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                Flow::Continue
            }
            KeyCode::Char(character) if character.is_ascii_digit() && character != '0' => {
                self.choose_number(character as usize - '1' as usize)
            }
            KeyCode::Enter => self.confirm(),
            _ => Flow::Continue,
        }
    }

    fn handle_text_key(&mut self, key: KeyEvent) -> Flow {
        match key.code {
            KeyCode::Esc => self.back(),
            KeyCode::Enter => self.confirm(),
            KeyCode::Backspace => {
                self.text_mut().pop();
                self.error = None;
                Flow::Continue
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.push_text(&character.to_string());
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    fn paste(&mut self, text: &str) {
        if self.is_text_entry() {
            self.push_text(text.trim());
        }
    }

    fn push_text(&mut self, text: &str) {
        let limit = self.text_limit();
        let text = text.chars().filter(|character| !character.is_control());
        let target = self.text_mut();
        let mut rejected = false;
        for character in text {
            if target.len() + character.len_utf8() > limit {
                rejected = true;
                break;
            }
            target.push(character);
        }
        self.error = rejected.then(|| format!("input is limited to {limit} bytes"));
    }

    fn is_text_entry(&self) -> bool {
        matches!(
            self.step,
            Step::Endpoint | Step::CustomModel | Step::Credential
                if self.step != Step::Credential
                    || matches!(self.definition().auth(), ProviderAuth::ApiKey(_))
        )
    }

    fn text_mut(&mut self) -> &mut String {
        match self.step {
            Step::Credential => &mut self.credential,
            Step::Endpoint => &mut self.endpoint,
            Step::CustomModel => &mut self.custom_model,
            Step::Provider | Step::Model | Step::Review => {
                unreachable!("only text-entry steps request mutable text")
            }
        }
    }

    fn text_limit(&self) -> usize {
        match self.step {
            Step::Credential => MAX_API_KEY_BYTES,
            Step::Endpoint => MAX_ENDPOINT_BYTES,
            Step::CustomModel => MAX_MODEL_BYTES,
            Step::Provider | Step::Model | Step::Review => 0,
        }
    }

    fn confirm(&mut self) -> Flow {
        match self.step {
            Step::Provider => self.reset_provider_fields(),
            Step::Credential => {
                if let ProviderAuth::ApiKey(_) = self.definition().auth()
                    && self.credential.trim().is_empty()
                    && (self.mode == SetupMode::Login || !self.can_reuse_api_key())
                {
                    self.error = Some("Paste an API key to continue".into());
                    return Flow::Continue;
                }
                if self.mode == SetupMode::Login && !self.definition().configurable_base_url() {
                    return Flow::Finish;
                }
            }
            Step::Endpoint => {
                if let Err(error) = self
                    .definition()
                    .validate_base_url(Some(self.endpoint.trim()))
                {
                    self.error = Some(error.to_string());
                    return Flow::Continue;
                }
                if self.mode == SetupMode::Login {
                    return Flow::Finish;
                }
            }
            Step::CustomModel if self.custom_model.trim().is_empty() => {
                self.error = Some("Model ID cannot be empty".into());
                return Flow::Continue;
            }
            Step::Review => {
                if let Err(error) = self.authentication_ready() {
                    self.error = Some(error.to_string());
                    return Flow::Continue;
                }
                if let Err(error) = self.agent_composition(&self.original) {
                    self.error = Some(error.to_string());
                    return Flow::Continue;
                }
                return Flow::Finish;
            }
            Step::Model | Step::CustomModel => {}
        }
        self.advance();
        Flow::Continue
    }

    fn advance(&mut self) {
        let steps = self.steps();
        let index = steps
            .iter()
            .position(|step| *step == self.step)
            .unwrap_or_default();
        if let Some(next) = steps.get(index + 1) {
            self.step = *next;
        }
        self.error = None;
    }

    fn back(&mut self) -> Flow {
        let steps = self.steps();
        let index = steps
            .iter()
            .position(|step| *step == self.step)
            .unwrap_or_default();
        let Some(previous) = index.checked_sub(1).and_then(|index| steps.get(index)) else {
            return Flow::Cancel;
        };
        self.step = *previous;
        self.error = None;
        Flow::Continue
    }

    fn reset_provider_fields(&mut self) {
        self.credential.clear();
        let definition = self.definition();
        let current = &self.original.provider;
        let same_provider = current.provider == definition.id();
        self.endpoint = if same_provider {
            current
                .base_url
                .as_deref()
                .or_else(|| definition.default_base_url())
        } else {
            definition.default_base_url()
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
            0
        };
        self.custom_model = if same_provider && self.model == definition.models().len() {
            current.model.clone()
        } else {
            String::new()
        };
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

    fn can_reuse_api_key(&self) -> bool {
        self.entry().status.configured && !self.definition().configurable_base_url()
    }

    fn authentication_ready(&self) -> Result<()> {
        match self.definition().auth() {
            ProviderAuth::ApiKey(_)
                if self.credential.trim().is_empty() && !self.can_reuse_api_key() =>
            {
                Err(Error::Config(
                    "Paste an API key for the selected endpoint".into(),
                ))
            }
            ProviderAuth::Browser(auth)
                if !self.entry().status.configured && !auth.supports_device_login() =>
            {
                Err(Error::Config(
                    "the selected provider does not support device login".into(),
                ))
            }
            ProviderAuth::ApiKey(_) | ProviderAuth::Browser(_) => Ok(()),
        }
    }

    fn take_authentication(&mut self) -> Result<Authentication> {
        self.authentication_ready()?;
        match self.definition().auth() {
            ProviderAuth::ApiKey(_) if !self.credential.trim().is_empty() => {
                Ok(Authentication::ApiKey(take_trimmed(&mut self.credential)))
            }
            ProviderAuth::ApiKey(_) => Ok(Authentication::Reuse),
            ProviderAuth::Browser(_)
                if self.mode == SetupMode::Agent && self.entry().status.configured =>
            {
                Ok(Authentication::Reuse)
            }
            ProviderAuth::Browser(_) => Ok(Authentication::DeviceCode),
        }
    }

    fn agent_composition(&self, current: &AgentComposition) -> Result<AgentComposition> {
        let definition = self.definition();
        let model = self.selected_model();
        let same_model =
            current.provider.provider == definition.id() && current.provider.model == model;
        let reasoning_effort = if same_model {
            current.provider.reasoning_effort.clone()
        } else {
            definition
                .model(model)
                .and_then(|model| model.default_reasoning)
                .map(str::to_string)
        };
        let web_search = if current.provider.provider == definition.id()
            && definition
                .web_search()
                .contains(&current.provider.web_search)
        {
            current.provider.web_search
        } else {
            definition.web_search().first().copied().unwrap_or_default()
        };
        let base_url = self.selected_base_url();
        definition.build_config_is_valid(
            model,
            base_url.as_deref(),
            reasoning_effort.as_deref(),
            web_search,
        )?;
        let api_key_env = (current.provider.provider == definition.id()
            && !definition.configurable_base_url()
            && matches!(definition.auth(), ProviderAuth::ApiKey(_)))
        .then(|| current.provider.api_key_env.clone())
        .flatten();
        let mut config = current.clone();
        config.provider = ProviderConfig {
            provider: definition.id().into(),
            model: model.into(),
            base_url,
            api_key_env,
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
            success: false,
        });
    }

    fn show_device_code(&mut self, verification_url: String, user_code: String) {
        self.progress = Some(Progress {
            title: "Complete device login",
            detail: "Open the verification URL and enter this one-time code.".into(),
            verification: Some((verification_url, user_code)),
            success: false,
        });
    }

    fn show_success(&mut self) {
        self.progress = Some(Progress {
            title: "Setup complete",
            detail: match self.mode {
                SetupMode::Login => "The provider credential is ready on the gateway.",
                SetupMode::Agent => "The gateway is ready with the selected agent configuration.",
            }
            .into(),
            verification: None,
            success: true,
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

async fn edit(terminal: &mut SetupTerminal, state: &mut SetupState) -> Result<bool> {
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
                Flow::Finish => return Ok(true),
                Flow::Cancel => return Ok(false),
            }
        }
    }
}

async fn apply(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    ready: &mut ReadyPayload,
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
            *ready = set_credential(
                terminal,
                state,
                sender,
                events,
                provider.clone(),
                base_url,
                api_key,
            )
            .await?;
        }
        Authentication::DeviceCode => {
            state.set_progress("Starting device login", "Requesting a one-time login code…");
            draw(terminal, state)?;
            *ready = device_login(terminal, state, sender, events, provider.clone()).await?;
        }
    }
    if state.mode == SetupMode::Login {
        return Ok(());
    }

    let expected_revision = ready.config.revision;
    let config = state.agent_composition(&ready.config.config)?;
    if config == ready.config.config {
        return Ok(());
    }
    state.set_progress(
        "Applying agent configuration",
        "The gateway is restarting the agent while preserving this session…",
    );
    draw(terminal, state)?;
    *ready = configure_agent(terminal, state, sender, events, expected_revision, config).await?;
    Ok(())
}

async fn set_credential(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    provider: String,
    base_url: Option<String>,
    api_key: String,
) -> Result<ReadyPayload> {
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
    wait_for_response(
        terminal,
        state,
        events,
        &request_id,
        ExpectedResponse::Credential(&provider),
    )
    .await
}

async fn device_login(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    provider: String,
) -> Result<ReadyPayload> {
    let request_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::StartProviderLogin {
            request_id: request_id.clone(),
            provider: provider.clone(),
        })
        .await
        .map_err(gateway_error)?;
    wait_for_response(
        terminal,
        state,
        events,
        &request_id,
        ExpectedResponse::Login(&provider),
    )
    .await
}

async fn configure_agent(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    expected_revision: u64,
    config: AgentComposition,
) -> Result<ReadyPayload> {
    let request_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::ConfigureAgent {
            request_id: request_id.clone(),
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
        ExpectedResponse::Configure(expected_revision),
    )
    .await
}

#[derive(Clone, Copy)]
enum ExpectedResponse<'a> {
    Credential(&'a str),
    Login(&'a str),
    Configure(u64),
}

async fn wait_for_response(
    terminal: &mut SetupTerminal,
    state: &mut SetupState,
    events: &mut GatewayEvents,
    request_id: &str,
    expected: ExpectedResponse<'_>,
) -> Result<ReadyPayload> {
    let mut accepted = matches!(expected, ExpectedResponse::Credential(_));
    let mut completed = matches!(expected, ExpectedResponse::Configure(_));
    loop {
        match next_message(
            terminal,
            state,
            events,
            matches!(expected, ExpectedResponse::Login(_)),
        )
        .await?
        {
            ServerMessage::Ready { payload }
                if accepted
                    && completed
                    && !matches!(expected, ExpectedResponse::Configure(revision) if payload.config.revision <= revision) =>
            {
                return Ok(payload);
            }
            ServerMessage::Accepted { request_id: actual } if actual == request_id => {
                accepted = true;
            }
            ServerMessage::ProviderCredentialStatus {
                request_id: actual,
                provider,
                configured: true,
            } if actual == request_id
                && matches!(expected, ExpectedResponse::Credential(expected) if provider == expected) =>
            {
                completed = true;
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
                state.show_device_code(verification_url, user_code);
                draw(terminal, state)?;
            }
            ServerMessage::ProviderLoginFinished {
                request_id: actual,
                provider,
                ..
            } if actual == request_id
                && matches!(expected, ExpectedResponse::Login(expected) if provider == expected) =>
            {
                completed = true;
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
                return Err(Error::Stopped(
                    "gateway returned an invalid setup response".into(),
                ));
            }
            ServerMessage::Rejected {
                request_id: actual,
                message,
                ..
            } if actual == request_id => return Err(Error::Stopped(message)),
            ServerMessage::Error { message, .. } => return Err(Error::Stopped(message)),
            _ => {}
        }
    }
}

async fn next_message(
    terminal: &mut SetupTerminal,
    state: &SetupState,
    events: &mut GatewayEvents,
    cancellable: bool,
) -> Result<ServerMessage> {
    loop {
        tokio::select! {
            frame = events.next() => {
                return frame
                    .map_err(gateway_error)?
                    .map(|frame| frame.message)
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

async fn wait_for_acknowledgement(terminal: &mut SetupTerminal, state: &SetupState) -> Result<()> {
    let mut tick = tokio::time::interval(INPUT_POLL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        for _ in 0..MAX_INPUT_BATCH {
            let Some(event) = poll_event()? else {
                break;
            };
            match event {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                        && (matches!(
                            key.code,
                            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')
                        ) || key.modifiers.contains(KeyModifiers::CONTROL)
                            && matches!(key.code, KeyCode::Char('c' | 'd'))) =>
                {
                    return Ok(());
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
    let steps = state.steps();
    let current_step = steps
        .iter()
        .position(|step| *step == state.step)
        .unwrap_or_default();
    lines.push(Line::styled(
        format!("Step {} of {}", current_step + 1, steps.len()),
        theme.style(Role::Muted),
    ));
    let completed = completed_lines(state, &steps[..current_step]);
    if !completed.is_empty() {
        lines.push(Line::from(""));
        lines.extend(completed);
    }
    lines.push(Line::from(""));
    let (title, context) = step_prompt(state);
    lines.push(Line::styled(
        format!("  {title}"),
        theme.style(Role::Text).add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::styled(
        format!("  {context}"),
        theme.style(Role::Muted),
    ));
    lines.push(Line::from(""));
    render_step(lines, state);
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

fn completed_lines(state: &SetupState, steps: &[Step]) -> Vec<Line<'static>> {
    let theme = current();
    steps
        .iter()
        .filter_map(|step| {
            let (label, value) = match step {
                Step::Provider => ("Provider", state.definition().label().to_string()),
                Step::Credential => (
                    "Credential",
                    match state.definition().auth() {
                        ProviderAuth::ApiKey(_) if state.credential.trim().is_empty() => {
                            "reuse configured key".into()
                        }
                        ProviderAuth::ApiKey(_) => "new API key".into(),
                        ProviderAuth::Browser(_) if state.entry().status.configured => {
                            "reuse device login".into()
                        }
                        ProviderAuth::Browser(_) => "device login".into(),
                    },
                ),
                Step::Endpoint => ("Endpoint", terminal_text(state.endpoint.trim())),
                Step::Model | Step::CustomModel => {
                    if *step == Step::CustomModel {
                        return None;
                    }
                    ("Model", terminal_text(state.selected_model()))
                }
                Step::Review => return None,
            };
            Some(Line::from(vec![
                Span::styled("✓ ", theme.style(Role::Success)),
                Span::styled(format!("{label}: "), theme.style(Role::Muted)),
                Span::styled(value, theme.style(Role::Text)),
            ]))
        })
        .collect()
}

fn step_prompt(state: &SetupState) -> (&'static str, String) {
    match state.step {
        Step::Provider => (
            "Choose a model provider",
            "Only providers validated against this CLI are shown.".into(),
        ),
        Step::Credential => match state.definition().auth() {
            ProviderAuth::ApiKey(_) => (
                "API key",
                if state.mode == SetupMode::Agent && state.can_reuse_api_key() {
                    "Paste a replacement key, or leave blank to reuse the configured key."
                } else {
                    "Paste the provider API key. It is masked and sent only to the gateway."
                }
                .into(),
            ),
            ProviderAuth::Browser(auth) => (
                "Device login",
                if state.mode == SetupMode::Agent && state.entry().status.configured {
                    format!("{} is configured and will be reused.", auth.label())
                } else {
                    format!("Continue to start {} device login.", auth.label())
                },
            ),
        },
        Step::Endpoint => (
            "Responses endpoint",
            "Enter an HTTPS base URL, ending in /v1 when the provider requires it.".into(),
        ),
        Step::Model => (
            "Choose a model",
            "Select a manifest model or choose Custom model for another ID.".into(),
        ),
        Step::CustomModel => (
            "Custom model",
            "Enter the exact model ID accepted by this provider.".into(),
        ),
        Step::Review => (
            "Ready to apply",
            "Existing middleware, approvals, and system prompt will be preserved.".into(),
        ),
    }
}

fn render_step(lines: &mut Vec<Line<'static>>, state: &SetupState) {
    let theme = current();
    match state.step {
        Step::Provider => {
            for (index, entry) in state.providers.iter().enumerate() {
                let configured = if entry.status.configured {
                    "configured"
                } else {
                    "login required"
                };
                choice(
                    lines,
                    index,
                    entry.definition.label(),
                    &format!("{} · {configured}", entry.definition.description()),
                    index == state.provider,
                );
            }
        }
        Step::Credential => match state.definition().auth() {
            ProviderAuth::ApiKey(_) => {
                lines.push(Line::styled(
                    format!("  {}▏", masked_credential(&state.credential)),
                    theme.style(Role::Info),
                ));
            }
            ProviderAuth::Browser(_) => {
                lines.push(Line::styled(
                    "  Press Enter to continue.",
                    theme.style(Role::Info),
                ));
            }
        },
        Step::Endpoint => {
            lines.push(Line::styled(
                format!("  {}▏", terminal_text(&state.endpoint)),
                theme.style(Role::Info),
            ));
        }
        Step::Model => {
            for (index, model) in state.definition().models().iter().enumerate() {
                choice(
                    lines,
                    index,
                    model.label,
                    model.description,
                    index == state.model,
                );
            }
            let custom = state.definition().models().len();
            choice(
                lines,
                custom,
                "Custom model…",
                "Enter another model ID",
                state.model == custom,
            );
        }
        Step::CustomModel => {
            lines.push(Line::styled(
                format!("  {}▏", terminal_text(&state.custom_model)),
                theme.style(Role::Info),
            ));
        }
        Step::Review => {
            lines.push(Line::styled(
                "  Press Enter to send the selected settings to the gateway.",
                theme.style(Role::Info),
            ));
            lines.push(Line::styled(
                "  The CLI does not write credentials or agent configuration to disk.",
                theme.style(Role::Muted),
            ));
        }
    }
}

fn choice(
    lines: &mut Vec<Line<'static>>,
    index: usize,
    label: &str,
    description: &str,
    selected: bool,
) {
    let theme = current();
    let role = if selected {
        Role::Selection
    } else {
        Role::Text
    };
    lines.push(Line::styled(
        format!(
            "{} {}. {}",
            if selected { "›" } else { " " },
            index + 1,
            terminal_text(label)
        ),
        theme.style(role),
    ));
    lines.push(Line::styled(
        format!("     {}", terminal_text(description)),
        theme.style(if selected {
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
    if state.is_text_entry() {
        "  type or paste · enter continue · esc back · ctrl-c quit"
    } else if state.step == Step::Review {
        "  enter apply · esc back · q quit"
    } else {
        "  ↑↓/j k move · 1-9 select · enter confirm · esc back · q quit"
    }
}

fn render_progress(lines: &mut Vec<Line<'static>>, progress: &Progress) {
    let theme = current();
    let role = if progress.success {
        Role::Success
    } else {
        Role::Text
    };
    lines.push(Line::styled(
        format!("  {}", progress.title),
        theme.style(role).add_modifier(Modifier::BOLD),
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
        if progress.success {
            "  enter return to Horus"
        } else if progress.verification.is_some() {
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
        SetupState::from_parts(mode, providers, original).expect("setup state")
    }

    #[test]
    fn agent_wizard_moves_from_provider_through_model_to_review() {
        let mut state = state(SetupMode::Agent, "openai_socket", true);

        assert_eq!(state.confirm(), Flow::Continue);
        assert_eq!(state.step, Step::Credential);
        assert_eq!(state.confirm(), Flow::Continue);
        assert_eq!(state.step, Step::Model);
        assert_eq!(state.confirm(), Flow::Continue);

        assert_eq!(state.step, Step::Review);
    }

    #[test]
    fn custom_provider_login_confirms_the_endpoint() {
        let mut state = state(SetupMode::Login, "responses", false);

        state.confirm();
        state.credential = "secret".into();
        assert_eq!(state.confirm(), Flow::Continue);
        assert_eq!(state.step, Step::Endpoint);
        assert_eq!(state.confirm(), Flow::Finish);
    }

    #[test]
    fn agent_composition_preserves_non_provider_settings() {
        let mut state = state(SetupMode::Agent, "responses", true);
        state.endpoint = "https://example.com/v1".into();
        state.custom_model = "example-model".into();
        state.model = state.definition().models().len();
        let original = state.original.clone();

        let configured = state
            .agent_composition(&original)
            .expect("agent composition");

        assert_eq!(
            (
                configured.middleware,
                configured.approval,
                configured.system_prompt
            ),
            (
                original.middleware,
                original.approval,
                original.system_prompt
            )
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
    fn custom_provider_requires_an_endpoint_key_even_when_configured() {
        let state = state(SetupMode::Agent, "responses", true);

        assert!(!state.can_reuse_api_key());
    }

    #[test]
    fn credential_entry_is_masked_and_supports_backspace() {
        let mut state = state(SetupMode::Login, "openai_socket", false);
        state.confirm();
        state.paste("abc123\n");
        state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

        assert_eq!(masked_credential(&state.credential), "•••••");
    }
}
