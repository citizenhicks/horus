## Highlights

- Advances the wire protocol to 21 with neutral session-file upload, listing, reading, and
  downloadable artifact records.
- Adds the default-enabled Artifacts middleware and its approval-gated `send_artifact` tool.
- Bounds artifact catalog responses to the gateway frame limit and reports when older records were
  truncated instead of dropping the client connection.
- Bundles `horus` 0.6.5.

## Upgrade

- Upgrade clients and the gateway together. Protocol 21 renames the attachment upload/list/read
  frames and fields to session-file operations and adds the `file` artifact kind.
- Gateway config remains version 12, chat specs and checkpoints remain version 5, and SQLite remains
  schema 4. Existing explicit middleware selections do not automatically gain `artifacts`; enable it
  once in the saved default or an existing chat where desired.
- Existing `uploads/.attachment.json` data is not read from the new
  `session-files/.session-file.json` layout. Reattach affected files after upgrading.
