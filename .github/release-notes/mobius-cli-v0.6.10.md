## Highlights

- Bundles `mobius-gateway` 0.6.10 and `mobius` 0.6.9 with durable commentary replay for
  intermediate model updates.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.6.10
mobius-gateway serve --background
```

- Upgrade clients and the gateway together for protocol 22. Gateway configuration version 13,
  chat specification version 6, checkpoint version 5, and SQLite schema 4 are unchanged.
