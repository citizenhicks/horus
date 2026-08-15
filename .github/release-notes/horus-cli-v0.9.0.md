# Horus CLI 0.9.0

- Bundles Horus 0.9.0 and Horus Gateway 0.9.0.
- Reorganizes dashboard, setup, and terminal test ownership without changing the command surface.

Upgrade both installed commands together:

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.9.0
horus-gateway serve --background
```
