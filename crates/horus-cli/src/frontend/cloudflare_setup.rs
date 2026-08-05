//! First-run setup for one existing user-owned Cloudflare Tunnel.

use std::io;

use horus::{Error, Result};
use horus_gateway::config::CloudflareConfig;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tokio::time::MissedTickBehavior;

use super::terminal::{INPUT_POLL, MAX_INPUT_BATCH, TerminalGuard, poll_event};
use super::terminal_text;
use super::theme::{Role, current};

const MAX_HOSTNAME_BYTES: usize = 253;
const MAX_TOKEN_BYTES: usize = 16 * 1024;

/// Validated values consumed by gateway initialization and never displayed again.
pub struct CloudflareInit {
    pub hostname: String,
    pub token: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Hostname,
    Token,
    Connect,
}

struct State {
    field: Field,
    hostname: String,
    token: String,
    error: Option<String>,
}

impl State {
    fn new() -> Self {
        Self {
            field: Field::Hostname,
            hostname: String::new(),
            token: String::new(),
            error: None,
        }
    }

    fn move_field(&mut self, delta: isize) {
        let current = match self.field {
            Field::Hostname => 0,
            Field::Token => 1,
            Field::Connect => 2,
        };
        self.field = match (current + delta).rem_euclid(3) {
            0 => Field::Hostname,
            1 => Field::Token,
            _ => Field::Connect,
        };
        self.error = None;
    }

    fn push(&mut self, text: &str) {
        let (target, limit) = match self.field {
            Field::Hostname => (&mut self.hostname, MAX_HOSTNAME_BYTES),
            Field::Token => (&mut self.token, MAX_TOKEN_BYTES),
            Field::Connect => return,
        };
        for character in text.chars().filter(|character| !character.is_control()) {
            if target.len() + character.len_utf8() > limit {
                self.error = Some(format!("input is limited to {limit} bytes"));
                return;
            }
            target.push(character);
        }
        self.error = None;
    }

    fn backspace(&mut self) {
        match self.field {
            Field::Hostname => {
                self.hostname.pop();
            }
            Field::Token => {
                self.token.pop();
            }
            Field::Connect => return,
        }
        self.error = None;
    }

    fn finish(&mut self) -> Result<CloudflareInit> {
        let cloudflare = CloudflareConfig::new(&self.hostname).map_err(gateway_error)?;
        CloudflareConfig::validate_token(&self.token).map_err(gateway_error)?;
        Ok(CloudflareInit {
            hostname: cloudflare.hostname().to_owned(),
            token: std::mem::take(&mut self.token).trim().to_owned(),
        })
    }
}

fn gateway_error(error: horus_gateway::Error) -> Error {
    Error::Config(error.to_string())
}

/// Collects an existing tunnel hostname and connector token without echoing the token.
pub async fn run() -> Result<Option<CloudflareInit>> {
    let mut guard = TerminalGuard::alternate()?;
    guard.set_mouse_capture(false)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut state = State::new();
    terminal.clear()?;
    let mut tick = tokio::time::interval(INPUT_POLL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut dirty = true;
    loop {
        if dirty {
            terminal.draw(|frame| render(frame, &state))?;
            dirty = false;
        }
        tick.tick().await;
        for _ in 0..MAX_INPUT_BATCH {
            let Some(event) = poll_event()? else {
                break;
            };
            dirty = true;
            match event {
                Event::Key(key) => match handle_key(&mut state, key) {
                    Action::Continue => {}
                    Action::Cancel => return Ok(None),
                    Action::Finish => match state.finish() {
                        Ok(config) => return Ok(Some(config)),
                        Err(error) => state.error = Some(error.to_string()),
                    },
                },
                Event::Paste(text) => state.push(text.trim()),
                Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Mouse(_) => {}
            }
        }
    }
}

enum Action {
    Continue,
    Cancel,
    Finish,
}

fn handle_key(state: &mut State, key: KeyEvent) -> Action {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return Action::Continue;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'd'))
    {
        return Action::Cancel;
    }
    match key.code {
        KeyCode::Esc => Action::Cancel,
        KeyCode::Up | KeyCode::BackTab => {
            state.move_field(-1);
            Action::Continue
        }
        KeyCode::Down | KeyCode::Tab => {
            state.move_field(1);
            Action::Continue
        }
        KeyCode::Enter if state.field == Field::Connect => Action::Finish,
        KeyCode::Enter => {
            state.move_field(1);
            Action::Continue
        }
        KeyCode::Backspace => {
            state.backspace();
            Action::Continue
        }
        KeyCode::Char(character)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            state.push(&character.to_string());
            Action::Continue
        }
        _ => Action::Continue,
    }
}

fn render(frame: &mut ratatui::Frame<'_>, state: &State) {
    let theme = current();
    let hostname = if state.hostname.is_empty() {
        "horus.example.com".into()
    } else {
        terminal_text(&state.hostname)
    };
    let token = if state.token.is_empty() {
        "paste the tunnel token".into()
    } else {
        masked_token(&state.token)
    };
    let selected = |field| {
        if state.field == field {
            theme.style(Role::Selection)
        } else {
            theme.style(Role::Text)
        }
    };
    let mut lines = vec![
        Line::from(""),
        Line::styled(
            "  Use a dedicated named tunnel already created in your Cloudflare account.",
            theme.style(Role::Muted),
        ),
        Line::styled(
            "  Published application service: http://127.0.0.1:8741",
            theme.style(Role::Muted),
        ),
        Line::from(""),
        Line::styled("  Public hostname", theme.style(Role::Muted)),
        Line::from(Span::styled(
            format!("  {hostname}"),
            selected(Field::Hostname),
        )),
        Line::from(""),
        Line::styled("  Tunnel token", theme.style(Role::Muted)),
        Line::from(Span::styled(format!("  {token}"), selected(Field::Token))),
        Line::from(""),
        Line::from(Span::styled("  Connect tunnel", selected(Field::Connect))),
        Line::from(""),
    ];
    if let Some(error) = &state.error {
        lines.push(Line::styled(
            format!("  {}", terminal_text(error)),
            theme.style(Role::Error),
        ));
        lines.push(Line::from(""));
    }
    lines.push(Line::styled(
        "  tab/↑↓ select · enter continue · esc cancel",
        theme.style(Role::Muted),
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Cloudflare Tunnel "),
            )
            .style(theme.style(Role::Canvas))
            .wrap(Wrap { trim: false }),
        frame.area(),
    );
}

fn masked_token(token: &str) -> String {
    let count = token.chars().count();
    let mut masked = "•".repeat(count.min(24));
    if count > 24 {
        masked.push('…');
    }
    masked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_token_is_never_rendered() {
        let token = "secret-tunnel-token";

        assert!(!masked_token(token).contains(token));
    }

    #[test]
    fn finish_normalizes_the_hostname_and_moves_the_token() {
        let mut state = State {
            field: Field::Connect,
            hostname: " Horus.Example.com ".into(),
            token: " secret-tunnel-token ".into(),
            error: None,
        };

        let config = state.finish().expect("valid setup");

        assert_eq!(
            (config.hostname.as_str(), config.token.as_str()),
            ("horus.example.com", "secret-tunnel-token")
        );
    }
}
