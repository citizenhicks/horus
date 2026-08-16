# möbius CLI 0.7.16

- Bundles möbius 0.7.15 and möbius Gateway 0.7.16.
- Supports the `full_access` approval policy and the ordered parallel tool scheduler through both
  CLI and bundled gateway execution.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.7.16
mobius-gateway serve --background
```
