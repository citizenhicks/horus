## Highlights

- Bundles `horus-gateway` 0.6.8 and `horus` 0.6.7 with manifest-driven provider and middleware
  configuration.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.6.8
horus-gateway serve --background
```

- Gateway protocol remains at version 21. Gateway configuration remains version 12,
  checkpoint JSON remains version 5, and SQLite remains schema 4.
