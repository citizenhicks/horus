## Highlights

- Renders gateway-supplied live, history, preview, artifact, and middleware blocks without local
  capability-name branches.
- Reopens the selected chat after provider or agent settings restart its agent, restoring the
  authoritative transcript instead of showing a blank new-chat view.
- Bundles `horus-gateway` 0.2.2 and `horus` 0.2.2, including reliable protected-workspace lock
  release and the live-only resume behavior that prevents terminal flicker.

## Compatibility

- This client speaks protocol version 5 only and must be installed with the bundled 0.2.2 gateway.
  It has no fallback transport or legacy gateway mode.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli
horus
```
