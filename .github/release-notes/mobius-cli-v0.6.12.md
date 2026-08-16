## Highlights

- Bundles `mobius-gateway` 0.6.12 with bounded reconnect replay and support for gateway frames up to
  20 MiB, preventing long transcripts from entering a reconnect loop.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.6.12
mobius-gateway serve --background
```

- Upgrade clients and the gateway together. Protocol 22, gateway configuration version 13, chat
  specification version 6, checkpoint version 5, and SQLite schema 4 are unchanged.
