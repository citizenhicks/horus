## Highlights

- Bundles `mobius-gateway` 0.7.6 and `mobius` 0.7.5.
- Prevents a completed scheduled run from retaining its overlap lock through duplicated or
  inherited file handles.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.7.6
mobius-gateway serve --background
```

- Gateway protocol 24 and all persisted state formats remain unchanged.
