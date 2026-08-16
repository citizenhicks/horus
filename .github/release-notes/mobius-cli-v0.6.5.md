## Highlights

- Renders gateway-supplied queued active input at the transcript tail without capability-specific
  terminal dispatch.
- Bundles gateway protocol 20, `mobius-gateway` 0.6.5, and `mobius` 0.6.4.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.6.5
```

- Gateway configuration must be version 12 and checkpoint JSON version 5 before restart.
- No automatic migration is provided. Back up the gateway state directory before upgrading. If it
  is incompatible, run `mobius-gateway init` and explicitly confirm reinitialization; this permanently
  deletes the old configuration, chats, providers, and paired devices before creating fresh state.
- If the gateway is not already running after initialization, start it with
  `mobius-gateway serve --background`.
