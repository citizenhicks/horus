## Highlights

- Explicitly releases completed cron overlap locks before their file handles close, so duplicated
  or inherited descriptors cannot keep a finished schedule marked as running.
- Adds a deterministic regression covering session deletion while a duplicated task-lock handle
  remains open.

## Upgrade

- Gateway protocol 24, checkpoint version 5, SQLite schema 5, configuration version 15, and chat
  specification version 7 are unchanged.
