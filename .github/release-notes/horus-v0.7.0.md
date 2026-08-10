## Highlights

- Adds an optional route-aware usage observer to `AgentConfig` for recording normalized token
  increments outside the session checkpoint.
- Reports usage from primary, reviewer, middleware, and cloned-agent model routes without exposing
  provider-private response fields.
- Fails closed before committing checkpoint usage when the configured observer rejects an
  increment.

## Upgrade

- Checkpoint version 5 and SQLite schema 4 are unchanged.
