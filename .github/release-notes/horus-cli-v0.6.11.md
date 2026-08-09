## Highlights

- Renders assistant commentary as durable assistant messages instead of transient event status.
- Bundles `horus-gateway` 0.6.11 and `horus` 0.6.10 with bounded compaction for long Codex and
  Responses runs.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.6.11
horus-gateway serve --background
```

- Upgrade clients and the gateway together for protocol 22. Gateway configuration version 13,
  chat specification version 6, checkpoint version 5, and SQLite schema 4 are unchanged.
