use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io;
use std::io::IsTerminal;
use std::path::Path;
use std::path::PathBuf;

use horus::backend::model::provider::{ProviderAuth, ProviderDefinition, provider, providers};
use horus::{BoxFuture, Error, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::time::MissedTickBehavior;

use self::render::render;
use self::state::{Flow, SetupMode, SetupState, Step};
use super::terminal::{INPUT_POLL, MAX_INPUT_BATCH, TerminalGuard, poll_event};
use crate::config::{
    BuiltAgentConfig, FileConfig, ModelSettings, SaveMode, auth_path, config_path, parse_config,
    save_config, state_dir,
};

mod render;
mod state;

enum MissingCredential {
    ApiKey {
        definition: &'static ProviderDefinition,
        name: String,
    },
    Browser {
        definition: &'static ProviderDefinition,
    },
}

pub(crate) async fn load_agent_config(workspace: &Path) -> Result<BuiltAgentConfig> {
    let state_dir = state_dir()?;
    let path = config_path(workspace, &state_dir);
    let auth_path = auth_path(&state_dir);
    let mut config = match fs::read_to_string(&path) {
        Ok(contents) => parse_config(&path, &contents)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !io::stdin().is_terminal() {
                return Err(Error::Config(format!(
                    "configuration is missing at {}; run Horus in a terminal for setup",
                    path.display()
                )));
            }
            let config = run(None, auth_path.clone()).await?;
            save_config(&path, &config, SaveMode::New)?;
            config
        }
        Err(error) => return Err(error.into()),
    };
    if io::stdin().is_terminal() {
        for requirement in missing_credentials(&config, &auth_path)? {
            match requirement {
                MissingCredential::ApiKey { definition, name } => {
                    let api_key = credential(
                        definition,
                        &format!("{name} is not set for one or more configured model routes."),
                        auth_path.clone(),
                    )
                    .await?
                    .ok_or_else(|| Error::Config("API-key setup returned no key".into()))?;
                    set_missing_api_key(&mut config, &name, &api_key)?;
                    save_config(&path, &config, SaveMode::Replace)?;
                }
                MissingCredential::Browser { definition } => {
                    credential(
                        definition,
                        "Browser login is required for one or more configured model routes.",
                        auth_path.clone(),
                    )
                    .await?;
                }
            }
        }
    }
    let session_id = env::var("HORUS_SESSION_ID").ok();
    let workspace = workspace.to_path_buf();
    // Assembly does blocking filesystem and SQLite work (skill discovery, credential
    // reads, checkpoint schema setup); keep it off the executor.
    tokio::task::spawn_blocking(move || config.build(&workspace, &state_dir, session_id))
        .await
        .map_err(|error| Error::Config(format!("agent assembly task failed: {error}")))?
}

pub(crate) async fn add_provider(workspace: &Path) -> Result<()> {
    let state_dir = state_dir()?;
    let path = config_path(workspace, &state_dir);
    let auth_path = auth_path(&state_dir);
    let contents = fs::read_to_string(&path)?;
    let mut config = parse_config(&path, &contents)?;
    config.validate()?;

    let mut configured_credentials = config
        .models
        .iter()
        .filter(|(route, settings)| {
            route.as_str() != config.agent.model
                && settings
                    .api_key
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
        })
        .map(|(_, settings)| settings.provider.clone())
        .collect::<BTreeSet<_>>();
    for definition in providers() {
        if let ProviderAuth::Browser(auth) = definition.auth()
            && auth.configured(&auth_path)?
        {
            configured_credentials.insert(definition.id().to_string());
        }
    }
    let settings = run_provider(configured_credentials, auth_path).await?;
    upsert_provider(&mut config, settings);
    save_config(&path, &config, SaveMode::Replace)
}

async fn run(repair_message: Option<&str>, auth_path: PathBuf) -> Result<FileConfig> {
    let config = run_state(SetupState::new(repair_message, auth_path, SetupMode::Full))
        .await?
        .config();
    config.validate()?;
    Ok(config)
}

async fn run_provider(
    configured_credentials: BTreeSet<String>,
    auth_path: PathBuf,
) -> Result<ModelSettings> {
    let mut state = SetupState::new(None, auth_path, SetupMode::Provider);
    state.configured_credentials = configured_credentials;
    Ok(run_state(state).await?.model_settings())
}

async fn credential(
    provider: &ProviderDefinition,
    message: &str,
    auth_path: PathBuf,
) -> Result<Option<String>> {
    let mut state = SetupState::new(Some(message), auth_path, SetupMode::Credential);
    state.provider = providers()
        .iter()
        .position(|candidate| candidate.id() == provider.id())
        .ok_or_else(|| Error::Config(format!("unknown provider `{}`", provider.id())))?;
    let state = run_state(state).await?;
    Ok(matches!(state.provider().auth(), ProviderAuth::ApiKey(_))
        .then(|| state.credential.trim().to_string()))
}

async fn run_state(mut state: SetupState) -> Result<SetupState> {
    let _guard = TerminalGuard::alternate()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut tick = tokio::time::interval(INPUT_POLL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut pending_login: Option<BoxFuture<'static, Result<()>>> = None;
    let mut dirty = true;

    loop {
        if dirty {
            let step = state.step;
            terminal.draw(|frame| render(frame, &state, step))?;
            dirty = false;
        }
        tokio::select! {
            result = wait_for_login(&mut pending_login) => {
                pending_login = None;
                state.oauth_url = None;
                match result {
                    Ok(()) => {
                        let provider = state.provider().id().to_string();
                        state.configured_credentials.insert(provider);
                        if state.mode == SetupMode::Credential {
                            return Ok(state);
                        }
                        state.advance();
                    }
                    Err(error) => state.error = Some(error.to_string()),
                }
                dirty = true;
            }
            _ = tick.tick() => {
                for _ in 0..MAX_INPUT_BATCH {
                    let Some(event) = poll_event()? else {
                        break;
                    };
                    dirty = true;
                    if pending_login.is_some() {
                        if cancels_login(&event) {
                            pending_login = None;
                            state.oauth_url = None;
                            state.error = Some("stopped waiting for browser login".into());
                        }
                        continue;
                    }
                    match event {
                        Event::Key(key) => match state.handle_key(key) {
                            Flow::Continue => {}
                            Flow::Finish => return Ok(state),
                            Flow::Cancel => {
                                return Err(Error::Config("setup cancelled".into()));
                            }
                            Flow::Authenticate => {
                                let ProviderAuth::Browser(auth) = state.provider().auth() else {
                                    return Err(Error::Config(
                                        "provider does not support browser login".into(),
                                    ));
                                };
                                let login = auth.start().await?;
                                state.oauth_url = Some(login.url().to_string());
                                login.open_browser();
                                pending_login = Some(login.complete(state.auth_path.clone()));
                            }
                        },
                        Event::Paste(text) => state.paste(&text),
                        Event::Resize(_, _)
                        | Event::FocusGained
                        | Event::FocusLost
                        | Event::Mouse(_) => {}
                    }
                }
            }
        }
    }
}

async fn wait_for_login(pending_login: &mut Option<BoxFuture<'static, Result<()>>>) -> Result<()> {
    match pending_login {
        Some(login) => login.await,
        None => std::future::pending().await,
    }
}

fn cancels_login(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    key.code == KeyCode::Esc
        || key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'd'))
}

fn missing_credentials(config: &FileConfig, auth_path: &Path) -> Result<Vec<MissingCredential>> {
    let mut missing = BTreeSet::new();
    let mut requirements = Vec::new();
    for settings in config.models.values() {
        let definition = provider(&settings.provider)?;
        match definition.auth() {
            ProviderAuth::ApiKey(default_env) => {
                let name = settings.api_key_env.as_deref().unwrap_or(default_env);
                let available = settings
                    .api_key
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                    || env::var(name).is_ok_and(|value| !value.trim().is_empty());
                if !available && missing.insert(name.to_string()) {
                    requirements.push(MissingCredential::ApiKey {
                        definition,
                        name: name.to_string(),
                    });
                }
            }
            ProviderAuth::Browser(auth)
                if !auth.configured(auth_path)? && missing.insert(definition.id().to_string()) =>
            {
                requirements.push(MissingCredential::Browser { definition });
            }
            ProviderAuth::Browser(_) => {}
        }
    }
    Ok(requirements)
}

fn set_missing_api_key(config: &mut FileConfig, name: &str, api_key: &str) -> Result<()> {
    for settings in config.models.values_mut() {
        let definition = provider(&settings.provider)?;
        let ProviderAuth::ApiKey(default_env) = definition.auth() else {
            continue;
        };
        let route_api_key_env = settings.api_key_env.as_deref().unwrap_or(default_env);
        if route_api_key_env == name
            && settings
                .api_key
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        {
            settings.api_key = Some(api_key.to_string());
        }
    }
    Ok(())
}

fn upsert_provider(config: &mut FileConfig, mut settings: ModelSettings) {
    let provider = settings.provider.as_str();
    let existing = config
        .models
        .iter()
        .find(|(route, candidate)| {
            route.as_str() != config.agent.model
                && candidate.provider == provider
                && candidate
                    .api_key
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
        })
        .or_else(|| {
            config.models.iter().find(|(route, candidate)| {
                route.as_str() != config.agent.model && candidate.provider == provider
            })
        })
        .map(|(route, _)| route.clone());
    if let Some(route) = existing {
        if settings.api_key.is_none() {
            settings.api_key = config.models[&route].api_key.clone();
        }
        config.models.insert(route, settings);
        return;
    }

    let base = settings.provider.clone();
    let mut route = base.clone();
    let mut suffix = 2;
    while config.models.contains_key(&route) {
        route = format!("{base}-{suffix}");
        suffix += 1;
    }
    config.models.insert(route, settings);
}

impl SetupState {
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
            KeyCode::Char(' ') if self.step == Step::Features => {
                self.toggle_feature(self.feature);
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
                self.text_mut().push(character);
                self.error = None;
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    fn paste(&mut self, text: &str) {
        if self.is_text_entry() {
            self.text_mut().push_str(text.trim());
            self.error = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use horus::backend::model::provider::HostedWebSearch;

    use super::*;

    #[test]
    fn browser_login_only_cancels_for_escape_or_control_c_and_d() {
        let outcomes = [
            (KeyCode::Esc, KeyModifiers::NONE),
            (KeyCode::Char('c'), KeyModifiers::CONTROL),
            (KeyCode::Char('d'), KeyModifiers::CONTROL),
            (KeyCode::Char('c'), KeyModifiers::NONE),
        ]
        .map(|(code, modifiers)| cancels_login(&Event::Key(KeyEvent::new(code, modifiers))));

        assert_eq!(outcomes, [true, true, true, false]);
    }

    #[test]
    fn updating_a_provider_preserves_its_api_key() {
        let mut config = SetupState::new(None, PathBuf::new(), SetupMode::Full).config();
        let settings = |model: &str, api_key: Option<String>| ModelSettings {
            provider: "kimi".into(),
            model: model.into(),
            base_url: None,
            api_key,
            api_key_env: None,
            context_window: None,
            reasoning_effort: None,
            web_search: HostedWebSearch::Off,
        };
        config.models.insert(
            "kimi".into(),
            settings("old-model", Some("test-key".into())),
        );

        upsert_provider(&mut config, settings("kimi-k3", None));

        assert_eq!(config.models["kimi"].model, "kimi-k3");
        assert_eq!(config.models["kimi"].api_key.as_deref(), Some("test-key"));
    }
}
