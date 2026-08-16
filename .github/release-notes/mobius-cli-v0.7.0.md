## Highlights

- Bundles `mobius-gateway` 0.7.0 and `mobius` 0.7.0 with provider-attributed token usage.
- Displays the provider for each daily usage record while retaining aggregate daily and lifetime
  totals.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.7.0
mobius-gateway serve --background
```

- Gateway protocol 23 and configuration version 14 are required.
