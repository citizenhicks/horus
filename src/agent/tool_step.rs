//! Tool execution and result persistence.

use std::collections::BTreeSet;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::Runner;
use super::input::ActiveRoute;
use super::input::ActiveTurnRouter;
use super::input::Wait;
use crate::Error;
use crate::Result;
use crate::backend::model::ToolCall;
use crate::backend::model::tool_output;
use crate::backend::sandbox::SandboxPermissions;
use crate::middleware::QueuedInputBaseline;
use crate::middleware::tools::ToolResult;
use crate::middleware::tools::execute_batch;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::Submission;
use crate::protocol::ToolCallBeginEvent;
use crate::protocol::ToolCallEndEvent;

impl Runner {
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
                        session_id: &self.config.session_id,
                        metadata: &self.config.metadata,
                        turn_id,
                        queued_input: &mut self.state.pending_input,
                        queued_before: QueuedInputBaseline::default(),
                        deferred: &mut self.deferred,
                        events: &self.events,
                        expected_approval: None,
                    })
                    .route(submission)
                    .await?
                    {
                        ActiveRoute::Accepted(change) => {
                            self.persist_active_change(change).await?;
                            if interrupt_on_active_input {
                                break Wait::Ready(interrupted_results(
                                    calls,
                                    "execution interrupted; result unknown after active input",
                                ));
                            }
                        }
                        ActiveRoute::Changed(change) => {
                            self.persist_active_change(change).await?;
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
        Ok(Wait::Ready(results))
    }

    pub(super) async fn persist_tool_results(
        &mut self,
        submission_id: &str,
        turn_id: &str,
        results: Vec<ToolResult>,
    ) -> Result<()> {
        let events = tool_result_events(submission_id, turn_id, &results);
        self.append_tool_results(results)?;
        self.persist_with_events(events, None).await?;
        Ok(())
    }

    pub(super) fn append_tool_results(&mut self, results: Vec<ToolResult>) -> Result<()> {
        let tool_calls = u64::try_from(results.len())
            .map_err(|_| Error::Checkpoint("execution tool-call count is unsupported".into()))?;
        let failed_tool_calls = u64::try_from(
            results.iter().filter(|result| result.is_error).count(),
        )
        .map_err(|_| Error::Checkpoint("execution failed-tool count is unsupported".into()))?;
        self.record_tools(tool_calls, failed_tool_calls)?;
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
        Ok(())
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
        if results.is_empty() {
            return Ok(());
        }
        self.persist_tool_results(submission_id, turn_id, results)
            .await
    }
}

fn tool_result_events(submission_id: &str, turn_id: &str, results: &[ToolResult]) -> Vec<Event> {
    results
        .iter()
        .map(|result| Event {
            submission_id: Some(submission_id.to_string()),
            msg: EventMsg::ToolCallEnd(ToolCallEndEvent {
                turn_id: turn_id.to_string(),
                call_id: result.call_id.clone(),
                name: result.name.clone(),
                output: result.output.clone(),
                is_error: result.is_error,
            }),
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
