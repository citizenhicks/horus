//! Sandboxed execution and its approval boundary.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::Deserialize;
use serde::Serialize;

use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::backend::checkpoint::CheckpointStore;
use crate::backend::model::ToolCall;
use crate::protocol::EventMsg;
use crate::protocol::FrontendBlock;
use crate::protocol::FrontendContribution;
use crate::protocol::FrontendEvent;
use crate::protocol::ReviewDecision;

mod approval;
mod background;
pub mod local;
mod process_group;

pub use approval::ApprovalPolicy;
#[doc(hidden)]
pub use process_group::ProcessGroupGuard;

use approval::Approval;
pub(crate) use background::BackgroundCommandPoll;
use background::BackgroundCommands;

/// Deny-by-default macOS Seatbelt prelude shared by first-party sandbox backends.
///
/// Backends must append their own filesystem allow rules before using it.
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub const MACOS_SEATBELT_BASE_POLICY: &str = include_str!("seatbelt_base_policy.sbpl");

/// Whether a sandbox backend permits network access for one command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAccess {
    #[default]
    Denied,
    Allowed,
}

/// Bounded output from a sandboxed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// One byte stream emitted by a sandboxed command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStream {
    Stdout,
    Stderr,
}

/// Optional observer for bounded command output consumers.
#[derive(Clone, Default)]
pub struct CommandOutputSink {
    callback: Option<Arc<CommandOutputCallback>>,
}

type CommandOutputCallback = dyn Fn(CommandStream, &[u8]) + Send + Sync;

impl CommandOutputSink {
    pub(crate) fn new(callback: impl Fn(CommandStream, &[u8]) + Send + Sync + 'static) -> Self {
        Self {
            callback: Some(Arc::new(callback)),
        }
    }

    /// Publishes one output chunk while the backend continues draining the stream.
    pub fn write(&self, stream: CommandStream, bytes: &[u8]) {
        if let Some(callback) = &self.callback {
            callback(stream, bytes);
        }
    }
}

/// Implements one sandbox execution environment.
pub trait SandboxBackend: Send + Sync {
    /// Reads a UTF-8 file.
    fn read<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String>>;

    /// Writes a UTF-8 file.
    fn write<'a>(&'a self, path: &'a str, content: &'a str) -> BoxFuture<'a, Result<()>>;

    /// Runs a shell command and forwards drained output under the requested isolation.
    fn execute<'a>(
        &'a self,
        command: &'a str,
        network_access: NetworkAccess,
        output: CommandOutputSink,
    ) -> BoxFuture<'a, Result<CommandOutput>>;
}

/// Approval-owning boundary around one execution backend.
pub struct Sandbox {
    backend: Arc<dyn SandboxBackend>,
    approval: Approval,
    background: BackgroundCommands,
}

impl Sandbox {
    /// Creates a sandbox with its initial approval policy.
    #[must_use]
    pub fn new(backend: Arc<dyn SandboxBackend>, policy: ApprovalPolicy) -> Self {
        Self {
            backend,
            approval: Approval::new(policy),
            background: BackgroundCommands::default(),
        }
    }

    /// Reads a UTF-8 file.
    pub fn read<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String>> {
        self.backend.read(path)
    }

    /// Writes a UTF-8 file when this call has mutation authority.
    pub fn write<'a>(
        &'a self,
        path: &'a str,
        content: &'a str,
        permissions: &'a ToolPermissions,
    ) -> BoxFuture<'a, Result<()>> {
        if !permissions.mutation {
            return Box::pin(async {
                Err(Error::Sandbox(
                    "tool call is not authorized to mutate the workspace".into(),
                ))
            });
        }
        self.backend.write(path, content)
    }

    /// Runs a command when this call has mutation authority.
    pub fn execute<'a>(
        &'a self,
        command: &'a str,
        permissions: &'a ToolPermissions,
    ) -> BoxFuture<'a, Result<CommandOutput>> {
        if !permissions.mutation {
            return Box::pin(async {
                Err(Error::Sandbox(
                    "tool call is not authorized to execute commands".into(),
                ))
            });
        }
        self.backend.execute(
            command,
            permissions.network_access,
            CommandOutputSink::default(),
        )
    }

    pub(crate) fn start_background(
        &self,
        command: String,
        permissions: &ToolPermissions,
    ) -> Result<String> {
        if !permissions.mutation {
            return Err(Error::Sandbox(
                "tool call is not authorized to execute commands".into(),
            ));
        }
        self.background.start(
            &permissions.session_id,
            Arc::clone(&self.backend),
            command,
            permissions.network_access,
        )
    }

    pub(crate) async fn poll_background(
        &self,
        id: &str,
        permissions: &ToolPermissions,
    ) -> Result<BackgroundCommandPoll> {
        self.background.poll(&permissions.session_id, id).await
    }

    pub(crate) async fn stop_background(
        &self,
        id: &str,
        permissions: &ToolPermissions,
    ) -> Result<BackgroundCommandPoll> {
        self.background.stop(&permissions.session_id, id).await
    }

    pub(crate) fn name(&self) -> &'static str {
        "sandbox"
    }

    pub(crate) fn frontend(&self) -> FrontendContribution {
        self.approval.frontend()
    }

    pub(crate) fn render(&self, event: &EventMsg) -> Option<FrontendBlock> {
        self.approval.render(event)
    }

    pub(crate) async fn initialize(
        &self,
        session_id: &str,
        checkpoints: &Arc<dyn CheckpointStore>,
    ) -> Result<Vec<FrontendEvent>> {
        self.approval.initialize(session_id, checkpoints).await
    }

    pub(crate) async fn command(
        &self,
        session_id: &str,
        checkpoints: &Arc<dyn CheckpointStore>,
        command: &str,
        arguments: &str,
    ) -> Result<Vec<FrontendEvent>> {
        self.approval
            .command(session_id, checkpoints, command, arguments)
            .await
    }

    pub(crate) fn authorize(
        &self,
        session_id: &str,
        calls: &[ToolCall],
        mutation_call_ids: &[String],
    ) -> Result<SandboxAuthorization> {
        self.approval
            .authorize(session_id, calls, mutation_call_ids)
    }

    pub(crate) fn resolve_approval(
        &self,
        session_id: &str,
        calls: &[ToolCall],
        approval_call_ids: &[String],
        decision: &ReviewDecision,
        permissions: SandboxPermissions,
    ) -> Result<SandboxPermissions> {
        self.approval
            .resolve(session_id, calls, approval_call_ids, decision, permissions)
    }

    pub(crate) async fn shutdown(&self, session_id: &str) -> Result<()> {
        let approval = self.approval.shutdown(session_id);
        let background = self.background.shutdown(session_id).await;
        approval.and(background)
    }
}

/// Batch authority issued by the sandbox approval policy.
#[derive(Debug)]
pub(crate) struct SandboxPermissions {
    session_id: String,
    network_access: NetworkAccess,
    mutation_call_ids: BTreeSet<String>,
}

impl SandboxPermissions {
    fn new(
        session_id: impl Into<String>,
        network_access: NetworkAccess,
        mutation_call_ids: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            network_access,
            mutation_call_ids: mutation_call_ids.into_iter().collect(),
        }
    }

    pub(crate) fn restore(
        session_id: impl Into<String>,
        network_access: NetworkAccess,
        mutation_call_ids: impl IntoIterator<Item = String>,
    ) -> Self {
        Self::new(session_id, network_access, mutation_call_ids)
    }

    pub(crate) fn network_access(&self) -> NetworkAccess {
        self.network_access
    }

    pub(crate) fn mutation_call_ids(&self) -> Vec<String> {
        self.mutation_call_ids.iter().cloned().collect()
    }

    pub(crate) fn for_call(&self, call_id: &str) -> ToolPermissions {
        ToolPermissions {
            session_id: self.session_id.clone(),
            network_access: self.network_access,
            mutation: self.mutation_call_ids.contains(call_id),
        }
    }

    fn allow_mutations(&mut self, call_ids: impl IntoIterator<Item = String>) {
        self.mutation_call_ids.extend(call_ids);
    }
}

/// Opaque authority attached to exactly one tool call.
pub struct ToolPermissions {
    session_id: String,
    network_access: NetworkAccess,
    mutation: bool,
}

pub(crate) enum SandboxAuthorization {
    Execute(SandboxPermissions),
    Approval {
        request: SandboxApprovalRequest,
        permissions: SandboxPermissions,
    },
}

pub(crate) struct SandboxApprovalRequest {
    pub(crate) id: String,
    pub(crate) reason: String,
    pub(crate) call_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::sandbox::local::LocalSandbox;

    #[tokio::test]
    async fn mutation_fails_closed_without_per_call_authority() {
        let workspace = tempfile::tempdir().expect("workspace");
        let sandbox = Sandbox::new(
            Arc::new(LocalSandbox::new(workspace.path()).expect("backend")),
            ApprovalPolicy::On,
        );
        let permissions = SandboxPermissions::new("session", NetworkAccess::Allowed, Vec::new());

        assert!(
            sandbox
                .write("blocked.txt", "blocked", &permissions.for_call("call"))
                .await
                .is_err()
        );
        assert!(!workspace.path().join("blocked.txt").exists());
    }
}
