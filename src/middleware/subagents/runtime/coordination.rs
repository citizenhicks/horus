use std::collections::BTreeSet;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
use tokio::time::Instant;
use tokio::time::timeout_at;
use uuid::Uuid;

use super::AgentRecord;
use super::AgentStatus;
use super::MAX_MAILBOX_ITEMS;
use super::OnPersistFailure;
use super::Shared;
use super::Stage;
use super::ensure_concurrency_available;
use crate::Error;
use crate::Result;
use crate::agent::AgentSender;

#[derive(Clone, Serialize, Deserialize)]
pub(in crate::middleware::subagents) struct Mail {
    pub(super) id: String,
    pub(super) recipient: String,
    pub(super) from: String,
    pub(super) body: MailBody,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) enum MailBody {
    Message(String),
    Finished {
        status: String,
        message: Option<String>,
    },
}

impl Mail {
    fn message(recipient: &str, from: &str, message: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            recipient: recipient.into(),
            from: from.into(),
            body: MailBody::Message(message),
        }
    }

    pub(in crate::middleware::subagents) fn internal_kind(&self) -> String {
        format!("subagent_mail:{}", self.id)
    }

    pub(in crate::middleware::subagents) fn render(&self) -> String {
        match &self.body {
            MailBody::Message(message) => {
                format!(
                    "<subagent_message from=\"{}\">\n{message}\n</subagent_message>",
                    self.from
                )
            }
            MailBody::Finished { status, message } => format!(
                "<subagent_update agent=\"{}\" status=\"{status}\">\n{}\n</subagent_update>",
                self.from,
                message.as_deref().unwrap_or_default()
            ),
        }
    }
}

pub(in crate::middleware::subagents) enum Followup {
    Queued,
    Start {
        record: AgentRecord,
        sender: Option<AgentSender>,
        previous: AgentStatus,
    },
}

impl Shared {
    pub(in crate::middleware::subagents) async fn receive_mail(
        &self,
        root_id: &str,
        recipient: &str,
        acknowledged: &BTreeSet<String>,
    ) -> Result<Vec<Mail>> {
        if !acknowledged.is_empty() {
            let root = self.root(root_id).await?;
            let has_acknowledged =
                root.state.lock().await.tree.mailbox.iter().any(|mail| {
                    mail.recipient == recipient && acknowledged.contains(mail.id.as_str())
                });
            if has_acknowledged {
                self.mutate_root(root_id, |root| {
                    root.tree.mailbox.retain(|mail| {
                        mail.recipient != recipient || !acknowledged.contains(mail.id.as_str())
                    });
                    Ok(())
                })
                .await?;
            }
        }
        let root = self.root(root_id).await?;
        Ok(root
            .state
            .lock()
            .await
            .tree
            .mailbox
            .iter()
            .filter(|mail| mail.recipient == recipient)
            .cloned()
            .collect())
    }

    pub(in crate::middleware::subagents) async fn queue_message(
        &self,
        root_id: &str,
        from: &str,
        target: &str,
        message: String,
    ) -> Result<()> {
        if from == target {
            return Err(Error::Tool("an agent cannot message itself".into()));
        }
        self.mutate_root(root_id, |root| {
            if target != "/root" && !root.tree.agents.contains_key(target) {
                return Err(Error::Unknown(format!("agent `{target}`")));
            }
            if root.tree.mailbox.len() >= MAX_MAILBOX_ITEMS {
                return Err(Error::Stopped("subagent mailbox is full".into()));
            }
            root.tree
                .mailbox
                .push_back(Mail::message(target, from, message));
            Ok(())
        })
        .await?;
        self.changed.notify_waiters();
        Ok(())
    }

    pub(in crate::middleware::subagents) async fn prepare_followup(
        &self,
        root_id: &str,
        from: &str,
        target: &str,
        message: String,
    ) -> Result<Followup> {
        if from == target {
            return Err(Error::Tool("an agent cannot follow up with itself".into()));
        }
        if target == "/root" {
            return Err(Error::Tool(
                "follow-up tasks cannot target the root agent".into(),
            ));
        }
        let max_concurrency = self.max_concurrency;
        let followup = self
            .commit_root(
                root_id,
                |root| {
                    let status = root
                        .tree
                        .agents
                        .get(target)
                        .ok_or_else(|| Error::Unknown(format!("agent `{target}`")))?
                        .status
                        .clone();
                    if matches!(status, AgentStatus::PendingInit) {
                        if root.tree.mailbox.len() >= MAX_MAILBOX_ITEMS {
                            return Err(Error::Stopped("subagent mailbox is full".into()));
                        }
                        root.tree
                            .mailbox
                            .push_back(Mail::message(target, from, message));
                        return Ok(Stage::Changed(Followup::Queued));
                    }
                    if matches!(status, AgentStatus::Running) {
                        let record = root
                            .tree
                            .agents
                            .get(target)
                            .ok_or_else(|| Error::Unknown(format!("agent `{target}`")))?
                            .clone();
                        let sender =
                            root.senders.get(target).cloned().ok_or_else(|| {
                                Error::Stopped("agent runtime is unavailable".into())
                            })?;
                        return Ok(Stage::Unchanged(Followup::Start {
                            record,
                            sender: Some(sender),
                            previous: status,
                        }));
                    }
                    if matches!(status, AgentStatus::Errored) {
                        return Err(Error::Stopped(format!(
                            "agent `{target}` is {}",
                            status.label()
                        )));
                    }
                    ensure_concurrency_available(&root.tree, max_concurrency)?;
                    let entry = root
                        .tree
                        .agents
                        .get_mut(target)
                        .ok_or_else(|| Error::Unknown(format!("agent `{target}`")))?;
                    let record = entry.clone();
                    entry.status = AgentStatus::PendingInit;
                    entry.last_message = None;
                    let sender = root.senders.get(target).cloned();
                    Ok(Stage::Changed(Followup::Start {
                        record,
                        sender,
                        previous: status,
                    }))
                },
                OnPersistFailure::Abort,
            )
            .await
            .map(Stage::into_output)?;
        if matches!(followup, Followup::Queued) {
            self.changed.notify_waiters();
        }
        Ok(followup)
    }

    pub(in crate::middleware::subagents) async fn wait(
        &self,
        root_id: &str,
        recipient: &str,
        duration: Duration,
    ) -> Result<Vec<String>> {
        let deadline = Instant::now() + duration;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let (sources, active) = self.pending_sources(root_id, recipient).await?;
            if !sources.is_empty() {
                return Ok(sources);
            }
            if !active {
                return Ok(Vec::new());
            }
            if timeout_at(deadline, notified).await.is_err() {
                return Ok(Vec::new());
            }
        }
    }

    async fn pending_sources(&self, root_id: &str, recipient: &str) -> Result<(Vec<String>, bool)> {
        let root = self.root(root_id).await?;
        let root = root.state.lock().await;
        let sources = root
            .tree
            .mailbox
            .iter()
            .filter(|mail| mail.recipient == recipient)
            .map(|mail| mail.from.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let active = root
            .tree
            .agents
            .iter()
            .any(|(path, agent)| path != recipient && agent.status.is_active());
        Ok((sources, active))
    }
}
