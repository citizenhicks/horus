use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::COMMAND_QUEUE_CAPACITY;
use super::MAX_MODEL_STEPS;
use super::Runner;
use super::input::ActiveRoute;
use super::input::ActiveTurnRouter;
use super::input::Wait;
use super::input::interruptible;
use super::try_send_event;
use super::unix_timestamp_ms;
use crate::Error;
use crate::Result;
use crate::backend::checkpoint::ActiveExecution;
use crate::backend::checkpoint::ExecutionOutcome;
use crate::backend::model::ModelEventSink;
use crate::backend::model::ModelRequest;
use crate::backend::model::tool_complete_boundaries;
use crate::backend::model::user_message_with_attachments;
use crate::backend::sandbox::SandboxAuthorization;
use crate::middleware::AfterModelContext;
use crate::middleware::ModelContext;
use crate::middleware::TurnEndContext;
use crate::protocol::AgentMessageEvent;
use crate::protocol::AgentMessagePhase;
use crate::protocol::ErrorEvent;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::MessageTarget;
use crate::protocol::Submission;
use crate::protocol::TokenCountEvent;
use crate::protocol::TokenUsageInfo;
use crate::protocol::TurnAbortedEvent;
use crate::protocol::TurnCompleteEvent;
use crate::protocol::TurnStartedEvent;
use crate::protocol::UserMessageEvent;

/// Outcome of `Runner::before_model_phase`.
enum BeforeModel {
    /// An interrupt aborted the turn; `continue_turn` returns.
    Aborted,
    /// Middleware queued input during the phase; re-run it before the model call.
    Repeat,
    /// Proceed to the model request.
    Ready(Vec<Value>),
}

impl Runner {
    pub(super) async fn ready_or_aborted<T>(
        &mut self,
        wait: Wait<T>,
        turn_id: &str,
    ) -> Result<Option<T>> {
        match wait {
            Wait::Ready(value) => Ok(Some(value)),
            Wait::Interrupted { submission_id } => {
                self.abort(
                    &submission_id,
                    turn_id,
                    "interrupted",
                    ExecutionOutcome::Aborted,
                )
                .await?;
                Ok(None)
            }
        }
    }

    pub(super) async fn fail_turn(&mut self, submission_id: &str, error: Error) -> Result<()> {
        let Some(turn_id) = self
            .state
            .active_execution
            .as_ref()
            .map(|execution| execution.turn_id.clone())
        else {
            return Err(error);
        };
        let message = error.to_string();
        self.emit(
            submission_id,
            EventMsg::Error(ErrorEvent {
                message: message.clone(),
            }),
        )
        .await?;
        self.abort(submission_id, &turn_id, &message, ExecutionOutcome::Failed)
            .await
    }

    pub(super) async fn start_turn(
        &mut self,
        commands: &mut mpsc::Receiver<Submission>,
        submission_id: String,
        message: String,
        attachments: Vec<crate::protocol::AttachmentReference>,
    ) -> Result<()> {
        let turn_id = Uuid::new_v4().to_string();
        if self.state.active_execution.is_some() {
            return Err(Error::Checkpoint(
                "cannot start a turn while another execution is active".into(),
            ));
        }
        self.state.active_execution = Some(ActiveExecution {
            submission_id: submission_id.clone(),
            turn_id: turn_id.clone(),
            started_at_ms: unix_timestamp_ms()?,
            model_calls: 0,
            tool_calls: 0,
            failed_tool_calls: 0,
            usage: crate::protocol::TokenUsage::default(),
        });
        self.emit(
            &submission_id,
            EventMsg::TurnStarted(TurnStartedEvent {
                turn_id: turn_id.clone(),
                model_context_window: Some(self.config.context_window),
            }),
        )
        .await?;
        if self.state.first_user_message.is_none() && !message.trim().is_empty() {
            self.state.first_user_message = Some(message.clone());
        }
        self.push_context(user_message_with_attachments(&message, &attachments));
        let batch_item_count = self.transcript_delta.len();
        let checkpoint_sequence = self.save().await?;
        self.emit(
            &submission_id,
            EventMsg::UserMessage(UserMessageEvent {
                message: message.clone(),
                attachments,
                message_target: Some(MessageTarget {
                    checkpoint_sequence,
                    batch_item_count,
                }),
            }),
        )
        .await?;
        self.continue_turn(commands, submission_id, turn_id).await
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
        let mut queued_during_middleware = Vec::new();
        let queued_before = self.state.pending_input.len();
        let mut checkpoint_changed = false;
        let mut request_input = self.state.context.clone();
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
            queued_input: &mut self.state.pending_input,
            last_usage: self.state.last_usage.as_ref(),
            tools: &self.catalog,
            events: &mut middleware_events,
            usage: &mut middleware_usage,
            checkpoint_changed: &mut checkpoint_changed,
        });
        let control = interruptible(
            commands,
            ActiveTurnRouter {
                middleware: &self.config.middleware,
                turn_id,
                queued_input: &mut queued_during_middleware,
                queued_before,
                deferred: &mut self.deferred,
                events: &self.events,
                expected_approval: None,
            },
            before_model,
        )
        .await?;
        let (hook_error, interrupted_by) = match control {
            Wait::Ready(Ok(())) => (None, None),
            Wait::Ready(Err(error)) => (Some(error), None),
            Wait::Interrupted { submission_id } => (None, Some(submission_id)),
        };
        let usage_changed = !middleware_usage.is_empty();
        if let Some(last_usage) = middleware_usage.last().cloned() {
            for usage in &middleware_usage {
                self.record_usage(usage)?;
            }
            self.state.last_usage = Some(last_usage);
        }
        checkpoint_changed |= usage_changed;
        if let Some(error) = hook_error {
            if checkpoint_changed {
                self.save().await?;
            }
            for event in middleware_events {
                self.emit(submission_id, event).await?;
            }
            if usage_changed {
                self.emit_usage(submission_id)?;
            }
            return Err(error);
        }
        if let Some(interrupt_submission_id) = interrupted_by {
            for event in middleware_events {
                self.emit(submission_id, event).await?;
            }
            if usage_changed {
                self.emit_usage(submission_id)?;
            }
            self.abort(
                &interrupt_submission_id,
                turn_id,
                "interrupted",
                ExecutionOutcome::Aborted,
            )
            .await?;
            return Ok(BeforeModel::Aborted);
        }
        if !queued_during_middleware.is_empty() {
            self.state
                .pending_input
                .append(&mut queued_during_middleware);
            self.save().await?;
            for event in middleware_events {
                self.emit(submission_id, event).await?;
            }
            if usage_changed {
                self.emit_usage(submission_id)?;
            }
            return Ok(BeforeModel::Repeat);
        }
        if !self.state.pending_input.is_empty() {
            return Err(Error::Config(
                "queued active input was not consumed by its middleware".into(),
            ));
        }
        if checkpoint_changed {
            self.save().await?;
        }
        for event in middleware_events {
            self.emit(submission_id, event).await?;
        }
        if usage_changed {
            self.emit_usage(submission_id)?;
        }
        Ok(BeforeModel::Ready(request_input))
    }

    pub(super) async fn continue_turn(
        &mut self,
        commands: &mut mpsc::Receiver<Submission>,
        submission_id: String,
        turn_id: String,
    ) -> Result<()> {
        let mut last_agent_message = None;
        for model_step in 0..MAX_MODEL_STEPS {
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
            let request_input = match self
                .before_model_phase(commands, &submission_id, &turn_id, model_step)
                .await?
            {
                BeforeModel::Aborted => return Ok(()),
                BeforeModel::Repeat => continue,
                BeforeModel::Ready(input) => input,
            };

            let item_id = Uuid::new_v4().to_string();
            let events = self.events.clone();
            let event_submission_id = submission_id.clone();
            let event_turn_id = turn_id.clone();
            let thread_id = self.state.session_id.clone();
            let stream: ModelEventSink = Arc::new(move |event| {
                let msg = event.into_event(&thread_id, &event_turn_id, &item_id);
                try_send_event(
                    &events,
                    Event {
                        submission_id: Some(event_submission_id.clone()),
                        msg,
                    },
                )
            });
            let tools = self.catalog.definitions();
            self.record_model_call()?;
            let response = self.config.model.respond(
                &self.config.provider,
                ModelRequest {
                    session_id: &self.state.session_id,
                    instructions: &self.system_prompt,
                    input: &request_input,
                    tools: &tools,
                    allow_hosted_tools: true,
                    allow_continuation: true,
                },
                stream,
            );
            let output = interruptible(
                commands,
                ActiveTurnRouter {
                    middleware: &self.config.middleware,
                    turn_id: &turn_id,
                    queued_input: &mut self.state.pending_input,
                    queued_before: 0,
                    deferred: &mut self.deferred,
                    events: &self.events,
                    expected_approval: None,
                },
                response,
            )
            .await?;
            let Some(output) = self.ready_or_aborted(output, &turn_id).await? else {
                return Ok(());
            };
            let output = output?;
            self.record_usage(&output.usage)?;
            self.state.last_usage = Some(output.usage.clone());
            let mut after_model_events = Vec::new();
            let after_model = self.config.middleware.after_model(AfterModelContext {
                provider: &self.config.provider,
                session_id: &self.config.session_id,
                session_context: &self.config.session_context,
                metadata: &self.config.metadata,
                turn_id: &turn_id,
                model_step,
                context_window: self.config.context_window,
                queued_input_count: self.state.pending_input.len(),
                output: &output,
                events: &mut after_model_events,
            });
            let after_model = interruptible(
                commands,
                ActiveTurnRouter {
                    middleware: &self.config.middleware,
                    turn_id: &turn_id,
                    queued_input: &mut self.state.pending_input,
                    queued_before: 0,
                    deferred: &mut self.deferred,
                    events: &self.events,
                    expected_approval: None,
                },
                after_model,
            )
            .await?;
            let Some(after_model) = self.ready_or_aborted(after_model, &turn_id).await? else {
                return Ok(());
            };
            after_model?;
            for event in after_model_events {
                self.emit(&submission_id, event).await?;
            }
            let context_before = self.state.context.len();
            let batch_before = self.transcript_delta.len();
            let message_index = output.output.iter().rposition(has_visible_output_text);
            self.extend_context(output.output);
            let message_boundary = message_index.map(|index| context_before + index + 1);
            let message_is_safe = message_boundary.is_some_and(|boundary| {
                tool_complete_boundaries(&self.state.context)
                    .binary_search(&boundary)
                    .is_ok()
            });
            self.state.pending_tools.clone_from(&output.tool_calls);
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
            let checkpoint_sequence = if complete {
                self.finish_and_save_execution(ExecutionOutcome::Completed)
                    .await?
            } else {
                self.save().await?
            };
            self.emit_usage(&submission_id)?;
            if !output.text.is_empty() {
                last_agent_message = Some(output.text.clone());
                self.emit(
                    &submission_id,
                    EventMsg::AgentMessage(AgentMessageEvent {
                        message: output.text,
                        phase: Some(AgentMessagePhase::FinalAnswer),
                        message_target: message_index.filter(|_| message_is_safe).map(|index| {
                            MessageTarget {
                                checkpoint_sequence,
                                batch_item_count: batch_before + index + 1,
                            }
                        }),
                    }),
                )
                .await?;
            }
            if no_tools {
                if !complete {
                    continue;
                }
                self.emit_turn_ended(&submission_id, &turn_id).await?;
                self.emit(
                    submission_id,
                    EventMsg::TurnComplete(TurnCompleteEvent {
                        turn_id,
                        last_agent_message,
                    }),
                )
                .await?;
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
            self.append_tool_results(results)?;
            self.state.pending_approval = None;
            self.save().await?;
        }
        Err(Error::Stopped(format!(
            "model exceeded {MAX_MODEL_STEPS} tool steps"
        )))
    }

    async fn drain_commands(
        &mut self,
        commands: &mut mpsc::Receiver<Submission>,
        turn_id: &str,
    ) -> Result<Option<String>> {
        for _ in 0..COMMAND_QUEUE_CAPACITY {
            let Ok(submission) = commands.try_recv() else {
                break;
            };
            if let ActiveRoute::Interrupted { submission_id } = (ActiveTurnRouter {
                middleware: &self.config.middleware,
                turn_id,
                queued_input: &mut self.state.pending_input,
                queued_before: 0,
                deferred: &mut self.deferred,
                events: &self.events,
                expected_approval: None,
            })
            .route(submission)
            .await?
            {
                return Ok(Some(submission_id));
            }
        }
        Ok(None)
    }

    pub(super) fn emit_usage(&self, submission_id: &str) -> Result<()> {
        let Some(last) = self.state.last_usage.clone() else {
            return Ok(());
        };
        try_send_event(
            &self.events,
            Event {
                submission_id: Some(submission_id.to_string()),
                msg: EventMsg::TokenCount(TokenCountEvent {
                    info: Some(TokenUsageInfo {
                        total_token_usage: self.state.total_usage.clone(),
                        last_token_usage: last,
                        model_context_window: Some(self.config.context_window),
                    }),
                    rate_limits: None,
                }),
            },
        )
    }

    pub(super) async fn abort(
        &mut self,
        submission_id: &str,
        turn_id: &str,
        reason: &str,
        outcome: ExecutionOutcome,
    ) -> Result<()> {
        self.finish_pending_tools(submission_id, turn_id, reason)
            .await?;
        self.state.pending_input.clear();
        self.state.pending_approval = None;
        self.finish_and_save_execution(outcome).await?;
        self.emit_turn_ended(submission_id, turn_id).await?;
        self.emit(
            submission_id,
            EventMsg::TurnAborted(TurnAbortedEvent {
                turn_id: turn_id.to_string(),
                reason: reason.to_string(),
            }),
        )
        .await
    }

    async fn emit_turn_ended(&self, submission_id: &str, turn_id: &str) -> Result<()> {
        let mut events = Vec::new();
        self.config.middleware.turn_ended(TurnEndContext {
            session_id: &self.config.session_id,
            turn_id,
            events: &mut events,
        })?;
        for event in events {
            self.emit(submission_id, event).await?;
        }
        Ok(())
    }
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
