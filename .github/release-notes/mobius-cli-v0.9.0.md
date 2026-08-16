# möbius CLI 0.9.0

- Bundles möbius 0.9.0 and möbius Gateway 0.9.0.
- Reorganizes dashboard, setup, and terminal test ownership without changing the command surface.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.9.0
mobius-gateway serve --background
```
