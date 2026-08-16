## Highlights

- Recovers ChatGPT OAuth sessions when the provider rejects an access token before its advertised
  expiry, refreshing and retrying both Responses HTTP and WebSocket requests once.
- Guards concurrent recovery with the rejected token, adopts credentials already refreshed by
  another request, and leaves API-key authentication single-attempt.
- Guides subagent spawns to use fresh context by default, recent turns only when required, and full
  parent history only when essential.

## Upgrade

- Gateway protocol 24, checkpoint version 5, SQLite schema 5, configuration version 15, and chat
  specification version 7 are unchanged.
