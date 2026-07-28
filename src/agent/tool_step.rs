use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::Runner;
use super::input::ActiveRoute;
use super::input::ActiveTurnRouter;
use super::input::Wait;
use crate::Error;
use crate::Result;
use crate::backend::checkpoint::PendingApproval;
use crate::backend::model::ToolCall;
use crate::backend::model::tool_output;
use crate::backend::sandbox::SandboxApprovalRequest;
use crate::backend::sandbox::SandboxPermissions;
use crate::middleware::tools::ToolResult;
use crate::middleware::tools::execute_batch;
use crate::protocol::ApprovalCall;
use crate::protocol::EventMsg;
use crate::protocol::ExecApprovalRequestEvent;
use crate::protocol::ReviewDecision;
use crate::protocol::Submission;
use crate::protocol::ToolCallBeginEvent;
use crate::protocol::ToolCallEndEvent;

struct ApprovalResponse {
    submission_id: String,
    decision: ReviewDecision,
}

impl Runner {
    pub(super) async fn pause_and_resolve(
        &mut self,
        commands: &mut mpsc::Receiver<Submission>,
        submission_id: &str,
        turn_id: &str,
        calls: Vec<ToolCall>,
        request: SandboxApprovalRequest,
        permissions: SandboxPermissions,
    ) -> Result<Option<Vec<ToolResult>>> {
        let known_call_ids = calls
            .iter()
            .map(|call| call.call_id.as_str())
            .collect::<BTreeSet<_>>();
        if request.call_ids.is_empty()
            || request
                .call_ids
                .iter()
                .any(|call_id| !known_call_ids.contains(call_id.as_str()))
        {
            return Err(Error::Config(
                "sandbox approval policy returned an invalid tool selection".into(),
            ));
        }
        let pending = PendingApproval {
            submission_id: submission_id.to_string(),
            turn_id: turn_id.to_string(),
            request_id: request.id,
            approval_call_ids: request.call_ids,
            authorized_call_ids: permissions.mutation_call_ids(),
            calls,
            reason: request.reason,
            network_access: permissions.network_access(),
            decision_received: false,
        };
        self.state.pending_approval = Some(pending.clone());
        self.save().await?;
        self.resolve_pending(commands, &pending).await
    }

    pub(super) async fn resume_pending(
        &mut self,
        commands: &mut mpsc::Receiver<Submission>,
        pending: PendingApproval,
    ) -> Result<()> {
        let Some(results) = self.resolve_pending(commands, &pending).await? else {
            return Ok(());
        };
        self.append_tool_results(results);
        self.state.pending_approval = None;
        self.save().await?;
        self.continue_turn(commands, pending.submission_id, pending.turn_id)
            .await
    }

    async fn resolve_pending(
        &mut self,
        commands: &mut mpsc::Receiver<Submission>,
        pending: &PendingApproval,
    ) -> Result<Option<Vec<ToolResult>>> {
        self.emit_approval(pending).await?;
        let approval = self.wait_for_approval(commands, pending).await?;
        let Some(approval) = self.ready_or_aborted(approval, &pending.turn_id).await? else {
            return Ok(None);
        };
        let decision = approval.decision;
        if let Some(current) = self.state.pending_approval.as_mut()
            && current.request_id == pending.request_id
        {
            current.decision_received = true;
            self.save().await?;
        }
        let approval_call_ids = pending.approval_call_ids.clone();
        let permissions = self.config.sandbox.resolve_approval(
            &self.config.session_id,
            &pending.calls,
            &approval_call_ids,
            &decision,
            SandboxPermissions::restore(
                pending.network_access,
                pending.authorized_call_ids.clone(),
            ),
        )?;
        match decision {
            ReviewDecision::Approved | ReviewDecision::ApprovedForSession => {
                let execution = self
                    .execute_tools(
                        commands,
                        &pending.submission_id,
                        &pending.turn_id,
                        &pending.calls,
                        permissions,
                    )
                    .await?;
                self.ready_or_aborted(execution, &pending.turn_id).await
            }
            ReviewDecision::Denied { rejection } => {
                let approval_call_ids = approval_call_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>();
                let allowed_calls = pending
                    .calls
                    .iter()
                    .filter(|call| !approval_call_ids.contains(call.call_id.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                let denied_calls = pending
                    .calls
                    .iter()
                    .filter(|call| approval_call_ids.contains(call.call_id.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                let mut results = if allowed_calls.is_empty() {
                    Vec::new()
                } else {
                    let execution = self
                        .execute_tools(
                            commands,
                            &pending.submission_id,
                            &pending.turn_id,
                            &allowed_calls,
                            permissions,
                        )
                        .await?;
                    let Some(results) = self.ready_or_aborted(execution, &pending.turn_id).await?
                    else {
                        return Ok(None);
                    };
                    results
                };
                results.extend(denied_results(&denied_calls, &rejection));
                Ok(Some(order_results(&pending.calls, results)))
            }
            ReviewDecision::Abort => {
                self.append_tool_results(denied_results(&pending.calls, "approval aborted"));
                self.state.pending_approval = None;
                self.abort(
                    &approval.submission_id,
                    &pending.turn_id,
                    "approval aborted",
                )
                .await?;
                Ok(None)
            }
        }
    }

    pub(super) async fn execute_tools(
        &mut self,
        commands: &mut mpsc::Receiver<Submission>,
        submission_id: &str,
        turn_id: &str,
        calls: &[ToolCall],
        permissions: SandboxPermissions,
    ) -> Result<Wait<Vec<ToolResult>>> {
        for call in calls {
            self.emit(
                submission_id,
                EventMsg::ToolCallBegin(ToolCallBeginEvent {
                    turn_id: turn_id.to_string(),
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                }),
            )
            .await?;
        }
        let catalog = self.catalog.clone();
        let interrupt_on_active_input = catalog.interrupts_on_active_input(calls);
        let execution = execute_batch(
            &catalog,
            calls,
            Arc::clone(&self.config.sandbox),
            &permissions,
        );
        tokio::pin!(execution);
        let results = loop {
            tokio::select! {
                results = &mut execution => break Wait::Ready(results),
                submission = commands.recv() => {
                    let Some(submission) = submission else {
                        return Err(Error::Stopped("frontend disconnected".into()));
                    };
                    match (ActiveTurnRouter {
                        middleware: &self.config.middleware,
                        turn_id,
                        queued_input: &mut self.state.pending_input,
                        queued_before: 0,
                        deferred: &mut self.deferred,
                        events: &self.events,
                        expected_approval: None,
                    })
                    .route(submission)
                    .await?
                    {
                        ActiveRoute::Accepted if interrupt_on_active_input => {
                            break Wait::Ready(interrupted_results(
                                calls,
                                "execution interrupted; result unknown after active input",
                            ));
                        }
                        ActiveRoute::Interrupted { submission_id } => {
                            break Wait::Interrupted { submission_id };
                        }
                        _ => {}
                    }
                }
            }
        };
        let results = match results {
            Wait::Ready(results) => results,
            Wait::Interrupted { submission_id } => {
                return Ok(Wait::Interrupted { submission_id });
            }
        };
        for result in &results {
            self.emit(
                submission_id,
                EventMsg::ToolCallEnd(ToolCallEndEvent {
                    turn_id: turn_id.to_string(),
                    call_id: result.call_id.clone(),
                    name: result.name.clone(),
                    output: result.output.clone(),
                    is_error: result.is_error,
                }),
            )
            .await?;
        }
        Ok(Wait::Ready(results))
    }

    pub(super) fn append_tool_results(&mut self, results: Vec<ToolResult>) {
        let completed = results
            .iter()
            .map(|result| result.call_id.as_str())
            .collect::<BTreeSet<_>>();
        self.state
            .pending_tools
            .retain(|call| !completed.contains(call.call_id.as_str()));
        for result in results {
            self.push_context(tool_output(
                &result.call_id,
                &result.output,
                result.is_error,
            ));
        }
    }

    pub(super) async fn finish_pending_tools(
        &mut self,
        submission_id: &str,
        turn_id: &str,
        reason: &str,
    ) -> Result<()> {
        let calls = std::mem::take(&mut self.state.pending_tools);
        let results = interrupted_results(
            &calls,
            &format!("execution interrupted; result unknown: {reason}"),
        );
        for result in &results {
            self.emit(
                submission_id,
                EventMsg::ToolCallEnd(ToolCallEndEvent {
                    turn_id: turn_id.to_string(),
                    call_id: result.call_id.clone(),
                    name: result.name.clone(),
                    output: result.output.clone(),
                    is_error: true,
                }),
            )
            .await?;
        }
        self.append_tool_results(results);
        Ok(())
    }

    async fn wait_for_approval(
        &mut self,
        commands: &mut mpsc::Receiver<Submission>,
        pending: &PendingApproval,
    ) -> Result<Wait<ApprovalResponse>> {
        while let Some(submission) = commands.recv().await {
            match (ActiveTurnRouter {
                middleware: &self.config.middleware,
                turn_id: &pending.turn_id,
                queued_input: &mut self.state.pending_input,
                queued_before: 0,
                deferred: &mut self.deferred,
                events: &self.events,
                expected_approval: Some(&pending.request_id),
            })
            .route(submission)
            .await?
            {
                ActiveRoute::Approval {
                    submission_id,
                    decision,
                } => {
                    return Ok(Wait::Ready(ApprovalResponse {
                        submission_id,
                        decision,
                    }));
                }
                ActiveRoute::Accepted => {
                    self.save().await?;
                }
                ActiveRoute::Interrupted { submission_id } => {
                    return Ok(Wait::Interrupted { submission_id });
                }
                ActiveRoute::Continue => {}
            }
        }
        Err(Error::Stopped(
            "frontend disconnected during approval".into(),
        ))
    }

    async fn emit_approval(&self, pending: &PendingApproval) -> Result<()> {
        let approval_call_ids = pending
            .approval_call_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        self.emit(
            &pending.submission_id,
            EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
                id: pending.request_id.clone(),
                turn_id: pending.turn_id.clone(),
                calls: pending
                    .calls
                    .iter()
                    .filter(|call| approval_call_ids.contains(&call.call_id))
                    .map(|call| ApprovalCall {
                        call_id: call.call_id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    })
                    .collect(),
                reason: pending.reason.clone(),
            }),
        )
        .await
    }
}

fn denied_results(calls: &[ToolCall], rejection: &str) -> Vec<ToolResult> {
    calls
        .iter()
        .map(|call| ToolResult {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            output: format!("tool denied: {rejection}"),
            is_error: true,
        })
        .collect()
}

fn interrupted_results(calls: &[ToolCall], message: &str) -> Vec<ToolResult> {
    calls
        .iter()
        .map(|call| ToolResult {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            output: message.to_string(),
            is_error: true,
        })
        .collect()
}

fn order_results(calls: &[ToolCall], results: Vec<ToolResult>) -> Vec<ToolResult> {
    let mut results = results
        .into_iter()
        .map(|result| (result.call_id.clone(), result))
        .collect::<BTreeMap<_, _>>();
    calls
        .iter()
        .filter_map(|call| results.remove(&call.call_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_results_do_not_claim_tools_were_denied() {
        let calls = [ToolCall {
            call_id: "call-1".into(),
            name: "write".into(),
            arguments: serde_json::json!({}),
        }];

        let results = interrupted_results(&calls, "execution interrupted; result unknown");

        assert_eq!(results[0].output, "execution interrupted; result unknown");
    }
}
