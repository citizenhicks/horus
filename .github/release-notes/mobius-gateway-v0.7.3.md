## Highlights

- Bundles `mobius` 0.7.3 and transports one canonical recorded-event shape for live delivery,
  reconnect replay, backward history pages, and subagent transcript previews.
- Projects typed semantic blocks once at the gateway boundary, preserving capability-scoped block,
  group, and artifact identity for every frontend.
- Bounds replay by both count and encoded bytes, retains exact journal cursors across compacted or
  transient records, and fails closed if a normalized event cannot be recorded.
- Drains each session writer before replacing an agent so live delivery remains in the same strict
  sequence as the durable journal.
- Deletes the requested session subtree and its files, event history, catalog metadata, and cron
  state instead of merely hiding it.

## Upgrade

- Gateway protocol 24 replaces protocol 23; clients and gateways must be upgraded together.
- SQLite schema 5 requires a fresh chat database. Gateway configuration and provider credentials
  are separate and can be retained.
- Configuration version 14, chat specification version 6, and checkpoint version 5 are unchanged.
