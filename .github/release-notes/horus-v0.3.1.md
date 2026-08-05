## Highlights

- Adds configurable context offloading that durably masks stale successful tool output while
  preserving call structure, errors, recent context, and the latest user turn.
- Adds frontend-neutral integer and select setting schemas to the core protocol contract so thin
  clients can render middleware configuration without capability-name logic.

## Contract

- Context offloading is independent of compaction and uses the existing four-bytes-per-token
  estimate.
