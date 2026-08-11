## Highlights

- Bundles `horus-gateway` 0.7.10 and `horus` 0.7.9.
- Allows authorized agent commands to modify the complete workspace, including initializing,
  staging, and committing repository changes, while gateway state and credentials remain outside
  that boundary.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.10
horus-gateway serve --background
```

- Gateway protocol 25 and all persisted data versions are unchanged.
