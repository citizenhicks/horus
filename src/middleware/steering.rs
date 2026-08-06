//! Queued turn steering middleware.

use super::ActiveSubmissionContext;
use super::ActiveSubmissionResult;
use super::Middleware;
use super::ModelContext;
use super::TurnEndContext;
use super::manifest::{MiddlewareManifest, MiddlewareSettingManifest};
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

/// Default number of queued steering messages retained during a turn.
pub const DEFAULT_MAX_PENDING: usize = 64;
const MAX_PENDING_MESSAGES: usize = 1_024;
const SETTINGS: &[MiddlewareSettingManifest] = &[MiddlewareSettingManifest::Integer {
    id: "max_pending",
    label: "Maximum pending messages",
    description: "Maximum steering messages queued during an active turn",
    min: 1,
    max: Some(MAX_PENDING_MESSAGES as i64),
    step: 1,
    default: DEFAULT_MAX_PENDING as i64,
}];

/// Configuration and presentation metadata for turn steering.
pub const MANIFEST: MiddlewareManifest = MiddlewareManifest {
    id: "steering",
    label: "Steering",
    description: "Accept guidance during an active turn",
    required: false,
    default_enabled: true,
    settings: SETTINGS,
};
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
        if max_pending == 0 || max_pending > MAX_PENDING_MESSAGES {
            return Err(Error::Config(format!(
                "steering queue limit must be between 1 and {MAX_PENDING_MESSAGES}"
            )));
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
                symbol: None,
                icon_only: false,
                progress: None,
                content: None,
                action: None,
            },
        }
    }
}

impl Middleware for Steering {
    fn name(&self) -> &'static str {
        MANIFEST.id
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
                context
                    .events
                    .push(EventMsg::Frontend(self.queued_widget(&[])));
            }
            for message in queued {
                let item = user_message(&message);
                let message_target = context.push_input(item);
                context.events.push(EventMsg::UserMessage(UserMessageEvent {
                    message: message.clone(),
                    message_target: Some(message_target),
                }));
            }
            Ok(())
        })
    }
}

fn preview(message: &str) -> &str {
    truncate_utf8(message, 80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steering_rejects_queue_sizes_outside_its_manifest_bounds() {
        assert!(Steering::new(0).is_err());
        assert!(Steering::new(MAX_PENDING_MESSAGES + 1).is_err());
    }
}
