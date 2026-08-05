## Highlights

- Introduces protocol version 7 and TOML configuration version 8 as the only accepted contracts.
- Publishes gateway-owned provider, model, reasoning, hosted-search, middleware, tool, session
  activity, artifact, and Git-branch data for thin clients.
- Publishes the complete middleware roster and schema-backed settings; clients only render generic
  toggles, integers, and exact gateway-supplied choices.
- Separates revisioned gateway defaults from active-chat configuration and stores an optional exact
  model route for default subagents.
- Adds supervised `horus-gateway connect` pairing and `serve --background`, with explicit TCP/TLS
  listener and client-endpoint logging.
- Adds binary WSS transport plus owner-only, token-file-based supervision of an existing
  user-owned Cloudflare Tunnel. Pairing prints plain setup text plus an iPhone/iPad QR; release
  archives include a checksum-pinned `cloudflared` sidecar.
- Adds a no-command live dashboard plus direct `horus-gateway provider` setup; leaving the
  dashboard keeps the gateway running. Device and chat histories scroll, and confirmed unpairing
  immediately revokes the selected device credential and disconnects its live clients.
- Rejects malformed provider catalogs, stale configuration revisions, unsafe remote plaintext, and
  branch changes outside the selected workspace.

## Upgrade

- Initialize fresh version-8 state. Earlier state is rejected; there is
  no discovery, conversion, migration, fallback, or dual-read path.
