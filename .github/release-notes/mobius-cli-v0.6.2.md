## Highlights

- Bundles gateway protocol 17 with hardened attachment handling and idle WebSocket keepalives.
- Keeps workspace references bounded and aligned with the gateway's Git-aware file catalog.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.6.2
mobius-gateway serve --background
```

- Gateway configuration remains version 11 and checkpoint storage remains schema 4.
