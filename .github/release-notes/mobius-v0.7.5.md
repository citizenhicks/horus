## Highlights

- Keeps Scratchpad's management command registered when agent access is disabled, so its stored
  session and global notes can still refresh through the declared frontend widgets.
- Continues to remove disabled Scratchpad tools and prompt instructions from the model while
  rejecting every note mutation at the capability boundary.
- Exercises disabled read-only operations through the middleware stack's real command-registration
  path to prevent the management UI and command catalog from drifting apart again.

## Upgrade

- Gateway protocol 24, checkpoint version 5, SQLite schema 5, configuration version 15, and chat
  specification version 7 are unchanged.
