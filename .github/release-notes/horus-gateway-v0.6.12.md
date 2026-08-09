## Highlights

- Raises the maximum gateway frame envelope to 20 MiB for long conversations while keeping initial
  transcript replay bounded and earlier history paginated.
- Bounds the short-lived reconnect cache by both event count and encoded bytes, and avoids sending
  duplicate raw transcript events alongside their rendered presentation.

## Upgrade

- Upgrade clients and the gateway together for the larger frame envelope. Protocol 22, gateway
  configuration version 13, chat specification version 6, checkpoint version 5, and SQLite schema
  4 are unchanged.
