use horus::backend::model::provider::HostedWebSearch;
use horus::backend::model::provider::ProviderAuth;
use horus::backend::model::provider::providers;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;

use crate::frontend::theme::Role;
use crate::frontend::theme::current;

use super::state::APPROVALS;
use super::state::FEATURES;
use super::state::SetupState;
use super::state::Step;

pub(super) fn render(frame: &mut Frame<'_>, state: &SetupState, step: Step) {
    let theme = current();
    frame.render_widget(
        Block::default().style(theme.style(Role::Canvas)),
        frame.area(),
    );
    let area = content_area(frame.area());
    let steps = state.steps();
    let current = steps.iter().position(|item| *item == step).unwrap_or(0);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("◉ ", theme.style(Role::AccentStrong)),
            Span::styled(
                "HORUS",
                theme.style(Role::Accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" setup", theme.style(Role::Muted)),
        ]),
        Line::styled(
            format!("Step {} of {}", current + 1, steps.len()),
            theme.style(Role::Muted),
        ),
    ];
    if let Some(message) = &state.repair_message {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("! {message}"),
            theme.style(Role::Error),
        ));
    }
    let completed = completed_lines(state, &steps[..current]);
    if !completed.is_empty() {
        lines.push(Line::from(""));
        lines.extend(completed);
    }
    lines.push(Line::from(""));
    render_step(&mut lines, state, step);
    if let Some(error) = &state.error {
        lines.push(Line::from(""));
        lines.push(Line::styled(format!("  {error}"), theme.style(Role::Error)));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(footer(state, step), theme.style(Role::Muted)));
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

fn completed_lines(state: &SetupState, steps: &[Step]) -> Vec<Line<'static>> {
    let theme = current();
    steps
        .iter()
        .filter_map(|step| {
            let (label, value) = match step {
                Step::Provider => ("Provider", state.provider().label().to_string()),
                Step::Credential => match state.provider().auth() {
                    ProviderAuth::ApiKey(_) => ("API key", "saved in local config".into()),
                    ProviderAuth::Browser(auth) => (auth.label(), "logged in".into()),
                },
                Step::Endpoint => ("Endpoint", state.endpoint.trim().into()),
                Step::Model => (
                    "Model",
                    state
                        .model_preset()
                        .map_or_else(|| "Custom model".into(), |model| model.id.into()),
                ),
                Step::CustomModel => ("Custom model", state.custom_model.trim().into()),
                Step::CustomContext => ("Context window", state.custom_context.trim().into()),
                Step::Reasoning => ("Reasoning", reasoning_label(state)),
                Step::Search => (
                    "Web search",
                    state.provider().web_search()[state.search].label().into(),
                ),
                Step::Features => (
                    "Features",
                    format!("{}/{} enabled", state.features.len(), FEATURES.len()),
                ),
                Step::Approvals => ("Approvals", APPROVALS[state.approvals].label.into()),
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

fn render_step(lines: &mut Vec<Line<'static>>, state: &SetupState, step: Step) {
    let theme = current();
    let (title, description) = step_prompt(state, step);
    lines.push(Line::styled(
        format!("  {title}"),
        theme.style(Role::Text).add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::styled(
        format!("  {description}"),
        theme.style(Role::Muted),
    ));
    lines.push(Line::from(""));
    match step {
        Step::Credential => match state.provider().auth() {
            ProviderAuth::ApiKey(_) => {
                let masked = "•".repeat(state.credential.chars().count().min(32));
                lines.push(Line::styled(
                    format!("  {masked}▏"),
                    theme.style(Role::Info),
                ));
            }
            ProviderAuth::Browser(auth) => {
                if let Some(url) = &state.oauth_url {
                    lines.push(Line::styled(
                        "  Waiting for browser approval…",
                        theme.style(Role::Info),
                    ));
                    lines.push(Line::from(""));
                    lines.push(Line::styled(format!("  {url}"), theme.style(Role::Info)));
                } else {
                    lines.push(Line::styled(
                        format!("  Press Enter to open {} login.", auth.label()),
                        theme.style(Role::Info),
                    ));
                }
            }
        },
        Step::Endpoint => {
            lines.push(Line::styled(
                format!("  {}▏", state.endpoint),
                theme.style(Role::Info),
            ));
        }
        Step::CustomModel => {
            lines.push(Line::styled(
                format!("  {}▏", state.custom_model),
                theme.style(Role::Info),
            ));
        }
        Step::CustomContext => {
            lines.push(Line::styled(
                format!("  {}▏", state.custom_context),
                theme.style(Role::Info),
            ));
        }
        Step::Review => {
            lines.push(Line::styled(
                "  API credentials entered here will be saved in the local configuration.",
                theme.style(Role::Muted),
            ));
        }
        Step::Provider
        | Step::Model
        | Step::Reasoning
        | Step::Search
        | Step::Features
        | Step::Approvals => {
            for (index, (label, description)) in choices(state, step).iter().enumerate() {
                choice(lines, index, label, description, index == state.selection());
            }
        }
    }
}

fn step_prompt(state: &SetupState, step: Step) -> (&'static str, String) {
    match step {
        Step::Provider => (
            "Choose a model provider",
            "Provider capabilities drive the remaining setup.".into(),
        ),
        Step::Credential => match state.provider().auth() {
            ProviderAuth::ApiKey(name) => (
                "API key",
                format!("{name} is not set. Paste a key to save in Horus's local configuration."),
            ),
            ProviderAuth::Browser(auth) => (
                "Sign in with browser",
                format!(
                    "Complete {} authentication; tokens stay in Horus's owner-only auth file.",
                    auth.label()
                ),
            ),
        },
        Step::Endpoint => (
            "Responses endpoint",
            "Enter the provider base URL, ending in /v1.".into(),
        ),
        Step::Model => (
            "Choose a model",
            "Models and context limits come from the provider.".into(),
        ),
        Step::CustomModel => (
            "Custom model",
            "Enter the exact model ID accepted by this provider.".into(),
        ),
        Step::CustomContext => (
            "Model context window",
            "Enter the provider's maximum context size in tokens.".into(),
        ),
        Step::Reasoning => (
            "Choose reasoning effort",
            "Only levels advertised by this model are shown.".into(),
        ),
        Step::Search => (
            "Hosted web search",
            "Choose a mode supported by this provider.".into(),
        ),
        Step::Features => (
            "Choose features",
            "All optional middleware starts enabled.".into(),
        ),
        Step::Approvals => (
            "Tool permissions",
            "Choose prompting and network access; filesystem sandboxing always remains active."
                .into(),
        ),
        Step::Review => (
            "Ready to start",
            "Review the selections above, then save the configuration.".into(),
        ),
    }
}

fn choices(state: &SetupState, step: Step) -> Vec<(String, String)> {
    match step {
        Step::Provider => providers()
            .iter()
            .map(|provider| (provider.label().into(), provider.description().into()))
            .collect(),
        Step::Model => state
            .provider()
            .models()
            .iter()
            .map(|model| (model.label.into(), model.description.into()))
            .chain([("Custom model…".into(), "Enter another model ID".into())])
            .collect(),
        Step::Reasoning => {
            let default = state
                .model_preset()
                .and_then(|model| model.default_reasoning)
                .map_or_else(
                    || "Use the provider default".into(),
                    |value| format!("Use the provider default ({value})"),
                );
            std::iter::once(("Provider default".into(), default))
                .chain(
                    state
                        .model_preset()
                        .into_iter()
                        .flat_map(|model| model.reasoning)
                        .map(|preset| (preset.label.into(), preset.description.into())),
                )
                .collect()
        }
        Step::Search => state
            .provider()
            .web_search()
            .iter()
            .map(|search| (search.label().into(), search_description(*search).into()))
            .collect(),
        Step::Features => FEATURES
            .iter()
            .map(|feature| {
                let mark = if state.features.contains(feature) {
                    "✓"
                } else {
                    " "
                };
                (
                    format!("[{mark}] {}", feature.label()),
                    feature.description().into(),
                )
            })
            .collect(),
        Step::Approvals => APPROVALS
            .iter()
            .map(|choice| (choice.label.into(), choice.description.into()))
            .collect(),
        Step::Credential
        | Step::Endpoint
        | Step::CustomModel
        | Step::CustomContext
        | Step::Review => Vec::new(),
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
    let row = format!(
        "{} {}. {label}",
        if selected { "›" } else { " " },
        index + 1
    );
    lines.push(Line::styled(
        row,
        theme.style(if selected {
            Role::Selection
        } else {
            Role::Text
        }),
    ));
    let description = format!("     {description}");
    lines.push(Line::styled(
        description,
        theme.style(if selected {
            Role::Selection
        } else {
            Role::Muted
        }),
    ));
}

fn reasoning_label(state: &SetupState) -> String {
    state
        .model_preset()
        .and_then(|model| {
            state
                .reasoning
                .checked_sub(1)
                .and_then(|index| model.reasoning.get(index))
        })
        .map_or_else(|| "Provider default".into(), |preset| preset.label.into())
}

fn search_description(search: HostedWebSearch) -> &'static str {
    match search {
        HostedWebSearch::Off => "Do not expose hosted web search",
        HostedWebSearch::Cached => "Prefer cached search results",
        HostedWebSearch::Live => "Allow live hosted searches",
    }
}

fn footer(state: &SetupState, step: Step) -> &'static str {
    if step == Step::Credential && matches!(state.provider().auth(), ProviderAuth::Browser(_)) {
        return if state.oauth_url.is_some() {
            "  complete login in your browser · esc cancel"
        } else {
            "  enter sign in · esc back · q quit"
        };
    }
    if matches!(
        step,
        Step::Credential | Step::Endpoint | Step::CustomModel | Step::CustomContext
    ) {
        "  type or paste · enter continue · esc back · ctrl-c quit"
    } else if step == Step::Features {
        "  ↑↓/j k move · space/1-5 toggle · enter continue · esc back · q quit"
    } else if step == Step::Review {
        "  enter save · esc go back · q quit"
    } else {
        "  ↑↓/j k move · 1-9 select · enter confirm · esc back · q quit"
    }
}
