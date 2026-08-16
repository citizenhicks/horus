## Fixes

- Releases the protected-workspace process lock explicitly so inherited child descriptors cannot
  retain it after journal shutdown, preventing false `another process is executing in this workspace`
  errors.

## Contract

- This patch does not add aliases, adapters, fallback dispatch, or migration behavior.
