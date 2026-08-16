## Highlights

- Moves provider model catalogs, reasoning choices, search modes, middleware prompts, and
  configurable defaults into validated capability-local TOML resources.
- Keeps primary context usage stable while the sandbox reviewer runs, preventing transient context
  percentage drops during tool approval.
- Normalizes provider-private Responses reasoning fields before replaying model context.

## Upgrade

- This release does not change the gateway protocol, configuration storage, checkpoint JSON, or
  SQLite schema.
