//! User uploads and session-bound model access.

use std::sync::Arc;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::manifest::MiddlewareManifest;
use super::session_files::{SessionFileChunk, SessionFileStore};
use super::tools::{
    Catalog, ExecutionMode, Tool, ToolContext, labeled_tool_heading, render_tool_event,
};
use super::{Middleware, ModelContext, PromptSection, RuntimeContext};
use crate::backend::model::{ToolDefinition, internal_user_message};
use crate::protocol::{
    ATTACHMENT_CONTEXT_MARKER, ATTACHMENTS_FIELD, EventMsg, FrontendBlock, FrontendContribution,
    INTERNAL_MESSAGE_FIELD, SessionFileReference, internal_message_kind,
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
const MATERIALIZED_ATTACHMENTS_FIELD: &str = "_horus_attachment_blobs";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterializedAttachment {
    reference: SessionFileReference,
    content_hash: Option<String>,
    image_media_type: Option<String>,
    unavailable_reason: Option<String>,
}
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

    fn before_model<'a>(&'a self, context: &'a mut ModelContext<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let Some((message_index, references)) = referenced_attachments(context.input())?.pop()
            else {
                return Ok(());
            };
            if materialization_matches(context.input(), message_index, &references)? {
                return Ok(());
            }
            if message_index + 1 != context.input().len() {
                return Err(Error::Checkpoint(
                    "attachment-bearing user message is missing adjacent materialization".into(),
                ));
            }
            let mut direct_image_bytes = 0_usize;
            let mut materialized = Vec::with_capacity(references.len());
            let mut first_error = None;
            for reference in references {
                let content_hash = match self
                    .store
                    .upload_content_hash(context.session_id, &reference)
                    .await
                {
                    Ok(content_hash) => content_hash,
                    Err(error) => {
                        let reason = error.to_string();
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                        materialized.push(MaterializedAttachment {
                            reference,
                            content_hash: None,
                            image_media_type: None,
                            unavailable_reason: Some(reason),
                        });
                        continue;
                    }
                };
                let image_media_type = if reference.media_type.starts_with("image/") {
                    let result = usize::try_from(reference.size)
                        .ok()
                        .and_then(|size| direct_image_bytes.checked_add(size))
                        .filter(|size| *size <= MAX_DIRECT_IMAGE_BYTES)
                        .ok_or_else(|| {
                            Error::Provider(
                                "image attachments exceed the 8 MiB model-input limit".into(),
                            )
                        });
                    match result {
                        Ok(next_image_bytes) => {
                            let bytes = self
                                .store
                                .read_content_blob(&content_hash, reference.size)
                                .await;
                            match bytes
                                .and_then(|bytes| raster_media_type(&bytes).map(str::to_string))
                            {
                                Ok(media_type) => {
                                    direct_image_bytes = next_image_bytes;
                                    Some(media_type)
                                }
                                Err(error) => {
                                    let reason = error.to_string();
                                    if first_error.is_none() {
                                        first_error = Some(error);
                                    }
                                    materialized.push(MaterializedAttachment {
                                        reference,
                                        content_hash: Some(content_hash),
                                        image_media_type: None,
                                        unavailable_reason: Some(reason),
                                    });
                                    continue;
                                }
                            }
                        }
                        Err(error) => {
                            let reason = error.to_string();
                            if first_error.is_none() {
                                first_error = Some(error);
                            }
                            materialized.push(MaterializedAttachment {
                                reference,
                                content_hash: Some(content_hash),
                                image_media_type: None,
                                unavailable_reason: Some(reason),
                            });
                            continue;
                        }
                    }
                } else {
                    None
                };
                materialized.push(MaterializedAttachment {
                    reference,
                    content_hash: Some(content_hash),
                    image_media_type,
                    unavailable_reason: None,
                });
            }
            context.append_model_input(materialization_message(&materialized)?);
            first_error.map_or(Ok(()), Err)
        })
    }

    fn decorate_model_request<'a>(
        &'a self,
        context: &'a mut ModelContext<'_>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let latest_user = context.request_input().iter().rposition(is_real_user);
            let supports_image_input = context.model.supports_image_input(context.provider)?;
            let mut direct_image_bytes = 0_usize;
            let mut input = context.request_input().to_vec();
            let mut changed = false;
            for message_index in (0..input.len()).rev() {
                let Some(materialized) = materialized_attachments(&input[message_index])? else {
                    continue;
                };
                let current = source_user_index(&input, message_index) == latest_user;
                let mut images = Vec::new();
                for attachment in materialized {
                    if attachment.unavailable_reason.is_some() {
                        continue;
                    }
                    let Some(media_type) = attachment.image_media_type else {
                        continue;
                    };
                    let content_hash = attachment.content_hash.ok_or_else(|| {
                        Error::Checkpoint(
                            "available materialized attachment omitted content hash".into(),
                        )
                    })?;
                    if !supports_image_input {
                        if current {
                            return Err(Error::Provider(
                                "the selected model does not support image input".into(),
                            ));
                        }
                        continue;
                    }
                    let Some(next_image_bytes) = usize::try_from(attachment.reference.size)
                        .ok()
                        .and_then(|size| direct_image_bytes.checked_add(size))
                        .filter(|size| *size <= MAX_DIRECT_IMAGE_BYTES)
                    else {
                        if current {
                            return Err(Error::Provider(
                                "image attachments exceed the 8 MiB model-input limit".into(),
                            ));
                        }
                        continue;
                    };
                    let bytes = self
                        .store
                        .read_content_blob(&content_hash, attachment.reference.size)
                        .await?;
                    if raster_media_type(&bytes)? != media_type {
                        return Err(Error::Checkpoint(
                            "materialized attachment media type changed".into(),
                        ));
                    }
                    images.push(serde_json::json!({
                        "type": "input_image",
                        "media_type": media_type,
                        "data": base64::engine::general_purpose::STANDARD.encode(bytes)
                    }));
                    direct_image_bytes = next_image_bytes;
                }
                if images.is_empty() {
                    continue;
                }
                let content = input[message_index]
                    .get_mut("content")
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| {
                        Error::Checkpoint(
                            "materialized attachment context has invalid content".into(),
                        )
                    })?;
                content.extend(images);
                changed = true;
            }
            if changed {
                context.replace_request_input(input);
            }
            Ok(())
        })
    }
}

pub(crate) fn is_attachment_materialization(item: &Value) -> bool {
    internal_message_kind(item) == Some(ATTACHMENT_CONTEXT_MARKER)
}

fn is_real_user(item: &Value) -> bool {
    item.get("role").and_then(Value::as_str) == Some("user")
        && item.get(INTERNAL_MESSAGE_FIELD).is_none()
}

fn source_user_index(input: &[Value], materialization_index: usize) -> Option<usize> {
    input[..materialization_index]
        .iter()
        .rposition(is_real_user)
}

fn materialization_message(attachments: &[MaterializedAttachment]) -> Result<Value> {
    let available = attachments
        .iter()
        .filter(|attachment| attachment.unavailable_reason.is_none())
        .map(|attachment| attachment.reference.clone())
        .collect::<Vec<_>>();
    let unavailable = attachments
        .iter()
        .filter(|attachment| attachment.unavailable_reason.is_some())
        .map(|attachment| attachment.reference.clone())
        .collect::<Vec<_>>();
    let mut message = internal_user_message(
        ATTACHMENT_CONTEXT_MARKER,
        &render_attachment_context(&available, &unavailable),
    );
    message[MATERIALIZED_ATTACHMENTS_FIELD] = serde_json::to_value(attachments)?;
    Ok(message)
}

fn materialized_attachments(item: &Value) -> Result<Option<Vec<MaterializedAttachment>>> {
    if !is_attachment_materialization(item) {
        return Ok(None);
    }
    let value = item.get(MATERIALIZED_ATTACHMENTS_FIELD).ok_or_else(|| {
        Error::Checkpoint("materialized attachment context omitted blob metadata".into())
    })?;
    let attachments = serde_json::from_value(value.clone()).map_err(|error| {
        Error::Checkpoint(format!("invalid materialized attachment context: {error}"))
    })?;
    Ok(Some(attachments))
}

fn materialization_matches(
    input: &[Value],
    user_index: usize,
    references: &[SessionFileReference],
) -> Result<bool> {
    let Some(item) = input.get(user_index + 1) else {
        return Ok(false);
    };
    let Some(materialized) = materialized_attachments(item)? else {
        return Ok(false);
    };
    Ok(materialized
        .iter()
        .map(|attachment| &attachment.reference)
        .eq(references))
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
