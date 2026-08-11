# Horus CLI 0.7.12

- Bundles Horus 0.7.11 and Horus Gateway 0.7.12 using gateway protocol 26.
- Separates interrupted partial output from fresh retry attempts, closes unfinished hosted-search
  entries, and renders a concise reconnect notice while recovery is in progress.

Upgrade both installed commands together:

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.12
horus-gateway serve --background
```
