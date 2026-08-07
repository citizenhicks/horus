## Highlights

- Keeps idle WebSocket clients alive with transport-level ping frames, avoiding intermediary idle
  disconnects without adding protocol messages or configuration.
- Makes workspace-file browsing honor Git ignore rules while retaining bounded catalog results.
- Hardens attachment upload cleanup, quotas, previews, and session-scoped lifecycle handling.

## Upgrade

- Upgrade clients and the gateway together; the wire protocol advances from 16 to 17.
- Gateway configuration remains version 11 and checkpoint storage remains schema 4.
