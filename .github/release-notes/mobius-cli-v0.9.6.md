# möbius CLI 0.9.6

- Bundles möbius 0.9.7 and möbius Gateway 0.9.11 using protocol 44.
- Safely upgrades Gateway 0.9.9 host state for portable extensions and Git/SSH credential setup.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.9.6
mobius-gateway serve --background
```
