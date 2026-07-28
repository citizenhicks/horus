use super::*;

struct PanickingTool;

struct InterruptibleTool {
    name: &'static str,
    interruptible: bool,
}

impl Tool for InterruptibleTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: String::new(),
            parameters: serde_json::json!({}),
        }
    }

    fn interrupt_on_active_input(&self) -> bool {
        self.interruptible
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        _arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok(String::new()) })
    }
}

impl Tool for PanickingTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "panicking".into(),
            description: String::new(),
            parameters: serde_json::json!({}),
        }
    }

    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        _arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        panic!("intentional tool panic")
    }
}

#[tokio::test]
async fn parallel_tool_panic_preserves_call_identity() {
    let mut catalog = Catalog::default();
    catalog
        .register(Arc::new(PanickingTool))
        .expect("register tool");
    let calls = [ToolCall {
        call_id: "call-1".into(),
        name: "panicking".into(),
        arguments: serde_json::json!({}),
    }];
    let backend =
        Arc::new(crate::backend::sandbox::local::LocalSandbox::new(".").expect("local sandbox"));
    let sandbox = Arc::new(crate::backend::sandbox::Sandbox::new(
        backend,
        crate::backend::sandbox::ApprovalPolicy::On,
    ));
    let permissions =
        SandboxPermissions::restore(crate::backend::sandbox::NetworkAccess::Denied, Vec::new());

    assert_eq!(
        execute_batch(&catalog, &calls, sandbox, &permissions).await,
        vec![ToolResult {
            call_id: "call-1".into(),
            name: "panicking".into(),
            output: "tool panicked".into(),
            is_error: true,
        }]
    );
}

#[test]
fn only_wholly_interruptible_batches_stop_for_active_input() {
    let mut catalog = Catalog::default();
    for (name, interruptible) in [("wait", true), ("write", false)] {
        catalog
            .register(Arc::new(InterruptibleTool {
                name,
                interruptible,
            }))
            .expect("register tool");
    }
    let call = |name: &str| ToolCall {
        call_id: name.into(),
        name: name.into(),
        arguments: serde_json::json!({}),
    };

    assert!(catalog.interrupts_on_active_input(&[call("wait")]));
    assert!(!catalog.interrupts_on_active_input(&[call("wait"), call("write"),]));
    assert!(!catalog.interrupts_on_active_input(&[]));
}

#[tokio::test]
async fn edit_file_returns_a_unified_diff_after_writing() {
    let workspace = tempfile::tempdir().expect("workspace");
    let path = workspace.path().join("note.txt");
    std::fs::write(&path, "first\nold\nlast\n").expect("write fixture");
    let mut catalog = Catalog::default();
    catalog
        .register(Arc::new(EditFile))
        .expect("register edit tool");
    let sandbox = Arc::new(Sandbox::new(
        Arc::new(
            crate::backend::sandbox::local::LocalSandbox::new(workspace.path())
                .expect("local sandbox"),
        ),
        crate::backend::sandbox::ApprovalPolicy::On,
    ));
    let permissions = SandboxPermissions::restore(
        crate::backend::sandbox::NetworkAccess::Denied,
        ["call-1".into(), "call-2".into()],
    );

    let result = execute_batch(
        &catalog,
        &[ToolCall {
            call_id: "call-1".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({
                "path": "note.txt",
                "old_text": "old",
                "new_text": "new"
            }),
        }],
        Arc::clone(&sandbox),
        &permissions,
    )
    .await
    .pop()
    .expect("tool result");

    assert!(!result.is_error, "{}", result.output);
    let patch = diffy::Patch::from_str(&result.output).expect("unified diff");
    assert_eq!(
        (patch.original(), patch.modified()),
        (Some("note.txt"), Some("note.txt"))
    );
    assert!(result.output.contains("-old\n+new\n"));
    assert_eq!(
        diffy::apply("first\nold\nlast\n", &patch).expect("apply generated patch"),
        "first\nnew\nlast\n"
    );
    assert_eq!(
        std::fs::read_to_string(path).expect("read edited file"),
        "first\nnew\nlast\n"
    );

    let no_op = execute_batch(
        &catalog,
        &[ToolCall {
            call_id: "call-2".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({
                "path": "note.txt",
                "old_text": "new",
                "new_text": "new"
            }),
        }],
        sandbox,
        &permissions,
    )
    .await
    .pop()
    .expect("no-op result");
    assert!(no_op.is_error);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("note.txt")).expect("read unchanged file"),
        "first\nnew\nlast\n"
    );
}

#[test]
fn tools_do_not_claim_footer_space() {
    assert!(Tools::coding().frontend().widgets.is_empty());
}
