## Highlights

- Bundles `mobius-gateway` 0.7.4 and `mobius` 0.7.4.
- Uses the refreshed capability-owned system prompt whenever a chat runtime is recreated.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.7.4
mobius-gateway serve --background
```

- Gateway protocol 24 remains unchanged. Gateway configuration version 15 and chat specification
  version 7 are required; preserve provider setup and recreate 0.7.3 chats.
