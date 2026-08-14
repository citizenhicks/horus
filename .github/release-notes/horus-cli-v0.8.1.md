# Horus CLI 0.8.1

- Bundles Horus 0.8.1 and Horus Gateway 0.8.1.

Upgrade both installed commands together:

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.8.1
horus-gateway serve --background
```
