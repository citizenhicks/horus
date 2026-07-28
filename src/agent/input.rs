use std::collections::VecDeque;
use std::future::Future;

use tokio::sync::mpsc;

use crate::Error;
use crate::Result;
use crate::middleware::ActiveSubmissionContext;
use crate::middleware::ActiveSubmissionResult;
use crate::middleware::MiddlewareStack;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::Op;
use crate::protocol::ReviewDecision;
use crate::protocol::Submission;
use crate::protocol::WarningEvent;

use super::MAX_DEFERRED_SUBMISSIONS;
use super::send_event;

pub(super) enum Wait<T> {
    Ready(T),
    Interrupted { submission_id: String },
}

pub(super) enum ActiveRoute {
    Continue,
    Accepted,
    Interrupted {
        submission_id: String,
    },
    Approval {
        submission_id: String,
        decision: ReviewDecision,
    },
}

pub(super) struct ActiveTurnRouter<'a> {
    pub middleware: &'a MiddlewareStack,
    pub turn_id: &'a str,
    pub queued_input: &'a mut Vec<String>,
    pub queued_before: usize,
    pub deferred: &'a mut VecDeque<Submission>,
    pub events: &'a mpsc::Sender<Event>,
    pub expected_approval: Option<&'a str>,
}

pub(super) async fn interruptible<F, T>(
    commands: &mut mpsc::Receiver<Submission>,
    mut active: ActiveTurnRouter<'_>,
    future: F,
) -> Result<Wait<T>>
where
    F: Future<Output = T>,
{
    tokio::pin!(future);
    loop {
        tokio::select! {
            output = &mut future => return Ok(Wait::Ready(output)),
            submission = commands.recv() => {
                let Some(submission) = submission else {
                    return Err(Error::Stopped("frontend disconnected".into()));
                };
                if let ActiveRoute::Interrupted { submission_id } =
                    active.route(submission).await?
                {
                    return Ok(Wait::Interrupted { submission_id });
                }
            }
        }
    }
}

impl ActiveTurnRouter<'_> {
    pub async fn route(&mut self, submission: Submission) -> Result<ActiveRoute> {
        let Submission { id, op } = submission;
        match op {
            Op::UserInput { text } => {
                defer_submission(
                    self.deferred,
                    self.events,
                    Submission {
                        id,
                        op: Op::UserInput { text },
                    },
                )
                .await?;
                Ok(ActiveRoute::Continue)
            }
            Op::Interrupt { turn_id } if turn_id == self.turn_id => {
                Ok(ActiveRoute::Interrupted { submission_id: id })
            }
            Op::Interrupt { .. } => {
                warn(self.events, id, "interrupt targeted a stale turn").await?;
                Ok(ActiveRoute::Continue)
            }
            Op::ExecApproval {
                id: approval_id,
                decision,
            } if self.expected_approval == Some(approval_id.as_str()) => {
                Ok(ActiveRoute::Approval {
                    submission_id: id,
                    decision,
                })
            }
            Op::ExecApproval { .. } => {
                warn(
                    self.events,
                    id,
                    "approval response targeted a stale request",
                )
                .await?;
                Ok(ActiveRoute::Continue)
            }
            op
            @ (Op::CapabilityCommand { .. } | Op::SetModel { .. } | Op::ResumeSession { .. }) => {
                defer_submission(self.deferred, self.events, Submission { id, op }).await?;
                Ok(ActiveRoute::Continue)
            }
            Op::ActiveInput {
                operation,
                turn_id,
                text,
            } => {
                let mut messages = Vec::new();
                let result = self
                    .middleware
                    .active_submission(&mut ActiveSubmissionContext {
                        operation: &operation,
                        active_turn_id: self.turn_id,
                        target_turn_id: &turn_id,
                        text: &text,
                        queued_input: self.queued_input,
                        queued_before: self.queued_before,
                        events: &mut messages,
                    })?;
                for msg in messages {
                    send_event(
                        self.events,
                        Event {
                            submission_id: Some(id.clone()),
                            msg,
                        },
                    )
                    .await?;
                }
                match result {
                    Some(ActiveSubmissionResult::Accepted) => Ok(ActiveRoute::Accepted),
                    Some(ActiveSubmissionResult::Rejected(message)) => {
                        warn(self.events, id, &message).await?;
                        Ok(ActiveRoute::Continue)
                    }
                    None => {
                        warn(
                            self.events,
                            id,
                            "active operation middleware is not installed",
                        )
                        .await?;
                        Ok(ActiveRoute::Continue)
                    }
                }
            }
        }
    }
}

pub(super) async fn defer_submission(
    deferred: &mut VecDeque<Submission>,
    events: &mpsc::Sender<Event>,
    submission: Submission,
) -> Result<()> {
    if deferred.len() >= MAX_DEFERRED_SUBMISSIONS {
        warn(events, submission.id, "deferred command queue is full").await?;
        return Ok(());
    }
    deferred.push_back(submission);
    Ok(())
}

async fn warn(events: &mpsc::Sender<Event>, id: String, message: &str) -> Result<()> {
    send_event(
        events,
        Event {
            submission_id: Some(id),
            msg: EventMsg::Warning(WarningEvent {
                message: message.into(),
            }),
        },
    )
    .await
}
