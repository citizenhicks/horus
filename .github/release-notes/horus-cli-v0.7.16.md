# Horus CLI 0.7.16

- Bundles Horus 0.7.15 and Horus Gateway 0.7.16.
- Supports the `full_access` approval policy and the ordered parallel tool scheduler through both
  CLI and bundled gateway execution.

Upgrade both installed commands together:

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.16
horus-gateway serve --background
```
