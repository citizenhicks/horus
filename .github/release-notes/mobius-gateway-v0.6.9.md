## Highlights

- Exposes the generic per-turn model-step budget through agent composition, defaulting to 256 with
  no product-level upper bound.
- Bundles `mobius` 0.6.8, including corrected Codex Responses history replay.

## Upgrade

- Upgrade clients and the gateway together for protocol 22.
- Gateway protocol is now 22, gateway configuration version is 13, and chat specification version
  is 6. Existing version 12 configuration and version 5 chat specifications are intentionally
  incompatible; SQLite schema 4 is unchanged.
