//! Gateway-native provider login and agent setup wizard.

mod runtime;
mod state;
mod view;

use std::collections::BTreeSet;
use std::io;

use mobius::backend::model::provider::HostedWebSearch;
use mobius::protocol::{
    FrontendSetting, FrontendSettingKind, FrontendSettingValue, MiddlewareFeature,
};
use mobius::{Error, Result};
use mobius_gateway::client::{GatewayEvents, GatewaySender, MAX_PENDING_FRAMES};
use mobius_gateway::wire::{
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

use self::runtime::*;
use self::state::*;
use self::view::*;

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

#[cfg(test)]
mod tests {
    use mobius::protocol::{FrontendSettingOption, FrontendSymbol};
    use mobius_gateway::wire::{ProviderAuthKind, ProviderModel, ProviderStatus, ReasoningChoice};
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
        let reasoning_efforts = if model_ids_configurable {
            vec!["medium".into()]
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
            reasoning_efforts,
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

        let missing_custom_reasoning = reject(
            status("responses", true),
            ProviderConfig {
                provider: "responses".into(),
                model: AgentComposition::default().provider.model,
                base_url: Some("https://api.openai.com/v1".into()),
                reasoning_effort: Some("missing".into()),
                web_search: HostedWebSearch::Off,
            },
        );
        assert!(missing_custom_reasoning.contains("unconfigured reasoning"));
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
