## Highlights

- Persists compaction markers as small private transcript records so they remain visible in the
  correct timeline position after reconnects, gateway restarts, and paginated history loads.
- Keeps replay markers out of provider context and does not persist compaction summaries or
  provider-private payloads for presentation.

## Upgrade

- The public API, protocol 22, checkpoint version 5, and SQLite schema 4 are unchanged.
