## Highlights

- Gives authorized foreground and background commands write access to the complete workspace,
  without hardcoded exceptions for Git or other workspace metadata.
- Allows repository initialization, staging, commits, and branch changes through the same command
  path as every other workspace mutation.
- Keeps commands non-root and confined to the workspace, with network and external denied-read
  paths still enforced by the existing approval and sandbox policies.

## Upgrade

- Gateway protocol 25, checkpoint version 5, SQLite schema 5, configuration version 15, and chat
  specification version 7 are unchanged.
