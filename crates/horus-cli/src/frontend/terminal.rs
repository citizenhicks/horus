use std::io;
use std::time::Duration;

use horus::Result;
use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

pub(super) const INPUT_POLL: Duration = Duration::from_millis(16);
pub(super) const MAX_INPUT_BATCH: usize = 64;

pub(super) fn poll_event() -> Result<Option<Event>> {
    event::poll(Duration::ZERO)?
        .then(event::read)
        .transpose()
        .map_err(Into::into)
}

pub(super) struct TerminalGuard;

impl TerminalGuard {
    pub(super) fn alternate() -> Result<Self> {
        enable_raw_mode()?;
        let guard = Self;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            Hide
        )?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            Show,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
    }
}
