## Highlights

- Bundles `mobius-gateway` 0.7.8 and `mobius` 0.7.7.
- Merges subagent preview pages by stable identity, prepends older transcript content without
  duplication, and preserves the next continuation.
- Adds an `O older` action to load earlier subagent messages while keeping the compact agent picker
  focused on task name and status.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.7.8
mobius-gateway serve --background
```

- Gateway protocol 25 is required; persisted checkpoint, SQLite, configuration, and chat
  specification versions are unchanged.
