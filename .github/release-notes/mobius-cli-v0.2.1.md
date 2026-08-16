## Highlights

- Renders gateway-supplied live, history, preview, artifact, and middleware blocks without local
  capability-name branches.
- Reopens the selected chat after provider or agent settings restart its agent, restoring the
  authoritative transcript instead of showing a blank new-chat view.
- Bundles `mobius-gateway` 0.2.1 and `mobius` 0.2.1, including the live-only resume fix that removes
  repeated terminal flicker and cursor-position timeouts.

## Compatibility

- This client speaks protocol version 5 only and must be installed with the bundled 0.2.1 gateway.
  It has no fallback transport or legacy gateway mode.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli
mobius
```
