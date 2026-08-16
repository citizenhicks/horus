use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

static PROCESS_START: OnceLock<Instant> = OnceLock::new();

pub(super) fn line(text: &str, base: Color, highlight: Color) -> Line<'static> {
    line_at(text, elapsed_since_start(), base, highlight)
}

fn elapsed_since_start() -> Duration {
    PROCESS_START.get_or_init(Instant::now).elapsed()
}

fn line_at(text: &str, elapsed: Duration, base: Color, highlight: Color) -> Line<'static> {
    let characters = text.chars().collect::<Vec<_>>();
    if characters.is_empty() {
        return Line::default();
    }

    let padding = 10;
    let period = characters.len() + padding * 2;
    let sweep_seconds = 2.0;
    let position =
        ((elapsed.as_secs_f32() % sweep_seconds) / sweep_seconds * period as f32) as isize;
    let band_half_width = 5.0;

    Line::from(
        characters
            .into_iter()
            .enumerate()
            .map(|(index, character)| {
                let distance = ((index + padding) as isize - position).abs() as f32;
                let intensity = if distance <= band_half_width {
                    let angle = std::f32::consts::PI * (distance / band_half_width);
                    0.5 * (1.0 + angle.cos())
                } else {
                    0.0
                };
                Span::styled(
                    character.to_string(),
                    Style::default()
                        .fg(blend(highlight, base, intensity * 0.9))
                        .add_modifier(Modifier::BOLD),
                )
            })
            .collect::<Vec<_>>(),
    )
}

fn blend(foreground: Color, background: Color, alpha: f32) -> Color {
    let (Color::Rgb(fr, fg, fb), Color::Rgb(br, bg, bb)) = (foreground, background) else {
        return if alpha >= 0.5 { foreground } else { background };
    };
    Color::Rgb(
        (f32::from(fr) * alpha + f32::from(br) * (1.0 - alpha)) as u8,
        (f32::from(fg) * alpha + f32::from(bg) * (1.0 - alpha)) as u8,
        (f32::from(fb) * alpha + f32::from(bb) * (1.0 - alpha)) as u8,
    )
}
