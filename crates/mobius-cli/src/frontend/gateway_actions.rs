use std::path::PathBuf;

use mobius::{Error, Result};
use mobius_gateway::wire::{
    ClientMessage, CronRun, CronTask, ProfileSnapshot, ProviderInstance, ServerMessage,
};
use uuid::Uuid;

use super::catalog::GatewayAction;
use super::provider_instance_label;

pub(super) type PreparedAction = Box<ClientMessage>;

pub(super) fn prepare(action: GatewayAction) -> Result<PreparedAction> {
    match action {
        GatewayAction::Workspace(arguments) => prepare_workspace(&arguments),
        GatewayAction::Pair => Ok(send(|request_id| ClientMessage::CreatePairingCode {
            request_id,
        })),
        GatewayAction::Profile => Ok(send(|request_id| ClientMessage::GetProfile { request_id })),
    }
}

pub(super) fn render_response(
    message: &ServerMessage,
    selected_session_id: &str,
    provider_instances: &[ProviderInstance],
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
        ServerMessage::Profile { profile, .. } => Some(render_profile(profile, provider_instances)),
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

fn required<'a>(value: &'a str, usage: &str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        Err(Error::Config(usage.into()))
    } else {
        Ok(value)
    }
}

fn request_id() -> String {
    Uuid::new_v4().to_string()
}

fn send(build: impl FnOnce(String) -> ClientMessage) -> PreparedAction {
    Box::new(build(request_id()))
}

fn render_profile(profile: &ProfileSnapshot, provider_instances: &[ProviderInstance]) -> String {
    let mut lines = vec![profile.user_name.as_deref().unwrap_or("user").into()];
    lines.extend(profile.daily_usage.iter().map(|day| {
        let provider =
            provider_instance_label(provider_instances, &day.provider).unwrap_or(&day.provider);
        format!(
            "day {} · {} · {} tokens · {} cached",
            day.unix_day, provider, day.usage.total_tokens, day.usage.cached_input_tokens
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
    use mobius_gateway::wire::{AgentComposition, DailyUsage, RunStats};

    use super::*;

    #[test]
    fn profile_usage_names_each_provider() {
        let mut selection = AgentComposition::default().provider;
        selection.instance = "provider-instance".into();
        let instances = [ProviderInstance {
            label: "Work".into(),
            tint: Default::default(),
            configured: true,
            selection,
            model_ids: Vec::new(),
            reasoning_efforts: Vec::new(),
        }];
        let profile = ProfileSnapshot {
            user_name: Some("user".into()),
            daily_usage: vec![DailyUsage {
                unix_day: 7,
                provider: "provider-instance".into(),
                usage: TokenUsage {
                    total_tokens: 11,
                    ..TokenUsage::default()
                },
            }],
            run_stats: RunStats::default(),
            recent_run_groups: Vec::new(),
        };

        assert_eq!(
            render_profile(&profile, &instances),
            "user\nday 7 · Work · 11 tokens · 0 cached"
        );
    }

    #[test]
    fn generic_acceptance_is_not_transcript_content() {
        let accepted = ServerMessage::Accepted {
            request_id: "request".into(),
        };

        assert!(render_response(&accepted, "session-a", &[]).is_none());
    }

    #[test]
    fn scoped_responses_do_not_render_in_another_chat() {
        let tasks = ServerMessage::CronTasks {
            request_id: "request".into(),
            session_id: "session-b".into(),
            tasks: Vec::new(),
        };

        assert!(render_response(&tasks, "session-a", &[]).is_none());
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
