# möbius CLI 0.9.3

- Adds named provider-instance setup and safe removal with confirmation.
- Shows provider labels consistently in defaults and usage instead of exposing instance IDs.
- Bundles möbius 0.9.5 and möbius Gateway 0.9.7 using protocol 39.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.9.3
mobius-gateway serve --background
```
