## Highlights

- Bundles `horus-gateway` 0.7.0 and `horus` 0.7.0 with provider-attributed token usage.
- Displays the provider for each daily usage record while retaining aggregate daily and lifetime
  totals.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.0
horus-gateway serve --background
```

- Gateway protocol 23 and configuration version 14 are required.
