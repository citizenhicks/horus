## Highlights

- Persists daily token usage by model provider for provider-level usage reporting in remote
  frontends.
- Bundles `mobius` 0.7.0 and attributes primary, reviewer, and middleware usage through model routes.
- Keeps aggregate CLI totals while exposing provider identity in profile records.

## Upgrade

- Gateway protocol 23 adds the provider to each daily usage record.
- Gateway configuration version 14 replaces the old provider-neutral usage history. Preserve the
  state directory before upgrading; version 13 configuration is intentionally not read.
- Chat specification version 6, checkpoint version 5, and SQLite schema 4 are unchanged.
