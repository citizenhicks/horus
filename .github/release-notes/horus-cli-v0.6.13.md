## Highlights

- Bundles `horus-gateway` 0.6.13 and `horus` 0.6.11 with durable transcript markers for context
  compaction.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.6.13
horus-gateway serve --background
```

- Protocol 22, gateway configuration version 13, chat specification version 6, checkpoint version
  5, and SQLite schema 4 are unchanged.
