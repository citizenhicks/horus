## Highlights

- Bundles `horus-gateway` 0.7.5 and `horus` 0.7.5.
- Keeps disabled Scratchpad management surfaces refreshable while stored notes remain read-only.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.5
horus-gateway serve --background
```

- Gateway protocol 24 and all persisted state formats remain unchanged.
