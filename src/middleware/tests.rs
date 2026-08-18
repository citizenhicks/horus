use super::context::provisional_message_target;
use super::*;
use crate::backend::checkpoint::MAX_QUEUED_INPUTS;
use crate::backend::checkpoint::sqlite::SqliteCheckpoint;
use crate::backend::model::ToolDefinition;
use crate::middleware::tools::Tool;
use crate::middleware::tools::ToolContext;
use crate::protocol::FrontendAction;
use crate::protocol::FrontendReference;
use crate::protocol::FrontendSymbol;
use crate::protocol::Op;
use crate::protocol::SessionContext;

struct LifecycleProbe {
    id: &'static str,
    fail: bool,
    calls: Arc<std::sync::Mutex<Vec<String>>>,
}

struct CompactInputRewrite;

impl Middleware for CompactInputRewrite {
    fn name(&self) -> &'static str {
        "compact_input_rewrite"
    }

    fn session_start<'a>(
        &'a self,
        context: &'a mut SessionStartContext<'_>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            context.retain_input(|item| item != "remove");
            context.input.reverse();
            Ok(())
        })
    }
}

impl Middleware for LifecycleProbe {
    fn name(&self) -> &'static str {
        self.id
    }

    fn session_start<'a>(
        &'a self,
        context: &'a mut SessionStartContext<'_>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.calls
                .lock()
                .expect("lifecycle calls")
                .push(format!("start:{}", self.id));
            context.push_input(serde_json::json!(self.id));
            if self.fail {
                return Err(Error::Config(format!("{} failed", self.id)));
            }
            Ok(())
        })
    }

    fn session_end<'a>(&'a self, _runtime: &'a RuntimeContext) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.calls
                .lock()
                .expect("lifecycle calls")
                .push(format!("end:{}", self.id));
            Ok(())
        })
    }
}

fn lifecycle_probe(
    id: &'static str,
    fail: bool,
    calls: &Arc<std::sync::Mutex<Vec<String>>>,
) -> Arc<dyn Middleware> {
    Arc::new(LifecycleProbe {
        id,
        fail,
        calls: Arc::clone(calls),
    })
}

fn lifecycle_runtime(path: &std::path::Path) -> RuntimeContext {
    RuntimeContext {
        checkpoints: Arc::new(
            SqliteCheckpoint::new(path.join("checkpoints.sqlite3")).expect("checkpoint store"),
        ),
        session_id: "session".into(),
        model_route: "model".into(),
        model: "model".into(),
        approval_policy: crate::backend::sandbox::ApprovalPolicy::Ask,
        session_context: SessionContext::default(),
        metadata: BTreeMap::new(),
        role: crate::agent::AgentRole::Main,
        frontend: Arc::new(|_| Ok(())),
    }
}

#[tokio::test]
async fn session_lifecycle_starts_forward_and_ends_or_rolls_back_in_reverse() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stack = MiddlewareStack::new(vec![
        lifecycle_probe("a", false, &calls),
        lifecycle_probe("b", false, &calls),
    ])
    .expect("middleware stack");

    let runtime = lifecycle_runtime(temporary.path());
    let mut input = Vec::new();
    let started = stack
        .session_start(&runtime, &[], SessionStartSource::Startup, &mut input)
        .await
        .expect("session start");
    stack.session_end(&runtime).await.expect("session end");
    assert_eq!(input, [serde_json::json!("a"), serde_json::json!("b")]);
    assert!(started.input_changed);
    assert_eq!(
        *calls.lock().expect("lifecycle calls"),
        ["start:a", "start:b", "end:b", "end:a"]
    );

    calls.lock().expect("lifecycle calls").clear();
    let failing = MiddlewareStack::new(vec![
        lifecycle_probe("a", false, &calls),
        lifecycle_probe("b", true, &calls),
    ])
    .expect("middleware stack");
    let mut failing_input = Vec::new();
    assert!(
        failing
            .session_start(
                &runtime,
                &[],
                SessionStartSource::Resume,
                &mut failing_input,
            )
            .await
            .is_err()
    );
    assert_eq!(
        *calls.lock().expect("lifecycle calls"),
        ["start:a", "start:b", "end:a"]
    );

    calls.lock().expect("lifecycle calls").clear();
    let mut compact_input = vec![serde_json::json!("compacted")];
    assert!(
        failing
            .session_start(
                &runtime,
                &[],
                SessionStartSource::Compact,
                &mut compact_input,
            )
            .await
            .is_err()
    );
    assert_eq!(compact_input, [serde_json::json!("compacted")]);
    assert_eq!(
        *calls.lock().expect("lifecycle calls"),
        ["start:a", "start:b"]
    );
}

#[tokio::test]
async fn compact_session_start_restores_removed_and_reordered_input_on_failure() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stack = MiddlewareStack::new(vec![
        Arc::new(CompactInputRewrite),
        lifecycle_probe("failure", true, &calls),
    ])
    .expect("middleware stack");
    let runtime = lifecycle_runtime(temporary.path());
    let original = vec![
        serde_json::json!("first"),
        serde_json::json!("remove"),
        serde_json::json!("last"),
    ];
    let mut input = original.clone();

    stack
        .session_start(&runtime, &[], SessionStartSource::Compact, &mut input)
        .await
        .expect_err("later middleware must fail");

    assert_eq!(input, original);
}

#[test]
fn hook_policy_decisions_are_monotonic_and_stop_continuation_is_bounded() {
    let tools = Catalog::default();
    let mut permission_events = Vec::new();
    let mut permission = PermissionRequestContext {
        turn: TurnIdentity {
            session_id: "session",
            turn_id: "turn",
            model: "model",
            approval_policy: crate::backend::sandbox::ApprovalPolicy::Ask,
        },
        calls: &[],
        requested_call_ids: &[],
        reason: "test",
        events: &mut permission_events,
        tools: &tools,
        decision: None,
    };
    permission.allow();
    permission.deny("blocked").expect("deny permission");
    permission.allow();
    assert_eq!(
        permission.decision(),
        Some(&crate::protocol::ReviewDecision::Denied {
            rejection: "blocked".into()
        })
    );

    let mut events = Vec::new();
    let mut stop = StopContext {
        turn: permission.turn,
        role: &crate::agent::AgentRole::Main,
        stop_hook_active: false,
        last_assistant_message: Some("done"),
        events: &mut events,
        continuation: None,
    };
    stop.continue_with("first").expect("first continuation");
    stop.continue_with("second").expect("first decision wins");
    assert_eq!(stop.continuation(), Some("first"));
    let mut active = StopContext {
        stop_hook_active: true,
        continuation: None,
        ..stop
    };
    assert!(active.continue_with("again").is_err());
}

#[test]
fn lifecycle_stop_decisions_keep_the_first_reason() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let runtime = lifecycle_runtime(temporary.path());
    let mut input = Vec::new();
    let mut start = SessionStartContext {
        runtime: &runtime,
        source: SessionStartSource::Startup,
        queued_input: QueuedInputSnapshot::default(),
        input: &mut input,
        input_changed: false,
        stop_reason: None,
    };
    start.stop("first").expect("first session stop");
    start.stop("second").expect("second session stop");

    let mut events = Vec::new();
    let mut compact = CompactContext {
        session_id: "session",
        turn_id: "turn",
        model: "model",
        input: &[],
        events: &mut events,
        stop_reason: None,
    };
    compact.stop("first").expect("first compaction stop");
    compact.stop("second").expect("second compaction stop");

    assert_eq!(
        (start.stop_reason(), compact.stop_reason()),
        (Some("first"), Some("first"))
    );
}

#[test]
fn pre_tool_rewrite_rejects_invalid_calls_without_mutation() {
    let tools = Catalog::default();
    let original = crate::backend::model::ToolCall {
        call_id: "call".into(),
        name: "read".into(),
        arguments: serde_json::json!({"path": "README.md"}),
    };
    for (name, arguments) in [
        ("", serde_json::json!({})),
        ("search", serde_json::json!([])),
    ] {
        let mut call = original.clone();
        let mut events = Vec::new();
        let error = PreToolUseContext {
            turn: TurnIdentity {
                session_id: "session",
                turn_id: "turn",
                model: "model",
                approval_policy: crate::backend::sandbox::ApprovalPolicy::Ask,
            },
            events: &mut events,
            tools: &tools,
            call: &mut call,
            input: Vec::new(),
            denial: None,
        }
        .replace(name, arguments)
        .expect_err("invalid rewrite must fail");

        assert!(matches!(error, Error::Tool(_)));
        assert_eq!(call, original);
    }
}

fn queued(owner: &str, id: &str, text: &str) -> DurableQueuedInput {
    DurableQueuedInput::new(owner, id, text).expect("valid queued input")
}

fn scoped_queue<'a>(
    items: &'a mut Vec<DurableQueuedInput>,
    owner: &'static str,
    baseline: QueuedInputBaseline,
) -> QueuedInputQueue<'a> {
    let mut queue = QueuedInputQueue::new(items, baseline);
    queue.scope(owner);
    queue
}

#[test]
fn queued_input_queue_cannot_observe_or_drain_another_owner() {
    let mut items = vec![
        queued("alpha", "one", "first"),
        queued("beta", "one", "private"),
    ];
    let drained = {
        let mut queue = scoped_queue(
            &mut items,
            "alpha",
            QueuedInputBaseline::from_items(&[
                queued("alpha", "prior-one", "prior"),
                queued("alpha", "prior-two", "prior"),
                queued("beta", "prior-private", "prior"),
            ]),
        );
        assert_eq!(queue.count(), 3);
        assert_eq!(queue.latest().map(|item| item.id()), Some("one"));
        queue.drain()
    };

    assert_eq!(drained[0].text(), "first");
    assert_eq!(items, vec![queued("beta", "one", "private")]);
}

#[test]
fn queued_input_enqueue_rejects_duplicates_without_mutation() {
    let mut items = vec![queued("alpha", "one", "first")];
    let original = items.clone();
    let inserted = scoped_queue(&mut items, "alpha", QueuedInputBaseline::default())
        .enqueue("one", "replacement")
        .expect("valid input");

    assert!(!inserted);
    assert_eq!(items, original);
}

#[test]
fn queued_input_enqueue_honors_the_in_flight_baseline() {
    let baseline = QueuedInputBaseline::from_items(&[
        queued("alpha", "one", "being consumed"),
        queued("beta", "one", "another owner"),
    ]);
    let mut items = Vec::new();
    let inserted = scoped_queue(&mut items, "alpha", baseline)
        .enqueue("one", "duplicate")
        .expect("valid input");

    assert!(!inserted);
    assert!(items.is_empty());
}

#[test]
fn queued_input_take_is_exact_and_owner_scoped() {
    let mut items = vec![
        queued("alpha", "one", "first"),
        queued("alpha", "two", "second"),
        queued("beta", "private", "other owner"),
    ];
    {
        let mut queue = scoped_queue(&mut items, "alpha", QueuedInputBaseline::default());
        assert!(queue.take("stale").expect("stale comparison").is_none());
        let taken = queue
            .take("one")
            .expect("valid comparison")
            .expect("matching item");
        assert_eq!(taken.text(), "first");
        assert!(queue.take("one").expect("already taken").is_none());
    }

    assert_eq!(
        items,
        vec![
            queued("alpha", "two", "second"),
            queued("beta", "private", "other owner")
        ]
    );
}

#[test]
fn queued_input_invalid_mutations_are_atomic() {
    let mut items = vec![queued("alpha", "one", "first")];
    let original = items.clone();
    {
        let mut queue = scoped_queue(&mut items, "alpha", QueuedInputBaseline::default());
        assert!(queue.enqueue("", "second").is_err());
        assert!(queue.enqueue("two", "   ").is_err());
        assert!(
            queue
                .enqueue(
                    "two",
                    &"x".repeat(crate::protocol::MAX_CAPABILITY_INPUT_BYTES + 1),
                )
                .is_err()
        );
        assert!(queue.replace("one", "edit-one", "   ").is_err());
        assert!(
            !queue
                .replace("missing", "edit-one", "replacement")
                .expect("stale replacement")
        );
        assert!(queue.take("").expect("stale comparison").is_none());
    }

    assert_eq!(items, original);
}

#[test]
fn queued_input_enqueue_enforces_the_core_item_bound() {
    let baseline_items: Vec<_> = (0..MAX_QUEUED_INPUTS)
        .map(|index| queued("alpha", &index.to_string(), "item"))
        .collect();
    let mut items = Vec::new();
    let inserted = scoped_queue(
        &mut items,
        "alpha",
        QueuedInputBaseline::from_items(&baseline_items),
    )
    .enqueue("overflow", "item")
    .expect("valid input");

    assert!(!inserted);
    assert!(items.is_empty());
}

struct UnrenderedTool;

impl Tool for UnrenderedTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "unrendered".into(),
            description: String::new(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        _arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok(String::new()) })
    }
}

struct ToolOwner;

impl Middleware for ToolOwner {
    fn name(&self) -> &'static str {
        "tool_owner"
    }

    fn register(&self, catalog: &mut Catalog, _runtime: &RuntimeContext) -> Result<()> {
        catalog.register(Arc::new(UnrenderedTool))
    }
}

struct CatchAllRenderer;

impl Middleware for CatchAllRenderer {
    fn name(&self) -> &'static str {
        "catch_all"
    }

    fn render(&self, _event: &EventMsg, _session_id: &str) -> Option<FrontendBlock> {
        Some(FrontendBlock {
            id: None,
            group: None,
            update: crate::protocol::FrontendBlockUpdate::Replace,
            state: crate::protocol::FrontendBlockState::Complete,
            role: crate::protocol::FrontendBlockRole::Notice,
            title: String::new(),
            text: String::new(),
            symbol: None,
            files: Vec::new(),
            format: crate::protocol::FrontendBlockFormat::PlainText,
            tone: FrontendTone::Neutral,
        })
    }
}

#[test]
fn catalog_requires_the_registering_middleware_to_render_its_tools() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let runtime = RuntimeContext {
        checkpoints: Arc::new(
            SqliteCheckpoint::new(temporary.path().join("checkpoints.sqlite3"))
                .expect("checkpoint store"),
        ),
        session_id: "session".into(),
        model_route: "model".into(),
        model: "model".into(),
        approval_policy: crate::backend::sandbox::ApprovalPolicy::Ask,
        session_context: SessionContext::default(),
        metadata: BTreeMap::new(),
        role: crate::agent::AgentRole::Main,
        frontend: Arc::new(|_| Ok(())),
    };
    let stack = MiddlewareStack::new(vec![Arc::new(CatchAllRenderer), Arc::new(ToolOwner)])
        .expect("middleware stack");

    assert_eq!(
        stack
            .catalog(&runtime)
            .err()
            .expect("unrendered tool should be rejected")
            .to_string(),
        "configuration error: middleware `tool_owner` registered tool `unrendered` but does not render `ToolCallBegin`"
    );
}

struct Extension;

impl Middleware for Extension {
    fn name(&self) -> &'static str {
        "extension"
    }

    fn frontend(&self) -> FrontendContribution {
        FrontendContribution {
            capability: self.name().into(),
            accepts_file_attachments: false,
            count: None,
            commands: Vec::new(),
            widgets: Vec::new(),
            references: vec![FrontendReference {
                trigger: ' ',
                value: "item".into(),
                description: String::new(),
            }],
            active_input: None,
        }
    }
}

#[test]
fn frontend_rejects_malformed_reference_triggers() {
    assert_eq!(
        MiddlewareStack::new(vec![Arc::new(Extension)])
            .expect("middleware stack")
            .frontend()
            .expect_err("invalid frontend extension")
            .to_string(),
        "configuration error: invalid frontend reference ` item`"
    );
}

#[test]
fn frontend_surfaces_require_generic_content() {
    let contribution = FrontendContribution {
        capability: "example".into(),
        accepts_file_attachments: false,
        count: None,
        commands: Vec::new(),
        widgets: vec![crate::protocol::FrontendWidget {
            id: "page".into(),
            slot: FrontendSlot::Navigation,
            text: "Example".into(),
            tone: FrontendTone::Neutral,
            symbol: None,
            icon_only: false,
            progress: None,
            content: None,
            action: None,
        }],
        references: Vec::new(),
        active_input: None,
    };

    assert!(validate_frontend(&[contribution]).is_err());
}

#[test]
fn action_lists_reject_invalid_and_duplicate_rows() {
    let action = FrontendAction {
        id: "edit:item".into(),
        label: "Edit".into(),
        symbol: FrontendSymbol::Edit,
        tone: FrontendTone::Neutral,
        op: Op::SetModel {
            route: "default".into(),
        },
    };
    let item = FrontendActionListItem {
        id: "item".into(),
        text: "One note".into(),
        state: crate::protocol::FrontendListItemState::Plain,
        actions: vec![action.clone()],
    };

    assert!(validate_action_list("", std::slice::from_ref(&item)).is_err());
    assert!(validate_action_list("Notes", &[item.clone(), item.clone()]).is_err());
    let mut status = item.clone();
    status.actions.clear();
    assert!(validate_action_list("Tasks", &[status]).is_ok());
    let mut duplicate_action = item;
    duplicate_action.actions.push(action);
    assert!(validate_action_list("Notes", &[duplicate_action]).is_err());
}

#[test]
fn widget_ids_are_unique_per_capability_across_slots() {
    let content = crate::protocol::FrontendWidgetContent::Blocks {
        title: "Example".into(),
        blocks: Vec::new(),
    };
    let navigation = crate::protocol::FrontendWidget {
        id: "shared".into(),
        slot: FrontendSlot::Navigation,
        text: "Example".into(),
        tone: FrontendTone::Neutral,
        symbol: None,
        icon_only: false,
        progress: None,
        content: Some(content),
        action: None,
    };
    let mut chat_menu = navigation.clone();
    chat_menu.slot = FrontendSlot::ChatMenu;
    let contribution = FrontendContribution {
        capability: "example".into(),
        accepts_file_attachments: false,
        count: None,
        commands: Vec::new(),
        widgets: vec![navigation, chat_menu],
        references: Vec::new(),
        active_input: None,
    };

    assert!(validate_frontend(&[contribution]).is_err());
}

#[test]
fn provisional_message_target_rejects_sequence_overflow() {
    assert!(matches!(
        provisional_message_target(u64::MAX, 1),
        Err(Error::Checkpoint(message)) if message == "checkpoint sequence overflow"
    ));
}
