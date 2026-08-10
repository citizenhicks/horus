## Highlights

- Bundles `horus` 0.7.4 and rebuilds each resident agent from the chat's current instructions and
  enabled middleware when its runtime recipe changes.
- Keeps the assembled system prompt separate from chronological chat history across agent
  replacement, restart, and compaction.
- Advertises scheduling as an optional, default-enabled capability; disabling it removes its model
  tool and prompt section. Sandboxing, tools, sessions, and steering remain always available.
- Retains the Scratchpad management snapshots when its agent capability is disabled, so stored
  notes remain visible without exposing Scratchpad tools or instructions to the model.

## Upgrade

- Gateway protocol 24, checkpoint version 5, and SQLite schema 5 are unchanged.
- Gateway configuration version 15 and chat specification version 7 define the new minimum
  middleware set without legacy interpretation. Preserve the provider catalog and credentials,
  then recreate 0.7.3 chat recipes.
