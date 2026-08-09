use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use super::AgentConfig;
use super::AgentSender;
use super::EVENT_QUEUE_CAPACITY;
use super::create_agent;
use super::try_send_event;
use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::backend::checkpoint::Checkpoint;
use crate::backend::checkpoint::CheckpointStore;
use crate::backend::checkpoint::ExecutionOutcome;
use crate::backend::checkpoint::ExecutionPageRequest;
use crate::backend::checkpoint::QueuedInput;
use crate::backend::checkpoint::TranscriptPageRequest;
use crate::backend::checkpoint::sqlite::SqliteCheckpoint;
use crate::backend::model::Model;
use crate::backend::model::ModelEventSink;
use crate::backend::model::ModelOutput;
use crate::backend::model::ModelRequest;
use crate::backend::model::ModelRouter;
use crate::backend::model::TOOL_ERROR_FIELD;
use crate::backend::model::ToolCall;
use crate::backend::model::ToolDefinition;
use crate::backend::model::user_message;
use crate::backend::sandbox::ApprovalPolicy;
use crate::backend::sandbox::Sandbox;
use crate::backend::sandbox::local::LocalSandbox;
use crate::middleware::ActiveSubmissionContext;
use crate::middleware::ActiveSubmissionResult;
use crate::middleware::Middleware;
use crate::middleware::MiddlewareStack;
use crate::middleware::ModelContext;
use crate::middleware::RuntimeContext;
use crate::middleware::steering::Steering;
use crate::middleware::tools::ApprovalRequirement;
use crate::middleware::tools::Catalog;
use crate::middleware::tools::Tool;
use crate::middleware::tools::ToolContext;
use crate::middleware::tools::Tools;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::FrontendEvent;
use crate::protocol::FrontendSlot;
use crate::protocol::FrontendTone;
use crate::protocol::FrontendWidget;
use crate::protocol::MAX_USER_INPUT_BYTES;
use crate::protocol::Op;
use crate::protocol::SessionContext;
use crate::protocol::TokenUsage;
use crate::protocol::ToolCallEndEvent;
use crate::protocol::WarningEvent;
use crate::protocol::internal_message_kind;
use serde_json::Value;
use tokio::sync::Notify;

struct TestModel;

struct ScriptedModel {
    outputs: Mutex<VecDeque<ModelOutput>>,
    tool_counts: Mutex<Vec<usize>>,
    inputs: Mutex<Vec<Vec<Value>>>,
}

struct RequestOnlyMiddleware;

struct DurableBeforeModel;

struct FailingBeforeModel;

struct ApprovalRequiredTestTool;

struct SaturatingMiddleware;

struct QueueingMiddleware;

struct BlockingBeforeModelMiddleware {
    started: Arc<Notify>,
    release: Arc<Notify>,
    blocked: AtomicBool,
}

struct BlockingTailMiddleware {
    started: Arc<Notify>,
    release: Arc<Notify>,
    blocked: AtomicBool,
}

struct BlockingModel {
    started: Arc<Notify>,
    release: Arc<Notify>,
    calls: AtomicUsize,
}

const QUEUE_OPERATION: &str = "queue";
const QUEUE_OPERATIONS: &[&str] = &[QUEUE_OPERATION];

fn accept_queued_input(
    context: &mut ActiveSubmissionContext<'_>,
) -> Result<ActiveSubmissionResult> {
    if !context
        .queued_input
        .enqueue(context.submission_id, context.text)?
    {
        return Ok(ActiveSubmissionResult::Rejected(
            "active input could not be queued".into(),
        ));
    }
    context
        .events
        .push(EventMsg::Frontend(FrontendEvent::Widget {
            capability: "queueing".into(),
            item: FrontendWidget {
                id: "queued".into(),
                slot: FrontendSlot::TranscriptTail,
                text: context.text.into(),
                tone: FrontendTone::Neutral,
                symbol: None,
                icon_only: false,
                progress: None,
                content: None,
                action: None,
            },
        }));
    Ok(ActiveSubmissionResult::Accepted)
}

fn consume_queued_input(context: &mut ModelContext<'_>) {
    let queued = context.queued_input.drain();
    if !queued.is_empty() {
        context
            .events
            .push(EventMsg::Frontend(FrontendEvent::RemoveWidget {
                capability: "queueing".into(),
                id: "queued".into(),
            }));
    }
    for item in queued {
        context.push_input(crate::backend::model::user_message(item.text()));
    }
}

impl Middleware for QueueingMiddleware {
    fn name(&self) -> &'static str {
        "queueing"
    }

    fn active_operations(&self) -> &'static [&'static str] {
        QUEUE_OPERATIONS
    }

    fn active_submission(
        &self,
        context: &mut ActiveSubmissionContext<'_>,
    ) -> Result<ActiveSubmissionResult> {
        accept_queued_input(context)
    }

    fn before_model<'a>(&'a self, context: &'a mut ModelContext<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            consume_queued_input(context);
            Ok(())
        })
    }
}

impl Middleware for BlockingBeforeModelMiddleware {
    fn name(&self) -> &'static str {
        "queueing"
    }

    fn active_operations(&self) -> &'static [&'static str] {
        QUEUE_OPERATIONS
    }

    fn active_submission(
        &self,
        context: &mut ActiveSubmissionContext<'_>,
    ) -> Result<ActiveSubmissionResult> {
        accept_queued_input(context)
    }

    fn before_model<'a>(&'a self, context: &'a mut ModelContext<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if !self.blocked.swap(true, Ordering::SeqCst) {
                self.started.notify_one();
                self.release.notified().await;
            }
            consume_queued_input(context);
            Ok(())
        })
    }
}

impl Middleware for BlockingTailMiddleware {
    fn name(&self) -> &'static str {
        "blocking_tail"
    }

    fn before_model<'a>(&'a self, _context: &'a mut ModelContext<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if !self.blocked.swap(true, Ordering::SeqCst) {
                self.started.notify_one();
                self.release.notified().await;
            }
            Ok(())
        })
    }
}

impl Middleware for SaturatingMiddleware {
    fn name(&self) -> &'static str {
        "saturating"
    }

    fn initialize<'a>(&'a self, context: RuntimeContext) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            for index in 0..=EVENT_QUEUE_CAPACITY {
                (context.frontend)(FrontendEvent::RemoveWidget {
                    capability: "saturating".into(),
                    id: index.to_string(),
                })?;
            }
            Ok(())
        })
    }
}

struct MetadataProbe {
    observed: Arc<Mutex<Option<std::collections::BTreeMap<String, serde_json::Value>>>>,
}

impl Middleware for MetadataProbe {
    fn name(&self) -> &'static str {
        "metadata_probe"
    }

    fn register(&self, _catalog: &mut Catalog, runtime: &RuntimeContext) -> Result<()> {
        *self.observed.lock().expect("metadata probe lock") = Some(runtime.metadata.clone());
        Ok(())
    }
}

impl Middleware for RequestOnlyMiddleware {
    fn name(&self) -> &'static str {
        "request_only"
    }

    fn before_model<'a>(&'a self, context: &'a mut ModelContext<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut input = context.request_input().to_vec();
            input.push(crate::backend::model::internal_user_message(
                "request_only",
                "temporary",
            ));
            context.replace_request_input(input);
            Ok(())
        })
    }
}

impl Middleware for DurableBeforeModel {
    fn name(&self) -> &'static str {
        "durable_before_model"
    }

    fn before_model<'a>(&'a self, context: &'a mut ModelContext<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            context.push_input(crate::backend::model::internal_user_message(
                "settled", "durable",
            ));
            context.usage.push(scripted_usage());
            context.events.push(EventMsg::ContextCompacted);
            Ok(())
        })
    }
}

impl Middleware for FailingBeforeModel {
    fn name(&self) -> &'static str {
        "failing_before_model"
    }

    fn before_model<'a>(&'a self, _context: &'a mut ModelContext<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Err(Error::Provider("later middleware failed".into())) })
    }
}

#[test]
fn sender_rejects_oversized_input_before_queueing() {
    let (commands, _receiver) = tokio::sync::mpsc::channel(1);
    let sender = AgentSender { commands };

    assert!(
        sender
            .submit(Op::UserInput {
                text: "x".repeat(MAX_USER_INPUT_BYTES + 1),
                attachments: Vec::new(),
            })
            .is_err()
    );
}

#[test]
fn sender_reports_a_full_live_queue_as_busy() {
    let (commands, _receiver) = tokio::sync::mpsc::channel(1);
    let sender = AgentSender { commands };
    sender
        .submit(Op::UserInput {
            text: "first".into(),
            attachments: Vec::new(),
        })
        .expect("fill queue");

    let error = sender
        .submit(Op::UserInput {
            text: "second".into(),
            attachments: Vec::new(),
        })
        .expect_err("queue should be full");

    assert!(matches!(error, Error::Busy(_)));
}

#[test]
fn event_queue_saturation_returns_an_error_without_reordering_queued_events() {
    let (events, mut receiver) = tokio::sync::mpsc::channel(1);
    try_send_event(
        &events,
        Event {
            submission_id: None,
            msg: EventMsg::Warning(WarningEvent {
                message: "first".into(),
            }),
        },
    )
    .expect("queue first event");

    let error = try_send_event(
        &events,
        Event {
            submission_id: None,
            msg: EventMsg::Warning(WarningEvent {
                message: "second".into(),
            }),
        },
    )
    .expect_err("full queue must fail");
    let EventMsg::Warning(queued) = receiver.try_recv().expect("first queued event").msg else {
        panic!("expected queued warning");
    };

    assert_eq!(
        (error.to_string(), queued.message.as_str()),
        (
            "agent stopped: frontend event queue is full".to_string(),
            "first"
        )
    );
}

#[tokio::test]
async fn active_input_is_durable_before_a_blocked_model_completes() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let model = Arc::new(BlockingModel {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
        calls: AtomicUsize::new(0),
    });
    let mut agent = create_agent(
        AgentConfig::new(
            Arc::new(ModelRouter::new("blocking", model)),
            Arc::new(Sandbox::new(
                Arc::new(LocalSandbox::new(workspace.path()).expect("local sandbox")),
                ApprovalPolicy::Ask,
            )),
            checkpoint_store,
            MiddlewareStack::new(vec![Arc::new(QueueingMiddleware)]).expect("middleware"),
            "test prompt",
        )
        .session_id("blocked-model"),
    )
    .await
    .expect("create agent");
    agent
        .sender()
        .submit(Op::UserInput {
            text: "start".into(),
            attachments: Vec::new(),
        })
        .expect("start turn");
    let turn_id = loop {
        let event = agent.next_event().await.expect("turn event");
        if let EventMsg::TurnStarted(started) = event.msg {
            break started.turn_id;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
        .await
        .expect("model started");

    let queued_submission = agent
        .sender()
        .submit(Op::ActiveInput {
            operation: QUEUE_OPERATION.into(),
            turn_id,
            text: "persist me".into(),
        })
        .expect("queue active input");
    loop {
        let event = agent.next_event().await.expect("queue event");
        if event.submission_id.as_deref() == Some(queued_submission.as_str()) {
            break;
        }
    }
    let saved = checkpoints
        .load("blocked-model")
        .await
        .expect("load checkpoint")
        .expect("saved checkpoint");
    release.notify_one();

    assert!(saved.pending_input.iter().any(|item| {
        item.owner() == "queueing" && item.id() == queued_submission && item.text() == "persist me"
    }));
}

#[tokio::test]
async fn active_input_is_durable_while_before_model_is_blocked() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let middleware = BlockingBeforeModelMiddleware {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
        blocked: AtomicBool::new(false),
    };
    let mut agent = create_agent(
        AgentConfig::new(
            Arc::new(ModelRouter::new("test", Arc::new(TestModel))),
            Arc::new(Sandbox::new(
                Arc::new(LocalSandbox::new(workspace.path()).expect("local sandbox")),
                ApprovalPolicy::Ask,
            )),
            checkpoint_store,
            MiddlewareStack::new(vec![Arc::new(middleware)]).expect("middleware"),
            "test prompt",
        )
        .session_id("blocked-before-model"),
    )
    .await
    .expect("create agent");
    agent
        .sender()
        .submit(Op::UserInput {
            text: "start".into(),
            attachments: Vec::new(),
        })
        .expect("start turn");
    let turn_id = loop {
        let event = agent.next_event().await.expect("turn event");
        if let EventMsg::TurnStarted(started) = event.msg {
            break started.turn_id;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
        .await
        .expect("before-model hook started");

    let queued_submission = agent
        .sender()
        .submit(Op::ActiveInput {
            operation: QUEUE_OPERATION.into(),
            turn_id,
            text: "survive the hook".into(),
        })
        .expect("queue active input");
    let saved = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let saved = checkpoints
                .load("blocked-before-model")
                .await
                .expect("load checkpoint")
                .expect("saved checkpoint");
            if saved
                .pending_input
                .iter()
                .any(|item| item.id() == queued_submission)
            {
                break saved;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("active input persisted while hook was blocked");
    release.notify_one();
    loop {
        let event = agent.next_event().await.expect("queue event");
        if event.submission_id.as_deref() == Some(queued_submission.as_str()) {
            break;
        }
    }

    assert!(saved.pending_input.iter().any(|item| {
        item.owner() == "queueing"
            && item.id() == queued_submission
            && item.text() == "survive the hook"
    }));
}

#[tokio::test]
async fn before_model_events_and_targets_precede_active_changes_during_the_hook() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let mut checkpoint = Checkpoint::empty("ordered-hook-events");
    checkpoint
        .pending_input
        .push(QueuedInput::new("steering", "old", "old input").expect("queued input"));
    checkpoints
        .save(&checkpoint, &checkpoint.context, None)
        .await
        .expect("seed checkpoint");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let middleware = MiddlewareStack::new(vec![
        Arc::new(Steering::default()),
        Arc::new(BlockingTailMiddleware {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
            blocked: AtomicBool::new(false),
        }),
    ])
    .expect("middleware");
    let mut agent = create_agent(
        AgentConfig::new(
            Arc::new(ModelRouter::new("test", Arc::new(TestModel))),
            Arc::new(Sandbox::new(
                Arc::new(LocalSandbox::new(workspace.path()).expect("local sandbox")),
                ApprovalPolicy::Ask,
            )),
            checkpoints.clone(),
            middleware,
            "test prompt",
        )
        .session_id("ordered-hook-events"),
    )
    .await
    .expect("create agent");
    agent
        .sender()
        .submit(Op::UserInput {
            text: "start".into(),
            attachments: Vec::new(),
        })
        .expect("start turn");
    let turn_id = loop {
        let event = agent.next_event().await.expect("turn event");
        if let EventMsg::TurnStarted(started) = event.msg {
            break started.turn_id;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
        .await
        .expect("tail hook started");

    let queued_submission = agent
        .sender()
        .submit(Op::ActiveInput {
            operation: "steer".into(),
            turn_id,
            text: "new input".into(),
        })
        .expect("queue active input");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let saved = checkpoints
                .load("ordered-hook-events")
                .await
                .expect("load checkpoint")
                .expect("saved checkpoint");
            if saved
                .pending_input
                .iter()
                .any(|item| item.id() == queued_submission)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("new input persisted");
    release.notify_one();

    let (order, live_target) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        let mut order = Vec::new();
        let mut live_target = None;
        while order.len() < 2 || live_target.is_none() {
            let event = agent.next_event().await.expect("frontend event");
            match event.msg {
                EventMsg::Frontend(FrontendEvent::RemoveWidget { capability, id })
                    if capability == "steering" && id == "queued" =>
                {
                    order.push("remove")
                }
                EventMsg::Frontend(FrontendEvent::Widget { capability, item })
                    if capability == "steering"
                        && item.id == "queued"
                        && event.submission_id.as_deref() == Some(&queued_submission) =>
                {
                    order.push("widget")
                }
                EventMsg::UserMessage(message) if message.message == "old input" => {
                    live_target = message.message_target;
                }
                _ => {}
            }
        }
        (order, live_target.expect("live message target"))
    })
    .await
    .expect("ordered frontend events");
    let replay_target = checkpoints
        .transcript_page(
            "ordered-hook-events",
            TranscriptPageRequest {
                before_sequence: None,
                max_batches: 100,
            },
        )
        .await
        .expect("load transcript")
        .into_positioned_items_chronological()
        .into_iter()
        .find_map(|(target, item)| (item == user_message("old input")).then_some(target))
        .expect("replayed message target");

    assert_eq!(order, ["remove", "widget"]);
    assert_eq!(live_target, replay_target);
}

impl Model for TestModel {
    fn respond<'a>(
        &'a self,
        _request: ModelRequest,
        _events: ModelEventSink,
    ) -> BoxFuture<'a, Result<ModelOutput>> {
        Box::pin(async { Err(Error::Provider("response was not expected".into())) })
    }
}

impl Model for ScriptedModel {
    fn respond<'a>(
        &'a self,
        request: ModelRequest,
        _events: ModelEventSink,
    ) -> BoxFuture<'a, Result<ModelOutput>> {
        self.tool_counts
            .lock()
            .expect("tool count lock")
            .push(request.tools.len());
        self.inputs
            .lock()
            .expect("input lock")
            .push(request.input.to_vec());
        let output = self
            .outputs
            .lock()
            .expect("scripted output lock")
            .pop_front()
            .ok_or_else(|| Error::Provider("scripted output exhausted".into()));
        Box::pin(async move { output })
    }
}

impl Model for BlockingModel {
    fn respond<'a>(
        &'a self,
        _request: ModelRequest,
        _events: ModelEventSink,
    ) -> BoxFuture<'a, Result<ModelOutput>> {
        let should_wait = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        Box::pin(async move {
            if should_wait {
                self.started.notify_one();
                self.release.notified().await;
            }
            Ok(scripted_message("done"))
        })
    }
}

impl Tool for ApprovalRequiredTestTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "approval_required".into(),
            description: "performs one reviewed mutation".into(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    fn approval(&self) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        _arguments: serde_json::Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok("executed".into()) })
    }
}

fn scripted_usage() -> TokenUsage {
    TokenUsage {
        input_tokens: 1,
        total_tokens: 1,
        ..TokenUsage::default()
    }
}

fn scripted_tool_call() -> ModelOutput {
    ModelOutput::from_output(
        vec![serde_json::json!({
            "type": "function_call",
            "call_id": "reviewed-call",
            "name": "approval_required",
            "arguments": "{}"
        })],
        false,
        scripted_usage(),
    )
    .expect("tool output")
}

fn scripted_message(text: &str) -> ModelOutput {
    ModelOutput::from_output(
        vec![serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}]
        })],
        true,
        scripted_usage(),
    )
    .expect("message output")
}

fn auto_review_config(
    workspace: &Path,
    checkpoints: Arc<dyn CheckpointStore>,
    model: Arc<ScriptedModel>,
    session_id: &str,
) -> AgentConfig {
    AgentConfig::new(
        Arc::new(ModelRouter::new("main", model)),
        Arc::new(Sandbox::new(
            Arc::new(LocalSandbox::new(workspace).expect("local sandbox")),
            ApprovalPolicy::AutoApprove,
        )),
        checkpoints,
        MiddlewareStack::new(vec![Arc::new(Tools::new(vec![Arc::new(
            ApprovalRequiredTestTool,
        )]))])
        .expect("middleware"),
        "test prompt",
    )
    .session_id(session_id)
}

fn config(
    workspace: &Path,
    checkpoints: Arc<dyn CheckpointStore>,
    session_id: &str,
) -> AgentConfig {
    config_with_route(workspace, checkpoints, session_id, "test")
}

fn config_with_route(
    workspace: &Path,
    checkpoints: Arc<dyn CheckpointStore>,
    session_id: &str,
    route: &str,
) -> AgentConfig {
    AgentConfig::new(
        Arc::new(ModelRouter::new(route, Arc::new(TestModel))),
        Arc::new(Sandbox::new(
            Arc::new(LocalSandbox::new(workspace).expect("local sandbox")),
            ApprovalPolicy::Ask,
        )),
        checkpoints,
        MiddlewareStack::new(Vec::new()).expect("middleware"),
        "test prompt",
    )
    .session_id(session_id)
}

fn config_with_two_routes(
    workspace: &Path,
    checkpoints: Arc<dyn CheckpointStore>,
    session_id: &str,
    default: &str,
    alternate: &str,
) -> AgentConfig {
    let mut models = ModelRouter::new(default, Arc::new(TestModel));
    models
        .register(alternate, Arc::new(TestModel))
        .expect("alternate route");
    AgentConfig::new(
        Arc::new(models),
        Arc::new(Sandbox::new(
            Arc::new(LocalSandbox::new(workspace).expect("local sandbox")),
            ApprovalPolicy::Ask,
        )),
        checkpoints,
        MiddlewareStack::new(Vec::new()).expect("middleware"),
        "test prompt",
    )
    .session_id(session_id)
}

fn config_with_metadata_probe(
    workspace: &Path,
    checkpoints: Arc<dyn CheckpointStore>,
    session_id: &str,
    metadata: Option<std::collections::BTreeMap<String, serde_json::Value>>,
    observed: Arc<Mutex<Option<std::collections::BTreeMap<String, serde_json::Value>>>>,
) -> AgentConfig {
    let config = AgentConfig::new(
        Arc::new(ModelRouter::new("test", Arc::new(TestModel))),
        Arc::new(Sandbox::new(
            Arc::new(LocalSandbox::new(workspace).expect("local sandbox")),
            ApprovalPolicy::Ask,
        )),
        checkpoints,
        MiddlewareStack::new(vec![Arc::new(MetadataProbe { observed })]).expect("middleware"),
        "test prompt",
    )
    .session_id(session_id);
    match metadata {
        Some(metadata) => config.metadata(metadata),
        None => config,
    }
}

#[tokio::test]
async fn middleware_event_saturation_fails_agent_creation_instead_of_dropping_updates() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let mut agent_config = config(workspace.path(), checkpoints, "saturating-events");
    agent_config.middleware =
        MiddlewareStack::new(vec![Arc::new(SaturatingMiddleware)]).expect("middleware");

    let Err(error) = create_agent(agent_config).await else {
        panic!("agent creation should report the full event queue");
    };

    assert_eq!(
        error.to_string(),
        "agent stopped: frontend event queue is full"
    );
}

#[tokio::test]
async fn configured_approval_policy_ignores_checkpoint_middleware_state() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    checkpoints
        .save_state(
            "policy-authority",
            "sandbox.approval_policy",
            &serde_json::json!("allow_network"),
        )
        .await
        .expect("seed stale policy");
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints;
    let mut agent = create_agent(config(
        workspace.path(),
        checkpoint_store,
        "policy-authority",
    ))
    .await
    .expect("create agent");
    agent.next_event().await.expect("configured event");
    let EventMsg::Frontend(FrontendEvent::Widget { item, .. }) =
        agent.next_event().await.expect("sandbox widget").msg
    else {
        panic!("expected sandbox widget");
    };

    assert_eq!(item.text, "approval ASK");
}

#[tokio::test]
async fn request_only_input_reaches_the_model_without_entering_the_checkpoint() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let model = Arc::new(ScriptedModel {
        outputs: Mutex::new(VecDeque::from([scripted_message("done")])),
        tool_counts: Mutex::new(Vec::new()),
        inputs: Mutex::new(Vec::new()),
    });
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let config = AgentConfig::new(
        Arc::new(ModelRouter::new("main", model.clone())),
        Arc::new(Sandbox::new(
            Arc::new(LocalSandbox::new(workspace.path()).expect("local sandbox")),
            ApprovalPolicy::Ask,
        )),
        checkpoint_store,
        MiddlewareStack::new(vec![Arc::new(RequestOnlyMiddleware)]).expect("middleware"),
        "test prompt",
    )
    .session_id("request-only");
    let mut agent = create_agent(config).await.expect("create agent");
    agent.next_event().await.expect("configured event");
    agent.next_event().await.expect("sandbox widget");
    agent
        .sender()
        .submit(Op::UserInput {
            text: "hello".into(),
            attachments: Vec::new(),
        })
        .expect("submit input");
    loop {
        if matches!(
            agent.next_event().await.expect("agent event").msg,
            EventMsg::TurnComplete(_)
        ) {
            break;
        }
    }
    let saved = checkpoints
        .load("request-only")
        .await
        .expect("load checkpoint")
        .expect("saved checkpoint");

    assert!(
        model.inputs.lock().expect("input lock")[0]
            .iter()
            .any(|item| internal_message_kind(item) == Some("request_only"))
    );
    assert!(
        saved
            .context
            .iter()
            .all(|item| internal_message_kind(item) != Some("request_only"))
    );
}

#[tokio::test]
async fn completed_before_model_effects_are_settled_when_a_later_hook_fails() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let config = AgentConfig::new(
        Arc::new(ModelRouter::new("main", Arc::new(TestModel))),
        Arc::new(Sandbox::new(
            Arc::new(LocalSandbox::new(workspace.path()).expect("local sandbox")),
            ApprovalPolicy::Ask,
        )),
        checkpoint_store,
        MiddlewareStack::new(vec![
            Arc::new(DurableBeforeModel),
            Arc::new(FailingBeforeModel),
        ])
        .expect("middleware"),
        "test prompt",
    )
    .session_id("settled-hooks");
    let mut agent = create_agent(config).await.expect("create agent");
    agent.next_event().await.expect("configured event");
    agent.next_event().await.expect("sandbox widget");
    agent
        .sender()
        .submit(Op::UserInput {
            text: "hello".into(),
            attachments: Vec::new(),
        })
        .expect("submit input");
    let mut saw_effect = false;
    loop {
        match agent.next_event().await.expect("agent event").msg {
            EventMsg::ContextCompacted => saw_effect = true,
            EventMsg::TurnAborted(_) => break,
            _ => {}
        }
    }
    let saved = checkpoints
        .load("settled-hooks")
        .await
        .expect("load checkpoint")
        .expect("saved checkpoint");
    let execution = checkpoints
        .execution_page(
            "settled-hooks",
            ExecutionPageRequest {
                before_sequence: None,
                limit: 1,
            },
        )
        .await
        .expect("execution page")
        .executions
        .pop()
        .expect("failed execution");

    assert!(saw_effect);
    assert_eq!(saved.total_usage.total_tokens, 1);
    assert_eq!(
        (
            execution.outcome,
            execution.model_calls,
            execution.usage.total_tokens,
            saved.execution_stats.failed_run_count,
        ),
        (ExecutionOutcome::Failed, 0, 1, 1)
    );
    assert!(
        saved
            .context
            .iter()
            .any(|item| internal_message_kind(item) == Some("settled"))
    );
}

#[tokio::test]
async fn provider_failure_records_one_failed_execution() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let mut agent = create_agent(config(
        workspace.path(),
        checkpoint_store,
        "provider-failure",
    ))
    .await
    .expect("create agent");
    agent
        .sender()
        .submit(Op::UserInput {
            text: "fail".into(),
            attachments: Vec::new(),
        })
        .expect("submit input");
    while !matches!(
        agent.next_event().await.expect("agent event").msg,
        EventMsg::TurnAborted(_)
    ) {}

    let execution = checkpoints
        .execution_page(
            "provider-failure",
            ExecutionPageRequest {
                before_sequence: None,
                limit: 1,
            },
        )
        .await
        .expect("execution page")
        .executions
        .pop()
        .expect("failed execution");

    assert_eq!(
        (
            execution.outcome,
            execution.model_calls,
            execution.tool_calls
        ),
        (ExecutionOutcome::Failed, 1, 0)
    );
}

#[tokio::test]
async fn automatic_approval_counts_isolated_review_usage_without_replacing_primary_usage() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let mut review_output =
        scripted_message("{\"decision\":\"approve\",\"call_ids\":[\"reviewed-call\"]}");
    review_output.usage = TokenUsage {
        input_tokens: 7,
        total_tokens: 7,
        ..TokenUsage::default()
    };
    let model = Arc::new(ScriptedModel {
        outputs: Mutex::new(VecDeque::from([
            scripted_tool_call(),
            review_output,
            scripted_message("done"),
        ])),
        tool_counts: Mutex::new(Vec::new()),
        inputs: Mutex::new(Vec::new()),
    });
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let mut agent = create_agent(auto_review_config(
        workspace.path(),
        checkpoint_store,
        model.clone(),
        "auto-review",
    ))
    .await
    .expect("create agent");
    agent
        .sender()
        .submit(Op::UserInput {
            text: "do it".into(),
            attachments: Vec::new(),
        })
        .expect("submit input");
    let mut usage_events = Vec::new();
    loop {
        match agent.next_event().await.expect("agent event").msg {
            EventMsg::TokenCount(count) => {
                let usage = count.info.expect("usage info");
                usage_events.push((
                    usage.total_token_usage.total_tokens,
                    usage.last_token_usage.total_tokens,
                ));
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }
    let saved = checkpoints
        .load("auto-review")
        .await
        .expect("load checkpoint")
        .expect("saved checkpoint");
    let execution = checkpoints
        .execution_page(
            "auto-review",
            ExecutionPageRequest {
                before_sequence: None,
                limit: 1,
            },
        )
        .await
        .expect("execution page")
        .executions
        .pop()
        .expect("completed execution");

    assert_eq!(
        model
            .tool_counts
            .lock()
            .expect("tool count lock")
            .as_slice(),
        [1, 0, 1]
    );
    assert_eq!(saved.total_usage.total_tokens, 9);
    assert_eq!(saved.last_usage, Some(scripted_usage()));
    assert_eq!(usage_events, [(1, 1), (8, 1), (9, 1)]);
    assert_eq!(
        (
            execution.outcome,
            execution.model_calls,
            execution.tool_calls,
            execution.failed_tool_calls,
            execution.usage.total_tokens,
            saved.execution_stats.run_count,
        ),
        (ExecutionOutcome::Completed, 3, 1, 0, 9, 1)
    );
}

#[tokio::test]
async fn malformed_automatic_review_durably_asks_without_dropping_network_access() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let model = Arc::new(ScriptedModel {
        outputs: Mutex::new(VecDeque::from([
            scripted_tool_call(),
            scripted_message("not a review decision"),
        ])),
        tool_counts: Mutex::new(Vec::new()),
        inputs: Mutex::new(Vec::new()),
    });
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let mut agent = create_agent(auto_review_config(
        workspace.path(),
        checkpoint_store,
        model.clone(),
        "review-escalation",
    ))
    .await
    .expect("create agent");
    agent
        .sender()
        .submit(Op::UserInput {
            text: "do it".into(),
            attachments: Vec::new(),
        })
        .expect("submit input");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if matches!(
                agent.next_event().await.expect("agent event").msg,
                EventMsg::ExecApprovalRequest(_)
            ) {
                break;
            }
        }
    })
    .await
    .expect("approval escalation");
    let saved = checkpoints
        .load("review-escalation")
        .await
        .expect("load checkpoint")
        .expect("saved checkpoint");
    let pending = saved.pending_approval.expect("pending approval");

    assert_eq!(
        pending.network_access,
        crate::backend::sandbox::NetworkAccess::Allowed
    );
    assert_eq!(saved.total_usage.total_tokens, 2);
    assert_eq!(
        model
            .tool_counts
            .lock()
            .expect("tool count lock")
            .as_slice(),
        [1, 0]
    );
}

#[tokio::test]
async fn restart_falls_back_when_the_saved_model_route_was_removed() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let mut original = create_agent(config_with_route(
        workspace.path(),
        checkpoint_store,
        "target",
        "old",
    ))
    .await
    .expect("create original agent");
    original.next_event().await.expect("configured event");
    drop(original);

    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let mut restarted = create_agent(config_with_route(
        workspace.path(),
        checkpoint_store,
        "target",
        "new",
    ))
    .await
    .expect("restart with replacement route");
    let EventMsg::SessionConfigured(configured) = restarted
        .next_event()
        .await
        .expect("replacement configured event")
        .msg
    else {
        panic!("expected configured event");
    };
    let saved = checkpoints
        .load("target")
        .await
        .expect("load checkpoint")
        .expect("saved checkpoint");

    assert_eq!(
        (
            configured.model.route.as_str(),
            saved.model_route.as_deref()
        ),
        ("new", Some("new"))
    );
}

#[tokio::test]
async fn explicit_model_route_replaces_a_saved_route_that_is_still_registered() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let mut original = create_agent(config_with_two_routes(
        workspace.path(),
        checkpoint_store,
        "target",
        "kimi-k3",
        "kimi-k2.7",
    ))
    .await
    .expect("create original agent");
    original.next_event().await.expect("configured event");
    drop(original);

    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let mut restarted = create_agent(
        config_with_two_routes(
            workspace.path(),
            checkpoint_store,
            "target",
            "kimi-k2.7",
            "kimi-k3",
        )
        .override_saved_model_route(),
    )
    .await
    .expect("restart with explicit route");
    let EventMsg::SessionConfigured(configured) =
        restarted.next_event().await.expect("configured event").msg
    else {
        panic!("expected configured event");
    };
    let saved = checkpoints
        .load("target")
        .await
        .expect("load checkpoint")
        .expect("saved checkpoint");

    assert_eq!(configured.model.route, "kimi-k2.7");
    assert_eq!(saved.model_route.as_deref(), Some("kimi-k2.7"));
}

#[tokio::test]
async fn new_agent_uses_its_configured_model_instead_of_global_state() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    checkpoints
        .save_state(
            "agent",
            "model_route",
            &serde_json::Value::String("other".into()),
        )
        .await
        .expect("save unrelated global state");
    let mut models = ModelRouter::new("default", Arc::new(TestModel));
    models
        .register("other", Arc::new(TestModel))
        .expect("alternate route");
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let mut agent = create_agent(
        AgentConfig::new(
            Arc::new(models),
            Arc::new(Sandbox::new(
                Arc::new(LocalSandbox::new(workspace.path()).expect("local sandbox")),
                ApprovalPolicy::Ask,
            )),
            checkpoint_store,
            MiddlewareStack::new(Vec::new()).expect("middleware"),
            "test prompt",
        )
        .session_id("fresh"),
    )
    .await
    .expect("create agent");

    let EventMsg::SessionConfigured(configured) =
        agent.next_event().await.expect("configured event").msg
    else {
        panic!("expected configured event");
    };

    assert_eq!(configured.model.route, "default");
}

#[tokio::test]
async fn stale_save_does_not_leapfrog_winning_checkpoint() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let agent = create_agent(config(workspace.path(), checkpoint_store, "target"))
        .await
        .expect("create agent");
    let mut winner = checkpoints
        .load("target")
        .await
        .expect("load checkpoint")
        .expect("initial checkpoint");
    winner.sequence += 1;
    winner.context.push(serde_json::json!({"winner": true}));
    checkpoints
        .save(&winner, &winner.context, None)
        .await
        .expect("save competing checkpoint");

    let (sender, mut events) = agent.into_parts();
    sender
        .submit(Op::UserInput {
            text: "lose the checkpoint race".into(),
            attachments: Vec::new(),
        })
        .expect("submit turn");
    drop(sender);
    while events.recv().await.is_some() {}

    assert_eq!(
        checkpoints
            .load("target")
            .await
            .expect("load checkpoint")
            .expect("winning checkpoint"),
        winner
    );
}

#[tokio::test]
async fn resumed_agent_uses_the_durable_session_context() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let durable_context = SessionContext {
        workspace_label: Some("Project One".into()),
        origin_label: Some("cron".into()),
        ..SessionContext::default()
    };
    let mut agent = create_agent(
        config(workspace.path(), checkpoint_store, "target")
            .session_context(durable_context.clone()),
    )
    .await
    .expect("create agent");
    let EventMsg::SessionConfigured(created) =
        agent.next_event().await.expect("created session event").msg
    else {
        panic!("expected configured session");
    };
    drop(agent);
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints;
    let mut resumed = create_agent(
        config(workspace.path(), checkpoint_store, "target").session_context(SessionContext {
            workspace_label: Some("wrong workspace".into()),
            ..SessionContext::default()
        }),
    )
    .await
    .expect("resume agent");
    let EventMsg::SessionConfigured(restored) = resumed
        .next_event()
        .await
        .expect("resumed session event")
        .msg
    else {
        panic!("expected configured session");
    };

    assert_eq!(
        (created.context, restored.context),
        (durable_context.clone(), durable_context)
    );
}

#[tokio::test]
async fn resumed_agent_preserves_or_explicitly_replaces_durable_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let durable_metadata = std::collections::BTreeMap::from([(
        "gateway.chat".into(),
        serde_json::json!({"workspace": "/srv/project"}),
    )]);
    let created_metadata = Arc::new(Mutex::new(None));
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let agent = create_agent(config_with_metadata_probe(
        workspace.path(),
        checkpoint_store,
        "target",
        Some(durable_metadata.clone()),
        Arc::clone(&created_metadata),
    ))
    .await
    .expect("create agent");
    drop(agent);
    let resumed_metadata = Arc::new(Mutex::new(None));
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let resumed = create_agent(config_with_metadata_probe(
        workspace.path(),
        checkpoint_store,
        "target",
        None,
        Arc::clone(&resumed_metadata),
    ))
    .await
    .expect("resume agent");
    drop(resumed);
    let replacement_metadata = std::collections::BTreeMap::from([(
        "gateway.chat".into(),
        serde_json::json!({"workspace": "/srv/replacement"}),
    )]);
    let replaced_metadata = Arc::new(Mutex::new(None));
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let replaced = create_agent(config_with_metadata_probe(
        workspace.path(),
        checkpoint_store,
        "target",
        Some(replacement_metadata.clone()),
        Arc::clone(&replaced_metadata),
    ))
    .await
    .expect("replace metadata");
    drop(replaced);

    assert_eq!(
        created_metadata.lock().expect("created metadata").as_ref(),
        Some(&durable_metadata)
    );
    assert_eq!(
        resumed_metadata.lock().expect("resumed metadata").as_ref(),
        Some(&durable_metadata)
    );
    assert_eq!(
        replaced_metadata
            .lock()
            .expect("replaced metadata")
            .as_ref(),
        Some(&replacement_metadata)
    );
    assert_eq!(
        checkpoints
            .load("target")
            .await
            .expect("load checkpoint")
            .expect("saved checkpoint")
            .metadata,
        replacement_metadata
    );
}

#[tokio::test]
async fn resume_request_carries_the_target_session_context() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let target_context = SessionContext {
        workspace_id: Some("workspace-two".into()),
        workspace_label: Some("Project Two".into()),
        origin_label: Some("cron".into()),
        ..SessionContext::default()
    };
    let mut target = Checkpoint::empty("target");
    target.session_context.clone_from(&target_context);
    target.model_route = Some("foreign-workspace-route".into());
    checkpoints
        .save(&target, &[], None)
        .await
        .expect("save target session");
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints;
    let agent = create_agent(config(workspace.path(), checkpoint_store, "current"))
        .await
        .expect("create current agent");
    let (sender, mut events) = agent.into_parts();
    events.recv().await.expect("configured session event");
    let submission_id = sender
        .submit(Op::ResumeSession {
            session_id: "target".into(),
        })
        .expect("request resume");

    let event = loop {
        let event = events.recv().await.expect("resume requested event");
        if event.submission_id.as_deref() == Some(&submission_id) {
            break event;
        }
    };
    let EventMsg::SessionResumeRequested(request) = event.msg else {
        panic!("expected resume request");
    };

    assert_eq!(
        (event.submission_id, request.session_id, request.context),
        (Some(submission_id), "target".into(), target_context)
    );
}

#[tokio::test]
async fn restart_closes_uncertain_tool_calls_without_replaying_them() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let call = ToolCall {
        call_id: "call-1".into(),
        name: "write_file".into(),
        arguments: serde_json::json!({"path": "note.txt", "content": "hello"}),
    };
    let mut target = Checkpoint::empty("target");
    target.active_execution = Some(crate::backend::checkpoint::ActiveExecution {
        submission_id: "submission-1".into(),
        turn_id: "turn-1".into(),
        started_at_ms: 1,
        model_calls: 0,
        tool_calls: 0,
        failed_tool_calls: 0,
        usage: TokenUsage::default(),
    });
    target.context.push(serde_json::json!({
        "type": "function_call",
        "call_id": call.call_id.clone(),
        "name": call.name.clone(),
        "arguments": call.arguments.to_string()
    }));
    target.pending_tools.push(call);
    target
        .pending_input
        .push(QueuedInput::new("editable", "message-1", "queued after restart").expect("queue"));
    checkpoints
        .save(&target, &target.context, None)
        .await
        .expect("save target");
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();

    let mut agent = create_agent(config(workspace.path(), checkpoint_store, "target"))
        .await
        .expect("resume agent");
    assert!(matches!(
        agent.next_event().await.expect("session event").msg,
        EventMsg::SessionConfigured(_)
    ));
    let EventMsg::SessionHistory(history) = agent.next_event().await.expect("history event").msg
    else {
        panic!("expected atomic session history");
    };
    assert!(matches!(
        history.events.as_slice(),
        [
            EventMsg::ToolCallBegin(begin),
            EventMsg::ToolCallEnd(ToolCallEndEvent {
                turn_id,
                call_id,
                output,
                is_error: true,
                ..
            }),
            EventMsg::UserMessage(user)
        ] if begin.turn_id == "turn-1"
            && begin.call_id == "call-1"
            && turn_id == &begin.turn_id
            && call_id == &begin.call_id
            && output == "execution interrupted; result unknown after restart"
            && user.message == "queued after restart"
    ));

    let recovered = checkpoints
        .load("target")
        .await
        .expect("load checkpoint")
        .expect("recovered checkpoint");
    let execution = checkpoints
        .execution_page(
            "target",
            ExecutionPageRequest {
                before_sequence: None,
                limit: 1,
            },
        )
        .await
        .expect("execution page")
        .executions
        .pop()
        .expect("recovered execution");
    assert!(recovered.active_execution.is_none());
    assert!(recovered.pending_tools.is_empty());
    assert_eq!(
        recovered.context.iter().find_map(|item| {
            item.get(TOOL_ERROR_FIELD)
                .and_then(serde_json::Value::as_bool)
        }),
        Some(true)
    );
    assert_eq!(
        (
            execution.outcome,
            execution.tool_calls,
            execution.failed_tool_calls,
            recovered.execution_stats.aborted_run_count,
        ),
        (ExecutionOutcome::Aborted, 1, 1, 1)
    );
}
