## Highlights

- Hosts up to 32 independent chat agents behind one machine gateway. Each chat persists its own
  workspace, model, reasoning, capabilities, approval policy, and prompt; frontends can open
  different chats or subscribe to the same one.
- Owns the configured-model catalog and new-chat default. The first provider setup establishes
  that default, while each chat independently selects one route from the global catalog. Catalog
  changes are broadcast gateway-wide, including to authenticated clients without a selected chat.
- Preserves capability-owned checkpoint metadata when a chat recipe changes. Provider credentials
  and device login are gateway-owned, refresh every matching resident chat, and allow only one
  device-login poll across the machine.
- Adds an explicit, gateway-owned conversational scheduler started by `/cron new [task]`.
  The model asks for missing details, an approval-required tool saves the final Markdown task
  under the gateway state directory, and the existing scheduler runs it from there. Setup remains
  active across clarification turns, while execution chats cannot schedule more tasks. Cron setup,
  management, and history are scoped to the source chat.
- Makes framed event reads cancellation-safe, preventing partial frames from being lost while a
  terminal UI switches between input and gateway events.
- Stops the gateway after 72 continuous hours with neither a connected client nor a registered
  cron task. Any scheduled task disables the idle timer. When the gateway is stopped manually,
  scheduled work stops and missed runs are not replayed.
- Marks an active cron run failed if its agent stops unexpectedly, preventing a stale running
  record from blocking later invocations.
- Shares the gateway command runner with `mobius-cli`, which now installs the
  `mobius-gateway` executable for users. This crate remains the separately versioned runtime
  library, and this release still includes standalone gateway archives.

## Breaking changes

- The wire protocol is now version 4. Older clients are rejected; there is intentionally no
  compatibility or migration layer at this stage. Use `mobius-cli` 0.1.3 with this gateway.
- Gateway configuration is now version 3 and durable checkpoints use the `mobius` 0.1.2 version-3
  schema. Existing gateway state is not migrated; delete it and initialize a fresh gateway.
- Provider configs no longer accept API-key environment overrides. Without a stored credential,
  each provider uses the standard environment variable declared by its manifest.
- The public `read_frame` API now takes a `FrameReader`, which preserves partial frames across
  cancelled reads.
- `cargo install mobius-gateway` no longer installs the executable. Install
  `mobius-cli` 0.1.3 to receive both `mobius` and `mobius-gateway`, or use the standalone gateway
  archive attached to this release.

## Upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli
```

The one-time `--force` is required only when transferring an existing standalone
`mobius-gateway` binary to the combined CLI package. Before starting the new gateway, delete
the old gateway state and token file (normally `~/.mobius/gateway` and
`~/.mobius/gateway-tokens.json`), then run `mobius` to create and pair fresh state.
