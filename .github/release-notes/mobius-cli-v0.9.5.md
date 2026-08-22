# möbius CLI 0.9.5

- Bundles möbius 0.9.7 and möbius Gateway 0.9.10 using protocol 44.
- Supports portable authenticated MCP extensions and host Git/SSH credential setup through the bundled gateway.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.9.5
mobius-gateway serve --background
```
