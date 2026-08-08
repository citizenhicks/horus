## Highlights

- Adds approval-required artifact publishing as vertical middleware, backed by bounded,
  session-scoped immutable file storage shared with user uploads.
- Adds frontend-neutral downloadable file references to middleware presentation blocks.
- Adds bounded binary reads to the sandbox boundary so middleware never bypasses workspace
  confinement when publishing files.

## Upgrade

- This requested patch release intentionally cleans the public 0.x API: `AttachmentReference` is
  now `SessionFileReference`, `FrontendBlock` requires `files`, `Middleware::render` receives the
  destination session ID, and `SandboxBackend` requires `read_bytes`.
- Upload storage APIs move from `middleware::attachments` to `middleware::session_files`.
- Existing `uploads/.attachment.json` data is not read from the new
  `session-files/.session-file.json` layout. Reattach affected files after upgrading.
