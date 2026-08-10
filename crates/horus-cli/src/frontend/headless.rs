use horus::protocol::{EventMsg, Op, ReviewDecision, Submission};
use horus::{Error, Result};
use horus_gateway::client::{GatewayEvents, GatewaySender};
use horus_gateway::wire::{ClientMessage, ServerMessage};
use uuid::Uuid;

pub async fn run(
    sender: GatewaySender,
    mut events: GatewayEvents,
    session_id: String,
    task: String,
) -> Result<Option<String>> {
    let submission_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::Submit {
            session_id: session_id.clone(),
            submission: Submission {
                id: submission_id.clone(),
                op: Op::UserInput {
                    text: task,
                    attachments: Vec::new(),
                },
            },
        })
        .await
        .map_err(gateway_error)?;

    let mut turn_id = None;
    let mut first_error = None;
    let mut approval_error = None;
    let mut last_agent_message = None;
    loop {
        let frame =
            events.next().await.map_err(gateway_error)?.ok_or_else(|| {
                Error::Stopped("gateway disconnected before turn completion".into())
            })?;
        let event = match frame.message {
            ServerMessage::AgentEvent {
                session_id: actual,
                record,
                ..
            } if actual == session_id => record.event,
            ServerMessage::Rejected {
                request_id,
                message,
                ..
            } if request_id == submission_id => return Err(Error::Stopped(message)),
            ServerMessage::Error { message, .. } => return Err(Error::Stopped(message)),
            _ => continue,
        };
        match event.msg {
            EventMsg::TurnStarted(turn)
                if event.submission_id.as_deref() == Some(&submission_id) =>
            {
                turn_id = Some(turn.turn_id);
            }
            EventMsg::ExecApprovalRequest(request)
                if turn_id.as_deref() == Some(request.turn_id.as_str()) =>
            {
                approval_error = Some(Error::Config(
                    "headless run requested tool approval; configure a no-prompt gateway sandbox policy"
                        .into(),
                ));
                sender
                    .send(ClientMessage::Submit {
                        session_id: session_id.clone(),
                        submission: Submission {
                            id: Uuid::new_v4().to_string(),
                            op: Op::ExecApproval {
                                id: request.id,
                                decision: ReviewDecision::Abort,
                            },
                        },
                    })
                    .await
                    .map_err(gateway_error)?;
            }
            EventMsg::Error(error)
                if event.submission_id.as_deref() == Some(submission_id.as_str()) =>
            {
                first_error.get_or_insert(error.message);
            }
            EventMsg::AgentMessage(message)
                if turn_id.as_deref() == Some(message.turn_id.as_str()) =>
            {
                last_agent_message = Some(message.message);
            }
            EventMsg::TurnComplete(turn) if turn_id.as_deref() == Some(turn.turn_id.as_str()) => {
                if let Some(error) = approval_error {
                    return Err(error);
                }
                if let Some(error) = first_error {
                    return Err(Error::Stopped(error));
                }
                return Ok(last_agent_message);
            }
            EventMsg::TurnAborted(turn) if turn_id.as_deref() == Some(turn.turn_id.as_str()) => {
                return Err(approval_error
                    .unwrap_or_else(|| Error::Stopped(first_error.unwrap_or(turn.reason))));
            }
            _ => {}
        }
    }
}

fn gateway_error(error: horus_gateway::Error) -> Error {
    Error::Stopped(error.to_string())
}
