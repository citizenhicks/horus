# Horus CLI 0.8.0

- Bundles Horus 0.8.0 and Horus Gateway 0.8.0 with protocol version 27.
- Hydrates resumed chats before drawing so partially replayed transcripts do not flash in the TUI.
- Renders the new model-step diagnostics and session-file protocol data while keeping agent behavior gateway-owned.

Upgrade both installed commands together:

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.8.0
horus-gateway serve --background
```
