## Highlights

- Bundles `horus-gateway` 0.7.6 and `horus` 0.7.5.
- Prevents a completed scheduled run from retaining its overlap lock through duplicated or
  inherited file handles.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.6
horus-gateway serve --background
```

- Gateway protocol 24 and all persisted state formats remain unchanged.
