//! Active Input agent runtime tests.

use super::*;

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
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        drain_until_notified(&mut agent, &started),
    )
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
    assert!(saved.active_model_step.is_some());
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
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        drain_until_notified(&mut agent, &started),
    )
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
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let event = agent.next_event().await.expect("queue event");
            if event.submission_id.as_deref() == Some(queued_submission.as_str()) {
                break;
            }
        }
    })
    .await
    .expect("active input event delivered while hook was blocked");
    release.notify_one();

    assert!(saved.pending_input.iter().any(|item| {
        item.owner() == "queueing"
            && item.id() == queued_submission
            && item.text() == "survive the hook"
    }));
}

#[tokio::test]
async fn active_changes_precede_later_hook_events_in_recorded_order() {
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
    let agent = create_agent(
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
    let (sender, mut events) = agent.into_recorded_parts();
    sender
        .submit(Op::UserInput {
            text: "start".into(),
            attachments: Vec::new(),
        })
        .expect("start turn");
    let turn_id = loop {
        let event = events.recv().await.expect("turn event");
        if let EventMsg::TurnStarted(started) = event.event.msg {
            break started.turn_id;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            tokio::select! {
                () = started.notified() => break,
                event = events.recv() => {
                    event.expect("agent event while waiting");
                }
            }
        }
    })
    .await
    .expect("tail hook started");

    let queued_submission = sender
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

    let (order, sequences, live_target) =
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            let mut order = Vec::new();
            let mut sequences = Vec::new();
            let mut live_target = None;
            while order.len() < 2 || live_target.is_none() {
                let record = events.recv().await.expect("frontend event");
                let sequence = record.sequence;
                let event = record.event;
                match event.msg {
                    EventMsg::Frontend(FrontendEvent::RemoveWidget { capability, id })
                        if capability == "steering" && id == "old" =>
                    {
                        order.push("remove");
                        sequences.push(sequence);
                    }
                    EventMsg::Frontend(FrontendEvent::Widget { capability, item })
                        if capability == "steering"
                            && item.id == queued_submission
                            && event.submission_id.as_deref() == Some(&queued_submission) =>
                    {
                        order.push("widget");
                        sequences.push(sequence);
                    }
                    EventMsg::UserMessage(message) if message.message == "old input" => {
                        live_target = message.message_target;
                    }
                    _ => {}
                }
            }
            (order, sequences, live_target.expect("live message target"))
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
    assert_eq!(order, ["widget", "remove"]);
    assert!(sequences[0] < sequences[1]);
    assert_eq!(live_target, replay_target);
}
