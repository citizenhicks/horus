# möbius CLI 0.7.17

- Bundles möbius 0.7.16 and möbius Gateway 0.7.17 with the corrected Codex Responses transport
  fallback behavior.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.7.17
mobius-gateway serve --background
```
