## Highlights

- Bundles gateway protocol 16 with attachment capability metadata, uploaded-file operations,
  scoped Git diffs, and workspace-file browsing.
- Keeps terminal file references link-only; local uploads remain native Apple-app behavior.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.6.0
mobius-gateway serve --background
```

- Gateway configuration remains version 11 and checkpoint storage remains schema 4.
