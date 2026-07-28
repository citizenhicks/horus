//! Queued turn steering middleware.

use super::ActiveSubmissionContext;
use super::ActiveSubmissionResult;
use super::Middleware;
use super::ModelContext;
use super::TurnEndContext;
use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::backend::model::user_message;
use crate::protocol::EventMsg;
use crate::protocol::FrontendActiveInput;
use crate::protocol::FrontendContribution;
use crate::protocol::FrontendEvent;
use crate::protocol::FrontendSlot;
use crate::protocol::FrontendTone;
use crate::protocol::FrontendWidget;
use crate::protocol::UserMessageEvent;
use crate::truncate_utf8;

const DEFAULT_MAX_PENDING: usize = 64;
const OPERATION: &str = "steer";
const OPERATIONS: &[&str] = &[OPERATION];

/// Injects queued steering exactly once at the next model boundary.
pub struct Steering {
    max_pending: usize,
}

impl Default for Steering {
    fn default() -> Self {
        Self {
            max_pending: DEFAULT_MAX_PENDING,
        }
    }
}

impl Steering {
    /// Creates steering with a bounded pending-message queue.
    pub fn new(max_pending: usize) -> Result<Self> {
        if max_pending == 0 {
            return Err(Error::Config(
                "steering queue limit must be positive".into(),
            ));
        }
        Ok(Self { max_pending })
    }

    fn queued_widget(&self, messages: &[String]) -> FrontendEvent {
        let Some(message) = messages.last() else {
            return FrontendEvent::RemoveWidget {
                capability: self.name().into(),
                id: "queued".into(),
            };
        };
        let queued = if messages.len() == 1 {
            String::new()
        } else {
            format!(" +{}", messages.len() - 1)
        };
        FrontendEvent::Widget {
            capability: self.name().into(),
            item: FrontendWidget {
                id: "queued".into(),
                slot: FrontendSlot::ComposerHeader,
                text: format!("steering queued{queued} · {}", preview(message)),
                tone: FrontendTone::Neutral,
            },
        }
    }
}

impl Middleware for Steering {
    fn name(&self) -> &'static str {
        "steering"
    }

    fn active_operations(&self) -> &'static [&'static str] {
        OPERATIONS
    }

    fn frontend(&self) -> FrontendContribution {
        FrontendContribution {
            capability: self.name().into(),
            active_input: Some(FrontendActiveInput {
                operation: OPERATION.into(),
            }),
            ..FrontendContribution::default()
        }
    }

    fn active_submission(
        &self,
        context: &mut ActiveSubmissionContext<'_>,
    ) -> Result<ActiveSubmissionResult> {
        if context.operation != OPERATION {
            return Err(Error::Config("steering received another operation".into()));
        }
        if context.target_turn_id != context.active_turn_id {
            return Ok(ActiveSubmissionResult::Rejected(
                "steering targeted a stale turn".into(),
            ));
        }
        let pending = context
            .queued_before
            .saturating_add(context.queued_input.len());
        if pending >= self.max_pending {
            return Ok(ActiveSubmissionResult::Rejected(
                "steering queue is full".into(),
            ));
        }
        context.queued_input.push(context.text.to_string());
        context
            .events
            .push(EventMsg::Frontend(self.queued_widget(context.queued_input)));
        Ok(ActiveSubmissionResult::Accepted)
    }

    fn turn_ended(&self, context: &mut TurnEndContext<'_>) -> Result<()> {
        context
            .events
            .push(EventMsg::Frontend(self.queued_widget(&[])));
        Ok(())
    }

    fn before_model<'a>(&'a self, context: &'a mut ModelContext<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let queued = std::mem::take(context.queued_input);
            if !queued.is_empty() {
                *context.checkpoint_changed = true;
                context
                    .events
                    .push(EventMsg::Frontend(self.queued_widget(&[])));
            }
            for message in queued {
                let item = user_message(&message);
                context.events.push(EventMsg::UserMessage(UserMessageEvent {
                    message: message.clone(),
                }));
                context.push_input(item);
            }
            Ok(())
        })
    }
}

fn preview(message: &str) -> &str {
    truncate_utf8(message, 80)
}
