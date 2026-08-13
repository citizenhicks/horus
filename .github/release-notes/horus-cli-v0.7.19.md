# Horus CLI 0.7.19

- Bundles Horus 0.7.18 and Horus Gateway 0.7.19.
- Fixes ChatGPT Codex compaction for long-running transcripts and avoids unnecessary timer-driven WebSocket reconnects.

Upgrade both installed commands together:

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.19
horus-gateway serve --background
```
