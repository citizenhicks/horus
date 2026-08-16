## Highlights

- Shows durable run, model, tool, failure, elapsed-time, and token statistics.
- Restores middleware widgets from history and renders task completion states accurately.
- Supports backend-declared multi-model provider setup without provider-specific UI branches.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.5.2
mobius-gateway serve --background
```

- Migrate gateway configuration to version 11 and checkpoint storage to schema 4 before restart.
