use std::io;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::SynchronizedUpdate;
use ratatui::crossterm::event::Event as TerminalEvent;
use ratatui::crossterm::execute;
use ratatui::crossterm::style::Print;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use super::TranscriptTone;
use super::TuiState;
use super::events::handle_event;
use super::input::UiAction;
use super::view::render_preview;
use crate::frontend::FrontendExit;
use crate::frontend::catalog::UiCatalog;
use crate::frontend::terminal::{INPUT_POLL, MAX_INPUT_BATCH, TerminalGuard, poll_event};
use horus::Result;
use horus::agent::Agent;
use horus::protocol::Event;
use horus::protocol::EventMsg;
use horus::protocol::FrontendBlock;
use horus::protocol::Op;

const SHIMMER_INTERVAL: Duration = Duration::from_millis(32);
const ELAPSED_INTERVAL: Duration = Duration::from_secs(1);
const MAX_EVENT_BATCH: usize = 64;
const CLEAR_SCREEN_AND_SCROLLBACK: &str = "\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[3J\x1b[H";

pub(in crate::frontend) async fn run(agent: Agent, catalog: UiCatalog) -> Result<FrontendExit> {
    let mut workspace_inventory = catalog.start_workspace_inventory();
    let mut workspace_inventory_pending = true;
    let frontend = agent.frontend().clone();
    let render = |event: &EventMsg| frontend.render(event);
    let model = agent.model().clone();
    let model_route = agent.model_route().to_string();
    let mut state = TuiState::new(&catalog, std::env::current_dir()?, model, model_route);
    let (sender, mut events) = agent.into_parts();
    let guard = TerminalGuard::alternate()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut tick = tokio::time::interval(INPUT_POLL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut shimmer = tokio::time::interval(SHIMMER_INTERVAL);
    shimmer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut elapsed = tokio::time::interval(ELAPSED_INTERVAL);
    elapsed.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut events_open = true;
    let mut dirty = true;
    let mut exit = FrontendExit::Exit;
    let mut clear_on_exit = false;

    'ui: loop {
        if dirty {
            io::stdout().sync_update(|_| -> Result<()> {
                terminal.draw(|frame| {
                    if state.preview.is_some() {
                        render_preview(frame, &mut state);
                    } else {
                        super::view::render(frame, &mut state, &catalog);
                    }
                })?;
                Ok(())
            })??;
            dirty = false;
        }
        tokio::select! {
            event = events.recv(), if events_open => {
                match event {
                    Some(event) => {
                        handle_event_batch(&mut state, &render, event, &mut events);
                        if let Some(session_id) = state.requested_resume.take() {
                            clear_on_exit = true;
                            exit = FrontendExit::Resume(session_id);
                            break 'ui;
                        }
                    }
                    None => {
                        events_open = false;
                        state.disconnected = true;
                        state.finish_turn();
                        state.push("agent disconnected · press q to exit", TranscriptTone::Error);
                    }
                }
                dirty = true;
            }
            _ = tick.tick() => {
                for _ in 0..MAX_INPUT_BATCH {
                    let Some(event) = poll_event()? else {
                        break;
                    };
                    dirty = true;
                    let action = match event {
                        TerminalEvent::Key(key) => state.handle_key(key, &catalog),
                        TerminalEvent::Paste(text) => {
                            if state.preview.is_none() && state.picker.is_none() {
                                state.insert_paste(&text);
                            }
                            UiAction::None
                        }
                        TerminalEvent::Resize(_, _) => UiAction::None,
                        TerminalEvent::Mouse(mouse) => {
                            state.handle_mouse(mouse);
                            UiAction::None
                        }
                        TerminalEvent::FocusGained
                        | TerminalEvent::FocusLost => UiAction::None,
                    };
                    match action {
                        UiAction::None => {}
                        UiAction::Exit => {
                            if let Some(turn_id) = state.active_turn.clone() {
                                let _ = sender.submit(Op::Interrupt { turn_id });
                            }
                            break 'ui;
                        }
                        UiAction::New(model_route) => {
                            exit = FrontendExit::New(model_route);
                            break 'ui;
                        }
                        UiAction::Clear(model_route) => {
                            clear_on_exit = true;
                            exit = FrontendExit::New(model_route);
                            break 'ui;
                        }
                        UiAction::Setup => {
                            exit = FrontendExit::Setup;
                            break 'ui;
                        }
                        UiAction::Submit(op) => {
                            if let Err(error) = sender.submit(op) {
                                state.push(error.to_string(), TranscriptTone::Error);
                            }
                        }
                    }
                }
            }
            _ = shimmer.tick(), if state.is_working() => {
                dirty = true;
            }
            _ = elapsed.tick(), if state.active_turn.is_some() => {
                dirty = true;
            }
            result = &mut workspace_inventory, if workspace_inventory_pending => {
                let _ = result;
                workspace_inventory_pending = false;
                state.reference_cache = None;
                dirty = true;
            }
        }
    }
    drop(terminal);
    drop(guard);
    if clear_on_exit {
        execute!(io::stdout(), Print(CLEAR_SCREEN_AND_SCROLLBACK))?;
    }
    Ok(exit)
}

fn handle_event_batch<R>(
    state: &mut TuiState,
    render: &R,
    first: Event,
    events: &mut mpsc::Receiver<Event>,
) where
    R: Fn(&EventMsg) -> Vec<FrontendBlock>,
{
    handle_event(state, render, first.msg);
    for _ in 1..MAX_EVENT_BATCH {
        if state.requested_resume.is_some() {
            break;
        }
        let Ok(event) = events.try_recv() else {
            break;
        };
        handle_event(state, render, event.msg);
    }
}
