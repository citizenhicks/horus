## Highlights

- Adds a bounded, durable, capability-owned active-input queue to the generic agent loop.
- Lets middleware conditionally take its latest queued input while keeping queue state isolated by
  capability and persisted before frontend confirmation.
- Preserves middleware event order when active input arrives during an asynchronous model hook.

## Upgrade

- Checkpoint JSON advances from version 4 to 5; SQLite storage remains schema 4.
- The queued-input shape is intentionally strict, with no compatibility reader or automatic
  migration.
- Back up an existing checkpoint store before upgrading. Version 4 checkpoints must be converted
  offline or replaced with a fresh store before this release can open them.
