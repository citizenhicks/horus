//! Optional durable todo planning.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::manifest::MiddlewareManifest;
use super::tools::{Catalog, Tool, ToolContext, render_tool_event};
use super::{Middleware, MiddlewareCommandContext, MiddlewareCommandOutput, RuntimeContext};
use crate::backend::checkpoint::CheckpointStore;
use crate::backend::model::ToolDefinition;
use crate::protocol::{
    EventMsg, FrontendActionListItem, FrontendBlock, FrontendCommand, FrontendContribution,
    FrontendEvent, FrontendListItemState, FrontendProgress, FrontendSlot, FrontendSymbol,
    FrontendTone, FrontendWidget, FrontendWidgetContent,
};
use crate::{BoxFuture, Error, Result};

const STATE_KEY: &str = "tasks.v1";
const MAX_TODOS: usize = 50;
const MAX_TODO_BYTES: usize = 500;
const PROMPT: &str = "Use `write_todos` only for genuinely multi-step work. Keep the list short, mark work in_progress before starting and completed immediately after finishing, and end with a substantive answer.";

/// Configuration and presentation metadata for durable tasks.
pub const MANIFEST: MiddlewareManifest = MiddlewareManifest {
    id: "tasks",
    label: "Tasks",
    description: "Maintain a durable todo list for multi-step work",
    required: false,
    default_enabled: false,
    settings: &[],
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Todo {
    content: String,
    status: TodoStatus,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteTodosArgs {
    todos: Vec<Todo>,
}

/// Contributes one durable, whole-list todo tool and status view.
#[derive(Default)]
pub struct Tasks;

impl Middleware for Tasks {
    fn name(&self) -> &'static str {
        MANIFEST.id
    }

    fn register(&self, catalog: &mut Catalog, runtime: &RuntimeContext) -> Result<()> {
        catalog.register(Arc::new(WriteTodos {
            checkpoints: Arc::clone(&runtime.checkpoints),
            session_id: runtime.session_id.clone(),
            frontend: Arc::clone(&runtime.frontend),
        }))
    }

    fn prompt_fragment(&self, _runtime: &RuntimeContext) -> Result<Option<String>> {
        Ok(Some(PROMPT.into()))
    }

    fn frontend(&self) -> FrontendContribution {
        FrontendContribution {
            capability: self.name().into(),
            commands: vec![FrontendCommand {
                name: "tasks".into(),
                arguments: String::new(),
                description: "show the current todo list".into(),
            }],
            ..FrontendContribution::default()
        }
    }

    fn render(&self, event: &EventMsg) -> Option<FrontendBlock> {
        render_tool_event(
            event,
            |name| name == "write_todos",
            |_, arguments| {
                let count = arguments
                    .get("todos")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                format!("◉ Tasks {count}")
            },
        )
    }

    fn initialize<'a>(&'a self, context: RuntimeContext) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let todos = load_todos(&context.checkpoints, &context.session_id).await?;
            (context.frontend)(widget_event(&todos))
        })
    }

    fn command<'a>(
        &'a self,
        context: MiddlewareCommandContext<'a>,
    ) -> BoxFuture<'a, Result<MiddlewareCommandOutput>> {
        Box::pin(async move {
            if context.command != "tasks" || !context.arguments.trim().is_empty() {
                return Err(Error::Unknown(format!(
                    "tasks command `{}`",
                    context.command
                )));
            }
            let todos = load_todos(&context.checkpoints, context.session_id).await?;
            Ok(MiddlewareCommandOutput::render(
                "tasks",
                format_todos(&todos),
                FrontendTone::Neutral,
            ))
        })
    }
}

struct WriteTodos {
    checkpoints: Arc<dyn CheckpointStore>,
    session_id: String,
    frontend: super::FrontendEventSink,
}

impl Tool for WriteTodos {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_todos".into(),
            description: "Replace the session todo list; an empty list clears it.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "maxItems": MAX_TODOS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string", "maxLength": MAX_TODO_BYTES},
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["content", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["todos"],
                "additionalProperties": false
            }),
        }
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let mut arguments: WriteTodosArgs = serde_json::from_value(arguments)?;
            validate_todos(&mut arguments.todos).map_err(Error::Tool)?;
            self.checkpoints
                .save_state(
                    &self.session_id,
                    STATE_KEY,
                    &serde_json::to_value(&arguments.todos)?,
                )
                .await?;
            (self.frontend)(widget_event(&arguments.todos))?;
            Ok(format!("updated {} todos", arguments.todos.len()))
        })
    }
}

fn validate_todos(todos: &mut [Todo]) -> std::result::Result<(), String> {
    if todos.len() > MAX_TODOS {
        return Err(format!("todo count exceeds {MAX_TODOS}"));
    }
    for todo in todos {
        let content = todo.content.trim();
        if content.is_empty() || content.len() > MAX_TODO_BYTES {
            return Err(format!("todo content must be 1–{MAX_TODO_BYTES} bytes"));
        }
        todo.content = content.into();
    }
    Ok(())
}

async fn load_todos(checkpoints: &Arc<dyn CheckpointStore>, session_id: &str) -> Result<Vec<Todo>> {
    let mut todos: Vec<Todo> = checkpoints
        .load_state(session_id, STATE_KEY)
        .await?
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    validate_todos(&mut todos)
        .map_err(|error| Error::Checkpoint(format!("invalid tasks state: {error}")))?;
    Ok(todos)
}

fn widget_event(todos: &[Todo]) -> FrontendEvent {
    if todos.is_empty() {
        return FrontendEvent::RemoveWidget {
            capability: MANIFEST.id.into(),
            id: "status".into(),
        };
    }
    let completed = todos
        .iter()
        .filter(|todo| todo.status == TodoStatus::Completed)
        .count();
    FrontendEvent::Widget {
        capability: MANIFEST.id.into(),
        item: FrontendWidget {
            id: "status".into(),
            slot: FrontendSlot::ComposerFooter,
            text: format!("{completed}/{}", todos.len()),
            tone: if completed == todos.len() {
                FrontendTone::Success
            } else {
                FrontendTone::Neutral
            },
            symbol: Some(FrontendSymbol::Task),
            icon_only: false,
            progress: Some(FrontendProgress {
                completed,
                total: todos.len(),
            }),
            content: Some(FrontendWidgetContent::ActionList {
                title: "Tasks".into(),
                items: todos
                    .iter()
                    .enumerate()
                    .map(|(index, todo)| FrontendActionListItem {
                        id: index.to_string(),
                        text: todo.content.clone(),
                        state: match todo.status {
                            TodoStatus::Pending => FrontendListItemState::Pending,
                            TodoStatus::InProgress => FrontendListItemState::InProgress,
                            TodoStatus::Completed => FrontendListItemState::Completed,
                        },
                        actions: Vec::new(),
                    })
                    .collect(),
            }),
            action: None,
        },
    }
}

fn format_todos(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return "No tasks.".into();
    }
    todos
        .iter()
        .map(|todo| {
            let marker = match todo.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[~]",
                TodoStatus::Completed => "[x]",
            };
            format!("{marker} {}", todo.content)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use crate::backend::checkpoint::sqlite::SqliteCheckpoint;
    use crate::backend::model::ToolCall;
    use crate::backend::sandbox::local::LocalSandbox;
    use crate::backend::sandbox::{ApprovalPolicy, NetworkAccess, Sandbox, SandboxPermissions};
    use crate::middleware::tools::execute_batch;
    use crate::protocol::SessionContext;

    use super::*;

    #[tokio::test]
    async fn write_todos_persists_session_state_and_updates_the_widget() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(
            SqliteCheckpoint::new(temporary.path().join("checkpoints.sqlite3"))
                .expect("checkpoints"),
        );
        let frontend_events = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::clone(&frontend_events);
        let runtime = RuntimeContext {
            checkpoints: Arc::clone(&checkpoints),
            session_id: "session-a".into(),
            model_route: "default".into(),
            session_context: SessionContext::default(),
            metadata: BTreeMap::new(),
            frontend: Arc::new(move |event| {
                events.lock().expect("frontend events").push(event);
                Ok(())
            }),
        };
        let tasks = Tasks;
        let mut catalog = Catalog::default();
        tasks.register(&mut catalog, &runtime).expect("register");
        let sandbox = Arc::new(Sandbox::new(
            Arc::new(LocalSandbox::new(temporary.path()).expect("sandbox")),
            ApprovalPolicy::Ask,
        ));
        let permissions =
            SandboxPermissions::restore("session-a", NetworkAccess::Denied, Vec::new());

        let result = execute_batch(
            &catalog,
            &[ToolCall {
                call_id: "write".into(),
                name: "write_todos".into(),
                arguments: serde_json::json!({"todos": [
                    {"content": " inspect seams ", "status": "completed"},
                    {"content": "Implement tasks", "status": "in_progress"}
                ]}),
            }],
            sandbox,
            &permissions,
        )
        .await
        .pop()
        .expect("tool result");

        assert!(!result.is_error, "{}", result.output);
        assert_eq!(
            load_todos(&checkpoints, "session-a")
                .await
                .expect("saved todos")[0]
                .content,
            "inspect seams"
        );
        assert!(
            checkpoints
                .load_state("session-b", STATE_KEY)
                .await
                .expect("other session")
                .is_none()
        );
        assert!(matches!(
            frontend_events.lock().expect("frontend events").last(),
            Some(FrontendEvent::Widget { item, .. })
                if item.text == "1/2"
                    && item.progress == Some(FrontendProgress { completed: 1, total: 2 })
                    && item.action.is_none()
                    && matches!(
                        &item.content,
                        Some(FrontendWidgetContent::ActionList { title, items })
                            if title == "Tasks"
                                && items.len() == 2
                                && items[0].text == "inspect seams"
                                && items[0].state == FrontendListItemState::Completed
                                && items[1].state == FrontendListItemState::InProgress
                                && items.iter().all(|item| item.actions.is_empty())
                    )
        ));
    }
}
