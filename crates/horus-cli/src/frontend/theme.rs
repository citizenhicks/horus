use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;

#[derive(Clone, Copy)]
pub(crate) enum Role {
    Canvas,
    Text,
    Muted,
    Border,
    Accent,
    AccentStrong,
    Info,
    Reasoning,
    Code,
    Neutral,
    Success,
    Warning,
    Error,
    Selection,
}

pub(crate) struct Theme {
    surface: Color,
    foreground: Color,
    muted: Color,
    border: Color,
    accent: Color,
    accent_strong: Color,
    info: Color,
    reasoning: Color,
    code: Color,
    neutral: Color,
    success: Color,
    warning: Color,
    error: Color,
    diff_add: Color,
    diff_delete: Color,
}

const SORA: Theme = Theme {
    surface: Color::Rgb(34, 40, 56),
    foreground: Color::Rgb(200, 208, 224),
    muted: Color::Rgb(88, 100, 120),
    border: Color::Rgb(34, 40, 56),
    accent: Color::Rgb(212, 184, 120),
    accent_strong: Color::Rgb(224, 200, 136),
    info: Color::Rgb(128, 200, 224),
    reasoning: Color::Rgb(176, 160, 216),
    code: Color::Rgb(144, 200, 160),
    neutral: Color::Rgb(136, 152, 184),
    success: Color::Rgb(104, 168, 136),
    warning: Color::Rgb(200, 168, 96),
    error: Color::Rgb(196, 108, 120),
    diff_add: Color::Rgb(33, 58, 43),
    diff_delete: Color::Rgb(74, 34, 29),
};

pub(crate) const fn current() -> &'static Theme {
    &SORA
}

impl Theme {
    pub(crate) const fn color(&self, role: Role) -> Color {
        match role {
            Role::Canvas | Role::Text => self.foreground,
            Role::Muted => self.muted,
            Role::Border => self.border,
            Role::Accent => self.accent,
            Role::AccentStrong | Role::Selection => self.accent_strong,
            Role::Info => self.info,
            Role::Reasoning => self.reasoning,
            Role::Code => self.code,
            Role::Neutral => self.neutral,
            Role::Success => self.success,
            Role::Warning => self.warning,
            Role::Error => self.error,
        }
    }

    pub(crate) fn style(&self, role: Role) -> Style {
        if matches!(role, Role::Selection) {
            Style::default()
                .fg(self.color(role))
                .bg(self.surface)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.color(role))
        }
    }

    pub(crate) const fn diff_add_background(&self) -> Color {
        self.diff_add
    }

    pub(crate) const fn diff_delete_background(&self) -> Color {
        self.diff_delete
    }
}
