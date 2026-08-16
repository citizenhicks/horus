# möbius CLI 0.8.0

- Bundles möbius 0.8.0 and möbius Gateway 0.8.0 with protocol version 27.
- Hydrates resumed chats before drawing so partially replayed transcripts do not flash in the TUI.
- Renders the new model-step diagnostics and session-file protocol data while keeping agent behavior gateway-owned.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.8.0
mobius-gateway serve --background
```
