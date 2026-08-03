## Highlights

- Replaces `edit_file` with a bounded, single-file `apply_patch` tool that accepts unified diffs.
- Adds optional workspace instructions and durable todo middleware as independent vertical slices.
- Adds session-owned background command start, poll, and stop tools with incremental bounded output
  and sandbox-owned cleanup.

## Breaking changes

- `SandboxBackend::execute` now receives `CommandMode` and `CommandOutputSink` so backends can
  distinguish foreground deadlines and stream output safely.
- The removed `edit_file` schema has no alias; callers must use `apply_patch`.
