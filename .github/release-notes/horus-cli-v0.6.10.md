## Highlights

- Bundles `horus-gateway` 0.6.10 and `horus` 0.6.9 with durable commentary replay for
  intermediate model updates.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.6.10
horus-gateway serve --background
```

- Upgrade clients and the gateway together for protocol 22. Gateway configuration version 13,
  chat specification version 6, checkpoint version 5, and SQLite schema 4 are unchanged.
