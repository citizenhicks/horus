## Highlights

- Adds stable, paginated subagent transcript previews with continuations that can page inside a
  large inherited fork seed instead of materializing the full child conversation at once.
- Bounds every preview page to 8 MiB, preserves complete tool-call boundaries, and keeps inherited
  context available through older-page navigation.
- Publishes compact agent picker metadata plus the effective model, reasoning, status, and spawn
  context needed by frontends to render a useful preview header.

## Upgrade

- Gateway protocol 25 is required. Checkpoint version 5, SQLite schema 5, configuration version 15,
  and chat specification version 7 are unchanged.
- The private subagent runtime state starts at version 2; active agent rosters created by older
  builds are not restored after upgrading.
