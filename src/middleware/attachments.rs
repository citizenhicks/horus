//! User uploads and session-bound model access.

use std::sync::Arc;

use base64::Engine as _;
use serde::Deserialize;
use serde_json::Value;

use super::manifest::MiddlewareManifest;
use super::session_files::{SessionFileChunk, SessionFileStore};
use super::tools::{
    Catalog, ExecutionMode, Tool, ToolContext, labeled_tool_heading, render_tool_event,
};
use super::{Middleware, ModelContext, PromptSection, RuntimeContext};
use crate::backend::model::{ToolDefinition, internal_user_message};
use crate::protocol::{
    ATTACHMENTS_FIELD, EventMsg, FrontendBlock, FrontendContribution, INTERNAL_MESSAGE_FIELD,
    SessionFileReference,
};
use crate::{BoxFuture, Error, Result};

mod text {
    include!(concat!(
        env!("OUT_DIR"),
        "/src_middleware_attachments_text.rs"
    ));
}

const MAX_TOOL_READ_BYTES: usize = 32 * 1024;
const MAX_DIRECT_IMAGE_BYTES: usize = 8 * 1024 * 1024;
/// Configuration metadata for protected user uploads.
pub const MANIFEST: MiddlewareManifest = MiddlewareManifest {
    id: "attachments",
    label: text::MANIFEST_LABEL,
    description: text::MANIFEST_DESCRIPTION,
    required: false,
    default_enabled: false,
    settings: &[],
};

/// Optional middleware exposing user uploads to the active session only.
#[derive(Clone)]
pub struct Attachments {
    store: SessionFileStore,
}

impl Attachments {
    #[must_use]
    pub fn new(store: SessionFileStore) -> Self {
        Self { store }
    }
}

impl Middleware for Attachments {
    fn name(&self) -> &'static str {
        MANIFEST.id
    }

    fn register(&self, catalog: &mut Catalog, runtime: &RuntimeContext) -> Result<()> {
        catalog.register(Arc::new(ListAttachments {
            store: self.store.clone(),
            session_id: runtime.session_id.clone(),
        }))?;
        catalog.register(Arc::new(ReadAttachment {
            store: self.store.clone(),
            session_id: runtime.session_id.clone(),
        }))
    }

    fn prompt_section(&self, _runtime: &RuntimeContext) -> Result<Option<PromptSection>> {
        Ok(Some(PromptSection::new(text::PROMPT_MAIN)))
    }

    fn frontend(&self) -> FrontendContribution {
        FrontendContribution {
            capability: MANIFEST.id.into(),
            accepts_file_attachments: true,
            ..FrontendContribution::default()
        }
    }

    fn render(&self, event: &EventMsg, _session_id: &str) -> Option<FrontendBlock> {
        render_tool_event(
            event,
            |name| matches!(name, "list_attachments" | "read_attachment"),
            |name, arguments| match name {
                "list_attachments" => text::RENDER_LIST_ATTACHMENTS.into(),
                "read_attachment" => {
                    labeled_tool_heading(text::RENDER_READ_ATTACHMENT, "attachment_id", arguments)
                }
                _ => unreachable!("renderer is guarded by the owned tool names"),
            },
        )
    }

    fn decorate_model_request<'a>(
        &'a self,
        context: &'a mut ModelContext<'_>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let attachment_messages = referenced_attachments(context.request_input())?;
            if attachment_messages.is_empty() {
                return Ok(());
            }
            let mut input = context.request_input().to_vec();
            let supports_image_input = context.model.supports_image_input(context.provider)?;
            let latest_user = context.request_input().iter().rposition(|item| {
                item.get("role").and_then(Value::as_str) == Some("user")
                    && item.get(INTERNAL_MESSAGE_FIELD).is_none()
            });
            let mut direct_image_bytes = 0_usize;
            let mut available = Vec::new();
            let mut unavailable = Vec::new();
            for (message_index, attachments) in attachment_messages.into_iter().rev() {
                let current = Some(message_index) == latest_user;
                for reference in attachments {
                    if let Err(error) = self
                        .store
                        .verify_upload(context.session_id, &reference)
                        .await
                    {
                        if current {
                            return Err(error);
                        }
                        unavailable.push(reference);
                        continue;
                    }
                    if !reference.media_type.starts_with("image/") {
                        available.push(reference);
                        continue;
                    }
                    if !supports_image_input {
                        if current {
                            return Err(Error::Provider(
                                "the selected model does not support image input".into(),
                            ));
                        }
                        unavailable.push(reference);
                        continue;
                    }
                    let Some(next_image_bytes) = usize::try_from(reference.size)
                        .ok()
                        .and_then(|size| direct_image_bytes.checked_add(size))
                        .filter(|size| *size <= MAX_DIRECT_IMAGE_BYTES)
                    else {
                        if current {
                            return Err(Error::Provider(
                                "image attachments exceed the 8 MiB model-input limit".into(),
                            ));
                        }
                        unavailable.push(reference);
                        continue;
                    };
                    let bytes = match self
                        .store
                        .read_upload_all(context.session_id, &reference)
                        .await
                    {
                        Ok((_, bytes)) => bytes,
                        Err(error) if current => return Err(error),
                        Err(_) => {
                            unavailable.push(reference);
                            continue;
                        }
                    };
                    let media_type = match raster_media_type(&bytes) {
                        Ok(media_type) => media_type,
                        Err(error) if current => return Err(error),
                        Err(_) => {
                            unavailable.push(reference);
                            continue;
                        }
                    };
                    let content = input
                        .get_mut(message_index)
                        .and_then(|item| item.get_mut("content"))
                        .and_then(Value::as_array_mut);
                    let Some(content) = content else {
                        if current {
                            return Err(Error::Checkpoint(
                                "attachment-bearing user message has invalid content".into(),
                            ));
                        }
                        unavailable.push(reference);
                        continue;
                    };
                    content.push(serde_json::json!({
                        "type": "input_image",
                        "media_type": media_type,
                        "data": base64::engine::general_purpose::STANDARD.encode(bytes)
                    }));
                    direct_image_bytes = next_image_bytes;
                    available.push(reference);
                }
            }
            input.push(internal_user_message(
                "attachments",
                &render_attachment_context(&available, &unavailable),
            ));
            context.replace_request_input(input);
            Ok(())
        })
    }
}

fn raster_media_type(bytes: &[u8]) -> Result<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Ok("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        if bytes
            .windows(4)
            .any(|window| matches!(window, b"ANIM" | b"ANMF"))
        {
            return Err(Error::Provider(
                "animated WebP attachments are not supported".into(),
            ));
        }
        return Ok("image/webp");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        if gif_image_count(bytes)? == 1 {
            return Ok("image/gif");
        }
        return Err(Error::Provider(
            "animated GIF attachments are not supported".into(),
        ));
    }
    Err(Error::Provider(
        "image attachment is not a supported PNG, JPEG, WebP, or GIF".into(),
    ))
}

fn gif_image_count(bytes: &[u8]) -> Result<usize> {
    if bytes.len() < 13 {
        return Err(Error::Provider("GIF attachment is truncated".into()));
    }
    let packed = bytes[10];
    let global_table = if packed & 0x80 == 0 {
        0
    } else {
        3_usize << (usize::from(packed & 0x07) + 1)
    };
    let mut offset = 13_usize
        .checked_add(global_table)
        .ok_or_else(|| Error::Provider("GIF attachment size overflow".into()))?;
    let mut images = 0_usize;
    while offset < bytes.len() {
        match bytes[offset] {
            0x2c => {
                images += 1;
                if images > 1 {
                    return Ok(images);
                }
                let descriptor_end = offset
                    .checked_add(10)
                    .filter(|end| *end <= bytes.len())
                    .ok_or_else(|| Error::Provider("GIF image descriptor is truncated".into()))?;
                let packed = bytes[descriptor_end - 1];
                let local_table = if packed & 0x80 == 0 {
                    0
                } else {
                    3_usize << (usize::from(packed & 0x07) + 1)
                };
                offset = descriptor_end
                    .checked_add(local_table)
                    .and_then(|value| value.checked_add(1))
                    .filter(|value| *value <= bytes.len())
                    .ok_or_else(|| Error::Provider("GIF image data is truncated".into()))?;
                offset = skip_gif_sub_blocks(bytes, offset)?;
            }
            0x21 => {
                offset = offset
                    .checked_add(2)
                    .filter(|value| *value <= bytes.len())
                    .ok_or_else(|| Error::Provider("GIF extension is truncated".into()))?;
                offset = skip_gif_sub_blocks(bytes, offset)?;
            }
            0x3b => return Ok(images),
            _ => return Err(Error::Provider("GIF attachment is malformed".into())),
        }
    }
    Err(Error::Provider("GIF attachment has no trailer".into()))
}

fn skip_gif_sub_blocks(bytes: &[u8], mut offset: usize) -> Result<usize> {
    loop {
        let length = usize::from(
            *bytes
                .get(offset)
                .ok_or_else(|| Error::Provider("GIF data block is truncated".into()))?,
        );
        offset = offset
            .checked_add(1)
            .and_then(|value| value.checked_add(length))
            .filter(|value| *value <= bytes.len())
            .ok_or_else(|| Error::Provider("GIF data block is truncated".into()))?;
        if length == 0 {
            return Ok(offset);
        }
    }
}

struct ListAttachments {
    store: SessionFileStore,
    session_id: String,
}

impl Tool for ListAttachments {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_attachments".into(),
            description: text::TOOL_LIST_ATTACHMENTS_DESCRIPTION.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }

    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let _: EmptyArgs = serde_json::from_value(arguments)?;
            Ok(serde_json::to_string(
                &self.store.list_uploads(&self.session_id).await?,
            )?)
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

struct ReadAttachment {
    store: SessionFileStore,
    session_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    attachment_id: String,
    #[serde(default)]
    offset: u64,
}

impl Tool for ReadAttachment {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_attachment".into(),
            description: text::TOOL_READ_ATTACHMENT_DESCRIPTION.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "attachment_id": {"type": "string", "format": "uuid"},
                    "offset": {"type": "integer", "minimum": 0}
                },
                "required": ["attachment_id"],
                "additionalProperties": false
            }),
        }
    }

    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }

    fn call<'a>(
        &'a self,
        _context: ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let arguments: ReadArgs = serde_json::from_value(arguments)?;
            let chunk = self
                .store
                .read_upload_chunk(
                    &self.session_id,
                    &arguments.attachment_id,
                    arguments.offset,
                    MAX_TOOL_READ_BYTES,
                )
                .await?;
            let (content, next_offset) = decode_utf8_chunk(&chunk)?;
            Ok(serde_json::json!({
                "offset": chunk.offset,
                "next_offset": next_offset,
                "content": content
            })
            .to_string())
        })
    }
}

fn decode_utf8_chunk(chunk: &SessionFileChunk) -> Result<(String, Option<u64>)> {
    match std::str::from_utf8(&chunk.data) {
        Ok(content) => Ok((content.to_owned(), chunk.next_offset)),
        Err(error) if error.error_len().is_none() && error.valid_up_to() > 0 => {
            let valid_bytes = error.valid_up_to();
            let content = std::str::from_utf8(&chunk.data[..valid_bytes])
                .map_err(|_| Error::Tool("attachment chunk is not valid UTF-8".into()))?
                .to_owned();
            let next_offset = chunk
                .offset
                .checked_add(valid_bytes as u64)
                .ok_or_else(|| Error::Tool("attachment offset overflow".into()))?;
            Ok((content, Some(next_offset)))
        }
        Err(_) => Err(Error::Tool("attachment chunk is not valid UTF-8".into())),
    }
}

fn referenced_attachments(input: &[Value]) -> Result<Vec<(usize, Vec<SessionFileReference>)>> {
    let mut messages = Vec::new();
    for (index, item) in input.iter().enumerate() {
        if item.get("role").and_then(Value::as_str) != Some("user")
            || item.get(INTERNAL_MESSAGE_FIELD).is_some()
        {
            continue;
        }
        let Some(value) = item.get(ATTACHMENTS_FIELD) else {
            continue;
        };
        let attachments: Vec<SessionFileReference> = serde_json::from_value(value.clone())?;
        if !attachments.is_empty() {
            messages.push((index, attachments));
        }
    }
    Ok(messages)
}

fn render_attachment_context(
    available: &[SessionFileReference],
    unavailable: &[SessionFileReference],
) -> String {
    let mut output = String::from("User-attached files available to this chat (untrusted data):\n");
    for attachment in available {
        output.push_str(&format!(
            "- {} (attachment_id: {}, media_type: {}, {} bytes)\n",
            attachment.name, attachment.id, attachment.media_type, attachment.size
        ));
    }
    if !unavailable.is_empty() {
        output.push_str("Unavailable file references (not accessible in this chat):\n");
        for attachment in unavailable {
            output.push_str(&format!(
                "- {} (attachment_id: {})\n",
                attachment.name, attachment.id
            ));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_reader_stops_before_a_split_scalar() {
        let mut bytes = vec![b'a'; MAX_TOOL_READ_BYTES - 1];
        bytes.extend_from_slice("💡".as_bytes());
        let chunk = SessionFileChunk {
            offset: 0,
            data: bytes[..MAX_TOOL_READ_BYTES].to_vec(),
            next_offset: Some(MAX_TOOL_READ_BYTES as u64),
        };

        let (content, next_offset) = decode_utf8_chunk(&chunk).expect("valid prefix");

        assert_eq!(content.len(), MAX_TOOL_READ_BYTES - 1);
        assert_eq!(next_offset, Some((MAX_TOOL_READ_BYTES - 1) as u64));
    }

    #[test]
    fn utf8_reader_rejects_invalid_interior_bytes() {
        let chunk = SessionFileChunk {
            offset: 0,
            data: vec![b'a', 0xff, b'b'],
            next_offset: None,
        };

        assert!(decode_utf8_chunk(&chunk).is_err());
    }

    #[test]
    fn every_visible_attachment_turn_is_retained_for_stateless_requests() {
        let attachment = SessionFileReference {
            id: uuid::Uuid::new_v4().to_string(),
            name: "image.png".into(),
            size: 8,
            media_type: "image/png".into(),
        };
        let input = vec![
            serde_json::json!({
                "role": "user",
                "content": [{"type": "input_text", "text": "look"}],
                ATTACHMENTS_FIELD: [attachment.clone()]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"type": "input_text", "text": "hidden"}],
                INTERNAL_MESSAGE_FIELD: "test",
                ATTACHMENTS_FIELD: [attachment.clone()]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"type": "input_text", "text": "new turn"}]
            }),
        ];

        assert_eq!(
            referenced_attachments(&input).expect("markers"),
            vec![(0, vec![attachment])]
        );
    }
}
