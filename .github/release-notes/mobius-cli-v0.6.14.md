## Highlights

- Bundles `mobius-gateway` 0.6.14 and `mobius` 0.6.11 with durable transcript markers for context
  compaction.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.6.14
mobius-gateway serve --background
```

- Protocol 22, gateway configuration version 13, chat specification version 6, checkpoint version
  5, and SQLite schema 4 are unchanged.
