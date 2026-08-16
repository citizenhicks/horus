## Highlights

- Bundles `mobius-gateway` 0.6.8 and `mobius` 0.6.7 with manifest-driven provider and middleware
  configuration.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.6.8
mobius-gateway serve --background
```

- Gateway protocol remains at version 21. Gateway configuration remains version 12,
  checkpoint JSON remains version 5, and SQLite remains schema 4.
