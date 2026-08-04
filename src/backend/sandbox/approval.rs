use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;

use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use uuid::Uuid;

use super::NetworkAccess;
use super::SandboxApprovalRequest;
use super::SandboxAuthorization;
use super::SandboxPermissions;
use crate::Error;
use crate::Result;
use crate::backend::checkpoint::CheckpointStore;
use crate::backend::model::ToolCall;
use crate::preview_json;
use crate::protocol::EventMsg;
use crate::protocol::FrontendBlock;
use crate::protocol::FrontendCommand;
use crate::protocol::FrontendContribution;
use crate::protocol::FrontendEvent;
use crate::protocol::FrontendSlot;
use crate::protocol::FrontendTone;
use crate::protocol::FrontendWidget;
use crate::protocol::ReviewDecision;

const CAPABILITY: &str = "sandbox";
const POLICY_KEY: &str = "sandbox.approval_policy";
const MAX_SESSION_APPROVALS: usize = 64;

/// Whether approval-required tools pause before sandboxed execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    #[default]
    On,
    Allow,
    AllowNetwork,
}

impl ApprovalPolicy {
    fn label(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Allow => "allow (no network)",
            Self::AllowNetwork => "allow (network)",
        }
    }

    fn network_access(self) -> NetworkAccess {
        match self {
            Self::On | Self::Allow => NetworkAccess::Denied,
            Self::AllowNetwork => NetworkAccess::Allowed,
        }
    }
}

#[derive(Default)]
struct ApprovalState {
    policy: ApprovalPolicy,
    approved_for_session: BTreeSet<[u8; 32]>,
}

pub(super) struct Approval {
    default_policy: ApprovalPolicy,
    states: Mutex<BTreeMap<String, ApprovalState>>,
}

impl Approval {
    pub(super) fn new(default_policy: ApprovalPolicy) -> Self {
        Self {
            default_policy,
            states: Mutex::new(BTreeMap::new()),
        }
    }

    pub(super) fn frontend(&self) -> FrontendContribution {
        FrontendContribution {
            capability: CAPABILITY.into(),
            commands: vec![FrontendCommand {
                name: "permissions".into(),
                arguments: "<on|allow|network>".into(),
                description: "set approvals and sandbox network access".into(),
            }],
            widgets: vec![widget(self.default_policy)],
            references: Vec::new(),
            active_input: None,
        }
    }

    pub(super) fn render(&self, event: &EventMsg) -> Option<FrontendBlock> {
        let EventMsg::ExecApprovalRequest(request) = event else {
            return None;
        };
        let tools = request
            .calls
            .iter()
            .map(|call| format!("{} {}", call.name, preview_json(&call.arguments)))
            .collect::<Vec<_>>()
            .join("\n  ");
        Some(FrontendBlock {
            id: None,
            group: None,
            append: false,
            pending: false,
            text: format!("approval required\n{}\n  {tools}", request.reason),
            format: crate::protocol::FrontendBlockFormat::PlainText,
            tone: FrontendTone::Warning,
        })
    }

    pub(super) async fn initialize(
        &self,
        session_id: &str,
        checkpoints: &Arc<dyn CheckpointStore>,
    ) -> Result<Vec<FrontendEvent>> {
        let policy = checkpoints
            .load_state(session_id, POLICY_KEY)
            .await?
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or(self.default_policy);
        self.states.lock().map_err(|_| state_lock_error())?.insert(
            session_id.into(),
            ApprovalState {
                policy,
                approved_for_session: BTreeSet::new(),
            },
        );
        Ok(vec![FrontendEvent::Widget {
            capability: CAPABILITY.into(),
            item: widget(policy),
        }])
    }

    pub(super) async fn command(
        &self,
        session_id: &str,
        checkpoints: &Arc<dyn CheckpointStore>,
        command: &str,
        arguments: &str,
    ) -> Result<Vec<FrontendEvent>> {
        if command != "permissions" {
            return Err(Error::Unknown(format!("sandbox command `{command}`")));
        }
        let policy = match arguments.trim() {
            "on" => ApprovalPolicy::On,
            "allow" => ApprovalPolicy::Allow,
            "network" => ApprovalPolicy::AllowNetwork,
            "" => {
                let policy = self.state_policy(session_id)?;
                return Ok(vec![render(widget(policy).text, FrontendTone::Neutral)]);
            }
            _ => {
                return Ok(vec![render(
                    "! usage: permissions <on|allow|network>",
                    FrontendTone::Warning,
                )]);
            }
        };
        checkpoints
            .save_state(session_id, POLICY_KEY, &serde_json::to_value(policy)?)
            .await?;
        let mut states = self.states.lock().map_err(|_| state_lock_error())?;
        let state = states
            .get_mut(session_id)
            .ok_or_else(state_not_initialized)?;
        state.policy = policy;
        state.approved_for_session.clear();
        drop(states);
        Ok(vec![
            FrontendEvent::Widget {
                capability: CAPABILITY.into(),
                item: widget(policy),
            },
            render(
                format!("◆ permissions set to {}", policy.label()),
                FrontendTone::Success,
            ),
        ])
    }

    pub(super) fn authorize(
        &self,
        session_id: &str,
        calls: &[ToolCall],
        mutation_call_ids: &[String],
    ) -> Result<SandboxAuthorization> {
        let (policy, approved_for_session) = {
            let states = self.states.lock().map_err(|_| state_lock_error())?;
            let state = states.get(session_id).ok_or_else(state_not_initialized)?;
            (state.policy, state.approved_for_session.clone())
        };
        let calls_by_id = calls
            .iter()
            .map(|call| (call.call_id.as_str(), call))
            .collect::<BTreeMap<_, _>>();
        let mut approved = Vec::new();
        let mut requested = Vec::new();
        for call_id in mutation_call_ids {
            let call = calls_by_id
                .get(call_id.as_str())
                .ok_or_else(|| Error::Tool(format!("unknown mutation call `{call_id}`")))?;
            if policy != ApprovalPolicy::On
                || approved_for_session.contains(&call_key(session_id, call)?)
            {
                approved.push(call_id.clone());
            } else {
                requested.push(call_id.clone());
            }
        }
        let permissions = SandboxPermissions::new(session_id, policy.network_access(), approved);
        if requested.is_empty() {
            return Ok(SandboxAuthorization::Execute(permissions));
        }
        Ok(SandboxAuthorization::Approval {
            request: SandboxApprovalRequest {
                id: Uuid::new_v4().to_string(),
                reason: "one or more tools can mutate files or execute code".into(),
                call_ids: requested,
            },
            permissions,
        })
    }

    pub(super) fn resolve(
        &self,
        session_id: &str,
        calls: &[ToolCall],
        approval_call_ids: &[String],
        decision: &ReviewDecision,
        mut permissions: SandboxPermissions,
    ) -> Result<SandboxPermissions> {
        if !matches!(
            decision,
            ReviewDecision::Approved | ReviewDecision::ApprovedForSession
        ) {
            return Ok(permissions);
        }
        let calls_by_id = calls
            .iter()
            .map(|call| (call.call_id.as_str(), call))
            .collect::<BTreeMap<_, _>>();
        for call_id in approval_call_ids {
            if !calls_by_id.contains_key(call_id.as_str()) {
                return Err(Error::Tool(format!(
                    "approval references unknown call `{call_id}`"
                )));
            }
        }
        permissions.allow_mutations(approval_call_ids.iter().cloned());
        if !matches!(decision, ReviewDecision::ApprovedForSession) {
            return Ok(permissions);
        }
        let keys = approval_call_ids
            .iter()
            .map(|call_id| call_key(session_id, calls_by_id[call_id.as_str()]))
            .collect::<Result<Vec<_>>>()?;
        let mut states = self.states.lock().map_err(|_| state_lock_error())?;
        let state = states
            .get_mut(session_id)
            .ok_or_else(state_not_initialized)?;
        for key in keys {
            if state.approved_for_session.len() >= MAX_SESSION_APPROVALS {
                state.approved_for_session.clear();
            }
            state.approved_for_session.insert(key);
        }
        Ok(permissions)
    }

    pub(super) fn shutdown(&self, session_id: &str) -> Result<()> {
        self.states
            .lock()
            .map_err(|_| state_lock_error())?
            .remove(session_id);
        Ok(())
    }

    fn state_policy(&self, session_id: &str) -> Result<ApprovalPolicy> {
        self.states
            .lock()
            .map_err(|_| state_lock_error())?
            .get(session_id)
            .map(|state| state.policy)
            .ok_or_else(state_not_initialized)
    }
}

fn widget(policy: ApprovalPolicy) -> FrontendWidget {
    FrontendWidget {
        id: "approval_policy".into(),
        slot: FrontendSlot::Header,
        text: match policy {
            ApprovalPolicy::On => "approval ON".into(),
            ApprovalPolicy::Allow => "approval ALLOW".into(),
            ApprovalPolicy::AllowNetwork => "approval NETWORK".into(),
        },
        tone: if policy == ApprovalPolicy::On {
            FrontendTone::Neutral
        } else {
            FrontendTone::Warning
        },
        action: None,
    }
}

fn render(text: impl Into<String>, tone: FrontendTone) -> FrontendEvent {
    FrontendEvent::Render {
        capability: CAPABILITY.into(),
        block: FrontendBlock {
            id: None,
            group: None,
            append: false,
            pending: false,
            text: text.into(),
            format: crate::protocol::FrontendBlockFormat::PlainText,
            tone,
        },
    }
}

fn call_key(session_id: &str, call: &ToolCall) -> Result<[u8; 32]> {
    let mut writer = DigestWriter(Sha256::new());
    serde_json::to_writer(&mut writer, &(session_id, &call.name, &call.arguments))?;
    Ok(writer.0.finalize().into())
}

struct DigestWriter(Sha256);

impl Write for DigestWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn state_lock_error() -> Error {
    Error::Stopped("approval state lock poisoned".into())
}

fn state_not_initialized() -> Error {
    Error::Stopped("approval state is not initialized".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_network_permission_enables_backend_network() {
        assert_eq!(ApprovalPolicy::On.network_access(), NetworkAccess::Denied);
        assert_eq!(
            ApprovalPolicy::Allow.network_access(),
            NetworkAccess::Denied
        );
        assert_eq!(
            ApprovalPolicy::AllowNetwork.network_access(),
            NetworkAccess::Allowed
        );
    }

    #[test]
    fn approval_rendering_is_frontend_neutral() {
        let block = Approval::new(ApprovalPolicy::On)
            .render(&EventMsg::ExecApprovalRequest(
                crate::protocol::ExecApprovalRequestEvent {
                    id: "approval".into(),
                    turn_id: "turn".into(),
                    calls: vec![crate::protocol::ApprovalCall {
                        call_id: "call".into(),
                        name: "bash".into(),
                        arguments: serde_json::json!({"command": "true"}),
                    }],
                    reason: "command execution".into(),
                },
            ))
            .expect("approval block");

        assert!(
            block
                .text
                .starts_with("approval required\ncommand execution\n")
        );
        assert!(!block.text.contains('[') && !block.text.contains(']'));
    }

    #[test]
    fn approval_grants_only_the_reviewed_call() {
        let approval = Approval::new(ApprovalPolicy::On);
        approval
            .states
            .lock()
            .expect("approval state")
            .insert("session".into(), ApprovalState::default());
        let calls = [ToolCall {
            call_id: "write".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path": "a"}),
        }];
        let SandboxAuthorization::Approval {
            request,
            permissions,
        } = approval
            .authorize("session", &calls, &["write".into()])
            .expect("authorization")
        else {
            panic!("approval required");
        };
        assert!(!permissions.for_call("write").mutation);

        let permissions = approval
            .resolve(
                "session",
                &calls,
                &request.call_ids,
                &ReviewDecision::Approved,
                permissions,
            )
            .expect("resolution");
        assert!(permissions.for_call("write").mutation);
    }
}
