use std::collections::VecDeque;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::Agent;
use super::AgentConfig;
use super::AgentSender;
use super::COMMAND_QUEUE_CAPACITY;
use super::EVENT_QUEUE_CAPACITY;
use super::Runner;
use super::try_send_event;
use crate::Error;
use crate::Result;
use crate::backend::checkpoint::CHECKPOINT_VERSION;
use crate::backend::checkpoint::Checkpoint;
use crate::backend::checkpoint::TranscriptPageRequest;
use crate::backend::model::user_message;
use crate::middleware::FrontendExtensions;
use crate::middleware::RuntimeContext;
use crate::middleware::SessionEndContext;
use crate::protocol::ErrorEvent;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::ModelChangedEvent;
use crate::protocol::SessionConfiguredEvent;
use crate::protocol::SessionHistoryEvent;
use crate::protocol::TokenCountEvent;
use crate::protocol::TokenUsageInfo;
use crate::protocol::ToolCallEndEvent;
use crate::protocol::replay_events;

/// Validates capabilities, restores a checkpoint, and starts the agent loop.
pub async fn create_agent(mut config: AgentConfig) -> Result<Agent> {
    if config.context_window <= 0 {
        return Err(Error::Config("context window must be positive".into()));
    }
    if config.system_prompt.trim().is_empty() {
        return Err(Error::Config("system prompt cannot be empty".into()));
    }
    if config.initial_replay_batches == 0 {
        return Err(Error::Config(
            "initial replay batch limit must be positive".into(),
        ));
    }
    config.middleware = config
        .middleware
        .with_sandbox(Arc::clone(&config.sandbox))?;
    let (mut state, is_new) = match config.checkpoints.load(&config.session_id).await? {
        Some(state) => (state, false),
        None => (Checkpoint::empty(&config.session_id), true),
    };
    if state.version != CHECKPOINT_VERSION || state.session_id != config.session_id {
        return Err(Error::Checkpoint(
            "checkpoint does not match the requested session".into(),
        ));
    }
    let mut metadata_changed = false;
    if is_new {
        state.session_context.clone_from(&config.session_context);
        state.metadata.clone_from(&config.metadata);
    } else {
        config.session_context.clone_from(&state.session_context);
        if config.metadata_configured {
            metadata_changed = config.metadata != state.metadata;
            state.metadata.clone_from(&config.metadata);
        } else {
            config.metadata.clone_from(&state.metadata);
        }
    }
    let mut replay = if is_new {
        Vec::new()
    } else {
        let transcript = config
            .checkpoints
            .transcript_page(
                &config.session_id,
                TranscriptPageRequest {
                    before_sequence: None,
                    max_batches: config.initial_replay_batches,
                },
            )
            .await?
            .into_positioned_items_chronological();
        replay_events(&transcript, &config.session_id)
    };
    if let Some(turn_id) = &state.active_turn_id {
        for pending in &state.pending_tools {
            if let Some(call) = replay.iter_mut().rev().find_map(|event| match event {
                EventMsg::ToolCallBegin(call) if call.call_id == pending.call_id => Some(call),
                _ => None,
            }) {
                call.turn_id.clone_from(turn_id);
            }
        }
    }
    let mut recovery_delta = Vec::new();
    let route = if config.model_route_configured {
        config.provider.clone()
    } else {
        state
            .model_route
            .clone()
            .filter(|route| config.model.choices().any(|choice| choice.route == *route))
            .unwrap_or_else(|| config.provider.clone())
    };
    let choice = config.select_model(&route)?;
    let model = crate::backend::model::ModelInfo {
        model: choice.model,
        reasoning_effort: choice.reasoning_effort,
    };
    let (command_tx, command_rx) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
    let (event_tx, event_rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
    let session = SessionConfiguredEvent {
        session_id: config.session_id.clone(),
        context: config.session_context.clone(),
        model: ModelChangedEvent {
            route: config.provider.clone(),
            model: model.model.clone(),
            reasoning_effort: model.reasoning_effort.clone(),
            model_context_window: Some(config.context_window),
        },
    };
    try_send_event(
        &event_tx,
        Event {
            submission_id: None,
            msg: EventMsg::SessionConfigured(session.clone()),
        },
    )?;
    let middleware_events = event_tx.clone();
    let runtime = RuntimeContext {
        checkpoints: Arc::clone(&config.checkpoints),
        session_id: config.session_id.clone(),
        model_route: config.provider.clone(),
        session_context: config.session_context.clone(),
        metadata: config.metadata.clone(),
        frontend: Arc::new(move |update| {
            try_send_event(
                &middleware_events,
                Event {
                    submission_id: None,
                    msg: EventMsg::Frontend(update),
                },
            )
        }),
    };
    let system_prompt: Arc<str> = config
        .middleware
        .system_prompt(&config.system_prompt, &runtime)?
        .into();
    let catalog = config.middleware.catalog(&runtime)?;
    let tool_count = catalog.definitions().len();
    let frontend = FrontendExtensions::new(config.middleware.clone())?;
    let mut state_changed =
        metadata_changed || state.model_route.as_deref() != Some(route.as_str());
    state.model_route = Some(route);
    let uncertain_tools = !state.pending_tools.is_empty()
        && state
            .pending_approval
            .as_ref()
            .is_none_or(|pending| pending.decision_received);
    if uncertain_tools {
        let recovered_turn = state
            .active_turn_id
            .clone()
            .unwrap_or_else(|| "recovered".into());
        for call in std::mem::take(&mut state.pending_tools) {
            let output = "execution interrupted; result unknown after restart";
            let item = crate::backend::model::tool_output(&call.call_id, output, true);
            state.context.push(item.clone());
            recovery_delta.push(item);
            replay.push(EventMsg::ToolCallEnd(ToolCallEndEvent {
                turn_id: recovered_turn.clone(),
                call_id: call.call_id,
                name: call.name,
                output: output.into(),
                is_error: true,
            }));
        }
        for message in std::mem::take(&mut state.pending_input) {
            let item = user_message(&message);
            state.context.push(item.clone());
            recovery_delta.push(item);
        }
        state.pending_approval = None;
        state.active_turn_id = None;
        state_changed = true;
    } else if state.pending_approval.is_none() && state.active_turn_id.take().is_some() {
        for message in std::mem::take(&mut state.pending_input) {
            let item = user_message(&message);
            state.context.push(item.clone());
            recovery_delta.push(item);
        }
        state_changed = true;
    }
    if is_new || state_changed {
        if !is_new {
            state.sequence += 1;
        }
        config.checkpoints.save(&state, &recovery_delta).await?;
    }
    if !replay.is_empty() {
        try_send_event(
            &event_tx,
            Event {
                submission_id: None,
                msg: EventMsg::SessionHistory(SessionHistoryEvent { events: replay }),
            },
        )?;
    }
    if let Some(last_token_usage) = state.last_usage.clone() {
        try_send_event(
            &event_tx,
            Event {
                submission_id: None,
                msg: EventMsg::TokenCount(TokenCountEvent {
                    info: Some(TokenUsageInfo {
                        total_token_usage: state.total_usage.clone(),
                        last_token_usage,
                        model_context_window: Some(config.context_window),
                    }),
                    rate_limits: None,
                }),
            },
        )?;
    }
    let model_choices = config.model.choices().cloned().collect();
    config.middleware.initialize(runtime).await?;
    let mut runner = Runner {
        config,
        system_prompt,
        catalog,
        state,
        transcript_delta: Vec::new(),
        deferred: VecDeque::new(),
        events: event_tx.clone(),
    };
    let end_context = SessionEndContext {
        session_id: runner.config.session_id.clone(),
        metadata: runner.config.metadata.clone(),
    };
    tokio::spawn(async move {
        let run = runner.run(command_rx).await;
        let shutdown = runner.config.middleware.shutdown(end_context).await;
        if let Some(error) = run.err().or_else(|| shutdown.err()) {
            let _ = event_tx
                .send(Event {
                    submission_id: None,
                    msg: EventMsg::Error(ErrorEvent {
                        message: error.to_string(),
                    }),
                })
                .await;
        }
    });
    Ok(Agent {
        sender: AgentSender {
            commands: command_tx,
        },
        events: event_rx,
        frontend,
        session,
        model,
        model_choices,
        tool_count,
    })
}
