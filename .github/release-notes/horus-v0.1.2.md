## Highlights

- Persists capability-owned metadata with each durable checkpoint and restores it when an agent
  resumes. Explicit `AgentConfig::metadata` calls replace the saved value; omitting the call
  preserves it.
- Carries durable metadata through manual forks and subagent forks so hosts can keep runtime
  configuration attached to the chat that owns it.
- Lets a host explicitly replace a saved model route while rebuilding an existing agent, so
  chat-scoped model changes survive a restart without changing the router's global catalog.

## Breaking changes

- `Checkpoint` now includes a public `metadata` field.
- The checkpoint record and SQLite schema are version 3. There is intentionally no migration layer
  at this stage; existing version-2 checkpoint databases must be replaced or kept with the older
  crate version.
