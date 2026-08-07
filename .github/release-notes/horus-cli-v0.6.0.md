## Highlights

- Bundles gateway protocol 16 with attachment capability metadata, uploaded-file operations,
  scoped Git diffs, and workspace-file browsing.
- Keeps terminal file references link-only; local uploads remain native Apple-app behavior.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.6.0
horus-gateway serve --background
```

- Gateway configuration remains version 11 and checkpoint storage remains schema 4.
