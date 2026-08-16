## Highlights

- Bundles `mobius-gateway` 0.7.3 and `mobius` 0.7.3.
- Reduces the same typed recorded-event stream for live output, reconnects, history pages, and
  previews without parsing prose or capability-specific identifier prefixes.
- Preserves structured lifecycle, errors, semantic presentation, and capability-scoped grouping in
  both the terminal UI and headless output.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.7.3
mobius-gateway serve --background
```

- Gateway protocol 24 is a clean break and requires `mobius-gateway` 0.7.3.
