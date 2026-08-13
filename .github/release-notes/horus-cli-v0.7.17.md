# Horus CLI 0.7.17

- Bundles Horus 0.7.16 and Horus Gateway 0.7.17 with the corrected Codex Responses transport
  fallback behavior.

Upgrade both installed commands together:

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.17
horus-gateway serve --background
```
