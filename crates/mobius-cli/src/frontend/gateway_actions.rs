use std::path::PathBuf;

use mobius::{Error, Result};
use mobius_gateway::wire::{ClientMessage, CronRun, CronTask, ProfileSnapshot, ServerMessage};
use uuid::Uuid;

use super::catalog::GatewayAction;

pub(super) type PreparedAction = Box<ClientMessage>;

pub(super) fn prepare(action: GatewayAction, session_id: &str) -> Result<PreparedAction> {
    match action {
        GatewayAction::Workspace(arguments) => prepare_workspace(&arguments),
        GatewayAction::Pair => Ok(send(|request_id| ClientMessage::CreatePairingCode {
            request_id,
        })),
        GatewayAction::Profile => Ok(send(|request_id| ClientMessage::GetProfile { request_id })),
        GatewayAction::Artifacts => Ok(send(|request_id| ClientMessage::ListArtifacts {
            request_id,
            session_id: session_id.into(),
        })),
        GatewayAction::Cron(action) => prepare_cron(action, session_id),
    }
}

pub(super) fn render_response(
    message: &ServerMessage,
    selected_session_id: &str,
) -> Option<String> {
    match message {
        ServerMessage::Accepted { .. } => None,
        ServerMessage::Rejected { message, .. } | ServerMessage::Error { message, .. } => {
            Some(message.clone())
        }
        ServerMessage::ProviderCredentialSaved { provider, .. } => {
            Some(format!("{provider}: configured"))
        }
        ServerMessage::PairingCode {
            code, expires_at, ..
        } => Some(format!("one-time code {code} · expires {expires_at}")),
        ServerMessage::ProviderLoginStarted {
            provider,
            verification_url,
            user_code,
            ..
        } => Some(format!(
            "{provider} login · open {verification_url} · enter {user_code}"
        )),
        ServerMessage::ProviderLoginFinished { provider, .. } => {
            Some(format!("{provider} login complete"))
        }
        ServerMessage::Profile { profile, .. } => Some(render_profile(profile)),
        ServerMessage::CronTasks {
            session_id, tasks, ..
        } if session_id == selected_session_id => Some(render_cron_tasks(tasks)),
        ServerMessage::CronHistory {
            session_id, runs, ..
        } if session_id == selected_session_id => Some(render_cron_history(runs)),
        _ => None,
    }
}

fn prepare_workspace(arguments: &str) -> Result<PreparedAction> {
    let path = required(arguments, "usage: /workspace <gateway-path>")?;
    Ok(send(|request_id| ClientMessage::CreateSession {
        request_id,
        workspace: PathBuf::from(path),
    }))
}

fn prepare_cron(arguments: String, session_id: &str) -> Result<PreparedAction> {
    let arguments = arguments.trim();
    if arguments.is_empty() || arguments == "list" {
        return Ok(send(|request_id| ClientMessage::ListCron {
            request_id,
            session_id: session_id.into(),
        }));
    }
    let mut parts = arguments.split_ascii_whitespace();
    match parts.next() {
        Some("new") => {
            let task = parts.collect::<Vec<_>>().join(" ");
            let task = (!task.is_empty()).then_some(task);
            Ok(send(|request_id| {
                ClientMessage::StartCronSetup {
                    request_id,
                    session_id: session_id.into(),
                    task,
                }
            }))
        }
        Some("reschedule") => {
            let id = required(parts.next().unwrap_or_default(), "usage: /cron reschedule <id> <schedule>")?;
            let schedule = required_remainder(parts, "usage: /cron reschedule <id> <schedule>")?;
            Ok(send(|request_id| {
                ClientMessage::RescheduleCron {
                    request_id,
                    session_id: session_id.into(),
                    id: id.into(),
                    schedule,
                }
            }))
        }
        Some("delete") => one_id(parts, |request_id, id| ClientMessage::DeleteCron {
            request_id,
            session_id: session_id.into(),
            id,
        }),
        Some("run") => one_id(parts, |request_id, id| ClientMessage::RunCron {
            request_id,
            session_id: session_id.into(),
            id,
        }),
        Some("history") => {
            let id = parts.next().map(str::to_owned);
            if parts.next().is_some() {
                return Err(Error::Config("usage: /cron history [id]".into()));
            }
            Ok(send(|request_id| {
                ClientMessage::ListCronHistory {
                    request_id,
                    session_id: session_id.into(),
                    id,
                }
            }))
        }
        _ => Err(Error::Config(
            "usage: /cron [new [task]|list|reschedule <id> <schedule>|delete <id>|run <id>|history [id]]".into(),
        )),
    }
}

fn one_id<'a>(
    mut parts: impl Iterator<Item = &'a str>,
    build: impl FnOnce(String, String) -> ClientMessage,
) -> Result<PreparedAction> {
    let id = required(parts.next().unwrap_or_default(), "cron task ID is required")?;
    if parts.next().is_some() {
        return Err(Error::Config("cron command accepts one task ID".into()));
    }
    Ok(send(|request_id| build(request_id, id.into())))
}

fn required<'a>(value: &'a str, usage: &str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        Err(Error::Config(usage.into()))
    } else {
        Ok(value)
    }
}

fn required_remainder<'a>(parts: impl Iterator<Item = &'a str>, usage: &str) -> Result<String> {
    let value = parts.collect::<Vec<_>>().join(" ");
    required(&value, usage).map(str::to_owned)
}

fn request_id() -> String {
    Uuid::new_v4().to_string()
}

fn send(build: impl FnOnce(String) -> ClientMessage) -> PreparedAction {
    Box::new(build(request_id()))
}

fn render_profile(profile: &ProfileSnapshot) -> String {
    let mut lines = vec![profile.user_name.as_deref().unwrap_or("user").into()];
    lines.extend(profile.daily_usage.iter().map(|day| {
        format!(
            "day {} · {} · {} tokens · {} cached",
            day.unix_day, day.provider, day.usage.total_tokens, day.usage.cached_input_tokens
        )
    }));
    lines.join("\n")
}

fn render_cron_tasks(tasks: &[CronTask]) -> String {
    if tasks.is_empty() {
        return "no scheduled tasks".into();
    }
    tasks
        .iter()
        .map(|task| {
            format!(
                "{}  {}\n  task: {}",
                task.id,
                task.schedule,
                task.task.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_cron_history(runs: &[CronRun]) -> String {
    if runs.is_empty() {
        return "no cron runs".into();
    }
    runs.iter()
        .map(|run| {
            format!(
                "{} · {} · {:?} · started {}",
                run.id, run.task_id, run.status, run.started_at
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use mobius::protocol::TokenUsage;
    use mobius_gateway::wire::{DailyUsage, RunStats};

    use super::*;

    #[test]
    fn profile_usage_names_each_provider() {
        let profile = ProfileSnapshot {
            user_name: Some("user".into()),
            daily_usage: vec![DailyUsage {
                unix_day: 7,
                provider: "openai_socket".into(),
                usage: TokenUsage {
                    total_tokens: 11,
                    ..TokenUsage::default()
                },
            }],
            run_stats: RunStats::default(),
            recent_run_groups: Vec::new(),
        };

        assert_eq!(
            render_profile(&profile),
            "user\nday 7 · openai_socket · 11 tokens · 0 cached"
        );
    }

    #[test]
    fn cron_parser_covers_remote_scheduler_operations() {
        for input in [
            "list",
            "new",
            "new review open pull requests",
            "reschedule abc 0 4 * * *",
            "delete abc",
            "run abc",
            "history",
            "history abc",
        ] {
            assert!(prepare_cron(input.into(), "session-a").is_ok(), "{input}");
        }
    }

    #[test]
    fn cron_new_preserves_the_optional_task_for_gateway_setup() {
        let message = prepare_cron("new review open pull requests".into(), "session-a")
            .expect("prepare cron setup");

        assert!(matches!(
            *message,
            ClientMessage::StartCronSetup { session_id, task: Some(task), .. }
                if session_id == "session-a" && task == "review open pull requests"
        ));
    }

    #[test]
    fn cron_parser_rejects_direct_task_creation() {
        assert!(prepare_cron("add task.md @daily".into(), "session-a").is_err());
    }

    #[test]
    fn cron_queries_are_scoped_to_the_selected_chat() {
        let message = prepare_cron("list".into(), "session-a").expect("prepare cron list");

        assert!(matches!(
            *message,
            ClientMessage::ListCron { session_id, .. } if session_id == "session-a"
        ));
    }

    #[test]
    fn generic_acceptance_is_not_transcript_content() {
        let accepted = ServerMessage::Accepted {
            request_id: "request".into(),
        };

        assert!(render_response(&accepted, "session-a").is_none());
    }

    #[test]
    fn scoped_responses_do_not_render_in_another_chat() {
        let tasks = ServerMessage::CronTasks {
            request_id: "request".into(),
            session_id: "session-b".into(),
            tasks: Vec::new(),
        };

        assert!(render_response(&tasks, "session-a").is_none());
    }

    #[test]
    fn workspace_command_creates_a_chat_without_local_path_resolution() {
        let message = prepare_workspace("/srv/mobius/project").expect("prepare workspace");

        assert!(matches!(
            *message,
            ClientMessage::CreateSession { workspace, .. }
                if workspace == std::path::Path::new("/srv/mobius/project")
        ));
    }
}
