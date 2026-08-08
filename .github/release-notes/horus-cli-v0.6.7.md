## Highlights

- Bundles `horus-gateway` 0.6.7 and `horus` 0.6.6 with corrected provider identity metadata.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.6.7
horus-gateway serve --background
```

- Gateway protocol remains at version 21. Gateway configuration remains version 12,
  checkpoint JSON remains version 5, and SQLite remains schema 4.
