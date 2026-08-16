use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use mobius_cli::gateway_accounts::GatewayAccounts;
use mobius_gateway::client::Endpoint;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    match frontend_command(&arguments)? {
        Some(FrontendCommand::Init(state_dir)) => initialize_cloudflare(state_dir).await?,
        Some(FrontendCommand::Dashboard(state_dir)) => {
            mobius_cli::frontend::run_gateway_dashboard(state_dir).await?
        }
        Some(FrontendCommand::Provider(state_dir)) => {
            mobius_cli::frontend::run_gateway_provider(state_dir).await?
        }
        None => {
            mobius_gateway::command::run(arguments, save_local_client, load_local_client).await?
        }
    }
    Ok(())
}

enum FrontendCommand {
    Init(PathBuf),
    Dashboard(PathBuf),
    Provider(PathBuf),
}

fn frontend_command(arguments: &[OsString]) -> mobius_gateway::Result<Option<FrontendCommand>> {
    if arguments.is_empty() {
        return mobius_gateway::config::state_dir()
            .map(FrontendCommand::Dashboard)
            .map(Some);
    }
    if arguments.first().is_some_and(|value| value == "init") {
        let interactive = match &arguments[1..] {
            [] => true,
            [flag, _] => flag == OsStr::new("--state-dir"),
            _ => false,
        };
        if interactive {
            return state_dir_argument(&arguments[1..])
                .map(FrontendCommand::Init)
                .map(Some);
        }
    }
    if arguments.first().is_some_and(|value| value == "provider") {
        return state_dir_argument(&arguments[1..])
            .map(FrontendCommand::Provider)
            .map(Some);
    }
    if arguments
        .first()
        .is_some_and(|value| value == "--state-dir")
    {
        return state_dir_argument(arguments)
            .map(FrontendCommand::Dashboard)
            .map(Some);
    }
    Ok(None)
}

async fn initialize_cloudflare(state_dir: PathBuf) -> mobius_gateway::Result<()> {
    let Some(setup) = mobius_cli::frontend::run_cloudflare_setup().await? else {
        return Ok(());
    };
    if state_dir.try_exists()? {
        if !mobius_cli::frontend::confirm_gateway_reinitialize(&state_dir).await? {
            return Ok(());
        }
        mobius_gateway::command::reset_gateway_state(state_dir.clone())?;
    }
    match setup {
        mobius_cli::frontend::CloudflareInit::Quick => {
            mobius_gateway::command::initialize_quick_cloudflare(state_dir.clone())?;
        }
        mobius_cli::frontend::CloudflareInit::Named { hostname, token } => {
            mobius_gateway::command::initialize_named_cloudflare(
                state_dir.clone(),
                hostname,
                token,
            )?;
        }
    }
    mobius_gateway::command::run(
        vec![
            "connect".into(),
            "--state-dir".into(),
            state_dir.into_os_string(),
        ],
        save_local_client,
        load_local_client,
    )
    .await
}

fn save_local_client(endpoint: &Endpoint, token: String) -> mobius_gateway::Result<()> {
    let mut accounts = GatewayAccounts::load()?;
    accounts.add(endpoint, token)?;
    accounts.save()
}

fn load_local_client(endpoint: &Endpoint) -> mobius_gateway::Result<Option<String>> {
    Ok(GatewayAccounts::load()?.token(endpoint).map(str::to_owned))
}

fn state_dir_argument(arguments: &[OsString]) -> mobius_gateway::Result<PathBuf> {
    match arguments {
        [] => mobius_gateway::config::state_dir(),
        [flag, path] if flag == OsStr::new("--state-dir") => Ok(path.into()),
        _ => Err(mobius_gateway::Error::Config(
            mobius_gateway::command::USAGE.into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_command_and_provider_select_gateway_frontends() {
        assert!(matches!(
            frontend_command(&[]).expect("dashboard command"),
            Some(FrontendCommand::Dashboard(_))
        ));
        assert!(matches!(
            frontend_command(&["init".into(), "--state-dir".into(), "/tmp/gateway".into()])
                .expect("init command"),
            Some(FrontendCommand::Init(path)) if path == std::path::Path::new("/tmp/gateway")
        ));
        assert!(matches!(
            frontend_command(&["provider".into(), "--state-dir".into(), "/tmp/gateway".into()])
                .expect("provider command"),
            Some(FrontendCommand::Provider(path)) if path == std::path::Path::new("/tmp/gateway")
        ));
    }
}
