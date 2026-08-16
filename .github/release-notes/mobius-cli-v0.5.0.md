## Highlights

- Renders gateway-advertised middleware toggles, selects, numeric bounds, model inheritance, and
  descriptions without capability-specific setup branches.
- Adds generic gateway-dashboard capability overlays with Scratchpad action-list editing, deletion,
  and promotion controls.
- Aligns terminal settings into stable label, value, and muted-description columns and distinguishes
  inherited model values with the accent color.
- Uses gateway protocol v10 and the 0.5 framework and gateway contracts.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli
mobius-gateway init
```

- Existing version-9 gateway state is not migrated; back it up before initializing 0.5 state.
