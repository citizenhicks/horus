# möbius CLI 0.7.18

- Bundles möbius 0.7.17 and möbius Gateway 0.7.18.
- Includes direct host execution for Full Access shell commands on macOS and Linux.
- Keeps restricted command modes protected and preserves existing command lifecycle controls.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.7.18
mobius-gateway serve --background
```
