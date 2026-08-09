## Highlights

- Bundles `horus-gateway` 0.6.12 with bounded reconnect replay and support for gateway frames up to
  20 MiB, preventing long transcripts from entering a reconnect loop.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.6.12
horus-gateway serve --background
```

- Upgrade clients and the gateway together. Protocol 22, gateway configuration version 13, chat
  specification version 6, checkpoint version 5, and SQLite schema 4 are unchanged.
