//! The small event protocol shared by agent frontends.

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;

pub use self::replay::events as replay_events;
pub(crate) use self::replay::{
    ATTACHMENTS_FIELD, CONTEXT_COMPACTED_MARKER, INTERNAL_MESSAGE_FIELD, REPLAY_REASONING_FIELD,
    TOOL_ERROR_FIELD, internal_message_kind, is_internal_message, strip_attachment_references,
};

mod replay;

/// Maximum total UTF-8 bytes accepted in one user-input submission.
pub const MAX_USER_INPUT_BYTES: usize = 1024 * 1024;

/// Maximum UTF-8 bytes accepted in capability command input or queued active input.
pub const MAX_CAPABILITY_INPUT_BYTES: usize = 64 * 1024;

/// One immutable, session-bound file addressed by an opaque reference.
///
/// Only upload-origin references are valid in `Op::UserInput.attachments`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFileReference {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub media_type: String,
}

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
    UserInput {
        text: String,
        attachments: Vec<SessionFileReference>,
    },
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
        /// Optional caller-editable text kept separate from routing arguments.
        ///
        /// When embedded in a frontend action, a present value is its caller-editable text.
        #[serde(deserialize_with = "required_option")]
        input: Option<String>,
        #[serde(deserialize_with = "required_option")]
        target: Option<MessageTarget>,
    },
    /// Selects one immutable registered model route.
    SetModel { route: String },
    /// Requests that the frontend reopen an existing session.
    ResumeSession { session_id: String },
}

fn required_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
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
    ModelStepStarted(ModelStepStartedEvent),
    ModelStepCompleted(ModelStepCompletedEvent),
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebSearchAction {
    Search {
        queries: Vec<String>,
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
    pub fn into_event(self, session_id: &str, turn_id: &str, model_step_id: &str) -> EventMsg {
        match self {
            Self::TextDelta(delta) => {
                EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
                    session_id: session_id.into(),
                    turn_id: turn_id.into(),
                    model_step_id: model_step_id.into(),
                    delta,
                    phase: AgentMessagePhase::FinalAnswer,
                })
            }
            Self::CommentaryDelta(delta) => {
                EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
                    session_id: session_id.into(),
                    turn_id: turn_id.into(),
                    model_step_id: model_step_id.into(),
                    delta,
                    phase: AgentMessagePhase::Commentary,
                })
            }
            Self::ReasoningDelta(delta) => {
                EventMsg::AgentReasoningContentDelta(AgentReasoningContentDeltaEvent {
                    session_id: session_id.into(),
                    turn_id: turn_id.into(),
                    model_step_id: model_step_id.into(),
                    delta,
                })
            }
            Self::WebSearchStarted { call_id } => EventMsg::WebSearchBegin(WebSearchBeginEvent {
                session_id: session_id.into(),
                turn_id: turn_id.into(),
                model_step_id: model_step_id.into(),
                call_id,
            }),
            Self::WebSearchCompleted { call_id, action } => {
                EventMsg::WebSearchEnd(WebSearchEndEvent {
                    session_id: session_id.into(),
                    turn_id: turn_id.into(),
                    model_step_id: model_step_id.into(),
                    call_id,
                    action,
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
    /// Whether the composed runtime installs session-bound file attachment endpoints.
    pub accepts_file_attachments: bool,
    /// Optional capability-owned item count for generic summaries.
    pub count: Option<usize>,
    pub commands: Vec<FrontendCommand>,
    pub widgets: Vec<FrontendWidget>,
    pub references: Vec<FrontendReference>,
    pub active_input: Option<FrontendActiveInput>,
}

/// One middleware entry and its frontend-neutral configuration controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiddlewareFeature {
    pub id: String,
    pub label: String,
    pub description: String,
    pub required: bool,
    pub settings: Vec<FrontendSetting>,
}

/// One schema-advertised setting rendered by a thin frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendSetting {
    pub id: String,
    pub label: String,
    pub description: String,
    #[serde(flatten)]
    pub kind: FrontendSettingKind,
}

/// Generic control metadata for a schema-advertised setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FrontendSettingKind {
    Integer {
        min: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<i64>,
        step: i64,
    },
    Select {
        options: Vec<FrontendSettingOption>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unset_label: Option<String>,
    },
}

/// One exact value in a schema-advertised select control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendSettingOption {
    pub value: String,
    pub label: String,
    pub description: String,
}

/// Scalar value accepted by the generic setting controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FrontendSettingValue {
    Integer(i64),
    String(String),
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
    pub symbol: Option<FrontendSymbol>,
    pub icon_only: bool,
    pub progress: Option<FrontendProgress>,
    pub content: Option<FrontendWidgetContent>,
    /// Optional operation invoked when a frontend activates this widget.
    pub action: Option<Op>,
}

/// Determinate progress rendered by a frontend widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendProgress {
    pub completed: usize,
    pub total: usize,
}

/// Capability-owned content shown when a frontend widget is opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FrontendWidgetContent {
    Blocks {
        title: String,
        blocks: Vec<FrontendBlock>,
    },
    Picker {
        title: String,
        options: Vec<FrontendPickerOption>,
    },
    ActionList {
        title: String,
        items: Vec<FrontendActionListItem>,
    },
}

/// Stable locations a thin frontend shell makes available to capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendSlot {
    Header,
    ComposerHeader,
    ComposerFooter,
    MessageActions,
    /// A transient capability-owned item after the live transcript.
    TranscriptTail,
    /// A capability destination mounted by the frontend shell.
    Navigation,
    /// A capability action mounted in the current chat's menu.
    ChatMenu,
}

/// Capability-rendered transcript content with frontend-neutral formatting and tone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendBlock {
    pub id: Option<String>,
    pub group: Option<String>,
    pub update: FrontendBlockUpdate,
    pub state: FrontendBlockState,
    pub role: FrontendBlockRole,
    /// Compact, standalone row label. Frontends must not derive this from `text`.
    pub title: String,
    /// Expandable body or artifact content.
    pub text: String,
    pub symbol: Option<FrontendSymbol>,
    /// Downloadable files owned by the session rendering this block.
    pub files: Vec<SessionFileReference>,
    pub format: FrontendBlockFormat,
    pub tone: FrontendTone,
}

/// A block together with its explicit semantic owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedBlock {
    pub capability: String,
    pub block: FrontendBlock,
}

/// How a block changes the matching capability-scoped ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendBlockUpdate {
    Replace,
    Append,
}

/// Lifecycle state of one rendered transcript block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendBlockState {
    Pending,
    Complete,
}

/// Semantic category used for grouping, summaries, filtering, and icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendBlockRole {
    Activity,
    Tool,
    WebSearch,
    Artifact,
    Approval,
    Notice,
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
    pub detail: String,
    pub symbol: Option<FrontendSymbol>,
    pub shows_detail: bool,
    pub op: Op,
}

/// One compact status row with optional trailing actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendActionListItem {
    pub id: String,
    pub text: String,
    pub state: FrontendListItemState,
    pub actions: Vec<FrontendAction>,
}

/// Semantic state for one compact list row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendListItemState {
    Plain,
    Pending,
    InProgress,
    Completed,
}

/// One labeled, icon-forward action attached to a list item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendAction {
    pub id: String,
    pub label: String,
    pub symbol: FrontendSymbol,
    pub tone: FrontendTone,
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
        id: String,
        title: String,
        subtitle: String,
        page_id: String,
        update: FrontendPreviewUpdate,
        events: Vec<EventMsg>,
        next: Option<Op>,
    },
}

/// How one preview page changes the matching frontend preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendPreviewUpdate {
    Replace,
    Prepend,
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

impl EventMsg {
    /// Renders framework-owned semantic events without frontend prose parsing.
    #[must_use]
    pub fn presentation(&self) -> Option<RenderedBlock> {
        let block = match self {
            Self::Error(error) => FrontendBlock {
                id: None,
                group: None,
                update: FrontendBlockUpdate::Replace,
                state: FrontendBlockState::Complete,
                role: FrontendBlockRole::Notice,
                title: "Error".into(),
                text: error.message.clone(),
                symbol: None,
                files: Vec::new(),
                format: FrontendBlockFormat::PlainText,
                tone: FrontendTone::Error,
            },
            Self::Warning(warning) => FrontendBlock {
                id: None,
                group: None,
                update: FrontendBlockUpdate::Replace,
                state: FrontendBlockState::Complete,
                role: FrontendBlockRole::Notice,
                title: "Warning".into(),
                text: warning.message.clone(),
                symbol: None,
                files: Vec::new(),
                format: FrontendBlockFormat::PlainText,
                tone: FrontendTone::Warning,
            },
            Self::TurnAborted(turn) => FrontendBlock {
                id: None,
                group: Some(turn.turn_id.clone()),
                update: FrontendBlockUpdate::Replace,
                state: FrontendBlockState::Complete,
                role: FrontendBlockRole::Notice,
                title: "Turn aborted".into(),
                text: turn.reason.clone(),
                symbol: None,
                files: Vec::new(),
                format: FrontendBlockFormat::PlainText,
                tone: FrontendTone::Warning,
            },
            Self::WebSearchBegin(search) => FrontendBlock {
                id: Some(format!("{}/{}", search.model_step_id, search.call_id)),
                group: Some(search.turn_id.clone()),
                update: FrontendBlockUpdate::Replace,
                state: FrontendBlockState::Pending,
                role: FrontendBlockRole::WebSearch,
                title: "Searching the web".into(),
                text: String::new(),
                symbol: Some(FrontendSymbol::Search),
                files: Vec::new(),
                format: FrontendBlockFormat::PlainText,
                tone: FrontendTone::Neutral,
            },
            Self::WebSearchEnd(search) => {
                let (title, text) = match &search.action {
                    WebSearchAction::Search { queries } => ("Searched the web", queries.join("\n")),
                    WebSearchAction::OpenPage { url } => {
                        ("Opened a web page", url.clone().unwrap_or_default())
                    }
                    WebSearchAction::FindInPage { url, pattern } => {
                        let text = match (url, pattern) {
                            (Some(url), Some(pattern)) => format!("{pattern}\n{url}"),
                            (Some(url), None) => url.clone(),
                            (None, Some(pattern)) => pattern.clone(),
                            (None, None) => String::new(),
                        };
                        ("Searched a web page", text)
                    }
                    WebSearchAction::Other => ("Web search complete", String::new()),
                };
                FrontendBlock {
                    id: Some(format!("{}/{}", search.model_step_id, search.call_id)),
                    group: Some(search.turn_id.clone()),
                    update: FrontendBlockUpdate::Replace,
                    state: FrontendBlockState::Complete,
                    role: FrontendBlockRole::WebSearch,
                    title: title.into(),
                    text,
                    symbol: Some(FrontendSymbol::Search),
                    files: Vec::new(),
                    format: FrontendBlockFormat::PlainText,
                    tone: FrontendTone::Success,
                }
            }
            Self::Frontend(FrontendEvent::Render { capability, block }) => {
                return Some(RenderedBlock {
                    capability: capability.clone(),
                    block: block.clone(),
                });
            }
            _ => return None,
        };
        Some(RenderedBlock {
            capability: match self {
                Self::WebSearchBegin(_) | Self::WebSearchEnd(_) => "web_search",
                _ => "agent",
            }
            .into(),
            block,
        })
    }
}

/// A presentation hint rather than a name from any one icon set, the same way
/// [`FrontendTone`] names a role instead of a color.
///
/// A gateway does not know whether the frontend draws SF Symbols, terminal glyphs, or
/// SVGs, so it names what a glyph stands for and each frontend supplies its own artwork.
/// Most variants are roles. The rest are provider identity, where no role applies: some
/// name the vendor outright (`ChatGpt`, `Claude`, `Deepseek`, `Kimi`) and the others name
/// what the mark depicts (`Moon`, `Sparkle`) where a frontend has no vendor artwork to draw.
///
/// [`Self::Custom`] carries anything outside this list so a plugin can still ship a glyph
/// this enum has never heard of. It is explicitly best-effort: a frontend that cannot
/// resolve the name falls back to a placeholder, which is why everything shipped in-tree
/// should earn a variant instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontendSymbol {
    Agent,
    Brain,
    Branch,
    Chat,
    ChatGpt,
    Claude,
    Deepseek,
    Delete,
    Edit,
    Kimi,
    Moon,
    Promote,
    Route,
    Search,
    Sparkle,
    Storage,
    Task,
    Custom(String),
}

impl FrontendSymbol {
    /// The wire name. Also the stable token capabilities build action ids from.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Agent => "agent",
            Self::Brain => "brain",
            Self::Branch => "branch",
            Self::Chat => "chat",
            Self::ChatGpt => "chat_gpt",
            Self::Claude => "claude",
            Self::Deepseek => "deepseek",
            Self::Delete => "delete",
            Self::Edit => "edit",
            Self::Kimi => "kimi",
            Self::Moon => "moon",
            Self::Promote => "promote",
            Self::Route => "route",
            Self::Search => "search",
            Self::Sparkle => "sparkle",
            Self::Storage => "storage",
            Self::Task => "task",
            Self::Custom(name) => name,
        }
    }

    /// Unknown names become [`Self::Custom`] rather than an error: a frontend rendering a
    /// placeholder is a better outcome than a gateway refusing to decode a whole frame.
    fn from_wire(name: &str) -> Self {
        match name {
            "agent" => Self::Agent,
            "brain" => Self::Brain,
            "branch" => Self::Branch,
            "chat" => Self::Chat,
            "chat_gpt" => Self::ChatGpt,
            "claude" => Self::Claude,
            "deepseek" => Self::Deepseek,
            "delete" => Self::Delete,
            "edit" => Self::Edit,
            "kimi" => Self::Kimi,
            "moon" => Self::Moon,
            "promote" => Self::Promote,
            "route" => Self::Route,
            "search" => Self::Search,
            "sparkle" => Self::Sparkle,
            "storage" => Self::Storage,
            "task" => Self::Task,
            other => Self::Custom(other.to_owned()),
        }
    }
}

impl std::fmt::Display for FrontendSymbol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for FrontendSymbol {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FrontendSymbol {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // A known name round-trips out of `Custom` on the way back in, so the two spellings
        // of the same glyph cannot drift apart once a frame has crossed the wire.
        String::deserialize(deserializer).map(|name| Self::from_wire(&name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEvent {
    pub kind: ErrorKind,
    pub message: String,
    pub retryable: bool,
    pub status: Option<u16>,
    pub retry_after: Option<String>,
}

impl ErrorEvent {
    pub(crate) fn from_error(error: &crate::Error) -> Self {
        let (kind, retryable, status, retry_after) = match error {
            crate::Error::Config(_) => (ErrorKind::Configuration, false, None, None),
            crate::Error::Duplicate(_) => (ErrorKind::DuplicateRegistration, false, None, None),
            crate::Error::Unknown(_) => (ErrorKind::UnknownRegistration, false, None, None),
            crate::Error::Provider(error) => (
                ErrorKind::Provider,
                error.is_retryable(),
                error.status(),
                error.retry_after().map(str::to_owned),
            ),
            crate::Error::Auth(_) => (ErrorKind::Authentication, false, None, None),
            crate::Error::Sandbox(_) => (ErrorKind::Sandbox, false, None, None),
            crate::Error::Tool(_) => (ErrorKind::Tool, false, None, None),
            crate::Error::Checkpoint(_) => (ErrorKind::Checkpoint, false, None, None),
            crate::Error::Busy(_) => (ErrorKind::Busy, false, None, None),
            crate::Error::Stopped(_) => (ErrorKind::Stopped, false, None, None),
            crate::Error::Rollback { .. } => (ErrorKind::Rollback, false, None, None),
            crate::Error::Io(_) => (ErrorKind::Io, false, None, None),
            crate::Error::Http(_) => (ErrorKind::Http, false, None, None),
            crate::Error::Json(_) => (ErrorKind::Json, false, None, None),
            crate::Error::Sqlite(_) => (ErrorKind::Storage, false, None, None),
        };
        Self {
            kind,
            message: error.to_string(),
            retryable,
            status,
            retry_after,
        }
    }
}

/// Stable frontend classification for framework failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Configuration,
    DuplicateRegistration,
    UnknownRegistration,
    Provider,
    Authentication,
    Sandbox,
    Tool,
    Checkpoint,
    Busy,
    Stopped,
    Rollback,
    Io,
    Http,
    Json,
    Storage,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnAbortedEvent {
    pub turn_id: String,
    pub reason: String,
}

/// Exact durable transcript prefix selected by a message action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageTarget {
    /// Durable checkpoint sequence containing the selected message.
    pub checkpoint_sequence: u64,
    /// One-based item count within the checkpoint's transcript batch.
    #[serde(deserialize_with = "positive_usize")]
    pub batch_item_count: usize,
}

fn positive_usize<'de, D>(deserializer: D) -> std::result::Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    let value = usize::deserialize(deserializer)?;
    if value == 0 {
        return Err(serde::de::Error::custom(
            "message target item count must be positive",
        ));
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMessageEvent {
    pub message: String,
    pub attachments: Vec<SessionFileReference>,
    #[serde(deserialize_with = "required_option")]
    pub message_target: Option<MessageTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessageEvent {
    pub session_id: String,
    pub turn_id: String,
    pub model_step_id: String,
    pub message: String,
    pub phase: AgentMessagePhase,
    #[serde(deserialize_with = "required_option")]
    pub message_target: Option<MessageTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessageContentDeltaEvent {
    pub session_id: String,
    pub turn_id: String,
    pub model_step_id: String,
    pub delta: String,
    pub phase: AgentMessagePhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReasoningContentDeltaEvent {
    pub session_id: String,
    pub turn_id: String,
    pub model_step_id: String,
    pub delta: String,
}

/// One provider request becoming active within a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelStepStartedEvent {
    pub session_id: String,
    pub turn_id: String,
    pub model_step_id: String,
    pub step_index: usize,
    pub started_at_ms: i64,
}

/// The terminal record for one provider request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelStepCompletedEvent {
    pub session_id: String,
    pub turn_id: String,
    pub model_step_id: String,
    pub step_index: usize,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub outcome: ModelStepOutcome,
}

/// Provider-neutral outcome of a completed model step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ModelStepOutcome {
    Completed {
        end_turn: bool,
        tool_call_ids: Vec<String>,
        usage: TokenUsage,
        content: Vec<ModelStepContent>,
    },
    Failed,
    Interrupted,
}

/// One complete normalized text item produced by a model step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelStepContent {
    pub output_index: usize,
    pub part_index: usize,
    pub phase: ModelStepContentPhase,
    pub text: String,
    pub annotations: Vec<ModelStepAnnotation>,
}

/// A provider-neutral annotation attached to one complete text part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelStepAnnotation {
    UrlCitation {
        url: String,
        title: String,
        start_index: usize,
        end_index: usize,
    },
    FileCitation {
        file_id: String,
        filename: String,
        index: usize,
    },
    ContainerFileCitation {
        container_id: String,
        file_id: String,
        filename: String,
        start_index: usize,
        end_index: usize,
    },
    FilePath {
        file_id: String,
        index: usize,
    },
    DocumentCharacterCitation {
        cited_text: String,
        document_index: usize,
        document_title: Option<String>,
        file_id: Option<String>,
        start_char_index: usize,
        end_char_index: usize,
    },
    DocumentPageCitation {
        cited_text: String,
        document_index: usize,
        document_title: Option<String>,
        file_id: Option<String>,
        start_page_number: usize,
        end_page_number: usize,
    },
    DocumentContentBlockCitation {
        cited_text: String,
        document_index: usize,
        document_title: Option<String>,
        file_id: Option<String>,
        start_block_index: usize,
        end_block_index: usize,
    },
    SearchResultCitation {
        cited_text: String,
        search_result_index: usize,
        source: String,
        title: Option<String>,
        start_block_index: usize,
        end_block_index: usize,
    },
    WebSearchResultCitation {
        cited_text: String,
        encrypted_index: String,
        title: Option<String>,
        url: String,
    },
}

/// Semantic role of text preserved in a completed model step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelStepContentPhase {
    Reasoning,
    Commentary,
    FinalAnswer,
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
    pub session_id: String,
    pub turn_id: String,
    pub model_step_id: String,
    pub call_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchEndEvent {
    pub session_id: String,
    pub turn_id: String,
    pub model_step_id: String,
    pub call_id: String,
    pub action: WebSearchAction,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn model_events_keep_typed_correlation_and_web_search_fields() {
        let delta = ModelEvent::CommentaryDelta("Checking".into()).into_event(
            "session-1",
            "turn-1",
            "step-1",
        );
        let search = ModelEvent::WebSearchCompleted {
            call_id: "search-1".into(),
            action: WebSearchAction::Search {
                queries: vec!["Horus framework".into(), "Horus gateway".into()],
            },
        }
        .into_event("session-1", "turn-1", "step-1");

        assert_eq!(
            serde_json::to_value(delta).expect("serialize delta"),
            json!({
                "type": "agent_message_content_delta",
                "session_id": "session-1",
                "turn_id": "turn-1",
                "model_step_id": "step-1",
                "delta": "Checking",
                "phase": "commentary"
            })
        );
        assert_eq!(
            serde_json::to_value(search).expect("serialize web search"),
            json!({
                "type": "web_search_end",
                "session_id": "session-1",
                "turn_id": "turn-1",
                "model_step_id": "step-1",
                "call_id": "search-1",
                "action": {
                    "type": "search",
                    "queries": ["Horus framework", "Horus gateway"]
                }
            })
        );
    }

    #[test]
    fn middleware_settings_have_a_generic_wire_shape() {
        let feature = MiddlewareFeature {
            id: "example".into(),
            label: "Example".into(),
            description: "Example capability".into(),
            required: false,
            settings: vec![FrontendSetting {
                id: "limit".into(),
                label: "Limit".into(),
                description: "Example limit".into(),
                kind: FrontendSettingKind::Integer {
                    min: 1,
                    max: None,
                    step: 10,
                },
            }],
        };

        assert_eq!(
            serde_json::to_value(feature).expect("serialize middleware setting"),
            json!({
                "id": "example",
                "label": "Example",
                "description": "Example capability",
                "required": false,
                "settings": [{
                    "id": "limit",
                    "label": "Limit",
                    "description": "Example limit",
                    "type": "integer",
                    "min": 1,
                    "step": 10
                }]
            })
        );
    }

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
                symbol: Some(FrontendSymbol::Agent),
                icon_only: true,
                progress: None,
                content: None,
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
    fn capability_surface_slots_have_stable_wire_names() {
        assert_eq!(
            serde_json::to_value(FrontendSlot::Navigation).expect("navigation slot"),
            json!("navigation")
        );
        assert_eq!(
            serde_json::to_value(FrontendSlot::ChatMenu).expect("chat menu slot"),
            json!("chat_menu")
        );
        assert_eq!(
            serde_json::to_value(FrontendSlot::TranscriptTail).expect("transcript tail slot"),
            json!("transcript_tail")
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
                attachments: Vec::new(),
            },
        };

        assert_eq!(
            serde_json::to_value(submission).expect("serialize input"),
            json!({
                "id": "input-1",
                "op": {
                    "type": "user_input",
                    "text": "hello",
                    "attachments": []
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

    #[test]
    fn symbols_round_trip_and_keep_unknown_names() {
        for symbol in [
            FrontendSymbol::Agent,
            FrontendSymbol::Brain,
            FrontendSymbol::Branch,
            FrontendSymbol::Chat,
            FrontendSymbol::ChatGpt,
            FrontendSymbol::Claude,
            FrontendSymbol::Deepseek,
            FrontendSymbol::Delete,
            FrontendSymbol::Edit,
            FrontendSymbol::Kimi,
            FrontendSymbol::Moon,
            FrontendSymbol::Promote,
            FrontendSymbol::Route,
            FrontendSymbol::Search,
            FrontendSymbol::Sparkle,
            FrontendSymbol::Storage,
            FrontendSymbol::Task,
        ] {
            let json = serde_json::to_string(&symbol).expect("symbol serializes");
            assert_eq!(json, format!("\"{}\"", symbol.as_str()));
            let decoded: FrontendSymbol = serde_json::from_str(&json).expect("symbol deserializes");
            assert_eq!(decoded, symbol);
        }

        // A name this build has never heard of survives instead of failing the frame.
        let custom: FrontendSymbol =
            serde_json::from_str("\"telescope\"").expect("unknown symbol deserializes");
        assert_eq!(custom, FrontendSymbol::Custom("telescope".into()));
        assert_eq!(custom.as_str(), "telescope");

        // A known name never lingers as a `Custom` once it has crossed the wire, so the two
        // spellings of one glyph cannot compare unequal.
        let normalized: FrontendSymbol = serde_json::from_str(
            &serde_json::to_string(&FrontendSymbol::Custom("edit".into()))
                .expect("custom serializes"),
        )
        .expect("custom deserializes");
        assert_eq!(normalized, FrontendSymbol::Edit);
    }
}
