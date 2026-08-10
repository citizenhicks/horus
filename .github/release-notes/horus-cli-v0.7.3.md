## Highlights

- Bundles `horus-gateway` 0.7.3 and `horus` 0.7.3.
- Reduces the same typed recorded-event stream for live output, reconnects, history pages, and
  previews without parsing prose or capability-specific identifier prefixes.
- Preserves structured lifecycle, errors, semantic presentation, and capability-scoped grouping in
  both the terminal UI and headless output.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.3
horus-gateway serve --background
```

- Gateway protocol 24 is a clean break and requires `horus-gateway` 0.7.3.
