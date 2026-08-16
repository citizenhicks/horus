## Highlights

- Adds a built-in DeepSeek Responses provider using `DEEPSEEK_API_KEY`, with the
  `deepseek-v4-flash` model, low/high/maximum reasoning choices, and hosted live web search.
- Normalizes DeepSeek reasoning deltas and preserves reasoning content for durable transcript
  replay through the existing provider-neutral protocol.
- Renames the configurable Responses provider to "Local and Other" to make its existing support
  for loopback and third-party OpenAI-compatible inference endpoints explicit.

## Compatibility

- DeepSeek V4 Pro is not advertised because DeepSeek's Responses endpoint does not support it yet.
- This release does not change checkpoint formats or public protocol records.
