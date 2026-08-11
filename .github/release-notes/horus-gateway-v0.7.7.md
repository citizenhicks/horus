## Highlights

- Bundles `horus` 0.7.6 with revocation-aware ChatGPT OAuth recovery for Responses HTTP,
  compaction, and WebSocket handshakes.
- Retries a rejected OAuth credential once after refresh while preserving the original behavior for
  API keys and non-authentication provider errors.
- Makes fresh subagent context the explicit default strategy in the model-facing guidance.

## Upgrade

- Gateway protocol 24, checkpoint version 5, SQLite schema 5, configuration version 15, and chat
  specification version 7 are unchanged.
