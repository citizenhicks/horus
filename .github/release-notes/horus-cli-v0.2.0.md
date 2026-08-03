## Highlights

- Renders the gateway-advertised middleware catalog generically, including visible locked entries
  for required capabilities.
- Bundles `horus-gateway` 0.2.0 and `horus` 0.2.0 with patch application, workspace instructions,
  optional durable tasks, and background command sessions.
- Keeps the source package sliced to CLI code while the install/archive intentionally includes the
  sibling `horus-gateway` executable used for automatic local startup.

## Compatibility

- This client speaks gateway protocol version 5 and must be upgraded with the bundled gateway.
- Gateway 0.1.x state is not migrated; initialize fresh gateway state for this release.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli
horus
```
