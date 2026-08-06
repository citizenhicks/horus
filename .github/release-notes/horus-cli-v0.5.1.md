## Highlights

- Uses the 0.5.1 framework and gateway contracts with semantic provider and capability symbols.
- Speaks gateway protocol version 12.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.5.1
horus-gateway serve --background
```

- Existing version-10 gateway state is reused; no reset or migration is required.
