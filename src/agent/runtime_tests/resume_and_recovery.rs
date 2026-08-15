//! Resume And Recovery agent runtime tests.

use super::*;

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
async fn model_route_change_is_recorded_with_its_checkpoint() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();
    let mut agent = create_agent(config_with_two_routes(
        workspace.path(),
        checkpoint_store,
        "route-change",
        "kimi-k3",
        "kimi-k2.7",
    ))
    .await
    .expect("create agent");
    agent.next_event().await.expect("configured event");
    let submission_id = agent
        .sender()
        .submit(Op::SetModel {
            route: "kimi-k2.7".into(),
        })
        .expect("change route");

    let changed = loop {
        let event = agent.next_event().await.expect("model changed event");
        if event.submission_id.as_deref() == Some(&submission_id) {
            break event;
        }
    };
    let saved = checkpoints
        .load("route-change")
        .await
        .expect("load checkpoint")
        .expect("saved checkpoint");
    let recorded = checkpoints
        .event_page(
            "route-change",
            EventPageRequest {
                before_sequence: None,
                limit: 1,
            },
        )
        .await
        .expect("event journal")
        .events
        .pop()
        .expect("recorded model change");

    assert!(matches!(
        changed.msg,
        EventMsg::ModelChanged(event) if event.route == "kimi-k2.7"
    ));
    assert_eq!(saved.model_route.as_deref(), Some("kimi-k2.7"));
    assert_eq!(recorded.event.submission_id, Some(submission_id));
    assert!(matches!(
        recorded.event.msg,
        EventMsg::ModelChanged(event) if event.route == "kimi-k2.7"
    ));
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
async fn zero_replay_mode_emits_uncertain_tool_recovery_as_individual_events() {
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

    let mut agent = create_agent(
        config(workspace.path(), checkpoint_store, "target").initial_replay_batches(0),
    )
    .await
    .expect("resume agent");
    assert!(matches!(
        agent.next_event().await.expect("session event").msg,
        EventMsg::SessionConfigured(_)
    ));
    let history = [
        agent.next_event().await.expect("tool end").msg,
        agent.next_event().await.expect("queued user message").msg,
    ];
    assert!(matches!(
        history.as_slice(),
        [
            EventMsg::ToolCallEnd(ToolCallEndEvent {
                turn_id,
                call_id,
                output,
                is_error: true,
                ..
            }),
            EventMsg::UserMessage(user)
        ] if turn_id == "turn-1"
            && call_id == "call-1"
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

#[tokio::test]
async fn restart_closes_an_active_model_step_with_the_recovery_checkpoint() {
    let workspace = tempfile::tempdir().expect("workspace");
    let checkpoints = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("checkpoints.sqlite3"))
            .expect("checkpoint store"),
    );
    let mut checkpoint = Checkpoint::empty("recover-step");
    checkpoint.active_execution = Some(crate::backend::checkpoint::ActiveExecution {
        submission_id: "submission-1".into(),
        turn_id: "turn-1".into(),
        started_at_ms: 10,
        model_calls: 1,
        tool_calls: 0,
        failed_tool_calls: 0,
        usage: TokenUsage::default(),
    });
    checkpoint.active_model_step = Some(crate::backend::checkpoint::ActiveModelStep {
        model_step_id: "step-1".into(),
        step_index: 0,
        started_at_ms: 20,
    });
    checkpoints
        .save(&checkpoint, &[], None)
        .await
        .expect("save active step");
    let checkpoint_store: Arc<dyn CheckpointStore> = checkpoints.clone();

    let mut agent = create_agent(
        config(workspace.path(), checkpoint_store, "recover-step").initial_replay_batches(0),
    )
    .await
    .expect("recover agent");
    let configured = agent.next_event().await.expect("session event");
    let completed = agent.next_event().await.expect("step completion");
    let aborted = agent.next_event().await.expect("turn abort");
    let saved = checkpoints
        .load("recover-step")
        .await
        .expect("load checkpoint")
        .expect("recovered checkpoint");

    assert!(matches!(configured.msg, EventMsg::SessionConfigured(_)));
    assert!(matches!(
        completed.msg,
        EventMsg::ModelStepCompleted(event)
            if event.model_step_id == "step-1"
                && event.outcome == ModelStepOutcome::Interrupted
    ));
    assert!(matches!(
        aborted.msg,
        EventMsg::TurnAborted(event) if event.turn_id == "turn-1"
    ));
    assert!(saved.active_execution.is_none());
    assert!(saved.active_model_step.is_none());
}
