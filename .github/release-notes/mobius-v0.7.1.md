## Highlights

- Serves middleware-owned read-only commands while a turn is active and publishes their frontend
  events immediately without mutating durable turn state.
- Opens the subagent picker and transcript preview during generation instead of deferring them
  until the parent turn finishes.
- Resolves subagent transcript reads from the root session, including commands issued from a child
  session.

## Upgrade

- This requested patch release intentionally updates the public 0.x middleware API:
  `Middleware::active_command` is asynchronous, `ActiveCommandContext` includes session identity
  and metadata, and `ActiveSubmissionResult::Handled` represents event-only completion.
- Active commands pause polling of the model, tool, or lifecycle-hook future while they run. Keep
  implementations bounded and independent of resources held by that future.
- Checkpoint version 5 and SQLite schema 4 are unchanged.
