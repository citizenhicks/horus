# möbius CLI 0.9.1

- Adds terminal extension lifecycle and per-agent activation controls.
- Moves cron commands onto the generic capability-command path.
- Bundles möbius 0.9.2 and möbius Gateway 0.9.4.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.9.1
mobius-gateway serve --background
```
