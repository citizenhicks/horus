//! The small event protocol shared by agent frontends.

use serde::Deserialize;
use serde::Serialize;

pub(crate) use self::replay::{
    INTERNAL_MESSAGE_FIELD, REPLAY_REASONING_FIELD, TOOL_ERROR_FIELD, events as replay_events,
    internal_message_kind, is_internal_message,
};

mod replay;

/// Maximum total UTF-8 bytes accepted in one user-input submission.
pub const MAX_USER_INPUT_BYTES: usize = 1024 * 1024;

/// A command submitted by a frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    /// Correlates all events produced by this command.
    pub id: String,
    /// Command payload.
    pub op: Op,
}

/// Frontend-visible context for the session owner, workspace, and origin.
///
/// These values are correlation metadata, not authentication or authorization.
/// A remote host must derive them after authentication and inject tenant-scoped
/// backends when it creates the agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionContext {
    /// Opaque tenant or organization identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Opaque identifier for the user who owns the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Optional display label, such as the local operating-system user name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    /// Opaque workspace identifier; this is not a filesystem path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Optional frontend-facing workspace label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_label: Option<String>,
    /// Optional label describing what created the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_label: Option<String>,
}

/// Commands supported by the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Op {
    /// Start a user turn.
    UserInput { text: String },
    /// Submit capability-owned input while a turn is active.
    ActiveInput {
        operation: String,
        turn_id: String,
        text: String,
    },
    /// Abort one active turn.
    Interrupt { turn_id: String },
    /// Resolve a paused tool batch.
    ExecApproval {
        id: String,
        decision: ReviewDecision,
    },
    /// Invokes a command owned by one capability.
    CapabilityCommand {
        capability: String,
        command: String,
        arguments: String,
    },
    /// Selects one immutable registered model route.
    SetModel { route: String },
    /// Requests that the frontend reopen an existing session.
    ResumeSession { session_id: String },
}

/// An event emitted to a frontend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Submission ID that caused this event, if it was command-driven.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    /// Event payload.
    pub msg: EventMsg,
}

/// Events supported by the minimal frontend contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventMsg {
    Error(ErrorEvent),
    Warning(WarningEvent),
    SessionConfigured(SessionConfiguredEvent),
    #[serde(rename = "task_started")]
    TurnStarted(TurnStartedEvent),
    #[serde(rename = "task_complete")]
    TurnComplete(TurnCompleteEvent),
    TurnAborted(TurnAbortedEvent),
    UserMessage(UserMessageEvent),
    AgentMessage(AgentMessageEvent),
    AgentMessageContentDelta(AgentMessageContentDeltaEvent),
    AgentReasoningContentDelta(AgentReasoningContentDeltaEvent),
    SessionHistory(SessionHistoryEvent),
    ModelChanged(ModelChangedEvent),
    SessionResumeRequested(SessionResumeRequestedEvent),
    ToolCallBegin(ToolCallBeginEvent),
    ToolCallEnd(ToolCallEndEvent),
    ExecApprovalRequest(ExecApprovalRequestEvent),
    TokenCount(TokenCountEvent),
    ContextCompacted,
    WebSearchBegin(WebSearchBeginEvent),
    WebSearchEnd(WebSearchEndEvent),
    Frontend(FrontendEvent),
}

/// Provider-neutral streaming output before submission correlation is attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelEvent {
    TextDelta(String),
    CommentaryDelta(String),
    ReasoningDelta(String),
    WebSearchStarted {
        call_id: String,
    },
    WebSearchCompleted {
        call_id: String,
        action: WebSearchAction,
    },
}

/// Provider-neutral action reported by hosted web search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSearchAction {
    Search {
        query: Option<String>,
    },
    OpenPage {
        url: Option<String>,
    },
    FindInPage {
        url: Option<String>,
        pattern: Option<String>,
    },
    Other,
}

impl ModelEvent {
    /// Converts one normalized provider event into the frontend protocol.
    #[must_use]
    pub fn into_event(self, thread_id: &str, turn_id: &str, item_id: &str) -> EventMsg {
        match self {
            Self::TextDelta(delta) => {
                EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
                    thread_id: thread_id.into(),
                    turn_id: turn_id.into(),
                    item_id: item_id.into(),
                    delta,
                    phase: Some(AgentMessagePhase::FinalAnswer),
                })
            }
            Self::CommentaryDelta(delta) => {
                EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
                    thread_id: thread_id.into(),
                    turn_id: turn_id.into(),
                    item_id: item_id.into(),
                    delta,
                    phase: Some(AgentMessagePhase::Commentary),
                })
            }
            Self::ReasoningDelta(delta) => {
                EventMsg::AgentReasoningContentDelta(AgentReasoningContentDeltaEvent {
                    thread_id: thread_id.into(),
                    turn_id: turn_id.into(),
                    item_id: item_id.into(),
                    delta,
                })
            }
            Self::WebSearchStarted { call_id } => {
                EventMsg::WebSearchBegin(WebSearchBeginEvent { call_id })
            }
            Self::WebSearchCompleted { call_id, action } => {
                let (action, query) = match action {
                    WebSearchAction::Search { query } => ("search", query),
                    WebSearchAction::OpenPage { url } => ("open_page", url),
                    WebSearchAction::FindInPage { url, pattern } => {
                        ("find_in_page", pattern.or(url))
                    }
                    WebSearchAction::Other => ("other", None),
                };
                EventMsg::WebSearchEnd(WebSearchEndEvent {
                    call_id,
                    query,
                    action: action.into(),
                })
            }
        }
    }
}

/// A frontend command declared by a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendCommand {
    pub name: String,
    pub arguments: String,
    pub description: String,
}

/// UI metadata exported by one capability.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendContribution {
    pub capability: String,
    pub commands: Vec<FrontendCommand>,
    pub widgets: Vec<FrontendWidget>,
    #[serde(default)]
    pub references: Vec<FrontendReference>,
    pub active_input: Option<FrontendActiveInput>,
}

/// How normal composer input is submitted while a turn is active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendActiveInput {
    pub operation: String,
}

/// One chat reference supplied by a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendReference {
    pub trigger: char,
    pub value: String,
    pub description: String,
}

/// One capability-rendered view mounted into a standard frontend slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendWidget {
    pub id: String,
    pub slot: FrontendSlot,
    pub text: String,
    pub tone: FrontendTone,
    /// Optional operation invoked when a frontend activates this widget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Op>,
}

/// Stable locations a thin frontend shell makes available to capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendSlot {
    Header,
    ComposerHeader,
    ComposerFooter,
}

/// Capability-rendered transcript content with frontend-neutral formatting and tone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendBlock {
    pub id: Option<String>,
    pub group: Option<String>,
    pub append: bool,
    /// Whether this block represents work that has not completed yet.
    pub pending: bool,
    pub text: String,
    pub format: FrontendBlockFormat,
    pub tone: FrontendTone,
}

impl FrontendBlock {
    /// Scopes replacement and grouping IDs to one capability.
    #[must_use]
    pub fn namespaced(mut self, capability: &str) -> Self {
        if let Some(id) = self.id.take() {
            self.id = Some(format!("{capability}/{id}"));
        }
        if let Some(group) = self.group.take() {
            self.group = Some(format!("{capability}/{group}"));
        }
        self
    }
}

/// Frontend-neutral structure carried by a transcript block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendBlockFormat {
    PlainText,
    UnifiedDiff,
}

/// One selectable action supplied by a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendPickerOption {
    pub label: String,
    pub description: String,
    pub op: Op,
}

/// Generic capability UI updates understood by every frontend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frontend_type", rename_all = "snake_case")]
pub enum FrontendEvent {
    Render {
        capability: String,
        block: FrontendBlock,
    },
    Widget {
        capability: String,
        item: FrontendWidget,
    },
    RemoveWidget {
        capability: String,
        id: String,
    },
    Picker {
        title: String,
        options: Vec<FrontendPickerOption>,
    },
    Preview {
        title: String,
        events: Vec<EventMsg>,
    },
}

/// A presentation hint rather than a terminal-specific color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendTone {
    Neutral,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEvent {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarningEvent {
    pub message: String,
}

/// Immutable session data emitted once when an agent starts or resumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionConfiguredEvent {
    pub session_id: String,
    pub context: SessionContext,
    pub model: ModelChangedEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStartedEvent {
    pub turn_id: String,
    pub model_context_window: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCompleteEvent {
    pub turn_id: String,
    pub last_agent_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnAbortedEvent {
    pub turn_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMessageEvent {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessageEvent {
    pub message: String,
    pub phase: Option<AgentMessagePhase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessageContentDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    pub phase: Option<AgentMessagePhase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReasoningContentDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

/// A restored transcript kept distinct from live turn lifecycle events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHistoryEvent {
    pub events: Vec<EventMsg>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChangedEvent {
    pub route: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub model_context_window: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResumeRequestedEvent {
    pub session_id: String,
    pub context: SessionContext,
}

/// Assistant message phases understood by frontends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessagePhase {
    Commentary,
    FinalAnswer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallBeginEvent {
    pub turn_id: String,
    pub call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallEndEvent {
    pub turn_id: String,
    pub call_id: String,
    pub name: String,
    pub output: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecApprovalRequestEvent {
    pub id: String,
    pub turn_id: String,
    pub calls: Vec<ApprovalCall>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalCall {
    pub call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// A user's decision for a paused tool batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    ApprovedForSession,
    Denied { rejection: String },
    Abort,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    #[serde(default)]
    pub cache_write_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}

impl TokenUsage {
    /// Adds another response's usage, returning `None` on integer overflow.
    pub fn checked_add(&mut self, other: &Self) -> Option<()> {
        let input_tokens = self.input_tokens.checked_add(other.input_tokens)?;
        let cached_input_tokens = self
            .cached_input_tokens
            .checked_add(other.cached_input_tokens)?;
        let cache_write_input_tokens = self
            .cache_write_input_tokens
            .checked_add(other.cache_write_input_tokens)?;
        let output_tokens = self.output_tokens.checked_add(other.output_tokens)?;
        let reasoning_output_tokens = self
            .reasoning_output_tokens
            .checked_add(other.reasoning_output_tokens)?;
        let total_tokens = self.total_tokens.checked_add(other.total_tokens)?;
        *self = Self {
            input_tokens,
            cached_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens,
            total_tokens,
        };
        Some(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsageInfo {
    pub total_token_usage: TokenUsage,
    pub last_token_usage: TokenUsage,
    pub model_context_window: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCountEvent {
    pub info: Option<TokenUsageInfo>,
    pub rate_limits: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchBeginEvent {
    pub call_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchEndEvent {
    pub call_id: String,
    pub query: Option<String>,
    pub action: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn session_configured_has_a_stable_wire_shape() {
        let event = EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: "session-1".into(),
            context: SessionContext {
                tenant_id: Some("tenant-1".into()),
                user_id: Some("user-1".into()),
                user_name: Some("Ada".into()),
                workspace_id: Some("workspace-1".into()),
                workspace_label: Some("Project One".into()),
                origin_label: Some("cron".into()),
            },
            model: ModelChangedEvent {
                route: "default".into(),
                model: "test-model".into(),
                reasoning_effort: Some("high".into()),
                model_context_window: Some(128_000),
            },
        });

        assert_eq!(
            serde_json::to_value(event).expect("serialize session event"),
            json!({
                "type": "session_configured",
                "session_id": "session-1",
                "context": {
                    "tenant_id": "tenant-1",
                    "user_id": "user-1",
                    "user_name": "Ada",
                    "workspace_id": "workspace-1",
                    "workspace_label": "Project One",
                    "origin_label": "cron"
                },
                "model": {
                    "route": "default",
                    "model": "test-model",
                    "reasoning_effort": "high",
                    "model_context_window": 128_000
                }
            })
        );
    }

    #[test]
    fn session_resume_request_carries_the_target_context() {
        let event = EventMsg::SessionResumeRequested(SessionResumeRequestedEvent {
            session_id: "session-2".into(),
            context: SessionContext {
                workspace_label: Some("Project Two".into()),
                origin_label: Some("cron".into()),
                ..SessionContext::default()
            },
        });

        assert_eq!(
            serde_json::to_value(event).expect("serialize resume event"),
            json!({
                "type": "session_resume_requested",
                "session_id": "session-2",
                "context": {
                    "workspace_label": "Project Two",
                    "origin_label": "cron"
                }
            })
        );
    }

    #[test]
    fn frontend_event_has_a_distinct_nested_discriminator() {
        let event = EventMsg::Frontend(FrontendEvent::Widget {
            capability: "subagents".into(),
            item: FrontendWidget {
                id: "status".into(),
                slot: FrontendSlot::ComposerHeader,
                text: "2 agents".into(),
                tone: FrontendTone::Neutral,
                action: None,
            },
        });
        let value = serde_json::to_value(&event).expect("serialize frontend event");

        assert_eq!(value["type"], "frontend");
        assert_eq!(value["frontend_type"], "widget");
        assert_eq!(
            serde_json::from_value::<EventMsg>(value).expect("deserialize frontend event"),
            event
        );
    }

    #[test]
    fn interrupt_has_a_targeted_wire_shape() {
        let submission = Submission {
            id: "cancel-1".into(),
            op: Op::Interrupt {
                turn_id: "turn-1".into(),
            },
        };

        assert_eq!(
            serde_json::to_value(submission).expect("serialize interrupt"),
            json!({
                "id": "cancel-1",
                "op": {
                    "type": "interrupt",
                    "turn_id": "turn-1"
                }
            })
        );
    }

    #[test]
    fn user_input_has_one_text_payload() {
        let submission = Submission {
            id: "input-1".into(),
            op: Op::UserInput {
                text: "hello".into(),
            },
        };

        assert_eq!(
            serde_json::to_value(submission).expect("serialize input"),
            json!({
                "id": "input-1",
                "op": {
                    "type": "user_input",
                    "text": "hello"
                }
            })
        );
    }

    #[test]
    fn system_event_omits_submission_correlation() {
        let event = Event {
            submission_id: None,
            msg: EventMsg::Warning(WarningEvent {
                message: "system notice".into(),
            }),
        };

        assert_eq!(
            serde_json::to_value(event).expect("serialize system event"),
            json!({
                "msg": {
                    "type": "warning",
                    "message": "system notice"
                }
            })
        );
    }

    #[test]
    fn context_compacted_is_a_unit_event() {
        assert_eq!(
            serde_json::to_value(EventMsg::ContextCompacted).expect("serialize compaction"),
            json!({"type": "context_compacted"})
        );
    }

    #[test]
    fn token_usage_overflow_does_not_partially_update_the_total() {
        let mut total = TokenUsage {
            input_tokens: 7,
            total_tokens: i64::MAX,
            ..TokenUsage::default()
        };
        let original = total.clone();

        assert!(
            total
                .checked_add(&TokenUsage {
                    input_tokens: 1,
                    total_tokens: 1,
                    ..TokenUsage::default()
                })
                .is_none()
        );
        assert_eq!(total, original);
    }
}
