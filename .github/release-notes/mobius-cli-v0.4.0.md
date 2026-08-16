## Highlights

- Speaks gateway protocol version 8 only and renders gateway-supplied middleware configuration
  without capability-name dispatch.
- Forks any safe user or assistant message through the generic `/fork` picker using its exact
  durable transcript target.
- Includes the live gateway dashboard, provider setup, device unpairing, and scrollable device and
  chat histories.
- Lets `mobius-gateway connect` add another client while the gateway keeps serving existing clients.
- Drives provider, model, reasoning, hosted-search, subagent, and chat/default setup from gateway
  manifests.

## Compatibility

- This client requires the 0.4.0 gateway. There is no protocol downgrade, legacy gateway mode,
  compatibility transport, fallback state discovery, or migration.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli
mobius-gateway init
mobius-gateway connect
```
