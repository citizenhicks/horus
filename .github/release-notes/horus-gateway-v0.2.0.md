## Highlights

- Ships one gateway-owned middleware manifest used for composition, validation, and frontend-safe
  capability metadata.
- Stores only enabled optional middleware IDs; required scheduling and session capabilities are
  always installed and advertised as non-deselectable.
- Composes optional workspace instructions and durable tasks, and links the sandbox-owned
  background command lifecycle from `horus` 0.2.0.

## Compatibility

- The wire protocol is now version 5, gateway configuration version 4, and chat specification
  version 2. This release intentionally does not read the 0.1.x formats; initialize fresh gateway
  state when upgrading.
