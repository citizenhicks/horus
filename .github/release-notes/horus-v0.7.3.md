## Highlights

- Adds a canonical typed event journal with session, turn, model-step, tool, and search identity,
  durable sequence numbers and timestamps, structured errors, and per-stream timing metrics.
- Persists authoritative model-step snapshots for deterministic replay while compacting successful
  text deltas and excluding transient picker, preview, and widget controls from durable history.
- Makes frontend presentation semantic and capability-owned instead of inferring lifecycle, icons,
  groups, or roles from English text and identifier conventions.
- Preserves partial model output on interruption or failure and keeps every hosted-search query in
  its typed provider-neutral action.
- Retains provider text-part boundaries plus typed OpenAI annotations and Anthropic citations in
  completed-step snapshots, without opaque metadata bags.
- Commits checkpoint mutations and their semantic events together before delivery, including
  restart recovery, approvals, route changes, tool results, and terminal execution state.
- Physically deletes complete session trees and their checkpoint, transcript, event, file, and
  scheduled-task data.

## Upgrade

- This requested patch release intentionally updates the public 0.x protocol, checkpoint-store,
  middleware-rendering, and agent-event APIs.
- SQLite schema 5 is a clean break. Start with a fresh chat database; no migration or legacy event
  parsing is included.
- Checkpoint version 5 remains unchanged.
