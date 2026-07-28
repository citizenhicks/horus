use std::time::Instant;

use super::MAX_ENTRY_BYTES;
use super::PickerState;
use super::PreviewContent;
use super::PreviewState;
use super::TranscriptTone;
use super::TuiState;
use super::view::bounded_terminal_text;
use super::view::terminal_text;
use horus::protocol::AgentMessagePhase;
use horus::protocol::EventMsg;
use horus::protocol::FrontendBlock;
use horus::protocol::FrontendEvent;
use horus::protocol::Op;
use horus::protocol::TokenUsageInfo;

impl TuiState {
    pub(super) fn handle_agent_event(&mut self, event: EventMsg, blocks: Vec<FrontendBlock>) {
        let was_rendered = !blocks.is_empty();
        for block in blocks {
            self.apply_block(block);
        }
        match event {
            EventMsg::TurnStarted(turn) => {
                self.commit_stream();
                self.status_message.clear();
                self.active_turn = Some(turn.turn_id);
                self.turn_started_at = Some(Instant::now());
                self.clear_approval();
            }
            EventMsg::UserMessage(message) => {
                self.push(format!("› {}", message.message), TranscriptTone::User);
            }
            EventMsg::AgentMessageContentDelta(delta) => {
                if delta.phase == Some(AgentMessagePhase::Commentary) {
                    self.append_status(&delta.delta);
                } else {
                    self.status_message.clear();
                    self.append_stream(&delta.delta);
                }
            }
            EventMsg::AgentReasoningContentDelta(delta) => {
                self.append_reasoning(&delta.delta);
            }
            EventMsg::AgentMessage(message) => {
                if message.phase == Some(AgentMessagePhase::Commentary) {
                    self.status_message = super::bounded_status(&message.message);
                } else {
                    self.status_message.clear();
                    self.streaming.clear();
                    if !was_rendered {
                        self.push(message.message, TranscriptTone::Assistant);
                    }
                }
            }
            EventMsg::ContextCompacted => {}
            EventMsg::ExecApprovalRequest(request) => {
                self.active_turn = Some(request.turn_id);
                self.turn_started_at.get_or_insert_with(Instant::now);
                self.begin_approval(request.id);
            }
            EventMsg::TokenCount(tokens) => {
                if let Some(info) = tokens.info {
                    self.usage = usage_status(&info);
                }
            }
            EventMsg::ModelChanged(changed) => {
                self.model_route = changed.route;
                self.model.model = terminal_text(&changed.model);
                self.model.reasoning_effort = changed
                    .reasoning_effort
                    .map(|effort| terminal_text(&effort));
                self.usage.context_remaining = None;
            }
            EventMsg::SessionResumeRequested(request) => {
                self.requested_resume = Some(request.session_id);
            }
            EventMsg::WebSearchBegin(_) => {
                self.push("◉ searching the web", TranscriptTone::Warning);
            }
            EventMsg::WebSearchEnd(search) => {
                self.push(
                    format!("  searched: {}", search.query.unwrap_or(search.action)),
                    TranscriptTone::Success,
                );
            }
            EventMsg::TurnComplete(_) => {
                self.finish_turn();
            }
            EventMsg::TurnAborted(turn) => {
                self.finish_turn();
                self.push(
                    format!("turn aborted: {}", turn.reason),
                    TranscriptTone::Warning,
                );
            }
            EventMsg::Error(error) => {
                self.commit_stream();
                self.push(format!("error: {}", error.message), TranscriptTone::Error);
            }
            EventMsg::Warning(warning) => {
                self.push(
                    format!("warning: {}", warning.message),
                    TranscriptTone::Warning,
                );
            }
            EventMsg::Frontend(update) => match update {
                FrontendEvent::Widget {
                    capability,
                    mut item,
                } => {
                    item.text = bounded_terminal_text(&item.text, MAX_ENTRY_BYTES);
                    self.widgets.insert((capability, item.id.clone()), item);
                }
                FrontendEvent::RemoveWidget { capability, id } => {
                    self.widgets.remove(&(capability, id));
                }
                FrontendEvent::Render { capability, block } => {
                    self.apply_block(block.namespaced(&capability));
                }
                FrontendEvent::Picker { title, options } => {
                    let selected = options
                        .iter()
                        .position(|option| {
                            matches!(
                                &option.op,
                                Op::SetModel { route } if route == &self.model_route
                            )
                        })
                        .or_else(|| {
                            let group = self
                                .model_choices
                                .iter()
                                .find(|choice| choice.route == self.model_route)?
                                .group
                                .as_str();
                            options.iter().position(|option| {
                                let Op::SetModel { route } = &option.op else {
                                    return false;
                                };
                                self.model_choices
                                    .iter()
                                    .any(|choice| choice.route == *route && choice.group == group)
                            })
                        })
                        .unwrap_or_default();
                    self.picker = Some(PickerState {
                        title: terminal_text(&title),
                        options,
                        selected,
                    });
                }
                // Preview events are replayed with middleware renderers at the
                // frontend boundary. Nested previews are control events, not
                // transcript content.
                FrontendEvent::Preview { .. } => {}
            },
            _ => {}
        }
    }

    fn open_preview<R>(&mut self, title: String, events: Vec<EventMsg>, render: &R)
    where
        R: Fn(&EventMsg) -> Vec<FrontendBlock>,
    {
        let mut replay = Self::default();
        replay_preview_events(&mut replay, render, events);
        replay.commit_reasoning();
        replay.commit_stream();
        self.preview = Some(PreviewState::new(
            title,
            PreviewContent::Snapshot(replay.transcript),
        ));
    }
}

fn percentage(part: i64, whole: i64) -> Option<f64> {
    if whole <= 0 {
        return None;
    }
    Some((100.0 * part.max(0) as f64 / whole as f64).clamp(0.0, 100.0))
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct UsageStatus {
    pub(super) context_remaining: Option<f64>,
    pub(super) cache_hit: Option<f64>,
}

impl UsageStatus {
    pub(super) fn label(self) -> String {
        format!(
            "context {} · cache {}",
            self.context_remaining
                .map_or_else(|| "—".into(), |value| format!("{value:.1}%")),
            self.cache_hit
                .map_or_else(|| "—".into(), |value| format!("{value:.1}%"))
        )
    }
}

pub(super) fn handle_event<R>(state: &mut TuiState, render: &R, event: EventMsg)
where
    R: Fn(&EventMsg) -> Vec<FrontendBlock>,
{
    match event {
        EventMsg::Frontend(FrontendEvent::Preview { title, events }) => {
            state.open_preview(title, events, render);
        }
        EventMsg::SessionHistory(history) => {
            for event in history.events {
                if let EventMsg::UserMessage(message) = &event {
                    state.remember_composer_input(message.message.clone());
                }
                handle_event(state, render, event);
            }
            state.commit_reasoning();
            state.commit_stream();
        }
        event => {
            let blocks = render(&event);
            state.handle_agent_event(event, blocks);
        }
    }
}

fn replay_preview_events<R>(state: &mut TuiState, render: &R, events: Vec<EventMsg>)
where
    R: Fn(&EventMsg) -> Vec<FrontendBlock>,
{
    for event in events {
        match event {
            EventMsg::SessionHistory(history) => {
                replay_preview_events(state, render, history.events);
            }
            EventMsg::Frontend(
                FrontendEvent::Widget { .. }
                | FrontendEvent::RemoveWidget { .. }
                | FrontendEvent::Picker { .. }
                | FrontendEvent::Preview { .. },
            ) => {}
            event => {
                let blocks = render(&event);
                state.handle_agent_event(event, blocks);
            }
        }
    }
}

pub(super) fn usage_status(info: &TokenUsageInfo) -> UsageStatus {
    let input = info.last_token_usage.input_tokens.max(0);
    let used = info
        .last_token_usage
        .total_tokens
        .max(input.saturating_add(info.last_token_usage.output_tokens.max(0)));
    let window = info.model_context_window.unwrap_or_default().max(0);
    UsageStatus {
        context_remaining: percentage(window.saturating_sub(used), window),
        cache_hit: percentage(info.last_token_usage.cached_input_tokens, input),
    }
}
