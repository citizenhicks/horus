//! Gateway sandbox that hides gateway state from every command mode.

use std::path::Path;
use std::time::Duration;

use horus::backend::model::provider::{ProviderAuth, providers};
use horus::backend::sandbox::{
    CommandMode, CommandOutput, CommandOutputSink, NetworkAccess, SandboxBackend, SandboxMode,
    local::LocalSandbox,
};
use horus::{BoxFuture, Error, Result};

const GIT_ENVIRONMENT: [(&str, &str); 7] = [
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_DISCOVERY_ACROSS_FILESYSTEM", "1"),
    ("GIT_NO_LAZY_FETCH", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("LC_ALL", "C"),
];
const GIT_ARGUMENTS: [&str; 5] = [
    "--no-pager",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
];
const GATEWAY_CREDENTIAL_ENVIRONMENT: [&str; 3] =
    ["HORUS_GATEWAY_TOKEN", "TUNNEL_TOKEN", "TUNNEL_TOKEN_FILE"];

fn provider_credential_environment() -> impl Iterator<Item = &'static str> {
    providers()
        .iter()
        .filter_map(|provider| match provider.auth() {
            ProviderAuth::ApiKey(environment) => Some(environment),
            ProviderAuth::Browser(_) => None,
        })
}

/// Workspace backend that protects gateway state even from full-access commands.
pub struct GatewaySandbox {
    delegate: LocalSandbox,
}

impl GatewaySandbox {
    /// Creates a fail-closed command sandbox for a gateway host.
    pub fn new(
        workspace: &Path,
        state_dir: &Path,
        tls_key: Option<&Path>,
        timeout: Duration,
    ) -> Result<Self> {
        if timeout.is_zero() {
            return Err(Error::Config("command timeout must be positive".into()));
        }
        let root = std::fs::canonicalize(workspace)?;
        let state_dir = std::fs::canonicalize(state_dir)?;
        let tls_key = match tls_key {
            Some(path) => std::fs::canonicalize(path)?,
            None => state_dir.clone(),
        };
        if root.starts_with(&state_dir) || state_dir.starts_with(&root) {
            return Err(Error::Config(
                "gateway state directory and chat workspace must not overlap".into(),
            ));
        }
        if tls_key.starts_with(&root) {
            return Err(Error::Config(
                "TLS private key must be stored outside every chat workspace".into(),
            ));
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(Error::Config(
            "gateway command sandbox supports macOS and Linux only".into(),
        ));

        let mut delegate = LocalSandbox::new(&root)?
            .command_timeout(timeout)?
            .deny_read(&state_dir)?
            .deny_read(&tls_key)?;
        for environment in GATEWAY_CREDENTIAL_ENVIRONMENT
            .into_iter()
            .chain(provider_credential_environment())
        {
            delegate = delegate.deny_environment(environment);
        }
        Ok(Self { delegate })
    }

    pub(crate) async fn execute_git(&self, args: &[&str]) -> Result<CommandOutput> {
        let mut arguments = GIT_ARGUMENTS.to_vec();
        arguments.extend_from_slice(args);
        self.delegate
            .execute_read_only("git", &arguments, &GIT_ENVIRONMENT)
            .await
    }

    pub(crate) async fn read_workspace_range(
        &self,
        path: &str,
        offset: u64,
        max_bytes: usize,
    ) -> Result<(Vec<u8>, Option<u64>)> {
        self.delegate.read_range(path, offset, max_bytes).await
    }

    pub(crate) async fn switch_git_branch(&self, branch: &str) -> Result<CommandOutput> {
        let mut arguments = GIT_ARGUMENTS.to_vec();
        arguments.extend_from_slice(&[
            "switch",
            "--no-guess",
            "--no-recurse-submodules",
            "--",
            branch,
        ]);
        self.delegate
            .execute_git_mutation(&arguments, &GIT_ENVIRONMENT)
            .await
    }
}

impl SandboxBackend for GatewaySandbox {
    fn read<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String>> {
        self.delegate.read(path)
    }

    fn read_bytes<'a>(&'a self, path: &'a str, max_bytes: usize) -> BoxFuture<'a, Result<Vec<u8>>> {
        self.delegate.read_bytes(path, max_bytes)
    }

    fn write<'a>(&'a self, path: &'a str, content: &'a str) -> BoxFuture<'a, Result<()>> {
        self.delegate.write(path, content)
    }

    fn execute<'a>(
        &'a self,
        script: &'a str,
        sandbox_mode: SandboxMode,
        network_access: NetworkAccess,
        mode: CommandMode,
        output: CommandOutputSink,
    ) -> BoxFuture<'a, Result<CommandOutput>> {
        self.delegate
            .execute(script, sandbox_mode, network_access, mode, output)
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_provider_api_key_environment_is_hidden_from_commands() {
        assert_eq!(
            provider_credential_environment().collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "ANTHROPIC_API_KEY",
                "DEEPSEEK_API_KEY",
                "MOONSHOT_API_KEY",
                "OPENAI_API_KEY",
                "OPENROUTER_API_KEY",
            ])
        );
    }

    #[test]
    fn gateway_bearer_environment_is_hidden_from_commands() {
        assert_eq!(
            GATEWAY_CREDENTIAL_ENVIRONMENT,
            ["HORUS_GATEWAY_TOKEN", "TUNNEL_TOKEN", "TUNNEL_TOKEN_FILE"]
        );
    }

    #[test]
    fn construction_rejects_both_state_workspace_overlap_directions() {
        let workspace_parent = tempfile::tempdir().expect("workspace parent");
        let state_inside = workspace_parent.path().join("state");
        std::fs::create_dir(&state_inside).expect("nested state");
        let state_parent = tempfile::tempdir().expect("state parent");
        let workspace_inside = state_parent.path().join("workspace");
        std::fs::create_dir(&workspace_inside).expect("nested workspace");

        let state_inside_error = match GatewaySandbox::new(
            workspace_parent.path(),
            &state_inside,
            None,
            Duration::from_secs(5),
        ) {
            Ok(_) => panic!("state inside workspace must fail"),
            Err(error) => error,
        };
        let workspace_inside_error = match GatewaySandbox::new(
            &workspace_inside,
            state_parent.path(),
            None,
            Duration::from_secs(5),
        ) {
            Ok(_) => panic!("workspace inside state must fail"),
            Err(error) => error,
        };

        assert!(state_inside_error.to_string().contains("must not overlap"));
        assert!(
            workspace_inside_error
                .to_string()
                .contains("must not overlap")
        );
    }

    #[test]
    fn construction_rejects_a_tls_key_inside_the_chat_workspace() {
        let workspace = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        let private_key = workspace.path().join("private-key.pem");
        std::fs::write(&private_key, "private key").expect("private key");

        let error = match GatewaySandbox::new(
            workspace.path(),
            state.path(),
            Some(&private_key),
            Duration::from_secs(5),
        ) {
            Ok(_) => panic!("workspace TLS key must fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("outside every chat workspace"));
    }

    #[tokio::test]
    async fn commands_cannot_read_gateway_state_or_tls_key() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        let credentials = tempfile::tempdir().expect("credentials");
        let outside = tempfile::tempdir().expect("outside");
        let tls_key = credentials.path().join("private-key.pem");
        let initialized = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(workspace.path())
            .status()
            .expect("initialize Git repository");
        assert!(initialized.success());
        std::fs::write(state.path().join("sentinel"), "gateway-secret").expect("state sentinel");
        std::fs::write(&tls_key, "tls-secret").expect("TLS key");
        symlink(state.path(), workspace.path().join("state-link")).expect("state symlink");
        symlink(&tls_key, workspace.path().join("tls-link")).expect("TLS key symlink");
        let sandbox = GatewaySandbox::new(
            workspace.path(),
            state.path(),
            Some(&tls_key),
            Duration::from_secs(5),
        )
        .expect("gateway sandbox");

        for (label, sandbox_mode, mode, network_access, writes_outside) in [
            (
                "foreground",
                SandboxMode::WorkspaceWrite,
                CommandMode::Foreground,
                NetworkAccess::Denied,
                false,
            ),
            (
                "background",
                SandboxMode::WorkspaceWrite,
                CommandMode::Background,
                NetworkAccess::Allowed,
                false,
            ),
            (
                "full-access",
                SandboxMode::DangerFullAccess,
                CommandMode::Foreground,
                NetworkAccess::Allowed,
                true,
            ),
        ] {
            let outside_target = outside.path().join(label);
            let script = format!(
                "touch .git/{label}; touch {} || true; cat {}/sentinel || true; cat {} || true; cat state-link/sentinel || true; cat tls-link || true; printf changed > {}/sentinel || true; printf changed > {} || true; printf changed > state-link/sentinel || true; printf changed > tls-link || true; kill -0 {} && printf gateway-process-visible || true",
                outside_target.display(),
                state.path().display(),
                tls_key.display(),
                state.path().display(),
                tls_key.display(),
                std::process::id()
            );
            let output = sandbox
                .execute(
                    &script,
                    sandbox_mode,
                    network_access,
                    mode,
                    CommandOutputSink::default(),
                )
                .await
                .expect("sandboxed command");

            assert_eq!(output.exit_code, 0, "{}", output.stderr);
            assert!(workspace.path().join(".git").join(label).is_file());
            assert!(!output.stdout.contains("gateway-secret"));
            assert!(!output.stdout.contains("tls-secret"));
            assert!(!output.stdout.contains("gateway-process-visible"));
            assert_eq!(outside_target.is_file(), writes_outside);
            assert_eq!(
                std::fs::read_to_string(state.path().join("sentinel")).expect("state sentinel"),
                "gateway-secret"
            );
            assert_eq!(
                std::fs::read_to_string(&tls_key).expect("TLS key"),
                "tls-secret"
            );
        }
    }

    #[tokio::test]
    async fn binary_reads_preserve_workspace_file_bytes() {
        let workspace = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        let expected = [0, 159, 255, 10];
        std::fs::write(workspace.path().join("report.bin"), expected).expect("binary file");
        let sandbox =
            GatewaySandbox::new(workspace.path(), state.path(), None, Duration::from_secs(5))
                .expect("gateway sandbox");

        let actual = sandbox
            .read_bytes("report.bin", expected.len())
            .await
            .expect("read binary file");

        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn commands_inherit_the_host_home() {
        let workspace = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        let sandbox =
            GatewaySandbox::new(workspace.path(), state.path(), None, Duration::from_secs(5))
                .expect("gateway sandbox");

        let output = sandbox
            .execute(
                r#"printf '%s' "$HOME""#,
                SandboxMode::WorkspaceWrite,
                NetworkAccess::Denied,
                CommandMode::Foreground,
                CommandOutputSink::default(),
            )
            .await
            .expect("sandboxed command");

        assert_eq!(output.stdout, std::env::var("HOME").expect("host HOME"));
    }
}
