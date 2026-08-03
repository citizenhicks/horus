use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use horus::BoxFuture;
use horus::Error;
use horus::Result;
use horus::agent::AgentConfig;
use horus::agent::create_agent;
use horus::backend::checkpoint::Checkpoint;
use horus::backend::checkpoint::CheckpointStore;
use horus::backend::checkpoint::SessionPageRequest;
use horus::backend::checkpoint::TranscriptPageRequest;
use horus::backend::checkpoint::sqlite::SqliteCheckpoint;
use horus::backend::model::CompactOutput;
use horus::backend::model::CompactRequest;
use horus::backend::model::Model;
use horus::backend::model::ModelChoice;
use horus::backend::model::ModelEventSink;
use horus::backend::model::ModelOutput;
use horus::backend::model::ModelRequest;
use horus::backend::model::ModelRouter;
use horus::backend::sandbox::ApprovalPolicy;
use horus::backend::sandbox::CommandOutputSink;
use horus::backend::sandbox::NetworkAccess;
use horus::backend::sandbox::Sandbox;
use horus::backend::sandbox::SandboxBackend;
use horus::backend::sandbox::local::LocalSandbox;
use horus::middleware::Middleware;
use horus::middleware::MiddlewareStack;
use horus::middleware::RuntimeContext;
use horus::middleware::compaction::Compaction;
use horus::middleware::skills::Skills;
use horus::middleware::steering::Steering;
use horus::middleware::subagents::SubagentLaunch;
use horus::middleware::subagents::SubagentLauncher;
use horus::middleware::subagents::Subagents;
use horus::middleware::tools::Tools;
use horus::protocol::EventMsg;
use horus::protocol::ModelEvent;
use horus::protocol::Op;
use horus::protocol::ReviewDecision;
use horus::protocol::TokenUsage;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::Notify;

#[tokio::test]
async fn loop_executes_tool_and_returns_result_to_model() {
    let workspace = TempDir::new().expect("create workspace");
    std::fs::write(workspace.path().join("note.txt"), "hello").expect("write fixture");
    let model = Arc::new(ScriptedModel::new(vec![
        tool_response(
            "call-1",
            "read_file",
            serde_json::json!({"path": "note.txt"}),
        ),
        text_response("read hello"),
    ]));
    let mut agent = create_agent(test_config(
        workspace.path(),
        Arc::clone(&model),
        vec![Arc::new(Tools::coding())],
    ))
    .await
    .expect("create agent");

    agent
        .sender()
        .submit(Op::UserInput {
            text: "read note.txt".into(),
        })
        .expect("submit turn");

    assert_eq!(final_message(&mut agent).await, "read hello");
    assert!(
        model.requests.lock().expect("requests")[1]
            .input
            .iter()
            .any(|item| {
                item.get("type").and_then(Value::as_str) == Some("function_call_output")
                    && item.get("output").and_then(Value::as_str) == Some("hello")
            })
    );
}

#[tokio::test]
async fn middleware_prompt_is_composed_once_per_agent() {
    let workspace = TempDir::new().expect("create workspace");
    let model = Arc::new(ScriptedModel::new(vec![
        text_response("first"),
        text_response("second"),
    ]));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut agent = create_agent(test_config(
        workspace.path(),
        Arc::clone(&model),
        vec![Arc::new(PromptExtension(Arc::clone(&calls)))],
    ))
    .await
    .expect("create agent");

    for message in ["one", "two"] {
        agent
            .sender()
            .submit(Op::UserInput {
                text: message.into(),
            })
            .expect("submit turn");
        final_message(&mut agent).await;
    }

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        model
            .requests
            .lock()
            .expect("requests")
            .iter()
            .all(|request| request.instructions == "test system prompt\n\ncapability prompt")
    );
}

#[tokio::test]
async fn approval_allows_an_explicitly_approved_write() {
    let workspace = TempDir::new().expect("create workspace");
    let output_path = workspace.path().join("result.txt");
    let model = Arc::new(ScriptedModel::new(vec![
        tool_response(
            "call-1",
            "write_file",
            serde_json::json!({"path": "result.txt", "content": "approved"}),
        ),
        text_response("done"),
    ]));
    let mut agent = create_agent(test_config(
        workspace.path(),
        model,
        vec![Arc::new(Tools::coding())],
    ))
    .await
    .expect("create agent");
    let sender = agent.sender();
    sender
        .submit(Op::UserInput {
            text: "write the result".into(),
        })
        .expect("submit turn");

    while let Some(event) = agent.next_event().await {
        match event.msg {
            EventMsg::ExecApprovalRequest(request) => {
                assert!(!output_path.exists());
                sender
                    .submit(Op::ExecApproval {
                        id: request.id,
                        decision: ReviewDecision::Approved,
                    })
                    .expect("approve write");
            }
            EventMsg::TurnComplete(_) => break,
            EventMsg::Error(error) => panic!("{}", error.message),
            _ => {}
        }
    }

    assert_eq!(
        std::fs::read_to_string(output_path).expect("read result"),
        "approved"
    );
}

#[tokio::test]
async fn approval_denial_prevents_command_execution() {
    let workspace = TempDir::new().expect("create workspace");
    let output_path = workspace.path().join("denied.txt");
    let model = Arc::new(ScriptedModel::new(vec![
        tool_response(
            "call-1",
            "bash",
            serde_json::json!({"command": "printf unsafe > denied.txt"}),
        ),
        text_response("denied"),
    ]));
    let mut agent = create_agent(test_config(
        workspace.path(),
        model,
        vec![Arc::new(Tools::coding())],
    ))
    .await
    .expect("create agent");
    let sender = agent.sender();
    sender
        .submit(Op::UserInput {
            text: "run a command".into(),
        })
        .expect("submit turn");

    while let Some(event) = agent.next_event().await {
        match event.msg {
            EventMsg::ExecApprovalRequest(request) => {
                sender
                    .submit(Op::ExecApproval {
                        id: request.id,
                        decision: ReviewDecision::Denied {
                            rejection: "test denial".into(),
                        },
                    })
                    .expect("deny command");
            }
            EventMsg::TurnComplete(_) => break,
            EventMsg::Error(error) => panic!("{}", error.message),
            _ => {}
        }
    }

    assert!(!output_path.exists());
}

#[tokio::test]
async fn external_skill_resources_are_loaded_lazily() {
    let workspace = TempDir::new().expect("create workspace");
    let skill_root = TempDir::new().expect("create skill root");
    let skill_dir = skill_root.path().join("review");
    std::fs::create_dir_all(skill_dir.join("references")).expect("create skill");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review\ndescription: Review code.\n---\nRead references/details.md.",
    )
    .expect("write skill");
    std::fs::write(
        skill_dir.join("references/details.md"),
        "Always inspect every caller.",
    )
    .expect("write skill reference");
    let model = Arc::new(ScriptedModel::new(vec![
        tool_response(
            "call-1",
            "load_skill",
            serde_json::json!({"name": "review"}),
        ),
        tool_response(
            "call-2",
            "load_skill",
            serde_json::json!({"name": "review", "path": "references/details.md"}),
        ),
        text_response("review loaded"),
    ]));
    let skills = Skills::discover([skill_root.path().to_path_buf()])
        .expect("discover skill")
        .prompt("Load the relevant skill before following its instructions.")
        .expect("custom skill prompt");
    let mut agent = create_agent(test_config(
        workspace.path(),
        Arc::clone(&model),
        vec![Arc::new(skills)],
    ))
    .await
    .expect("create agent");

    agent
        .sender()
        .submit(Op::UserInput {
            text: "review this".into(),
        })
        .expect("submit turn");
    final_message(&mut agent).await;

    let requests = model.requests.lock().expect("requests");
    assert!(
        requests[0]
            .instructions
            .contains("Load the relevant skill before following its instructions.")
    );
    assert!(
        requests[1]
            .input
            .iter()
            .filter_map(|item| item.get("output").and_then(Value::as_str))
            .any(|output| output.contains("Read references/details.md."))
    );
    assert!(
        requests[2]
            .input
            .iter()
            .filter_map(|item| item.get("output").and_then(Value::as_str))
            .any(|output| output.contains("Always inspect every caller."))
    );
}

#[tokio::test]
async fn async_subagent_uses_configured_model_reasoning_and_durable_fork() {
    let workspace = TempDir::new().expect("create workspace");
    let root_model = Arc::new(ScriptedModel::new(vec![
        tool_response(
            "call-spawn",
            "spawn_agent",
            serde_json::json!({
                "task_name": "cheap",
                "message": "solve child task",
                "fork_turns": "none"
            }),
        ),
        tool_response(
            "call-wait",
            "wait_agent",
            serde_json::json!({"timeout_ms": 10_000}),
        ),
        text_response("root complete"),
    ]));
    let unused_child_model = Arc::new(ScriptedModel::new(Vec::new()));
    let child_model = Arc::new(ScriptedModel::new(vec![text_response("child complete")]));
    let root_route: Arc<dyn Model> = root_model.clone();
    let child_route: Arc<dyn Model> = unused_child_model;
    let child_high_route: Arc<dyn Model> = child_model.clone();
    let mut routes = ModelRouter::new("root", root_route);
    routes
        .register("child", child_route)
        .expect("register child route");
    routes
        .register("child-high", child_high_route)
        .expect("register child reasoning route");
    for choice in [
        ModelChoice {
            route: "root".into(),
            group: "root".into(),
            model: "root".into(),
            reasoning_effort: None,
            context_window: None,
        },
        ModelChoice {
            route: "child".into(),
            group: "child".into(),
            model: "child".into(),
            reasoning_effort: Some("low".into()),
            context_window: None,
        },
        ModelChoice {
            route: "child-high".into(),
            group: "child".into(),
            model: "child".into(),
            reasoning_effort: Some("high".into()),
            context_window: None,
        },
    ] {
        routes
            .configure_choice(choice)
            .expect("configure model choice");
    }
    let sandbox = Arc::new(Sandbox::new(
        Arc::new(LocalSandbox::new(workspace.path()).expect("local sandbox")),
        ApprovalPolicy::On,
    ));
    let checkpoint_store = Arc::new(
        SqliteCheckpoint::new(workspace.path().join("subagents.sqlite3"))
            .expect("open checkpoint database"),
    );
    let checkpoints: Arc<dyn CheckpointStore> = checkpoint_store.clone();
    let template = Arc::new(OnceLock::<AgentConfig>::new());
    let child_template = Arc::downgrade(&template);
    let launcher: SubagentLauncher = Arc::new(move |launch: SubagentLaunch| {
        let child_template = child_template.clone();
        Box::pin(async move {
            let config = child_template
                .upgrade()
                .expect("subagent template owner")
                .get()
                .expect("subagent template")
                .clone()
                .session_id(launch.session_id)
                .metadata(launch.metadata)
                .model_route(&launch.model, launch.reasoning_effort.as_deref())?;
            create_agent(config).await
        })
    });
    let config = AgentConfig::new(
        Arc::new(routes),
        sandbox,
        checkpoints,
        MiddlewareStack::new(vec![Arc::new(
            Subagents::new(1, 21, 64, launcher)
                .expect("subagents")
                .default_model("child")
                .default_reasoning("high")
                .expect("subagent reasoning"),
        )])
        .expect("middleware"),
        "test system prompt",
    )
    .session_id("root");
    assert!(template.set(config.clone()).is_ok());
    let mut agent = create_agent(config).await.expect("create agent");

    agent
        .sender()
        .submit(Op::UserInput {
            text: "delegate cheaply".into(),
        })
        .expect("submit turn");

    let message = final_message(&mut agent).await;
    let sessions = checkpoint_store
        .list_sessions_page(SessionPageRequest {
            cursor: None,
            limit: 10,
        })
        .await
        .expect("list sessions");
    let child = sessions
        .sessions
        .iter()
        .find(|session| session.session_id != "root")
        .expect("child session");

    assert_eq!(
        (
            message,
            child_model.requests.lock().expect("child requests").len(),
            child.parent_session_id.as_deref(),
        ),
        ("root complete".to_string(), 1, Some("root"))
    );
}

#[tokio::test]
async fn steering_is_injected_before_native_compaction() {
    let workspace = TempDir::new().expect("create workspace");
    let first = text_response_with_usage("draft", usage(100));
    let scripted = Arc::new(ScriptedModel::with_compaction(
        vec![first, text_response("done")],
        vec![
            CompactOutput::from_output(
                vec![serde_json::json!({
                    "type": "compaction",
                    "encrypted_content": "opaque"
                })],
                usage(100),
            )
            .expect("compaction output"),
        ],
    ));
    let model = Arc::new(GatedModel {
        inner: Arc::clone(&scripted),
        first: AtomicBool::new(true),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let mut agent = create_agent(test_config(
        workspace.path(),
        Arc::clone(&model),
        vec![
            Arc::new(Steering::default()),
            Arc::new(Compaction::new(50).expect("compaction")),
        ],
    ))
    .await
    .expect("create agent");
    let sender = agent.sender();
    sender
        .submit(Op::UserInput {
            text: "start".into(),
        })
        .expect("submit turn");

    let turn_id = loop {
        match agent.next_event().await.expect("turn event").msg {
            EventMsg::TurnStarted(turn) => break turn.turn_id,
            EventMsg::Error(error) => panic!("{}", error.message),
            _ => {}
        }
    };
    model.entered.notified().await;
    sender
        .submit(Op::ActiveInput {
            operation: "steer".into(),
            turn_id,
            text: "steered".into(),
        })
        .expect("steer active turn");
    model.release.notify_one();

    assert_eq!(final_message(&mut agent).await, "done");
    let requests = scripted.compact_requests.lock().expect("compact requests");
    assert_eq!(requests.len(), 1);
    assert!(
        serde_json::to_string(&requests[0].input)
            .expect("serialize compact input")
            .contains("steered")
    );
}

#[tokio::test]
async fn compaction_uses_the_context_window_of_a_new_model_route() {
    let workspace = TempDir::new().expect("create workspace");
    let large = Arc::new(ScriptedModel::new(vec![text_response("draft")]));
    let small = Arc::new(ScriptedModel::with_compaction(
        vec![text_response("done")],
        vec![
            CompactOutput::from_output(
                vec![serde_json::json!({
                    "type": "compaction",
                    "encrypted_content": "opaque"
                })],
                usage(10),
            )
            .expect("compaction output"),
        ],
    ));
    let large_model: Arc<dyn Model> = large.clone();
    let small_model: Arc<dyn Model> = small.clone();
    let mut router = ModelRouter::new("large", large_model);
    router.register("small", small_model).expect("small route");
    for (route, context_window) in [("large", 300_000), ("small", 8_000)] {
        router
            .configure_choice(ModelChoice {
                route: route.into(),
                group: route.into(),
                model: route.into(),
                reasoning_effort: None,
                context_window: Some(context_window),
            })
            .expect("route metadata");
    }
    let mut agent = create_agent(test_config_with_router(
        workspace.path(),
        router,
        vec![Arc::new(Compaction::default())],
    ))
    .await
    .expect("create agent");

    agent
        .sender()
        .submit(Op::UserInput {
            text: "first".into(),
        })
        .expect("submit first turn");
    assert_eq!(final_message(&mut agent).await, "draft");
    agent
        .sender()
        .submit(Op::SetModel {
            route: "small".into(),
        })
        .expect("select small route");
    agent
        .sender()
        .submit(Op::UserInput {
            text: "second".into(),
        })
        .expect("submit second turn");

    assert_eq!(final_message(&mut agent).await, "done");
    assert!(
        large
            .compact_requests
            .lock()
            .expect("large compact")
            .is_empty()
    );
    assert_eq!(
        small.compact_requests.lock().expect("small compact").len(),
        1
    );
}

#[tokio::test]
async fn interrupt_only_aborts_its_target_turn() {
    let workspace = TempDir::new().expect("create workspace");
    let scripted = Arc::new(ScriptedModel::new(vec![text_response("unused")]));
    let model = Arc::new(GatedModel {
        inner: scripted,
        first: AtomicBool::new(true),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let mut agent = create_agent(test_config(
        workspace.path(),
        Arc::clone(&model),
        Vec::new(),
    ))
    .await
    .expect("create agent");
    let sender = agent.sender();
    sender
        .submit(Op::UserInput {
            text: "start".into(),
        })
        .expect("submit turn");

    let configured = agent.next_event().await.expect("session event");
    assert!(configured.submission_id.is_none());
    let turn_id = loop {
        let event = agent.next_event().await.expect("turn event");
        if let EventMsg::TurnStarted(turn) = event.msg {
            break turn.turn_id;
        }
    };
    model.entered.notified().await;

    let stale_submission = sender
        .submit(Op::Interrupt {
            turn_id: "stale-turn".into(),
        })
        .expect("submit stale interrupt");
    loop {
        let event = agent.next_event().await.expect("stale interrupt event");
        if let EventMsg::Warning(warning) = event.msg {
            assert_eq!(
                (event.submission_id, warning.message),
                (
                    Some(stale_submission),
                    "interrupt targeted a stale turn".to_string()
                )
            );
            break;
        }
    }

    let interrupt_submission = sender
        .submit(Op::Interrupt {
            turn_id: turn_id.clone(),
        })
        .expect("submit targeted interrupt");
    loop {
        let event = agent.next_event().await.expect("interrupt event");
        if let EventMsg::TurnAborted(turn) = event.msg {
            assert_eq!(
                (event.submission_id, turn.turn_id),
                (Some(interrupt_submission), turn_id)
            );
            break;
        }
    }
}

#[tokio::test]
async fn compaction_falls_back_to_a_model_summary_and_keeps_recent_context() {
    let workspace = TempDir::new().expect("create workspace");
    let first = text_response_with_usage("draft", usage(40_000));
    let model = Arc::new(ScriptedModel::new(vec![
        first,
        text_response("## Goal\nContinue the task."),
        text_response("done"),
    ]));
    let mut agent = create_agent(test_config(
        workspace.path(),
        Arc::clone(&model),
        vec![Arc::new(
            Compaction::new(30_000).expect("compaction middleware"),
        )],
    ))
    .await
    .expect("create agent");

    agent
        .sender()
        .submit(Op::UserInput {
            text: "x".repeat(82_000),
        })
        .expect("submit first turn");
    assert_eq!(final_message(&mut agent).await, "draft");
    agent
        .sender()
        .submit(Op::UserInput {
            text: "continue".into(),
        })
        .expect("submit second turn");
    assert_eq!(final_message(&mut agent).await, "done");

    let requests = model.requests.lock().expect("requests");
    assert_eq!(requests.len(), 3);
    assert!(requests[1].instructions.contains("Summarize coding-agent"));
    let rebuilt = serde_json::to_string(&requests[2].input).expect("serialize rebuilt context");
    assert!(rebuilt.contains("<compacted_context>"));
    assert!(rebuilt.contains("continue"));
    assert!(
        model
            .compact_requests
            .lock()
            .expect("compact requests")
            .is_empty()
    );
}

#[tokio::test]
async fn local_sandbox_rejects_parent_path_escape() {
    let workspace = TempDir::new().expect("create workspace");
    let sandbox = LocalSandbox::new(workspace.path()).expect("sandbox");

    let error = sandbox.read("../outside").await.expect_err("reject escape");

    assert!(matches!(error, Error::Sandbox(_)));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn local_sandbox_confines_command_writes_to_the_workspace() {
    if std::env::var("CODEX_SANDBOX").as_deref() == Ok("seatbelt") {
        return;
    }
    let workspace = TempDir::new().expect("create workspace");
    let outside = workspace.path().parent().expect("parent").join(format!(
        "{}-outside.txt",
        workspace
            .path()
            .file_name()
            .expect("workspace name")
            .to_string_lossy()
    ));
    let probe = outside.with_extension("probe");
    let can_write_outside = std::process::Command::new("sh")
        .args(["-c", "printf probe > \"$1\"", "sh"])
        .arg(&probe)
        .status()
        .is_ok_and(|status| status.success());
    if !can_write_outside {
        return;
    }
    std::fs::remove_file(&probe).expect("clean sandbox probe");
    let sandbox = LocalSandbox::new(workspace.path()).expect("sandbox");

    let output = sandbox
        .execute(
            &format!(
                "printf horus > command.txt; printf blocked > ../{}",
                outside.file_name().expect("outside name").to_string_lossy()
            ),
            NetworkAccess::Denied,
            CommandOutputSink::default(),
        )
        .await
        .expect("execute sandboxed command");
    let escaped = outside.exists();
    if escaped {
        std::fs::remove_file(&outside).expect("clean escaped file");
    }

    let workspace_output = std::fs::read_to_string(workspace.path().join("command.txt"))
        .unwrap_or_else(|error| {
            panic!("read workspace output: {error}; command output: {output:?}")
        });
    assert_eq!(workspace_output, "horus");
    assert!(!escaped);
}

#[tokio::test]
async fn sqlite_persists_latest_checkpoint_transcript_and_fork_lineage() {
    let workspace = TempDir::new().expect("create workspace");
    let path = workspace.path().join("horus.sqlite3");
    let store = SqliteCheckpoint::new(&path).expect("open checkpoint database");
    let empty = Checkpoint::empty("session");
    store
        .save(&empty, &[])
        .await
        .expect("save empty checkpoint");
    let first_user = horus::backend::model::user_message("parent question");
    let assistant = serde_json::json!({
        "role": "assistant",
        "content": [{"type": "output_text", "text": "parent answer"}]
    });
    let mut first = empty.clone();
    first.sequence = 1;
    first.first_user_message = Some("parent question".into());
    first.context.push(first_user.clone());
    store
        .save(&first, std::slice::from_ref(&first_user))
        .await
        .expect("save first message");
    let mut state_only = first.clone();
    state_only.sequence = 2;
    state_only.total_usage.input_tokens = 1;
    state_only.catalog_visible = false;
    store.save(&state_only, &[]).await.expect("save state only");
    let mut grown = state_only.clone();
    grown.sequence = 3;
    grown.context.push(assistant.clone());
    store
        .save(&grown, std::slice::from_ref(&assistant))
        .await
        .expect("grow context");
    let compacted_item = serde_json::json!({
        "role": "assistant",
        "content": [{"type": "output_text", "text": "compact summary"}]
    });
    let post_compaction = horus::backend::model::user_message("parent follow-up");
    let mut compacted = grown;
    compacted.sequence = 4;
    compacted.context = vec![compacted_item.clone(), post_compaction.clone()];
    store
        .save(&compacted, std::slice::from_ref(&post_compaction))
        .await
        .expect("replace context and append transcript");
    let branch_user = horus::backend::model::user_message("branch question");
    let latest = compacted;
    let mut branch = Checkpoint::empty("branch");
    branch.context.clone_from(&latest.context);
    let branch_summary = store
        .fork("session", latest.sequence, &branch)
        .await
        .expect("fork session");
    let mut branch_latest = branch.clone();
    branch_latest.sequence = 1;
    branch_latest.first_user_message = Some("branch question".into());
    branch_latest.context.push(branch_user.clone());
    store
        .save(&branch_latest, std::slice::from_ref(&branch_user))
        .await
        .expect("append branch message");
    drop(store);

    let store = SqliteCheckpoint::new(&path).expect("reopen checkpoint database");
    let sessions = store
        .list_sessions_page(SessionPageRequest {
            cursor: None,
            limit: 100,
        })
        .await
        .expect("list sessions")
        .sessions;
    let parent_summary = sessions
        .iter()
        .find(|session| session.session_id == "session")
        .expect("parent summary");
    let persisted_branch = sessions
        .iter()
        .find(|session| session.session_id == "branch")
        .expect("branch summary");
    let parent_transcript = store
        .transcript_page(
            "session",
            TranscriptPageRequest {
                before_sequence: None,
                max_batches: 100,
            },
        )
        .await
        .expect("load transcript")
        .into_items_chronological();
    let branch_transcript = store
        .transcript_page(
            "branch",
            TranscriptPageRequest {
                before_sequence: None,
                max_batches: 100,
            },
        )
        .await
        .expect("load branch transcript")
        .into_items_chronological();

    assert_eq!(
        (
            store.load("session").await.expect("load checkpoint"),
            parent_transcript,
            store.load("branch").await.expect("load branch"),
            branch_transcript,
            parent_summary.first_user_message.as_deref(),
            persisted_branch.first_user_message.as_deref(),
            parent_summary.catalog_visible,
            persisted_branch.catalog_visible,
            branch_summary.first_user_message.as_deref(),
            branch_summary.parent_session_id.as_deref(),
            branch_summary.parent_sequence,
        ),
        (
            Some(latest.clone()),
            vec![
                first_user.clone(),
                assistant.clone(),
                post_compaction.clone()
            ],
            Some(branch_latest),
            vec![compacted_item, post_compaction, branch_user],
            Some("parent question"),
            Some("branch question"),
            false,
            true,
            None,
            Some("session"),
            Some(4),
        )
    );
}

struct ScriptedModel {
    responses: Mutex<VecDeque<ModelOutput>>,
    compact_outputs: Mutex<VecDeque<CompactOutput>>,
    requests: Mutex<Vec<RecordedRequest>>,
    compact_requests: Mutex<Vec<RecordedRequest>>,
    compaction_endpoint: bool,
}

struct RecordedRequest {
    instructions: String,
    input: Vec<Value>,
}

struct PromptExtension(Arc<AtomicUsize>);

impl Middleware for PromptExtension {
    fn name(&self) -> &'static str {
        "prompt_extension"
    }

    fn prompt_fragment(&self, _runtime: &RuntimeContext) -> Result<Option<String>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Some("capability prompt".into()))
    }
}

impl ScriptedModel {
    fn new(responses: Vec<ModelOutput>) -> Self {
        Self::with_compaction(responses, Vec::new())
    }

    fn with_compaction(responses: Vec<ModelOutput>, compact_outputs: Vec<CompactOutput>) -> Self {
        let compaction_endpoint = !compact_outputs.is_empty();
        Self {
            responses: Mutex::new(responses.into()),
            compact_outputs: Mutex::new(compact_outputs.into()),
            requests: Mutex::new(Vec::new()),
            compact_requests: Mutex::new(Vec::new()),
            compaction_endpoint,
        }
    }
}

impl Model for ScriptedModel {
    fn respond<'a>(
        &'a self,
        request: ModelRequest<'a>,
        events: ModelEventSink,
    ) -> BoxFuture<'a, Result<ModelOutput>> {
        Box::pin(async move {
            self.requests
                .lock()
                .expect("requests")
                .push(RecordedRequest {
                    instructions: request.instructions.into(),
                    input: request.input.to_vec(),
                });
            let output = self
                .responses
                .lock()
                .expect("responses")
                .pop_front()
                .ok_or_else(|| Error::Provider("script exhausted".into()))?;
            if !output.text().is_empty() {
                events(ModelEvent::TextDelta(output.text().into()))?;
            }
            Ok(output)
        })
    }

    fn compaction_endpoint(&self) -> bool {
        self.compaction_endpoint
    }

    fn compact<'a>(&'a self, request: CompactRequest<'a>) -> BoxFuture<'a, Result<CompactOutput>> {
        Box::pin(async move {
            self.compact_requests
                .lock()
                .expect("compact requests")
                .push(RecordedRequest {
                    instructions: request.instructions.into(),
                    input: request.input.to_vec(),
                });
            self.compact_outputs
                .lock()
                .expect("compact outputs")
                .pop_front()
                .ok_or_else(|| Error::Provider("compact script exhausted".into()))
        })
    }
}

struct GatedModel {
    inner: Arc<ScriptedModel>,
    first: AtomicBool,
    entered: Notify,
    release: Notify,
}

impl Model for GatedModel {
    fn respond<'a>(
        &'a self,
        request: ModelRequest<'a>,
        events: ModelEventSink,
    ) -> BoxFuture<'a, Result<ModelOutput>> {
        Box::pin(async move {
            if self.first.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.inner.respond(request, events).await
        })
    }

    fn compaction_endpoint(&self) -> bool {
        self.inner.compaction_endpoint()
    }

    fn compact<'a>(&'a self, request: CompactRequest<'a>) -> BoxFuture<'a, Result<CompactOutput>> {
        self.inner.compact(request)
    }
}

#[derive(Default)]
struct MemoryCheckpoints {
    sessions: Mutex<BTreeMap<String, Checkpoint>>,
    state: Mutex<BTreeMap<(String, String), Value>>,
}

impl CheckpointStore for MemoryCheckpoints {
    fn load<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<Option<Checkpoint>>> {
        Box::pin(async move {
            Ok(self
                .sessions
                .lock()
                .expect("checkpoint store")
                .get(session_id)
                .cloned())
        })
    }

    fn save<'a>(
        &'a self,
        checkpoint: &'a Checkpoint,
        _transcript_delta: &'a [Value],
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.sessions
                .lock()
                .expect("checkpoint store")
                .insert(checkpoint.session_id.clone(), checkpoint.clone());
            Ok(())
        })
    }

    fn load_state<'a>(
        &'a self,
        scope: &'a str,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Option<Value>>> {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .expect("checkpoint state")
                .get(&(scope.to_string(), key.to_string()))
                .cloned())
        })
    }

    fn save_state<'a>(
        &'a self,
        scope: &'a str,
        key: &'a str,
        value: &'a Value,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.state
                .lock()
                .expect("checkpoint state")
                .insert((scope.to_string(), key.to_string()), value.clone());
            Ok(())
        })
    }
}

fn test_config<M>(
    workspace: &std::path::Path,
    model: Arc<M>,
    middleware: Vec<Arc<dyn Middleware>>,
) -> AgentConfig
where
    M: Model + 'static,
{
    let model: Arc<dyn Model> = model;
    test_config_with_router(workspace, ModelRouter::new("test", model), middleware)
}

fn test_config_with_router(
    workspace: &std::path::Path,
    model: ModelRouter,
    middleware: Vec<Arc<dyn Middleware>>,
) -> AgentConfig {
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(MemoryCheckpoints::default());
    let sandbox = Arc::new(Sandbox::new(
        Arc::new(LocalSandbox::new(workspace).expect("local sandbox")),
        ApprovalPolicy::On,
    ));
    AgentConfig::new(
        Arc::new(model),
        sandbox,
        checkpoints,
        MiddlewareStack::new(middleware).expect("middleware"),
        "test system prompt",
    )
}

async fn final_message(agent: &mut horus::agent::Agent) -> String {
    let mut message = String::new();
    while let Some(event) = agent.next_event().await {
        match event.msg {
            EventMsg::AgentMessage(event) => message = event.message,
            EventMsg::TurnComplete(_) => return message,
            EventMsg::Error(error) => panic!("{}", error.message),
            _ => {}
        }
    }
    panic!("agent disconnected")
}

fn tool_response(call_id: &str, name: &str, arguments: Value) -> ModelOutput {
    ModelOutput::from_output(
        vec![serde_json::json!({
            "type": "function_call",
            "call_id": call_id,
            "name": name,
            "arguments": arguments.to_string()
        })],
        false,
        usage(10),
    )
    .expect("valid tool response")
}

fn text_response(text: &str) -> ModelOutput {
    text_response_with_usage(text, usage(10))
}

fn text_response_with_usage(text: &str, usage: TokenUsage) -> ModelOutput {
    ModelOutput::from_output(
        vec![serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}]
        })],
        true,
        usage,
    )
    .expect("valid text response")
}

fn usage(input_tokens: i64) -> TokenUsage {
    TokenUsage {
        input_tokens,
        total_tokens: input_tokens + 1,
        output_tokens: 1,
        ..TokenUsage::default()
    }
}
