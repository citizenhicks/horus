use std::path::Path;
use std::sync::Arc;

use super::AgentConfig;
use super::AgentSender;
use super::create_agent;
use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::backend::checkpoint::Checkpoint;
use crate::backend::checkpoint::CheckpointStore;
use crate::backend::checkpoint::sqlite::SqliteCheckpoint;
use crate::backend::model::Model;
use crate::backend::model::ModelEventSink;
use crate::backend::model::ModelOutput;
use crate::backend::model::ModelRequest;
use crate::backend::model::ModelRouter;
use crate::backend::model::TOOL_ERROR_FIELD;
use crate::backend::model::ToolCall;
use crate::backend::sandbox::ApprovalPolicy;
use crate::backend::sandbox::Sandbox;
use crate::backend::sandbox::local::LocalSandbox;
use crate::middleware::MiddlewareStack;
use crate::protocol::EventMsg;
use crate::protocol::MAX_USER_INPUT_BYTES;
use crate::protocol::Op;
use crate::protocol::ToolCallEndEvent;

struct TestModel;

#[test]
fn sender_rejects_oversized_input_before_queueing() {
    let (commands, _receiver) = tokio::sync::mpsc::channel(1);
    let sender = AgentSender { commands };

    assert!(
        sender
            .submit(Op::UserInput {
                text: "x".repeat(MAX_USER_INPUT_BYTES + 1),
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
        })
        .expect("fill queue");

    let error = sender
        .submit(Op::UserInput {
            text: "second".into(),
        })
        .expect_err("queue should be full");

    assert!(matches!(error, Error::Busy(_)));
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

fn config(
    workspace: &Path,
    checkpoints: Arc<dyn CheckpointStore>,
    session_id: &str,
) -> AgentConfig {
    AgentConfig::new(
        Arc::new(ModelRouter::new("test", Arc::new(TestModel))),
        Arc::new(Sandbox::new(
            Arc::new(LocalSandbox::new(workspace).expect("local sandbox")),
            ApprovalPolicy::On,
        )),
        checkpoints,
        MiddlewareStack::new(Vec::new()).expect("middleware"),
        "test prompt",
    )
    .session_id(session_id)
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
        .save(&winner, &winner.context)
        .await
        .expect("save competing checkpoint");

    let (sender, mut events) = agent.into_parts();
    sender
        .submit(Op::UserInput {
            text: "lose the checkpoint race".into(),
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
    target.active_turn_id = Some("turn-1".into());
    target.context.push(serde_json::json!({
        "type": "function_call",
        "call_id": call.call_id.clone(),
        "name": call.name.clone(),
        "arguments": call.arguments.to_string()
    }));
    target.pending_tools.push(call);
    checkpoints
        .save(&target, &target.context)
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
            })
        ] if begin.turn_id == "turn-1"
            && begin.call_id == "call-1"
            && turn_id == &begin.turn_id
            && call_id == &begin.call_id
            && output == "execution interrupted; result unknown after restart"
    ));

    let recovered = checkpoints
        .load("target")
        .await
        .expect("load checkpoint")
        .expect("recovered checkpoint");
    assert!(recovered.active_turn_id.is_none());
    assert!(recovered.pending_tools.is_empty());
    assert_eq!(
        recovered.context.last().and_then(|item| {
            item.get(TOOL_ERROR_FIELD)
                .and_then(serde_json::Value::as_bool)
        }),
        Some(true)
    );
}
