use serde::Deserialize;
use serde_json::Value;

use super::presentation::publish_current_widgets;
use super::{MAX_NOTE_BYTES, ScratchpadStore, WriteOutcome, text};
use crate::backend::model::ToolDefinition;
use crate::middleware::FrontendEventSink;
use crate::middleware::tools::{ApprovalRequirement, Tool, ToolContext};
use crate::{BoxFuture, Result};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteArgs {
    note: String,
}

pub(super) struct WriteScratchpad {
    pub(super) store: ScratchpadStore,
    pub(super) session_id: String,
    pub(super) frontend: FrontendEventSink,
}

impl Tool for WriteScratchpad {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_scratchpad".into(),
            description: text::TOOL_WRITE_SCRATCHPAD_DESCRIPTION.into(),
            parameters: note_schema(text::TOOL_WRITE_SCRATCHPAD_PARAMETER_NOTE_DESCRIPTION),
        }
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: NoteArgs = serde_json::from_value(arguments)?;
            let outcome = self
                .store
                .write_session(&self.session_id, &arguments.note)
                .await?;
            if outcome != WriteOutcome::Existing {
                publish_current_widgets(&self.store, &self.session_id, &self.frontend).await?;
            }
            Ok(match outcome {
                WriteOutcome::Added => text::MESSAGE_ADDED_SESSION.into(),
                WriteOutcome::Updated => text::MESSAGE_UPDATED_SESSION.into(),
                WriteOutcome::Existing => text::MESSAGE_EXISTING_SESSION.into(),
            })
        })
    }
}

pub(super) struct PromoteScratchpad {
    pub(super) store: ScratchpadStore,
    pub(super) session_id: String,
    pub(super) frontend: FrontendEventSink,
}

impl Tool for PromoteScratchpad {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "promote_scratchpad".into(),
            description: text::TOOL_PROMOTE_SCRATCHPAD_DESCRIPTION.into(),
            parameters: note_schema(text::TOOL_PROMOTE_SCRATCHPAD_PARAMETER_NOTE_DESCRIPTION),
        }
    }

    fn approval(&self) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: NoteArgs = serde_json::from_value(arguments)?;
            let outcome = self
                .store
                .promote_note(&self.session_id, &arguments.note)
                .await?;
            if outcome != WriteOutcome::Existing {
                publish_current_widgets(&self.store, &self.session_id, &self.frontend).await?;
            }
            Ok(match outcome {
                WriteOutcome::Added => text::MESSAGE_PROMOTED_GLOBAL.into(),
                WriteOutcome::Updated => text::MESSAGE_UPGRADED_GLOBAL.into(),
                WriteOutcome::Existing => text::MESSAGE_EXISTING_GLOBAL.into(),
            })
        })
    }
}

fn note_schema(description: &str) -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "note": {
                "type": "string",
                "description": description,
                "maxLength": MAX_NOTE_BYTES
            }
        },
        "required": ["note"],
        "additionalProperties": false
    })
}
