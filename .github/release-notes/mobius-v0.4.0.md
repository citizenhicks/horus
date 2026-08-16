## Highlights

- Adds configurable token-aged context offloading while preserving tool-call structure, failures,
  recent context, and the latest user turn.
- Adds frontend-neutral middleware setting schemas and generic message actions so clients render
  gateway-owned capabilities without capability-name branches.
- Gives every actionable user or assistant message an exact durable transcript target. Forking now
  copies only the selected, tool-complete prefix and records its historical checkpoint lineage.
- Shares tool-completion boundary logic across compaction, replay, and forking.

## Contract

- Message action targets are required protocol fields. This release adds no aliases, fallback
  dispatch, compatibility adapters, state discovery, or migration.
