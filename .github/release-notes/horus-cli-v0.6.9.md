## Highlights

- Bundles `horus-gateway` 0.6.9 and `horus` 0.6.8 with configurable model-step budgets and
  corrected Codex Responses history replay.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.6.9
horus-gateway serve --background
```

- Upgrade clients and the gateway together for protocol 22. Gateway configuration version is 13,
  chat specification version is 6, and SQLite schema 4 is unchanged.
