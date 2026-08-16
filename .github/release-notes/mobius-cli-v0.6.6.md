## Highlights

- Renders downloadable session-file metadata from generic middleware presentation blocks.
- Bundles gateway protocol 21, `mobius-gateway` 0.6.6, and `mobius` 0.6.5.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.6.6
mobius-gateway serve --background
```

- Upgrade every gateway client to protocol 21 at the same time.
- Gateway config remains version 12, checkpoint JSON remains version 5, and SQLite remains schema 4.
- Back up the gateway state directory before replacing the running binary.
