# möbius CLI 0.9.2

- Bundles möbius 0.9.3 and möbius Gateway 0.9.5.
- Uses the protocol 38 session-file contract.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.9.2
mobius-gateway serve --background
```
