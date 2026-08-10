## Highlights

- Bundles `horus-gateway` 0.7.4 and `horus` 0.7.4.
- Uses the refreshed capability-owned system prompt whenever a chat runtime is recreated.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.4
horus-gateway serve --background
```

- Gateway protocol 24 remains unchanged. Gateway configuration version 15 and chat specification
  version 7 are required; preserve provider setup and recreate 0.7.3 chats.
