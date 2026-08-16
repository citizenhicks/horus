# möbius CLI 0.7.12

- Bundles möbius 0.7.11 and möbius Gateway 0.7.12 using gateway protocol 26.
- Separates interrupted partial output from fresh retry attempts, closes unfinished hosted-search
  entries, and renders a concise reconnect notice while recovery is in progress.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.7.12
mobius-gateway serve --background
```
