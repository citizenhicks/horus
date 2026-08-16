## Highlights

- Bundles `mobius-gateway` 0.6.9 and `mobius` 0.6.8 with configurable model-step budgets and
  corrected Codex Responses history replay.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.6.9
mobius-gateway serve --background
```

- Upgrade clients and the gateway together for protocol 22. Gateway configuration version is 13,
  chat specification version is 6, and SQLite schema 4 is unchanged.
