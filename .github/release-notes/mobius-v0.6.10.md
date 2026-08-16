## Highlights

- Keeps native and local compaction bounded during long single-turn runs instead of restoring the
  tool-heavy history that compaction removed.
- Preserves only provider-neutral private metadata on the latest user message across compaction.
- Normalizes Responses compact output at the provider boundary, including Codex compaction items
  and replay-only wire fields.

## Upgrade

- The public API, checkpoint version 5, and SQLite schema 4 are unchanged.
