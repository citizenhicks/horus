## Highlights

- Bundles `mobius-gateway` 0.7.10 and `mobius` 0.7.9.
- Allows authorized agent commands to modify the complete workspace, including initializing,
  staging, and committing repository changes, while gateway state and credentials remain outside
  that boundary.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.7.10
mobius-gateway serve --background
```

- Gateway protocol 25 and all persisted data versions are unchanged.
