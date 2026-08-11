## Highlights

- Bundles `horus-gateway` 0.7.9 and `horus` 0.7.8.
- Improves coding-agent patch reliability with read-before-edit and raw unified-diff guidance.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.9
horus-gateway serve --background
```

- Gateway protocol 25 and all persisted data versions are unchanged.
