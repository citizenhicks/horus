## Highlights

- Bundles `mobius` 0.7.7 with bounded, cursor-based subagent transcript pagination, including large
  inherited-context pages and tool-safe boundaries.
- Projects stable preview identities, page updates, continuations, spawn context, effective model,
  reasoning, and status over gateway protocol 25.
- Avoids retaining a second raw preview payload while projecting frontend events, keeping large
  previews below the gateway frame limit.

## Upgrade

- Frontends must use gateway protocol 25. Persisted checkpoint, SQLite, configuration, and chat
  specification versions are unchanged.
