# möbius CLI 0.7.15

- Bundles möbius 0.7.14 and möbius Gateway 0.7.15 with gateway protocol 27.
- Shows automatic tool-review progress and closes pending model-step blocks after interruptions,
  retries, or crash recovery.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.7.15
mobius-gateway serve --background
```
