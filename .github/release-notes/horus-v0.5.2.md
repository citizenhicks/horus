## Highlights

- Adds a durable execution journal with per-run outcome, elapsed time, model-call, tool-call,
  failure, and usage statistics.
- Deepens paginated history with timestamps, stable replay identities, and durable frontend widget
  snapshots so task, subagent, and steering state survives reconnects.
- Adds frontend-neutral task state and provider model-catalog records.

## Breaking changes

- Checkpoint JSON advances from version 3 to 4 and SQLite checkpoint storage advances from schema
  3 to 4. Existing state requires an explicit migration.
- This release adds no compatibility reads or automatic migrations.
