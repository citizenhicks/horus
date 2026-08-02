//! Session catalog and durable forking middleware.

use uuid::Uuid;

use super::Middleware;
use super::MiddlewareCommandContext;
use super::MiddlewareCommandOutput;
use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::backend::checkpoint::Checkpoint;
use crate::backend::checkpoint::SessionCursor;
use crate::backend::checkpoint::SessionPage;
use crate::backend::checkpoint::SessionPageRequest;
use crate::backend::checkpoint::SessionSummary;
use crate::protocol::FrontendCommand;
use crate::protocol::FrontendContribution;
use crate::protocol::FrontendEvent;
use crate::protocol::FrontendPickerOption;
use crate::protocol::FrontendTone;
use crate::protocol::Op;

const DEFAULT_PAGE_SIZE: usize = 100;

/// Adds session discovery and branching without changing the core loop.
pub struct Sessions {
    page_size: usize,
}

impl Sessions {
    /// Creates session middleware with a bounded catalog page size.
    pub fn new(page_size: usize) -> Result<Self> {
        if page_size == 0 {
            return Err(Error::Config(
                "session catalog page size must be positive".into(),
            ));
        }
        Ok(Self { page_size })
    }
}

impl Default for Sessions {
    fn default() -> Self {
        Self {
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

impl Middleware for Sessions {
    fn name(&self) -> &'static str {
        "sessions"
    }

    fn frontend(&self) -> FrontendContribution {
        FrontendContribution {
            capability: self.name().into(),
            commands: vec![
                FrontendCommand {
                    name: "resume".into(),
                    arguments: String::new(),
                    description: "resume a saved session".into(),
                },
                FrontendCommand {
                    name: "fork".into(),
                    arguments: String::new(),
                    description: "create a resumable branch from this session".into(),
                },
            ],
            widgets: Vec::new(),
            references: Vec::new(),
            active_input: None,
        }
    }

    fn command<'a>(
        &'a self,
        context: MiddlewareCommandContext<'a>,
    ) -> BoxFuture<'a, Result<MiddlewareCommandOutput>> {
        Box::pin(async move {
            match context.command {
                "resume" => resume(context, self.page_size).await,
                "fork" => fork(context).await,
                command => Err(Error::Unknown(format!("sessions command `{command}`"))),
            }
        })
    }
}

async fn fork(context: MiddlewareCommandContext<'_>) -> Result<MiddlewareCommandOutput> {
    if !context.arguments.trim().is_empty() {
        return Ok(MiddlewareCommandOutput::render(
            "sessions",
            "! usage: fork",
            FrontendTone::Warning,
        ));
    }
    let checkpoint = manual_fork_checkpoint(context.checkpoint);
    context
        .checkpoints
        .fork(
            &context.checkpoint.session_id,
            context.checkpoint.sequence,
            &checkpoint,
        )
        .await?;
    Ok(MiddlewareCommandOutput::events(vec![
        FrontendEvent::Render {
            capability: "sessions".into(),
            block: crate::protocol::FrontendBlock {
                id: None,
                group: None,
                append: false,
                pending: false,
                text: format!("◇ forked session {}", compact_id(&checkpoint.session_id)),
                format: crate::protocol::FrontendBlockFormat::PlainText,
                tone: FrontendTone::Success,
            },
        },
        FrontendEvent::Picker {
            title: "Fork created".into(),
            options: vec![FrontendPickerOption {
                label: "Open fork".into(),
                description: "Continue in the new chat".into(),
                op: Op::ResumeSession {
                    session_id: checkpoint.session_id,
                },
            }],
        },
    ]))
}

fn manual_fork_checkpoint(parent: &Checkpoint) -> Checkpoint {
    let mut checkpoint = Checkpoint::empty(Uuid::new_v4().to_string());
    checkpoint.context.clone_from(&parent.context);
    checkpoint
        .first_user_message
        .clone_from(&parent.first_user_message);
    checkpoint.model_route.clone_from(&parent.model_route);
    checkpoint
        .session_context
        .clone_from(&parent.session_context);
    checkpoint.session_context.origin_label = None;
    checkpoint
}

async fn resume(
    context: MiddlewareCommandContext<'_>,
    page_size: usize,
) -> Result<MiddlewareCommandOutput> {
    let arguments = context.arguments.trim();
    let cursor = if arguments.is_empty() {
        None
    } else {
        match serde_json::from_str(arguments) {
            Ok(cursor) => Some(cursor),
            Err(_) => {
                return Ok(MiddlewareCommandOutput::render(
                    "sessions",
                    "! usage: resume",
                    FrontendTone::Warning,
                ));
            }
        }
    };
    let options = resume_options(&context, cursor, page_size).await?;
    if options.is_empty() {
        return Ok(MiddlewareCommandOutput::render(
            "sessions",
            "no saved sessions",
            FrontendTone::Neutral,
        ));
    }
    Ok(MiddlewareCommandOutput::events(vec![
        FrontendEvent::Picker {
            title: "Resume session".into(),
            options,
        },
    ]))
}

fn compact_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

async fn resume_options(
    context: &MiddlewareCommandContext<'_>,
    cursor: Option<SessionCursor>,
    page_size: usize,
) -> Result<Vec<FrontendPickerOption>> {
    let page = context
        .checkpoints
        .list_sessions_page(SessionPageRequest {
            cursor,
            limit: page_size,
        })
        .await?;
    resume_page_options(page, &context.checkpoint.session_id)
}

fn resume_page_options(
    page: SessionPage,
    current_session_id: &str,
) -> Result<Vec<FrontendPickerOption>> {
    let mut options = page
        .sessions
        .into_iter()
        .filter_map(|session| resume_option(session, current_session_id))
        .collect::<Vec<_>>();
    if let Some(cursor) = page.next_cursor {
        options.push(FrontendPickerOption {
            label: "More sessions…".into(),
            description: String::new(),
            op: Op::CapabilityCommand {
                capability: "sessions".into(),
                command: "resume".into(),
                arguments: serde_json::to_string(&cursor)?,
            },
        });
    }
    Ok(options)
}

fn resume_option(
    session: SessionSummary,
    current_session_id: &str,
) -> Option<FrontendPickerOption> {
    if !session.catalog_visible
        || session.session_id == current_session_id
        || (session.sequence == 0 && session.parent_session_id.is_none())
    {
        return None;
    }
    let description = session_description(&session);
    let label = session.first_user_message.map_or_else(
        || format!("Fork {}", compact_id(&session.session_id)),
        |message| {
            message
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(42)
                .collect::<String>()
                .trim_end()
                .into()
        },
    );
    Some(FrontendPickerOption {
        label,
        description,
        op: Op::ResumeSession {
            session_id: session.session_id,
        },
    })
}

fn session_description(session: &SessionSummary) -> String {
    let mut details = [
        session.session_context.workspace_label.as_deref(),
        session.session_context.origin_label.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    details.push(format!("created at Unix time {}", session.created_at));
    details.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_rejects_zero_page_size() {
        assert!(Sessions::new(0).is_err());
    }

    #[test]
    fn resume_lists_fresh_forks_but_not_empty_roots() {
        let summary = |session_id: &str, parent_session_id: Option<&str>| SessionSummary {
            session_id: session_id.into(),
            session_context: Default::default(),
            parent_session_id: parent_session_id.map(str::to_string),
            parent_sequence: parent_session_id.map(|_| 4),
            sequence: 0,
            catalog_visible: true,
            first_user_message: None,
            created_at: 0,
            updated_at: 0,
        };

        assert_eq!(
            resume_option(summary("branch-id", Some("parent")), "current")
                .map(|option| option.label),
            Some("Fork branch-i".into())
        );
        assert!(resume_option(summary("empty", None), "current").is_none());
    }

    #[test]
    fn resume_description_includes_workspace_and_origin_labels() {
        let option = resume_option(
            SessionSummary {
                session_id: "scheduled".into(),
                session_context: crate::protocol::SessionContext {
                    workspace_label: Some("Project One".into()),
                    origin_label: Some("cron".into()),
                    ..crate::protocol::SessionContext::default()
                },
                parent_session_id: None,
                parent_sequence: None,
                sequence: 1,
                catalog_visible: true,
                first_user_message: Some("Update dependencies".into()),
                created_at: 42,
                updated_at: 42,
            },
            "current",
        )
        .expect("resume option");

        assert_eq!(
            option.description,
            "Project One · cron · created at Unix time 42"
        );
    }

    #[test]
    fn manual_fork_keeps_context_and_workspace_but_clears_origin() {
        let mut parent = Checkpoint::empty("parent");
        parent.context = vec![serde_json::json!({"role": "user", "content": "Hello"})];
        parent.first_user_message = Some("Hello".into());
        parent.session_context = crate::protocol::SessionContext {
            workspace_id: Some("workspace-1".into()),
            workspace_label: Some("Project One".into()),
            origin_label: Some("cron".into()),
            ..crate::protocol::SessionContext::default()
        };

        let fork = manual_fork_checkpoint(&parent);

        assert_eq!(fork.context, parent.context);
        assert_eq!(fork.first_user_message, parent.first_user_message);
        assert_eq!(
            fork.session_context,
            crate::protocol::SessionContext {
                workspace_id: Some("workspace-1".into()),
                workspace_label: Some("Project One".into()),
                ..crate::protocol::SessionContext::default()
            }
        );
    }

    #[test]
    fn resume_page_preserves_the_next_catalog_cursor() {
        let cursor = SessionCursor {
            updated_at: 12,
            sequence: 4,
            session_id: "next".into(),
        };
        let options = resume_page_options(
            SessionPage {
                sessions: Vec::new(),
                next_cursor: Some(cursor.clone()),
            },
            "current",
        )
        .expect("build resume page");
        let Op::CapabilityCommand { arguments, .. } = &options[0].op else {
            panic!("expected middleware command");
        };

        assert_eq!(
            serde_json::from_str::<SessionCursor>(arguments).expect("decode cursor"),
            cursor
        );
    }
}
