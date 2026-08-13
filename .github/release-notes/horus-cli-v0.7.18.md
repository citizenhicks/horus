# Horus CLI 0.7.18

- Bundles Horus 0.7.17 and Horus Gateway 0.7.18.
- Includes direct host execution for Full Access shell commands on macOS and Linux.
- Keeps restricted command modes protected and preserves existing command lifecycle controls.

Upgrade both installed commands together:

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.18
horus-gateway serve --background
```
