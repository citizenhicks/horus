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
use horus::protocol::FrontendEvent;
use horus::protocol::ModelStepContentPhase;
use horus::protocol::ModelStepOutcome;
use horus::protocol::Op;
use horus::protocol::RenderedBlock;
use horus::protocol::TokenUsageInfo;
use horus_gateway::wire::RecordedEvent;
use horus_gateway::wire::RenderedEvent;

impl TuiState {
    pub(super) fn handle_agent_event(&mut self, event: EventMsg, blocks: Vec<RenderedBlock>) {
        let is_commentary = matches!(
            &event,
            EventMsg::AgentMessageContentDelta(delta)
                if delta.phase == AgentMessagePhase::Commentary
        ) || matches!(
            &event,
            EventMsg::AgentMessage(message)
                if message.phase == AgentMessagePhase::Commentary
        );
        if !is_commentary {
            self.commit_commentary_stream();
        }
        let was_rendered = !blocks.is_empty();
        for block in blocks {
            self.apply_block(block);
        }
        match event {
            EventMsg::TurnStarted(turn) => {
                self.commit_stream();
                self.active_turn = Some(turn.turn_id);
                self.turn_started_at = Some(Instant::now());
                self.clear_approval();
            }
            EventMsg::UserMessage(message) => {
                self.remember_composer_input(message.message.clone());
                self.push(format!("› {}", message.message), TranscriptTone::User);
            }
            EventMsg::AgentMessageContentDelta(delta) => {
                self.remember_streamed_phase(
                    &delta.model_step_id,
                    match delta.phase {
                        AgentMessagePhase::Commentary => ModelStepContentPhase::Commentary,
                        AgentMessagePhase::FinalAnswer => ModelStepContentPhase::FinalAnswer,
                    },
                );
                self.append_stream(&delta.delta, delta.phase);
            }
            EventMsg::AgentReasoningContentDelta(delta) => {
                self.remember_streamed_phase(
                    &delta.model_step_id,
                    ModelStepContentPhase::Reasoning,
                );
                self.append_reasoning(&delta.delta);
            }
            EventMsg::ModelStepStarted(step) => {
                self.streamed_step_phases
                    .entry(step.model_step_id)
                    .or_default();
            }
            EventMsg::ModelStepCompleted(step) => {
                self.commit_reasoning();
                self.commit_stream();
                let streamed = self
                    .streamed_step_phases
                    .remove(&step.model_step_id)
                    .unwrap_or_default();
                if let ModelStepOutcome::Completed { content, .. } = step.outcome {
                    for item in content {
                        if item.text.is_empty() || streamed.contains(item.phase) {
                            continue;
                        }
                        let tone = match item.phase {
                            ModelStepContentPhase::Reasoning => TranscriptTone::Reasoning,
                            ModelStepContentPhase::Commentary
                            | ModelStepContentPhase::FinalAnswer => TranscriptTone::Assistant,
                        };
                        self.push(item.text, tone);
                    }
                }
                self.completed_model_steps.insert(step.model_step_id);
            }
            EventMsg::AgentMessage(message) => {
                if self.completed_model_steps.contains(&message.model_step_id) {
                    return;
                }
                if self.streaming_phase == Some(message.phase) {
                    self.streaming.clear();
                    self.streaming_phase = None;
                } else {
                    self.commit_stream();
                }
                if !was_rendered {
                    self.push(message.message, TranscriptTone::Assistant);
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
                self.requested_resume = Some(request);
            }
            // Presentation for these typed actions is part of the canonical block list.
            EventMsg::WebSearchBegin(_) | EventMsg::WebSearchEnd(_) => {}
            EventMsg::TurnComplete(_) => {
                self.finish_turn();
            }
            EventMsg::TurnAborted(turn) => {
                self.finish_turn();
                if !was_rendered {
                    self.push(
                        format!("turn aborted: {}", turn.reason),
                        TranscriptTone::Warning,
                    );
                }
            }
            EventMsg::Error(error) => {
                self.commit_stream();
                if !was_rendered {
                    self.push(format!("error: {}", error.message), TranscriptTone::Error);
                }
            }
            EventMsg::Warning(warning) => {
                if !was_rendered {
                    self.push(
                        format!("warning: {}", warning.message),
                        TranscriptTone::Warning,
                    );
                }
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
                // The gateway has already projected this event into `blocks`.
                FrontendEvent::Render { .. } => {}
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
                // Preview transcripts arrive as gateway-rendered records.
                // Nested previews are control events, not transcript content.
                FrontendEvent::Preview { .. } => {}
            },
            _ => {}
        }
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

pub(super) fn handle_gateway_event(state: &mut TuiState, record: RecordedEvent) {
    if let Some(preview) = record.preview {
        let mut replay = TuiState::default();
        for rendered in preview.events {
            apply_rendered_event(&mut replay, rendered);
        }
        replay.commit_reasoning();
        replay.commit_stream();
        state.preview = Some(PreviewState::new(
            preview.title,
            PreviewContent::Snapshot(replay.transcript),
        ));
    } else {
        state.handle_agent_event(record.event.msg, record.blocks);
    }
}

pub(super) fn handle_gateway_history(state: &mut TuiState, records: Vec<RecordedEvent>) {
    for record in records {
        handle_gateway_event(state, record);
    }
    state.commit_reasoning();
    state.commit_stream();
}

fn apply_rendered_event(state: &mut TuiState, rendered: RenderedEvent) {
    state.handle_agent_event(rendered.event, rendered.blocks);
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
