## Fixes

- Uses `mobius` 0.2.2 so gateway commands release protected-workspace process locks reliably on
  Linux.

## Compatibility

- Protocol version 5 and TOML configuration version 5 remain the only accepted contracts. No
  legacy configuration discovery, conversion, migration, or fallback was added.
