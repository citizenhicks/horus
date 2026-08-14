# Horus 0.8.2

- Exposes user uploads as workspace-local files under `.horus/attachments/` so agents can inspect binary and text attachments with normal workspace tools.
- Keeps staged files independent from protected content-addressed blobs and removes them when the owning session is deleted.
- Preserves existing attachment checkpoints while replacing the dedicated `read_attachment` tool with workspace paths returned by `list_attachments`.
