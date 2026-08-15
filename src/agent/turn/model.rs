use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::turn_event;
use crate::agent::input::{ActiveRoute, ActiveTurnRouter, Wait};
use crate::agent::{EventRecorder, Runner, send_event, try_send_event, unix_timestamp_ms};
use crate::backend::checkpoint::{ActiveModelStep, Checkpoint, ContextRewrite, ExecutionOutcome};
use crate::backend::model::{
    ModelEventSink, ModelRequest, PromptCacheIdentity, STREAM_RETRY_LIMIT, prompt_cache_key,
    tool_complete_boundaries,
};
use crate::backend::sandbox::SandboxAuthorization;
use crate::middleware::{ModelContext, QueuedInputBaseline, QueuedInputQueue};
use crate::protocol::{
    AgentMessageEvent, AgentMessagePhase, Event, EventMsg, MessageTarget, ModelStepCompletedEvent,
    ModelStepDiagnostics, ModelStepOutcome, ModelStepStartedEvent, Submission, WebSearchAction,
    WebSearchEndEvent,
};
use crate::{Error, Result};

const STREAM_RETRY_BASE_DELAY_MS: u64 = 200;
const STREAM_RETRY_MAX_DELAY_MS: u64 = 3_200;

/// Outcome of `Runner::before_model_phase`.
enum BeforeModel {
    /// An interrupt aborted the turn; `continue_turn` returns.
    Aborted,
    /// Middleware queued input during the phase; re-run it before the model call.
    Repeat(Vec<crate::backend::checkpoint::ContextRewriteReason>),
    /// Proceed to the model request.
    Ready {
        input: Vec<Value>,
        rewrite_reasons: Vec<crate::backend::checkpoint::ContextRewriteReason>,
    },
}

impl Runner {
    async fn persist_before_model_changes(
        &mut self,
        submission_id: &str,
        mut middleware_events: Vec<EventMsg>,
        usage_changed: bool,
        checkpoint_changed: bool,
        provisional_target_sequence: u64,
    ) -> Result<()> {
        if checkpoint_changed {
            let durable_sequence = self
                .state
                .sequence
                .checked_add(1)
                .ok_or_else(|| Error::Checkpoint("checkpoint sequence overflow".into()))?;
            rebase_live_message_targets(
                &mut middleware_events,
                provisional_target_sequence,
                durable_sequence,
            );
        }
        let mut events = middleware_events
            .into_iter()
            .map(|message| turn_event(submission_id, message))
            .collect::<Vec<_>>();
        if usage_changed && let Some(usage) = self.usage_event(submission_id) {
            events.push(usage);
        }
        if checkpoint_changed {
            self.persist_with_events(events, None).await?;
        } else {
            for event in events {
                send_event(&self.events, event).await?;
            }
        }
        Ok(())
    }

    /// Runs `before_model` middleware with interruption routing, folds usage and
    /// events into state, and persists when anything changed.
    async fn before_model_phase(
        &mut self,
        commands: &mut mpsc::Receiver<Submission>,
        submission_id: &str,
        turn_id: &str,
        model_step: usize,
    ) -> Result<BeforeModel> {
        let mut middleware_events = Vec::new();
        let mut middleware_usage = Vec::new();
        let provisional_target_sequence = self
            .state
            .sequence
            .checked_add(1)
            .ok_or_else(|| Error::Checkpoint("checkpoint sequence overflow".into()))?;
        let queued_before = QueuedInputBaseline::from_items(&self.state.pending_input);
        let had_queued_input = !self.state.pending_input.is_empty();
        let mut durable_snapshot = self.state.clone();
        let original_pending_count = durable_snapshot.pending_input.len();
        let recorder = self.events.clone();
        let active_events = self.events.clone();
        let mut checkpoint_changed = false;
        let mut rewrite_reasons = Vec::new();
        let mut request_input = self.state.context.clone();
        let (control, mut queued_during_middleware, queue_changed) = {
            let mut queued_during_middleware = Vec::new();
            let mut queue_changed = false;
            let before_model = self.config.middleware.before_model(ModelContext {
                model: &self.config.model,
                provider: &self.config.provider,
                session_id: &self.config.session_id,
                session_context: &self.config.session_context,
                metadata: &self.config.metadata,
                turn_id,
                model_step,
                context_window: self.config.context_window,
                instructions: &self.system_prompt,
                checkpoint_sequence: self.state.sequence,
                request_input: &mut request_input,
                durable_input: &mut self.state.context,
                transcript_delta: &mut self.transcript_delta,
                context_epoch: &mut self.state.context_epoch,
                compaction_count: &mut self.state.compaction_count,
                rewrite_reasons: &mut rewrite_reasons,
                queued_input: QueuedInputQueue::new(
                    &mut self.state.pending_input,
                    QueuedInputBaseline::default(),
                ),
                last_usage: self.state.last_usage.as_ref(),
                tools: &self.catalog,
                events: &mut middleware_events,
                usage: &mut middleware_usage,
                checkpoint_changed: &mut checkpoint_changed,
            });
            tokio::pin!(before_model);
            let control = loop {
                tokio::select! {
                    output = &mut before_model => break Wait::Ready(output),
                    submission = commands.recv() => {
                        let Some(submission) = submission else {
                            return Err(Error::Stopped("frontend disconnected".into()));
                        };
                        let route = (ActiveTurnRouter {
                            middleware: &self.config.middleware,
                            session_id: &self.config.session_id,
                            metadata: &self.config.metadata,
                            turn_id,
                            queued_input: &mut queued_during_middleware,
                            queued_before: queued_before.clone(),
                            deferred: &mut self.deferred,
                            events: &active_events,
                            expected_approval: None,
                        })
                        .route(submission)
                        .await?;
                        match route {
                            ActiveRoute::Accepted(change) | ActiveRoute::Changed(change) => {
                                durable_snapshot.pending_input.truncate(original_pending_count);
                                durable_snapshot
                                    .pending_input
                                    .extend(queued_during_middleware.iter().cloned());
                                persist_queue_snapshot(
                                    &recorder,
                                    &mut durable_snapshot,
                                    change.into_events(),
                                )
                                .await?;
                                queue_changed = true;
                            }
                            ActiveRoute::Interrupted { submission_id } => {
                                break Wait::Interrupted { submission_id };
                            }
                            ActiveRoute::Continue | ActiveRoute::Approval { .. } => {}
                        }
                    }
                }
            };
            (control, queued_during_middleware, queue_changed)
        };
        self.state.sequence = durable_snapshot.sequence;
        self.state
            .pending_input
            .append(&mut queued_during_middleware);
        let (hook_error, interrupted_by) = match control {
            Wait::Ready(Ok(())) => (None, None),
            Wait::Ready(Err(error)) => (Some(error), None),
            Wait::Interrupted { submission_id } => (None, Some(submission_id)),
        };
        let usage_changed = !middleware_usage.is_empty();
        if !rewrite_reasons.is_empty() {
            self.state.last_context_rewrite = Some(ContextRewrite {
                epoch: self.state.context_epoch,
                reasons: rewrite_reasons.clone(),
            });
        }
        if usage_changed {
            let route = self.config.provider.clone();
            for usage in &middleware_usage {
                self.record_usage(&route, usage)?;
                self.state.last_usage = Some(usage.clone());
            }
        }
        checkpoint_changed |= usage_changed || had_queued_input || queue_changed;
        if let Some(error) = hook_error {
            self.persist_before_model_changes(
                submission_id,
                middleware_events,
                usage_changed,
                checkpoint_changed,
                provisional_target_sequence,
            )
            .await?;
            return Err(error);
        }
        if let Some(interrupt_submission_id) = interrupted_by {
            self.persist_before_model_changes(
                submission_id,
                middleware_events,
                usage_changed,
                checkpoint_changed,
                provisional_target_sequence,
            )
            .await?;
            self.abort(
                &interrupt_submission_id,
                turn_id,
                "interrupted",
                ExecutionOutcome::Aborted,
            )
            .await?;
            return Ok(BeforeModel::Aborted);
        }
        if queue_changed {
            self.persist_before_model_changes(
                submission_id,
                middleware_events,
                usage_changed,
                true,
                provisional_target_sequence,
            )
            .await?;
            return Ok(BeforeModel::Repeat(rewrite_reasons));
        }
        if !self.state.pending_input.is_empty() {
            return Err(Error::Config(
                "queued active input was not consumed by its middleware".into(),
            ));
        }
        self.persist_before_model_changes(
            submission_id,
            middleware_events,
            usage_changed,
            checkpoint_changed,
            provisional_target_sequence,
        )
        .await?;
        Ok(BeforeModel::Ready {
            input: request_input,
            rewrite_reasons,
        })
    }

    async fn fail_model_step(
        &mut self,
        submission_id: &str,
        started: &ModelStepStartedEvent,
        outcome: ModelStepOutcome,
        pending_searches: &Mutex<Vec<String>>,
    ) -> Result<()> {
        self.state.active_model_step = None;
        // A failed step can never complete the hosted searches it started, so the
        // backend closes them out instead of leaving every frontend to infer it.
        let dangling = pending_searches
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default();
        let mut events: Vec<Event> = dangling
            .into_iter()
            .map(|call_id| {
                turn_event(
                    submission_id,
                    EventMsg::WebSearchEnd(WebSearchEndEvent {
                        session_id: started.session_id.clone(),
                        turn_id: started.turn_id.clone(),
                        model_step_id: started.model_step_id.clone(),
                        call_id,
                        action: WebSearchAction::Interrupted,
                    }),
                )
            })
            .collect();
        events.push(model_step_completed_event(
            submission_id,
            started,
            outcome,
            None,
        )?);
        self.persist_with_events(events, None).await?;
        Ok(())
    }

    pub(in crate::agent) async fn continue_turn(
        &mut self,
        commands: &mut mpsc::Receiver<Submission>,
        submission_id: String,
        turn_id: String,
    ) -> Result<()> {
        let mut model_step = 0;
        loop {
            if let Some(interrupt_submission_id) = self.drain_commands(commands, &turn_id).await? {
                self.abort(
                    &interrupt_submission_id,
                    &turn_id,
                    "interrupted",
                    ExecutionOutcome::Aborted,
                )
                .await?;
                return Ok(());
            }
            if model_step >= self.config.max_model_steps {
                return Err(Error::Stopped(format!(
                    "turn reached the configured limit of {} model steps",
                    self.config.max_model_steps
                )));
            }
            let mut rewrite_reasons = Vec::new();
            let request_input = loop {
                match self
                    .before_model_phase(commands, &submission_id, &turn_id, model_step)
                    .await?
                {
                    BeforeModel::Aborted => return Ok(()),
                    BeforeModel::Repeat(reasons) => {
                        extend_rewrite_reasons(&mut rewrite_reasons, reasons);
                    }
                    BeforeModel::Ready {
                        input,
                        rewrite_reasons: reasons,
                    } => {
                        extend_rewrite_reasons(&mut rewrite_reasons, reasons);
                        break input;
                    }
                }
            };

            let tools = self.catalog.definitions();
            let model = Arc::clone(&self.config.model);
            let provider = self.config.provider.clone();
            let model_session_id = self.state.session_id.clone();
            let cache_key = prompt_cache_key(&model_session_id);
            let instructions = Arc::clone(&self.system_prompt);
            let mut stream_retries = 0;
            let (model_step_started, output, pending_searches) = loop {
                let model_step_started = ModelStepStartedEvent {
                    session_id: self.state.session_id.clone(),
                    turn_id: turn_id.clone(),
                    model_step_id: Uuid::new_v4().to_string(),
                    step_index: model_step,
                    started_at_ms: unix_timestamp_ms()?,
                };
                self.record_model_call()?;
                self.state.active_model_step = Some(ActiveModelStep {
                    model_step_id: model_step_started.model_step_id.clone(),
                    step_index: model_step_started.step_index,
                    started_at_ms: model_step_started.started_at_ms,
                });
                self.persist_with_events(
                    vec![turn_event(
                        &submission_id,
                        EventMsg::ModelStepStarted(model_step_started.clone()),
                    )],
                    None,
                )
                .await?;
                let events = self.events.clone();
                let event_submission_id = submission_id.clone();
                let event_turn_id = turn_id.clone();
                let event_session_id = self.state.session_id.clone();
                let event_model_step_id = model_step_started.model_step_id.clone();
                let pending_searches = Arc::new(Mutex::new(Vec::<String>::new()));
                let tracked_searches = Arc::clone(&pending_searches);
                let stream: ModelEventSink = Arc::new(move |event| {
                    match &event {
                        crate::protocol::ModelEvent::WebSearchStarted { call_id } => {
                            if let Ok(mut pending) = tracked_searches.lock() {
                                pending.push(call_id.clone());
                            }
                        }
                        crate::protocol::ModelEvent::WebSearchCompleted { call_id, .. } => {
                            if let Ok(mut pending) = tracked_searches.lock() {
                                pending.retain(|open| open != call_id);
                            }
                        }
                        _ => {}
                    }
                    let msg =
                        event.into_event(&event_session_id, &event_turn_id, &event_model_step_id);
                    try_send_event(
                        &events,
                        Event {
                            submission_id: Some(event_submission_id.clone()),
                            msg,
                        },
                    )
                });
                let response = model.respond(
                    &provider,
                    ModelRequest {
                        session_id: &model_session_id,
                        prompt_cache: Some(PromptCacheIdentity {
                            key: &cache_key,
                            context_epoch: self.state.context_epoch,
                        }),
                        instructions: &instructions,
                        input: &request_input,
                        tools: &tools,
                        allow_hosted_tools: true,
                        allow_continuation: true,
                    },
                    stream,
                );
                match self.wait_active(commands, &turn_id, response).await {
                    Ok(Wait::Ready(Ok(output))) => {
                        break (model_step_started, output, pending_searches);
                    }
                    Ok(Wait::Ready(Err(Error::Provider(error))))
                        if error.is_stream_interrupted() && stream_retries < STREAM_RETRY_LIMIT =>
                    {
                        let delay = stream_retry_delay(
                            &error,
                            stream_retries,
                            &model_step_started.model_step_id,
                        );
                        self.fail_model_step(
                            &submission_id,
                            &model_step_started,
                            ModelStepOutcome::Retrying,
                            &pending_searches,
                        )
                        .await?;
                        stream_retries += 1;
                        match self
                            .wait_active(commands, &turn_id, tokio::time::sleep(delay))
                            .await?
                        {
                            Wait::Ready(()) => {
                                if let Some(interrupt_submission_id) =
                                    self.drain_commands(commands, &turn_id).await?
                                {
                                    self.abort(
                                        &interrupt_submission_id,
                                        &turn_id,
                                        "interrupted",
                                        ExecutionOutcome::Aborted,
                                    )
                                    .await?;
                                    return Ok(());
                                }
                            }
                            Wait::Interrupted {
                                submission_id: interrupt_submission_id,
                            } => {
                                self.abort(
                                    &interrupt_submission_id,
                                    &turn_id,
                                    "interrupted",
                                    ExecutionOutcome::Aborted,
                                )
                                .await?;
                                return Ok(());
                            }
                        }
                    }
                    Ok(Wait::Ready(Err(error))) | Err(error) => {
                        self.fail_model_step(
                            &submission_id,
                            &model_step_started,
                            ModelStepOutcome::Failed,
                            &pending_searches,
                        )
                        .await?;
                        return Err(error);
                    }
                    Ok(Wait::Interrupted {
                        submission_id: interrupt_submission_id,
                    }) => {
                        self.fail_model_step(
                            &submission_id,
                            &model_step_started,
                            ModelStepOutcome::Interrupted,
                            &pending_searches,
                        )
                        .await?;
                        self.abort(
                            &interrupt_submission_id,
                            &turn_id,
                            "interrupted",
                            ExecutionOutcome::Aborted,
                        )
                        .await?;
                        return Ok(());
                    }
                }
            };
            if let Err(error) = self.record_usage(&provider, &output.usage) {
                self.fail_model_step(
                    &submission_id,
                    &model_step_started,
                    ModelStepOutcome::Failed,
                    &pending_searches,
                )
                .await?;
                return Err(error);
            }
            self.state.last_usage = Some(output.usage.clone());
            let context_before = self.state.context.len();
            let batch_before = self.transcript_delta.len();
            let message_index = output.output.iter().rposition(has_visible_output_text);
            self.extend_context(output.output.clone());
            let message_boundary = message_index.map(|index| context_before + index + 1);
            let message_is_safe = message_boundary.is_some_and(|boundary| {
                tool_complete_boundaries(&self.state.context)
                    .binary_search(&boundary)
                    .is_ok()
            });
            self.state.pending_tools.clone_from(&output.tool_calls);
            self.state.active_model_step = None;
            let diagnostics = model.model_step_diagnostics(
                &provider,
                self.state.context_epoch,
                rewrite_reasons
                    .iter()
                    .map(|reason| reason.as_str().into())
                    .collect(),
                &output.usage,
            )?;
            let checkpoint_sequence = self
                .state
                .sequence
                .checked_add(1)
                .ok_or_else(|| Error::Checkpoint("checkpoint sequence overflow".into()))?;
            let mut model_events = vec![model_step_completed_event(
                &submission_id,
                &model_step_started,
                ModelStepOutcome::Completed {
                    end_turn: output.end_turn,
                    tool_call_ids: output
                        .tool_calls
                        .iter()
                        .map(|call| call.call_id.clone())
                        .collect(),
                    usage: output.usage.clone(),
                    content: output.content().to_vec(),
                },
                Some(diagnostics),
            )?];
            if !output.text.is_empty() {
                model_events.push(turn_event(
                    &submission_id,
                    EventMsg::AgentMessage(AgentMessageEvent {
                        session_id: self.state.session_id.clone(),
                        turn_id: turn_id.clone(),
                        model_step_id: model_step_started.model_step_id.clone(),
                        message: output.text.clone(),
                        phase: AgentMessagePhase::FinalAnswer,
                        message_target: message_index.filter(|_| message_is_safe).map(|index| {
                            MessageTarget {
                                checkpoint_sequence,
                                batch_item_count: batch_before + index + 1,
                            }
                        }),
                    }),
                ));
            }
            if let Some(usage) = self.usage_event(&submission_id) {
                model_events.push(usage);
            }
            self.persist_with_events(model_events, None).await?;
            model_step += 1;
            let no_tools = output.tool_calls.is_empty();
            let complete = if no_tools && output.end_turn {
                if let Some(interrupt_submission_id) =
                    self.drain_commands(commands, &turn_id).await?
                {
                    self.abort(
                        &interrupt_submission_id,
                        &turn_id,
                        "interrupted",
                        ExecutionOutcome::Aborted,
                    )
                    .await?;
                    return Ok(());
                }
                self.state.pending_input.is_empty()
            } else {
                false
            };
            if no_tools {
                if !complete {
                    continue;
                }
                self.complete_turn(&submission_id, &turn_id).await?;
                return Ok(());
            }

            let mutation_call_ids = output
                .tool_calls
                .iter()
                .filter(|call| self.catalog.requires_approval(&call.name))
                .map(|call| call.call_id.clone())
                .collect::<Vec<_>>();
            let authorization = self.config.sandbox.authorize(
                &self.config.session_id,
                &output.tool_calls,
                &mutation_call_ids,
            )?;
            let results = match authorization {
                SandboxAuthorization::Execute(permissions) => {
                    let tools = self
                        .execute_tools(
                            commands,
                            &submission_id,
                            &turn_id,
                            &output.tool_calls,
                            permissions,
                        )
                        .await?;
                    let Some(results) = self.ready_or_aborted(tools, &turn_id).await? else {
                        return Ok(());
                    };
                    results
                }
                SandboxAuthorization::Approval {
                    request,
                    permissions,
                } => {
                    let Some(results) = self
                        .pause_and_resolve(
                            commands,
                            &submission_id,
                            &turn_id,
                            output.tool_calls,
                            request,
                            permissions,
                            Vec::new(),
                        )
                        .await?
                    else {
                        return Ok(());
                    };
                    results
                }
                SandboxAuthorization::Review(review) => {
                    let Some(results) = self
                        .review_and_resolve(
                            commands,
                            &submission_id,
                            &turn_id,
                            output.tool_calls,
                            review,
                        )
                        .await?
                    else {
                        return Ok(());
                    };
                    results
                }
            };
            self.state.pending_approval = None;
            self.persist_tool_results(&submission_id, &turn_id, results)
                .await?;
        }
    }
}

fn model_step_completed_event(
    submission_id: &str,
    started: &ModelStepStartedEvent,
    outcome: ModelStepOutcome,
    diagnostics: Option<ModelStepDiagnostics>,
) -> Result<Event> {
    Ok(turn_event(
        submission_id,
        EventMsg::ModelStepCompleted(ModelStepCompletedEvent {
            session_id: started.session_id.clone(),
            turn_id: started.turn_id.clone(),
            model_step_id: started.model_step_id.clone(),
            step_index: started.step_index,
            started_at_ms: started.started_at_ms,
            completed_at_ms: unix_timestamp_ms()?.max(started.started_at_ms),
            outcome,
            diagnostics,
        }),
    ))
}

fn extend_rewrite_reasons(
    collected: &mut Vec<crate::backend::checkpoint::ContextRewriteReason>,
    additional: Vec<crate::backend::checkpoint::ContextRewriteReason>,
) {
    for reason in additional {
        if !collected.contains(&reason) {
            collected.push(reason);
        }
    }
}

fn stream_retry_delay(error: &crate::ProviderError, retry: usize, model_step_id: &str) -> Duration {
    let exponential_ms = STREAM_RETRY_BASE_DELAY_MS
        .saturating_mul(1_u64 << retry.min(4))
        .min(STREAM_RETRY_MAX_DELAY_MS);
    let jitter = model_step_id.bytes().fold(retry as u64, |value, byte| {
        value.wrapping_mul(16_777_619).wrapping_add(u64::from(byte))
    });
    let jitter_percent = 80 + jitter % 41;
    let backoff = Duration::from_millis(exponential_ms.saturating_mul(jitter_percent) / 100);
    error
        .retry_after()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .map_or(backoff, |retry_after| retry_after.max(backoff))
}

fn rebase_live_message_targets(events: &mut [EventMsg], provisional: u64, durable: u64) {
    for target in events.iter_mut().filter_map(|event| match event {
        EventMsg::UserMessage(message) => message.message_target.as_mut(),
        EventMsg::AgentMessage(message) => message.message_target.as_mut(),
        _ => None,
    }) {
        if target.checkpoint_sequence == provisional {
            target.checkpoint_sequence = durable;
        }
    }
}

async fn persist_queue_snapshot(
    recorder: &EventRecorder,
    checkpoint: &mut Checkpoint,
    events: Vec<Event>,
) -> Result<()> {
    let previous_sequence = checkpoint.sequence;
    checkpoint.sequence = checkpoint
        .sequence
        .checked_add(1)
        .ok_or_else(|| Error::Checkpoint("checkpoint sequence overflow".into()))?;
    if let Err(error) = recorder.save(checkpoint, &[], None, events).await {
        checkpoint.sequence = previous_sequence;
        return Err(error);
    }
    Ok(())
}

fn has_visible_output_text(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) == Some("assistant")
        && item.get("phase").and_then(Value::as_str) != Some("commentary")
        && item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|part| {
                part.get("type").and_then(Value::as_str) == Some("output_text")
                    && part
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty())
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_retry_delay_respects_server_seconds() {
        let error = crate::ProviderError::stream_interrupted(Some("30".into()));

        assert_eq!(
            stream_retry_delay(&error, 0, "step-1"),
            Duration::from_secs(30)
        );
    }
}
