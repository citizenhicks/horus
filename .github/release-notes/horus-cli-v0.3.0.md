## Highlights

- Speaks gateway protocol version 7 only and renders the gateway-supplied middleware roster and
  nested settings without capability-name dispatch in the terminal frontend.
- Drives provider setup from gateway manifests, including model descriptions, allowed reasoning
  efforts, and hosted search fixed to off when a provider advertises no other mode.
- Lets users apply agent changes to the active chat or save them as revisioned defaults for future
  chats, including every schema-advertised middleware setting.
- Shows the current agent card for a new chat and preserves the active chat across agent reloads.
- Uses the supervised pairing flow and bundled background-capable gateway command.
- Adds first-run setup for an existing user-owned Cloudflare Tunnel and native WSS gateway
  connections on Apple clients, without a separate VPN app. A copyable short-lived setup payload
  and iPhone/iPad QR prefill the Apple pairing form without exposing the tunnel token.
- Bundles the scrollable no-command gateway dashboard, confirmed device unpairing, and direct
  `horus-gateway provider` setup.

## Compatibility

- This client requires the 0.3.0 gateway and has no protocol downgrade, legacy gateway mode,
  compatibility transport, or fallback state discovery.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli
horus-gateway init
horus-gateway connect
```
