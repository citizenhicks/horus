# Horus CLI 0.7.15

- Bundles Horus 0.7.14 and Horus Gateway 0.7.15 with gateway protocol 27.
- Shows automatic tool-review progress and closes pending model-step blocks after interruptions,
  retries, or crash recovery.

Upgrade both installed commands together:

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.15
horus-gateway serve --background
```
