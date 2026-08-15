mod model;

use tokio::sync::mpsc;
use uuid::Uuid;

use super::COMMAND_QUEUE_CAPACITY;
use super::Runner;
use super::input::{ActiveRoute, ActiveTurnRouter, Wait};
use super::unix_timestamp_ms;
use crate::backend::checkpoint::{ActiveExecution, ExecutionOutcome, QueuedInput};
use crate::backend::model::{
    has_prompt_cache_breakpoint, mark_prompt_cache_breakpoint, user_message_with_attachments,
};
use crate::middleware::{QueuedInputBaseline, TurnEndContext};
use crate::protocol::{
    ErrorEvent, Event, EventMsg, MessageTarget, Submission, TokenCountEvent, TokenUsageInfo,
    TurnAbortedEvent, TurnCompleteEvent, TurnStartedEvent, UserMessageEvent,
};
use crate::{Error, Result};

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
        let event = ErrorEvent::from_error(&error);
        let message = event.message.clone();
        self.abort_with_events(
            submission_id,
            &turn_id,
            &message,
            ExecutionOutcome::Failed,
            vec![turn_event(submission_id, EventMsg::Error(event))],
        )
        .await
    }

    pub(super) async fn start_turn(
        &mut self,
        commands: &mut mpsc::Receiver<Submission>,
        submission_id: String,
        message: String,
        attachments: Vec<crate::protocol::SessionFileReference>,
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
        if self.state.first_user_message.is_none() && !message.trim().is_empty() {
            self.state.first_user_message = Some(message.clone());
        }
        let mut user_message = user_message_with_attachments(&message, &attachments);
        if !has_prompt_cache_breakpoint(&self.state.context) {
            let _ = mark_prompt_cache_breakpoint(&mut user_message);
        }
        self.push_context(user_message);
        let batch_item_count = self.transcript_delta.len();
        let checkpoint_sequence = self
            .state
            .sequence
            .checked_add(1)
            .ok_or_else(|| Error::Checkpoint("checkpoint sequence overflow".into()))?;
        self.persist_with_events(
            vec![
                turn_event(
                    &submission_id,
                    EventMsg::TurnStarted(TurnStartedEvent {
                        turn_id: turn_id.clone(),
                        model_context_window: Some(self.config.context_window),
                    }),
                ),
                turn_event(
                    &submission_id,
                    EventMsg::UserMessage(UserMessageEvent {
                        message: message.clone(),
                        attachments,
                        message_target: Some(MessageTarget {
                            checkpoint_sequence,
                            batch_item_count,
                        }),
                    }),
                ),
            ],
            None,
        )
        .await?;
        self.continue_turn(commands, submission_id, turn_id).await
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
            let route = (ActiveTurnRouter {
                middleware: &self.config.middleware,
                session_id: &self.config.session_id,
                metadata: &self.config.metadata,
                turn_id,
                queued_input: &mut self.state.pending_input,
                queued_before: QueuedInputBaseline::default(),
                deferred: &mut self.deferred,
                events: &self.events,
                expected_approval: None,
            })
            .route(submission)
            .await?;
            match route {
                ActiveRoute::Accepted(change) | ActiveRoute::Changed(change) => {
                    self.persist_active_change(change).await?;
                }
                ActiveRoute::Interrupted { submission_id } => {
                    return Ok(Some(submission_id));
                }
                ActiveRoute::Continue | ActiveRoute::Approval { .. } => {}
            }
        }
        Ok(None)
    }

    pub(super) fn usage_event(&self, submission_id: &str) -> Option<Event> {
        let last = self.state.last_usage.clone()?;
        Some(turn_event(
            submission_id,
            EventMsg::TokenCount(TokenCountEvent {
                info: Some(TokenUsageInfo {
                    total_token_usage: self.state.total_usage.clone(),
                    last_token_usage: last,
                    model_context_window: Some(self.config.context_window),
                }),
                rate_limits: None,
            }),
        ))
    }

    async fn complete_turn(&mut self, submission_id: &str, turn_id: &str) -> Result<()> {
        let mut events =
            self.turn_ended_events(submission_id, turn_id, &self.state.pending_input)?;
        events.push(turn_event(
            submission_id,
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: turn_id.to_string(),
            }),
        ));
        self.finish_and_persist_execution(ExecutionOutcome::Completed, events)
            .await?;
        Ok(())
    }

    pub(super) async fn abort(
        &mut self,
        submission_id: &str,
        turn_id: &str,
        reason: &str,
        outcome: ExecutionOutcome,
    ) -> Result<()> {
        self.abort_with_events(submission_id, turn_id, reason, outcome, Vec::new())
            .await
    }

    async fn abort_with_events(
        &mut self,
        submission_id: &str,
        turn_id: &str,
        reason: &str,
        outcome: ExecutionOutcome,
        mut events: Vec<Event>,
    ) -> Result<()> {
        self.finish_pending_tools(submission_id, turn_id, reason)
            .await?;
        let pending_input = std::mem::take(&mut self.state.pending_input);
        self.state.pending_approval = None;
        events.extend(self.turn_ended_events(submission_id, turn_id, &pending_input)?);
        events.push(turn_event(
            submission_id,
            EventMsg::TurnAborted(TurnAbortedEvent {
                turn_id: turn_id.to_string(),
                reason: reason.to_string(),
            }),
        ));
        self.finish_and_persist_execution(outcome, events).await?;
        Ok(())
    }

    fn turn_ended_events(
        &self,
        submission_id: &str,
        turn_id: &str,
        queued_input: &[QueuedInput],
    ) -> Result<Vec<Event>> {
        let mut messages = Vec::new();
        self.config.middleware.turn_ended(TurnEndContext {
            session_id: &self.config.session_id,
            turn_id,
            queued_input,
            owner: None,
            events: &mut messages,
        })?;
        Ok(messages
            .into_iter()
            .map(|message| turn_event(submission_id, message))
            .collect())
    }
}

fn turn_event(submission_id: &str, msg: EventMsg) -> Event {
    Event {
        submission_id: Some(submission_id.to_string()),
        msg,
    }
}
