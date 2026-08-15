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
        session_context: SessionContext::default(),
        metadata: BTreeMap::new(),
        queued_input: QueuedInputSnapshot::default(),
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
