use std::io;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::SynchronizedUpdate;
use ratatui::crossterm::event::Event as TerminalEvent;
use ratatui::crossterm::event::KeyEventKind;
use ratatui::crossterm::execute;
use ratatui::crossterm::style::Print;
use tokio::time::MissedTickBehavior;

use super::TranscriptTone;
use super::TuiState;
use super::events::handle_gateway_event;
use super::input::UiAction;
use super::view::render_preview;
use crate::frontend::FrontendExit;
use crate::frontend::catalog::UiCatalog;
use crate::frontend::gateway_actions::{PreparedAction, prepare, render_response};
use crate::frontend::terminal::{INPUT_POLL, MAX_INPUT_BATCH, TerminalGuard, poll_event};
use horus::backend::model::ModelInfo;
use horus::protocol::{Op, Submission};
use horus::{Error, Result};
use horus_gateway::client::{GatewayEvents, GatewaySender};
use horus_gateway::wire::{ClientMessage, ReadyPayload, ServerMessage};
use uuid::Uuid;

const ELAPSED_INTERVAL: Duration = Duration::from_secs(1);
const CLEAR_SCREEN_AND_SCROLLBACK: &str = "\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[3J\x1b[H";

pub(in crate::frontend) async fn run(
    sender: GatewaySender,
    mut events: GatewayEvents,
    mut ready: ReadyPayload,
    catalog: UiCatalog,
    local_gateway: bool,
) -> Result<(FrontendExit, GatewaySender, GatewayEvents)> {
    let mut workspace_inventory = catalog.start_workspace_inventory(local_gateway);
    let mut workspace_inventory_pending = true;
    let model = ModelInfo {
        model: ready.session.model.model.clone(),
        reasoning_effort: ready.session.model.reasoning_effort.clone(),
    };
    let model_route = ready.session.model.route.clone();
    let workspace_id = ready.workspace.id.clone();
    let mut state = TuiState::new(
        &catalog,
        catalog.workspace().to_path_buf(),
        model,
        model_route,
    );
    let guard = TerminalGuard::alternate()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut tick = tokio::time::interval(INPUT_POLL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
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
            event = events.next(), if events_open => {
                match event {
                    Ok(Some(frame)) => {
                        match frame.message {
                            ServerMessage::AgentEvent { event, blocks, history, preview, .. } => {
                                handle_gateway_event(
                                    &mut state,
                                    event.msg,
                                    blocks,
                                    history,
                                    preview,
                                );
                            }
                            ServerMessage::Ready { payload } => {
                                exit = FrontendExit::Reload(Box::new(payload));
                                break 'ui;
                            }
                            ServerMessage::ConfigChanged { snapshot } => ready.config = snapshot,
                            ServerMessage::Artifacts { artifacts, .. } => {
                                for artifact in artifacts {
                                    state.push(artifact.title, TranscriptTone::Neutral);
                                    state.apply_block(artifact.block);
                                }
                            }
                            message => {
                                if let Some(message) = render_response(&message) {
                                    state.push(message, TranscriptTone::Neutral);
                                }
                            }
                        }
                        if let Some(request) = state.requested_resume.take() {
                            if same_workspace(
                                Some(&workspace_id),
                                request.context.workspace_id.as_deref(),
                            ) {
                                clear_on_exit = true;
                                exit = FrontendExit::Resume(request.session_id);
                                break 'ui;
                            }
                            state.push(
                                "session belongs to another workspace · start Horus there to resume",
                                TranscriptTone::Warning,
                            );
                        }
                    }
                    Ok(None) => {
                        events_open = false;
                        state.disconnected = true;
                        state.finish_turn();
                        state.push("gateway disconnected · press q to exit", TranscriptTone::Error);
                    }
                    Err(error) => {
                        events_open = false;
                        state.disconnected = true;
                        state.finish_turn();
                        state.push(error.to_string(), TranscriptTone::Error);
                    }
                }
                dirty = true;
            }
            _ = tick.tick() => {
                for _ in 0..MAX_INPUT_BATCH {
                    let Some(event) = poll_event()? else {
                        break;
                    };
                    let action = match event {
                        TerminalEvent::Key(key) => {
                            dirty |= matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat);
                            state.handle_key(key, &catalog)
                        }
                        TerminalEvent::Paste(text) => {
                            if state.preview.is_none() && state.picker.is_none() {
                                let before = (state.input.len(), state.input_limit_reached);
                                state.insert_paste(&text);
                                dirty |=
                                    before != (state.input.len(), state.input_limit_reached);
                            }
                            UiAction::None
                        }
                        TerminalEvent::Resize(_, _) => {
                            dirty = true;
                            UiAction::None
                        }
                        TerminalEvent::Mouse(mouse) => {
                            dirty |= state.handle_mouse(mouse);
                            UiAction::None
                        }
                        TerminalEvent::FocusGained
                        | TerminalEvent::FocusLost => UiAction::None,
                    };
                    match action {
                        UiAction::None => {}
                        UiAction::Exit => {
                            if let Some(turn_id) = state.active_turn.clone() {
                                let _ = send_op(&sender, Op::Interrupt { turn_id }).await;
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
                        UiAction::Submit(op) => {
                            if let Err(error) = send_op(&sender, op).await {
                                state.push(error.to_string(), TranscriptTone::Error);
                            }
                        }
                        UiAction::Gateway(action) => match prepare(action, &ready) {
                            Ok(PreparedAction::Print(message)) => {
                                state.push(message, TranscriptTone::Neutral);
                            }
                            Ok(PreparedAction::Send { message, .. }) => {
                                if let Err(error) = sender.send(message).await {
                                    state.push(error.to_string(), TranscriptTone::Error);
                                }
                            }
                            Err(error) => state.push(error.to_string(), TranscriptTone::Error),
                        },
                    }
                }
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
    Ok((exit, sender, events))
}

async fn send_op(sender: &horus_gateway::client::GatewaySender, op: Op) -> Result<()> {
    sender
        .send(ClientMessage::Submit {
            submission: Submission {
                id: Uuid::new_v4().to_string(),
                op,
            },
        })
        .await
        .map_err(|error| Error::Stopped(error.to_string()))
}

fn same_workspace(current: Option<&str>, target: Option<&str>) -> bool {
    current.is_some() && current == target
}

#[cfg(test)]
mod tests {
    use super::same_workspace;

    #[test]
    fn resume_requires_matching_known_workspace_ids() {
        assert!(same_workspace(Some("workspace-a"), Some("workspace-a")));
        assert!(!same_workspace(Some("workspace-a"), Some("workspace-b")));
        assert!(!same_workspace(None, None));
    }
}
