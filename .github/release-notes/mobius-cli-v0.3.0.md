## Highlights

- Speaks gateway protocol version 6 only and renders gateway-supplied middleware records without
  capability-name dispatch in the terminal frontend.
- Drives provider setup from gateway manifests, including model descriptions, allowed reasoning
  efforts, and hosted search fixed to off when a provider advertises no other mode.
- Lets users apply agent changes to the active chat or save them as revisioned defaults for future
  chats, including a default subagent model and reasoning route.
- Shows the current agent card for a new chat and preserves the active chat across agent reloads.
- Uses the supervised pairing flow and bundled background-capable gateway command.

## Compatibility

- This client requires the 0.3.0 gateway and has no protocol downgrade, legacy gateway mode,
  compatibility transport, or fallback state discovery.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli
mobius-gateway init
mobius-gateway connect
```
