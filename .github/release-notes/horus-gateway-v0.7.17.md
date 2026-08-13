# Horus Gateway 0.7.17

- Bundles Horus 0.7.16 with the corrected Codex Responses WebSocket and HTTP fallback behavior.
- Keeps transient provider transport failures on the WebSocket retry path instead of entering a
  sticky HTTP fallback session.
