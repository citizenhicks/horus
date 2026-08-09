//! Queued turn steering middleware.

use super::ActiveCommandContext;
use super::ActiveSubmissionContext;
use super::ActiveSubmissionResult;
use super::Middleware;
use super::ModelContext;
use super::QueuedInputView;
use super::RuntimeContext;
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
use crate::protocol::MAX_CAPABILITY_INPUT_BYTES;
use crate::protocol::Op;
use crate::protocol::UserMessageEvent;

mod text {
    include!(concat!(env!("OUT_DIR"), "/src_middleware_steering_text.rs"));
}

const MAX_PENDING_MESSAGES: usize = 1_024;
const _: () = {
    assert!(text::DEFAULTS_MAX_PENDING >= 1);
    assert!(text::DEFAULTS_MAX_PENDING <= MAX_PENDING_MESSAGES as i64);
    assert!(text::SETTING_MAX_PENDING_STEP > 0);
};
/// Default number of queued steering messages retained during a turn.
pub const DEFAULT_MAX_PENDING: usize = text::DEFAULTS_MAX_PENDING as usize;
const SETTINGS: &[MiddlewareSettingManifest] = &[MiddlewareSettingManifest::Integer {
    id: "max_pending",
    label: text::SETTING_MAX_PENDING_LABEL,
    description: text::SETTING_MAX_PENDING_DESCRIPTION,
    min: 1,
    max: Some(MAX_PENDING_MESSAGES as i64),
    step: text::SETTING_MAX_PENDING_STEP,
    default: DEFAULT_MAX_PENDING as i64,
}];

/// Configuration and presentation metadata for turn steering.
pub const MANIFEST: MiddlewareManifest = MiddlewareManifest {
    id: "steering",
    label: text::MANIFEST_LABEL,
    description: text::MANIFEST_DESCRIPTION,
    required: false,
    default_enabled: true,
    settings: SETTINGS,
};
const OPERATION: &str = "steer";
const EDIT_COMMAND: &str = "edit";
const STALE_EDIT: &str = "steering message is no longer queued";
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

    fn queued_widget(&self, message: Option<QueuedInputView<'_>>) -> FrontendEvent {
        let Some(message) = message else {
            return FrontendEvent::RemoveWidget {
                capability: self.name().into(),
                id: "queued".into(),
            };
        };
        FrontendEvent::Widget {
            capability: self.name().into(),
            item: FrontendWidget {
                id: "queued".into(),
                slot: FrontendSlot::TranscriptTail,
                text: message.text().into(),
                tone: FrontendTone::Neutral,
                symbol: None,
                icon_only: false,
                progress: None,
                content: None,
                action: Some(Op::CapabilityCommand {
                    capability: self.name().into(),
                    command: EDIT_COMMAND.into(),
                    arguments: message.id().into(),
                    input: Some(message.text().into()),
                    target: None,
                }),
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
        if context.text.len() > MAX_CAPABILITY_INPUT_BYTES {
            return Ok(ActiveSubmissionResult::Rejected(
                "steering message exceeds editable size limit".into(),
            ));
        }
        if context.queued_input.count() >= self.max_pending {
            return Ok(ActiveSubmissionResult::Rejected(
                "steering queue is full".into(),
            ));
        }
        if !context
            .queued_input
            .enqueue(context.submission_id, context.text)?
        {
            return Ok(ActiveSubmissionResult::Rejected(
                "steering message could not be queued".into(),
            ));
        }
        context.events.push(EventMsg::Frontend(
            self.queued_widget(context.queued_input.latest()),
        ));
        Ok(ActiveSubmissionResult::Accepted)
    }

    fn active_command(
        &self,
        context: &mut ActiveCommandContext<'_>,
    ) -> Result<Option<ActiveSubmissionResult>> {
        if context.command != EDIT_COMMAND {
            return Ok(None);
        }
        if context
            .queued_input
            .take_latest(context.arguments)?
            .is_none()
        {
            return Ok(Some(ActiveSubmissionResult::Rejected(STALE_EDIT.into())));
        }
        context.events.push(EventMsg::Frontend(
            self.queued_widget(context.queued_input.latest()),
        ));
        Ok(Some(ActiveSubmissionResult::Accepted))
    }

    fn turn_ended(&self, context: &mut TurnEndContext<'_>) -> Result<()> {
        context
            .events
            .push(EventMsg::Frontend(self.queued_widget(None)));
        Ok(())
    }

    fn initialize<'a>(&'a self, context: RuntimeContext) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if let Some(message) = context.queued_input.latest() {
                (context.frontend)(self.queued_widget(Some(message)))?;
            }
            Ok(())
        })
    }

    fn before_model<'a>(&'a self, context: &'a mut ModelContext<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let queued = context.queued_input.drain();
            if !queued.is_empty() {
                context
                    .events
                    .push(EventMsg::Frontend(self.queued_widget(None)));
            }
            for message in queued {
                let message = message.into_text();
                let item = user_message(&message);
                let message_target = context.push_input(item);
                context.events.push(EventMsg::UserMessage(UserMessageEvent {
                    message,
                    attachments: Vec::new(),
                    message_target: Some(message_target),
                }));
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::Mutex;

    use super::*;
    use crate::backend::checkpoint::CheckpointStore;
    use crate::backend::checkpoint::sqlite::SqliteCheckpoint;
    use crate::middleware::DurableQueuedInput;
    use crate::middleware::MiddlewareStack;
    use crate::middleware::QueuedInputBaseline;
    use crate::middleware::QueuedInputQueue;
    use crate::middleware::QueuedInputSnapshot;
    use crate::protocol::SessionContext;

    fn item(id: &str, text: &str) -> DurableQueuedInput {
        DurableQueuedInput::new(MANIFEST.id, id, text).expect("valid queued input")
    }

    fn queue(items: &mut Vec<DurableQueuedInput>) -> QueuedInputQueue<'_> {
        let mut queue = QueuedInputQueue::new(items, QueuedInputBaseline::default());
        queue.scope(MANIFEST.id);
        queue
    }

    #[test]
    fn steering_rejects_queue_sizes_outside_its_manifest_bounds() {
        assert!(Steering::new(0).is_err());
        assert!(Steering::new(MAX_PENDING_MESSAGES + 1).is_err());
    }

    #[test]
    fn edit_action_is_not_a_catalog_command() {
        assert!(Steering::default().frontend().commands.is_empty());
    }

    #[tokio::test]
    async fn initialize_restores_the_latest_owned_queue_widget() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(
            SqliteCheckpoint::new(temporary.path().join("checkpoints.sqlite3"))
                .expect("checkpoint store"),
        );
        let frontend_events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&frontend_events);
        let runtime = RuntimeContext {
            checkpoints,
            session_id: "session".into(),
            model_route: "model".into(),
            session_context: SessionContext::default(),
            metadata: BTreeMap::new(),
            queued_input: QueuedInputSnapshot::default(),
            frontend: Arc::new(move |event| {
                sink_events.lock().expect("frontend events").push(event);
                Ok(())
            }),
        };
        let pending = vec![
            DurableQueuedInput::new("other", "private", "hidden").expect("other item"),
            item("steering-1", "older"),
            item("steering-2", "latest"),
        ];
        let stack = MiddlewareStack::new(vec![Arc::new(Steering::default())]).expect("stack");

        stack
            .initialize(runtime, &pending)
            .await
            .expect("initialize middleware");

        let events = frontend_events.lock().expect("frontend events");
        let [FrontendEvent::Widget { item, .. }] = events.as_slice() else {
            panic!("expected one restored widget");
        };
        assert_eq!(item.text, "latest");
        assert!(matches!(
            &item.action,
            Some(Op::CapabilityCommand { arguments, .. }) if arguments == "steering-2"
        ));
    }

    #[test]
    fn active_submission_rejects_text_above_the_editable_limit() {
        let steering = Steering::default();
        let mut queued = Vec::new();
        let mut events = Vec::new();
        let text = "x".repeat(MAX_CAPABILITY_INPUT_BYTES + 1);

        let result = steering
            .active_submission(&mut ActiveSubmissionContext {
                submission_id: "steering-1",
                operation: OPERATION,
                active_turn_id: "turn-1",
                target_turn_id: "turn-1",
                text: &text,
                queued_input: queue(&mut queued),
                events: &mut events,
            })
            .expect("active submission");

        assert_eq!(
            result,
            ActiveSubmissionResult::Rejected("steering message exceeds editable size limit".into())
        );
        assert!(queued.is_empty());
        assert!(events.is_empty());
    }

    #[test]
    fn active_submission_queues_its_id_and_exact_edit_widget() {
        let steering = Steering::default();
        let mut queued = Vec::new();
        let mut events = Vec::new();
        let text = "keep this exact\nincluding the second line";

        let result = steering
            .active_submission(&mut ActiveSubmissionContext {
                submission_id: "steering-1",
                operation: OPERATION,
                active_turn_id: "turn-1",
                target_turn_id: "turn-1",
                text,
                queued_input: queue(&mut queued),
                events: &mut events,
            })
            .expect("active submission");

        assert_eq!(result, ActiveSubmissionResult::Accepted);
        assert_eq!(queued, vec![item("steering-1", text)]);
        let [EventMsg::Frontend(FrontendEvent::Widget { capability, item })] = events.as_slice()
        else {
            panic!("expected queued widget");
        };
        assert_eq!(capability, MANIFEST.id);
        assert_eq!(item.id, "queued");
        assert_eq!(item.slot, FrontendSlot::TranscriptTail);
        assert_eq!(item.text, text);
        assert_eq!(
            item.action,
            Some(Op::CapabilityCommand {
                capability: MANIFEST.id.into(),
                command: EDIT_COMMAND.into(),
                arguments: "steering-1".into(),
                input: Some(text.into()),
                target: None,
            })
        );
    }

    #[test]
    fn replayed_active_submission_is_rejected_without_ending_the_session() {
        let steering = Steering::default();
        let mut queued = vec![item("steering-1", "original")];
        let original = queued.clone();
        let mut events = Vec::new();

        let result = steering
            .active_submission(&mut ActiveSubmissionContext {
                submission_id: "steering-1",
                operation: OPERATION,
                active_turn_id: "turn-1",
                target_turn_id: "turn-1",
                text: "replayed",
                queued_input: queue(&mut queued),
                events: &mut events,
            })
            .expect("active submission");

        assert_eq!(
            result,
            ActiveSubmissionResult::Rejected("steering message could not be queued".into())
        );
        assert_eq!(queued, original);
        assert!(events.is_empty());
    }

    #[test]
    fn active_command_takes_only_the_latest_queued_message() {
        let steering = Steering::default();
        let mut queued = vec![item("steering-1", "older"), item("steering-2", "latest")];
        let mut events = Vec::new();

        let result = steering
            .active_command(&mut ActiveCommandContext {
                submission_id: "edit-1",
                active_turn_id: "turn-1",
                command: EDIT_COMMAND,
                arguments: "steering-2",
                input: Some("latest"),
                target: None,
                queued_input: queue(&mut queued),
                events: &mut events,
            })
            .expect("active command");

        assert_eq!(result, Some(ActiveSubmissionResult::Accepted));
        assert_eq!(queued, vec![item("steering-1", "older")]);
        let [
            EventMsg::Frontend(FrontendEvent::Widget {
                capability, item, ..
            }),
        ] = events.as_slice()
        else {
            panic!("expected prior widget");
        };
        assert_eq!(
            (capability.as_str(), item.text.as_str()),
            (MANIFEST.id, "older")
        );
    }

    #[test]
    fn active_command_rejects_a_stale_id_without_mutation() {
        let steering = Steering::default();
        let mut queued = vec![item("steering-2", "latest")];
        let original = queued.clone();
        let mut events = Vec::new();

        let result = steering
            .active_command(&mut ActiveCommandContext {
                submission_id: "edit-1",
                active_turn_id: "turn-1",
                command: EDIT_COMMAND,
                arguments: "steering-1",
                input: Some("latest"),
                target: None,
                queued_input: queue(&mut queued),
                events: &mut events,
            })
            .expect("active command");

        assert_eq!(
            result,
            Some(ActiveSubmissionResult::Rejected(STALE_EDIT.into()))
        );
        assert_eq!(queued, original);
        assert!(events.is_empty());
    }

    #[test]
    fn second_edit_from_the_same_widget_loses_the_revision_race() {
        let steering = Steering::default();
        let mut queued = vec![item("steering-1", "original")];
        let mut first_events = Vec::new();
        let first = steering
            .active_command(&mut ActiveCommandContext {
                submission_id: "edit-1",
                active_turn_id: "turn-1",
                command: EDIT_COMMAND,
                arguments: "steering-1",
                input: Some("original"),
                target: None,
                queued_input: queue(&mut queued),
                events: &mut first_events,
            })
            .expect("first edit");
        let mut stale_events = Vec::new();
        let stale = steering
            .active_command(&mut ActiveCommandContext {
                submission_id: "edit-2",
                active_turn_id: "turn-1",
                command: EDIT_COMMAND,
                arguments: "steering-1",
                input: Some("original"),
                target: None,
                queued_input: queue(&mut queued),
                events: &mut stale_events,
            })
            .expect("stale edit");

        assert_eq!(first, Some(ActiveSubmissionResult::Accepted));
        assert_eq!(
            stale,
            Some(ActiveSubmissionResult::Rejected(STALE_EDIT.into()))
        );
        assert!(queued.is_empty());
        assert!(matches!(
            first_events.as_slice(),
            [EventMsg::Frontend(FrontendEvent::RemoveWidget { .. })]
        ));
        assert!(stale_events.is_empty());
    }

    #[test]
    fn active_command_rejects_an_already_consumed_message() {
        let steering = Steering::default();
        let mut queued = Vec::new();
        let mut events = Vec::new();

        let result = steering
            .active_command(&mut ActiveCommandContext {
                submission_id: "edit-1",
                active_turn_id: "turn-1",
                command: EDIT_COMMAND,
                arguments: "steering-1",
                input: Some("too late"),
                target: None,
                queued_input: queue(&mut queued),
                events: &mut events,
            })
            .expect("active command");

        assert_eq!(
            result,
            Some(ActiveSubmissionResult::Rejected(STALE_EDIT.into()))
        );
        assert!(events.is_empty());
    }
}
