## Highlights

- Adds an explicit HTTP Responses opt-in for endpoints and models that support automatic reasoning
  summaries, making their short streamed activity headings available to frontends.
- Keeps every OpenAI-compatible HTTP endpoint summary-free by default instead of inferring model
  capability from its URL.
- Keeps authorization focused on credentials; endpoint capabilities now live in the model
  transport that owns the request shape.

## Upgrade

- No protocol, checkpoint, or storage schema versions changed.
- The persistent OpenAI WebSocket provider uses a curated reasoning-model catalog, already requests
  automatic summaries, and is unchanged.
