## Highlights

- One install now provides both user-facing commands: `horus` and `horus-gateway`. GitHub CLI
  archives also contain both binaries; the core `horus` 0.1.2 library is linked automatically.
- Each frontend creates or opens an independent chat whose workspace and runtime configuration are
  durable chat state. One gateway can run up to 32 chats while terminal, macOS, and iOS clients
  independently choose which chat to view. `/resume` is the single picker for saved chats across
  every workspace.
- Serializes automatic first-run startup so simultaneous local CLI processes pair with and reuse
  one gateway instead of racing multiple bootstrap children.
- `/login [provider]` is a focused three-page flow for provider selection, masked API key or
  device login, then model and reasoning confirmation with custom model editing.
- First launch opens that same `/login` flow when the gateway has no configured model. The first
  configured model becomes the new-chat default, and `/model` always reads the gateway-owned
  catalog instead of a stale chat-local list. The redundant `/providers` command is removed, and
  canceling setup or using headless mode before setup no longer leaves a bootstrap chat in
  `/resume`.
- `/agent` is a one-page capability and approval-policy editor, while `/model` remains the quick
  model picker.
- `/gateway` adds a two-page saved-gateway screen for reconnecting, pairing, and deleting
  accounts. Environment endpoint/token overrides remain authoritative and visibly lock changes.
- `/cron new [task]` explicitly starts the model-assisted scheduling flow. Ordinary chat cannot
  create scheduled tasks; `/cron` retains list, history, run, reschedule, and delete controls.
- The transcript view releases mouse capture so terminal-native drag-to-copy works.

## Breaking changes

- This client uses gateway protocol version 4 and requires `horus-gateway` 0.1.2. Older gateways
  are rejected; there is intentionally no compatibility layer.
- The bundled gateway uses configuration version 3 and version-3 checkpoints. Existing gateway
  state is not migrated; delete it and initialize fresh state.
- The owner-only gateway token file now stores `{selected_endpoint, tokens}`. The previous map-only
  format is not migrated. Delete the old token file and pair again when prompted.
- Provider and agent setup now use the guided screens rather than legacy environment/composition
  slash-command forms.

## Install or upgrade

```sh
cargo install --locked horus-cli
```

If `horus-gateway` was installed from its old standalone crate, transfer both commands once:

```sh
cargo install --force --locked horus-cli
```

Before starting the new gateway, delete the old gateway state and token file (normally
`~/.horus/gateway` and `~/.horus/gateway-tokens.json`), then run `horus` to create and pair fresh
state.

The GitHub archive is the alternative for systems without a Rust toolchain and includes checksums,
`LICENSE`, and `NOTICE`.
