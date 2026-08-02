use std::env;
use std::path::PathBuf;

use horus::backend::model::provider::{ProviderAuth, ProviderDefinition, provider};
use horus::{Error, Result};
use horus_gateway::wire::{
    AgentComposition, ArtifactRecord, ClientMessage, CronRun, CronTask, ProfileSnapshot,
    ProviderAuthKind, ProviderStatus, ReadyPayload, ServerMessage,
};
use uuid::Uuid;

use super::catalog::{CronAction, GatewayAction};

pub(super) enum PreparedAction {
    Print(String),
    Send {
        message: ClientMessage,
        request_id: String,
        response: ResponseKind,
    },
}

#[derive(Clone, Copy)]
pub(super) enum ResponseKind {
    Accepted,
    AgentRestart,
    WorkspaceRestart,
    ProviderAuth,
    Pairing,
    Profile,
    Artifacts,
    CronTasks,
    CronHistory,
}

pub(super) fn prepare(action: GatewayAction, ready: &ReadyPayload) -> Result<PreparedAction> {
    match action {
        GatewayAction::Agent(arguments) => prepare_agent(&arguments, ready),
        GatewayAction::Workspace(arguments) => prepare_workspace(&arguments),
        GatewayAction::Providers => Ok(PreparedAction::Print(render_providers(&ready.providers))),
        GatewayAction::Login(arguments) => prepare_login(&arguments, &ready.providers),
        GatewayAction::Pair => Ok(send(ResponseKind::Pairing, |request_id| {
            ClientMessage::CreatePairingCode { request_id }
        })),
        GatewayAction::Profile => Ok(send(ResponseKind::Profile, |request_id| {
            ClientMessage::GetProfile { request_id }
        })),
        GatewayAction::Artifacts => Ok(send(ResponseKind::Artifacts, |request_id| {
            ClientMessage::ListArtifacts { request_id }
        })),
        GatewayAction::Cron(action) => prepare_cron(action),
    }
}

fn prepare_login(arguments: &str, providers: &[ProviderStatus]) -> Result<PreparedAction> {
    prepare_login_with(arguments, providers, |name| env::var(name).ok())
}

fn prepare_login_with(
    arguments: &str,
    providers: &[ProviderStatus],
    environment: impl Fn(&str) -> Option<String>,
) -> Result<PreparedAction> {
    let mut parts = arguments.split_ascii_whitespace();
    let provider_id = required(
        parts.next().unwrap_or_default(),
        "usage: /login <provider> [env:NAME]",
    )?;
    let credential = parts.next();
    if parts.next().is_some() {
        return Err(Error::Config("usage: /login <provider> [env:NAME]".into()));
    }
    let definition = validated_provider(provider_id, providers)?;
    let ProviderAuth::ApiKey(default_environment) = definition.auth() else {
        if credential.is_some() {
            return Err(Error::Config(format!(
                "provider `{provider_id}` uses device login; run `/login {provider_id}`"
            )));
        }
        return Ok(send(ResponseKind::ProviderAuth, |request_id| {
            ClientMessage::StartProviderLogin {
                request_id,
                provider: provider_id.into(),
            }
        }));
    };

    let environment_name = match credential {
        Some(value) => value
            .strip_prefix("env:")
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                Error::Config(format!(
                    "provider `{provider_id}` requires `env:NAME`; run `/login {provider_id} env:NAME`"
                ))
            })?,
        None => default_environment,
    };
    let retry = credential.map_or_else(
        || format!("/login {provider_id}"),
        |_| format!("/login {provider_id} env:{environment_name}"),
    );
    let api_key = environment(environment_name)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            Error::Config(format!(
                "provider `{provider_id}` requires API key environment variable `{environment_name}`; set it before starting horus, then run `{retry}` again"
            ))
        })?;
    Ok(send(ResponseKind::ProviderAuth, |request_id| {
        ClientMessage::SetProviderCredential {
            request_id,
            provider: provider_id.into(),
            api_key,
        }
    }))
}

pub(super) fn validated_provider(
    provider_id: &str,
    advertised: &[ProviderStatus],
) -> Result<&'static ProviderDefinition> {
    let definition = provider(provider_id).map_err(|_| {
        Error::Config(format!(
            "unknown provider `{provider_id}`; run `/providers` to list available providers"
        ))
    })?;
    let status = advertised
        .iter()
        .find(|status| status.provider == provider_id)
        .ok_or_else(|| {
            Error::Config(format!(
                "provider `{provider_id}` is not advertised by this gateway"
            ))
        })?;
    let local_auth = match definition.auth() {
        ProviderAuth::ApiKey(_) => ProviderAuthKind::ApiKey,
        ProviderAuth::Browser(_) => ProviderAuthKind::DeviceCode,
    };
    if status.auth != local_auth {
        return Err(Error::Config(format!(
            "provider `{provider_id}` authentication does not match this CLI"
        )));
    }
    Ok(definition)
}

pub(super) fn render_response(message: &ServerMessage) -> Option<String> {
    match message {
        ServerMessage::Accepted { .. } => Some("gateway accepted the request".into()),
        ServerMessage::Rejected { message, .. } | ServerMessage::Error { message, .. } => {
            Some(message.clone())
        }
        ServerMessage::ProviderCredentialStatus {
            provider,
            configured,
            ..
        } => Some(format!(
            "{provider}: {}",
            if *configured {
                "configured"
            } else {
                "not configured"
            }
        )),
        ServerMessage::PairingCode {
            code, expires_at, ..
        } => Some(format!("pairing code {code} · expires {expires_at}")),
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
        ServerMessage::Artifacts { artifacts, .. } => Some(render_artifacts(artifacts)),
        ServerMessage::CronTasks { tasks, .. } => Some(render_cron_tasks(tasks)),
        ServerMessage::CronHistory { runs, .. } => Some(render_cron_history(runs)),
        _ => None,
    }
}

pub(super) fn render_terminal_response(
    message: &ServerMessage,
    request_id: &str,
    response: ResponseKind,
) -> Option<String> {
    let matches = match message {
        ServerMessage::Accepted { request_id: actual } => {
            actual == request_id
                && matches!(
                    response,
                    ResponseKind::Accepted
                        | ResponseKind::AgentRestart
                        | ResponseKind::WorkspaceRestart
                )
        }
        ServerMessage::ProviderCredentialStatus {
            request_id: actual, ..
        }
        | ServerMessage::ProviderLoginStarted {
            request_id: actual, ..
        }
        | ServerMessage::ProviderLoginFinished {
            request_id: actual, ..
        } => actual == request_id && matches!(response, ResponseKind::ProviderAuth),
        ServerMessage::Profile {
            request_id: actual, ..
        } => actual == request_id && matches!(response, ResponseKind::Profile),
        ServerMessage::PairingCode {
            request_id: actual, ..
        } => actual == request_id && matches!(response, ResponseKind::Pairing),
        ServerMessage::Artifacts {
            request_id: actual, ..
        } => actual == request_id && matches!(response, ResponseKind::Artifacts),
        ServerMessage::CronTasks {
            request_id: actual, ..
        } => actual == request_id && matches!(response, ResponseKind::CronTasks),
        ServerMessage::CronHistory {
            request_id: actual, ..
        } => actual == request_id && matches!(response, ResponseKind::CronHistory),
        _ => false,
    };
    matches.then(|| match response {
        ResponseKind::AgentRestart => "gateway agent restarted".into(),
        ResponseKind::WorkspaceRestart => "gateway workspace changed".into(),
        _ => render_response(message).unwrap_or_else(|| "gateway completed the request".into()),
    })
}

fn prepare_agent(arguments: &str, ready: &ReadyPayload) -> Result<PreparedAction> {
    let arguments = arguments.trim();
    if arguments.is_empty() {
        return serde_json::to_string_pretty(&ready.config.config)
            .map(PreparedAction::Print)
            .map_err(|error| Error::Config(format!("cannot encode agent composition: {error}")));
    }
    let config = parse_agent_composition(arguments)?;
    Ok(send(ResponseKind::AgentRestart, |request_id| {
        ClientMessage::ConfigureAgent {
            request_id,
            expected_revision: ready.config.revision,
            config,
        }
    }))
}

fn parse_agent_composition(arguments: &str) -> Result<AgentComposition> {
    serde_json::from_str(arguments)
        .map_err(|error| Error::Config(format!("invalid agent composition JSON: {error}")))
}

fn prepare_workspace(arguments: &str) -> Result<PreparedAction> {
    let path = required(arguments, "usage: /workspace <gateway-path>")?;
    Ok(send(ResponseKind::WorkspaceRestart, |request_id| {
        ClientMessage::SetWorkspace {
            request_id,
            path: PathBuf::from(path),
        }
    }))
}

fn prepare_cron(action: CronAction) -> Result<PreparedAction> {
    let arguments = match action {
        CronAction::Slash(arguments) => arguments,
        action => return Ok(prepare_structured_cron(action)),
    };
    let arguments = arguments.trim();
    if arguments.is_empty() || arguments == "list" {
        return Ok(send(ResponseKind::CronTasks, |request_id| {
            ClientMessage::ListCron { request_id }
        }));
    }
    let mut parts = arguments.split_ascii_whitespace();
    match parts.next() {
        Some("add") => {
            let task = required(parts.next().unwrap_or_default(), "usage: /cron add <task> <schedule>")?;
            let schedule = required_remainder(parts, "usage: /cron add <task> <schedule>")?;
            Ok(send(ResponseKind::Accepted, |request_id| ClientMessage::AddCron {
                request_id,
                task: PathBuf::from(task),
                schedule,
            }))
        }
        Some("reschedule") => {
            let id = required(parts.next().unwrap_or_default(), "usage: /cron reschedule <id> <schedule>")?;
            let schedule = required_remainder(parts, "usage: /cron reschedule <id> <schedule>")?;
            Ok(send(ResponseKind::Accepted, |request_id| {
                ClientMessage::RescheduleCron {
                    request_id,
                    id: id.into(),
                    schedule,
                }
            }))
        }
        Some("delete") => one_id(parts, |request_id, id| ClientMessage::DeleteCron {
            request_id,
            id,
        }),
        Some("run") => one_id(parts, |request_id, id| ClientMessage::RunCron {
            request_id,
            id,
        }),
        Some("history") => {
            let id = parts.next().map(str::to_owned);
            if parts.next().is_some() {
                return Err(Error::Config("usage: /cron history [id]".into()));
            }
            Ok(send(ResponseKind::CronHistory, |request_id| {
                ClientMessage::ListCronHistory { request_id, id }
            }))
        }
        _ => Err(Error::Config(
            "usage: /cron [list|add <task> <schedule>|reschedule <id> <schedule>|delete <id>|run <id>|history [id]]".into(),
        )),
    }
}

fn prepare_structured_cron(action: CronAction) -> PreparedAction {
    match action {
        CronAction::Add { task, schedule } => send(ResponseKind::Accepted, |request_id| {
            ClientMessage::AddCron {
                request_id,
                task,
                schedule,
            }
        }),
        CronAction::List => send(ResponseKind::CronTasks, |request_id| {
            ClientMessage::ListCron { request_id }
        }),
        CronAction::Reschedule { id, schedule } => send(ResponseKind::Accepted, |request_id| {
            ClientMessage::RescheduleCron {
                request_id,
                id,
                schedule,
            }
        }),
        CronAction::Delete(id) => send(ResponseKind::Accepted, |request_id| {
            ClientMessage::DeleteCron { request_id, id }
        }),
        CronAction::Run(id) => send(ResponseKind::Accepted, |request_id| {
            ClientMessage::RunCron { request_id, id }
        }),
        CronAction::History(id) => send(ResponseKind::CronHistory, |request_id| {
            ClientMessage::ListCronHistory { request_id, id }
        }),
        CronAction::Slash(_) => unreachable!("slash cron actions are parsed first"),
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
    Ok(send(ResponseKind::Accepted, |request_id| {
        build(request_id, id.into())
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

fn required_remainder<'a>(parts: impl Iterator<Item = &'a str>, usage: &str) -> Result<String> {
    let value = parts.collect::<Vec<_>>().join(" ");
    required(&value, usage).map(str::to_owned)
}

fn request_id() -> String {
    Uuid::new_v4().to_string()
}

fn send(response: ResponseKind, build: impl FnOnce(String) -> ClientMessage) -> PreparedAction {
    let request_id = request_id();
    PreparedAction::Send {
        message: build(request_id.clone()),
        request_id,
        response,
    }
}

fn render_providers(providers: &[ProviderStatus]) -> String {
    if providers.is_empty() {
        return "no providers available".into();
    }
    providers
        .iter()
        .map(|provider| {
            format!(
                "{} · {:?} · {}",
                provider.provider,
                provider.auth,
                if provider.configured {
                    "configured"
                } else {
                    "not configured"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_profile(profile: &ProfileSnapshot) -> String {
    let mut lines = vec![format!(
        "{} · {}",
        profile.user_name.as_deref().unwrap_or("user"),
        profile.workspace.label
    )];
    lines.extend(profile.daily_usage.iter().map(|day| {
        format!(
            "day {} · {} tokens · {} cached",
            day.unix_day, day.usage.total_tokens, day.usage.cached_input_tokens
        )
    }));
    lines.join("\n")
}

fn render_artifacts(artifacts: &[ArtifactRecord]) -> String {
    if artifacts.is_empty() {
        return "no artifacts".into();
    }
    artifacts
        .iter()
        .map(|artifact| {
            format!(
                "{} · {:?} · {}\n{}",
                artifact.id, artifact.kind, artifact.title, artifact.block.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
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
    use std::cell::Cell;

    use horus::backend::model::provider::HostedWebSearch;

    use super::*;

    fn provider_status(
        provider: &str,
        auth: ProviderAuthKind,
        default_api_key_env: Option<&str>,
    ) -> ProviderStatus {
        ProviderStatus {
            provider: provider.into(),
            label: provider.into(),
            configured: false,
            auth,
            default_model: None,
            default_base_url: None,
            default_api_key_env: default_api_key_env.map(str::to_owned),
            default_reasoning_effort: None,
            default_web_search: HostedWebSearch::Off,
        }
    }

    fn config_error(result: Result<PreparedAction>) -> String {
        match result {
            Err(Error::Config(message)) => message,
            Err(error) => panic!("expected configuration error, got {error}"),
            Ok(_) => panic!("expected configuration error"),
        }
    }

    #[test]
    fn cron_parser_covers_remote_scheduler_operations() {
        for input in [
            "list",
            "add tasks/check.md 0 3 * * *",
            "reschedule abc 0 4 * * *",
            "delete abc",
            "run abc",
            "history",
            "history abc",
        ] {
            assert!(
                prepare_cron(CronAction::Slash(input.into())).is_ok(),
                "{input}"
            );
        }
    }

    #[test]
    fn profile_request_does_not_finish_on_generic_acceptance() {
        let accepted = ServerMessage::Accepted {
            request_id: "request".into(),
        };

        assert!(render_terminal_response(&accepted, "request", ResponseKind::Profile).is_none());
    }

    #[test]
    fn agent_restart_waits_for_its_correlated_acceptance() {
        let unrelated = ServerMessage::Accepted {
            request_id: "other".into(),
        };

        assert!(
            render_terminal_response(&unrelated, "request", ResponseKind::AgentRestart).is_none()
        );
    }

    #[test]
    fn agent_parser_accepts_the_frontend_safe_composition_shape() {
        let json = serde_json::to_string(&AgentComposition::default()).expect("composition JSON");

        assert_eq!(
            parse_agent_composition(&json).expect("parse composition"),
            AgentComposition::default()
        );
    }

    #[test]
    fn workspace_command_maps_the_remote_path_without_local_resolution() {
        let PreparedAction::Send { message, .. } =
            prepare_workspace("/srv/horus/project").expect("prepare workspace")
        else {
            panic!("workspace change must send a gateway operation");
        };

        assert!(matches!(
            message,
            ClientMessage::SetWorkspace { path, .. }
                if path == std::path::Path::new("/srv/horus/project")
        ));
    }

    #[test]
    fn login_uses_only_the_local_manifest_default_environment() {
        let advertised = [provider_status(
            "kimi",
            ProviderAuthKind::ApiKey,
            Some("AWS_SECRET_ACCESS_KEY"),
        )];

        let PreparedAction::Send { message, .. } =
            prepare_login_with("kimi", &advertised, |name| {
                assert_eq!(name, "MOONSHOT_API_KEY");
                Some("moonshot-secret".into())
            })
            .expect("prepare login")
        else {
            panic!("API-key login must send a credential");
        };

        assert!(matches!(
            message,
            ClientMessage::SetProviderCredential { provider, api_key, .. }
                if provider == "kimi" && api_key == "moonshot-secret"
        ));
    }

    #[test]
    fn custom_endpoint_uses_its_manifest_default_environment() {
        let advertised = [provider_status(
            "responses",
            ProviderAuthKind::ApiKey,
            Some("OPENAI_API_KEY"),
        )];
        let lookups = Cell::new(0);

        let PreparedAction::Send { message, .. } =
            prepare_login_with("responses", &advertised, |name| {
                lookups.set(lookups.get() + 1);
                assert_eq!(name, "OPENAI_API_KEY");
                Some("responses-secret".into())
            })
            .expect("prepare custom login")
        else {
            panic!("custom login must send a credential");
        };

        assert_eq!(lookups.get(), 1);
        assert!(matches!(
            message,
            ClientMessage::SetProviderCredential { provider, api_key, .. }
                if provider == "responses" && api_key == "responses-secret"
        ));
    }

    #[test]
    fn browser_login_does_not_read_an_environment_variable() {
        let advertised = [provider_status(
            "openai_codex",
            ProviderAuthKind::DeviceCode,
            None,
        )];

        let PreparedAction::Send { message, .. } =
            prepare_login_with("openai_codex", &advertised, |_| {
                panic!("device login must not read an environment variable")
            })
            .expect("prepare device login")
        else {
            panic!("device login must send a request");
        };

        assert!(matches!(
            message,
            ClientMessage::StartProviderLogin { provider, .. } if provider == "openai_codex"
        ));
    }

    #[test]
    fn missing_default_environment_names_the_requirement_and_retry() {
        let advertised = [provider_status("kimi", ProviderAuthKind::ApiKey, None)];

        let message = config_error(prepare_login_with("kimi", &advertised, |_| None));

        assert!(message.contains("MOONSHOT_API_KEY"));
        assert!(message.contains("/login kimi"));
    }

    #[test]
    fn authentication_mismatch_fails_before_reading_a_secret() {
        let advertised = [provider_status("kimi", ProviderAuthKind::DeviceCode, None)];

        let message = config_error(prepare_login_with("kimi", &advertised, |_| {
            panic!("mismatched provider must not read an environment variable")
        }));

        assert!(message.contains("authentication does not match"));
    }
}
