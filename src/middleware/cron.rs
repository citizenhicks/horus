//! Conversational recurring-task setup.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use super::manifest::MiddlewareManifest;
use super::tools::{
    ApprovalRequirement, Catalog, Tool, ToolContext, labeled_tool_heading, render_tool_event,
};
use super::{Middleware, RuntimeContext};
use crate::backend::model::ToolDefinition;
use crate::protocol::{EventMsg, FrontendBlock};
use crate::{BoxFuture, Result};

const PROMPT: &str = "Use `schedule_task` only during an explicit recurring-task setup. During \
                      setup, ask only for missing task or timing details, then call it once with \
                      standalone task instructions and a five-field cron expression in the \
                      host's local time. Outside explicit setup, never call it.";

/// Configuration and presentation metadata for scheduled work.
pub const MANIFEST: MiddlewareManifest = MiddlewareManifest {
    id: "cron",
    label: "Scheduling",
    description: "Schedule recurring agent work; always available",
    required: true,
    default_enabled: true,
    settings: &[],
};

type TaskWriter = dyn Fn(&str, &str, &str) -> Result<String> + Send + Sync;

/// Lets the model turn a confirmed conversation into a recurring task.
pub struct Cron {
    write: Arc<TaskWriter>,
}

impl Cron {
    /// Creates recurring-task middleware backed by the host's task writer.
    pub fn new(write: impl Fn(&str, &str, &str) -> Result<String> + Send + Sync + 'static) -> Self {
        Self {
            write: Arc::new(write),
        }
    }
}

impl Middleware for Cron {
    fn name(&self) -> &'static str {
        MANIFEST.id
    }

    fn register(&self, catalog: &mut Catalog, runtime: &RuntimeContext) -> Result<()> {
        catalog.register(Arc::new(ScheduleTask {
            write: Arc::clone(&self.write),
            source_session_id: runtime.session_id.clone(),
        }))
    }

    fn prompt_fragment(&self, _runtime: &RuntimeContext) -> Result<Option<String>> {
        Ok(Some(PROMPT.into()))
    }

    fn render(&self, event: &EventMsg, _session_id: &str) -> Option<FrontendBlock> {
        render_tool_event(
            event,
            |name| name == "schedule_task",
            |_, arguments| labeled_tool_heading("Schedule", "schedule", arguments),
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleTaskArgs {
    task: String,
    schedule: String,
}

struct ScheduleTask {
    write: Arc<TaskWriter>,
    source_session_id: String,
}

impl Tool for ScheduleTask {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "schedule_task".into(),
            description: "Save the recurring task confirmed during explicit setup.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "Complete standalone task instructions in Markdown."
                    },
                    "schedule": {
                        "type": "string",
                        "description": "Five-field cron expression evaluated in the host's local time."
                    }
                },
                "required": ["task", "schedule"],
                "additionalProperties": false
            }),
        }
    }

    fn approval(&self) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: ScheduleTaskArgs = serde_json::from_value(arguments)?;
            let id = (self.write)(
                &self.source_session_id,
                &arguments.task,
                &arguments.schedule,
            )?;
            Ok(format!("scheduled `{}` as task {id}", arguments.schedule))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::backend::sandbox::local::LocalSandbox;
    use crate::backend::sandbox::{ApprovalPolicy, NetworkAccess, Sandbox, SandboxPermissions};

    #[test]
    fn schedule_task_events_render_as_cron_blocks() {
        let middleware = Cron::new(|_, _, _| Ok("task".into()));

        for event in [
            EventMsg::ToolCallBegin(crate::protocol::ToolCallBeginEvent {
                turn_id: "turn".into(),
                call_id: "call".into(),
                name: "schedule_task".into(),
                arguments: serde_json::json!({"schedule": "0 9 * * *"}),
            }),
            EventMsg::ToolCallEnd(crate::protocol::ToolCallEndEvent {
                turn_id: "turn".into(),
                call_id: "call".into(),
                name: "schedule_task".into(),
                output: String::new(),
                is_error: false,
            }),
        ] {
            assert!(middleware.render(&event, "session").is_some());
        }
    }

    #[tokio::test]
    async fn schedule_task_uses_the_injected_writer_and_requires_approval() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&calls);
        let tool = ScheduleTask {
            write: Arc::new(move |session, task, schedule| {
                recorded.lock().expect("calls").push((
                    session.to_string(),
                    task.to_string(),
                    schedule.to_string(),
                ));
                Ok("task-id".into())
            }),
            source_session_id: "session-a".into(),
        };
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = Arc::new(Sandbox::new(
            Arc::new(LocalSandbox::new(workspace.path()).expect("local sandbox")),
            ApprovalPolicy::Ask,
        ));
        let permissions =
            SandboxPermissions::restore("session-a", NetworkAccess::Denied, ["call".into()])
                .for_call("call");

        let output = tool
            .call(
                ToolContext {
                    sandbox,
                    permissions,
                },
                serde_json::json!({
                    "task": "Review open pull requests",
                    "schedule": "0 9 * * 1"
                }),
            )
            .await
            .expect("schedule task");

        assert_eq!(tool.definition().name, "schedule_task");
        assert_eq!(tool.approval(), ApprovalRequirement::Always);
        assert_eq!(output, "scheduled `0 9 * * 1` as task task-id");
        assert_eq!(
            *calls.lock().expect("calls"),
            [(
                "session-a".into(),
                "Review open pull requests".into(),
                "0 9 * * 1".into()
            )]
        );
    }
}
