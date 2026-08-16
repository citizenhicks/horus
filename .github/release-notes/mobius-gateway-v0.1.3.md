## Highlights

- Links `mobius` 0.1.3, so the gateway's existing provider catalog and setup operations now expose
  DeepSeek without gateway-specific dispatch or configuration.
- Accepts DeepSeek credentials through the standard provider flow using `DEEPSEEK_API_KEY` or the
  gateway credential store.
- Reports the renamed "Local and Other" Responses provider directly from the framework catalog.

## Compatibility

- The gateway wire protocol, configuration, and checkpoint formats are unchanged.
- Upgrade the CLI with the gateway; older CLIs do not include the DeepSeek provider manifest.

## Upgrade

The `mobius-cli` package installs both user-facing binaries:

```sh
mobius-gateway exit
cargo install --locked mobius-cli
```

Standalone gateway archives remain available from this release.
