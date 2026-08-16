# möbius CLI 0.7.19

- Bundles möbius 0.7.18 and möbius Gateway 0.7.19.
- Fixes ChatGPT Codex compaction for long-running transcripts and avoids unnecessary timer-driven WebSocket reconnects.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.7.19
mobius-gateway serve --background
```
