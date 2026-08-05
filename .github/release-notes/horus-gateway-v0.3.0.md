## Highlights

- Introduces protocol version 6 and TOML configuration version 6 as the only accepted contracts.
- Publishes gateway-owned provider, model, reasoning, hosted-search, middleware, tool, session
  activity, artifact, and Git-branch data for thin clients.
- Separates revisioned gateway defaults from active-chat configuration and stores an optional exact
  model route for default subagents.
- Adds supervised `horus-gateway connect` pairing and `serve --background`, with explicit TCP/TLS
  listener and client-endpoint logging.
- Rejects malformed provider catalogs, stale configuration revisions, unsafe remote plaintext, and
  branch changes outside the selected workspace.

## Upgrade

- Stop the 0.2 gateway and initialize fresh version-6 state. Version-5 state is rejected; there is
  no discovery, conversion, migration, fallback, or dual-read path.
