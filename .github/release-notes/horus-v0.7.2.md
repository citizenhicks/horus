## Highlights

- Requests automatic reasoning summaries when the HTTP Responses adapter targets OpenAI's native
  API, making its short streamed activity headings available to frontends.
- Keeps arbitrary OpenAI-compatible endpoints summary-free by default, with an explicit opt-in
  for endpoints that implement the same capability.
- Keeps authorization focused on credentials; endpoint capabilities now live in the model
  transport that owns the request shape.

## Upgrade

- No protocol, checkpoint, or storage schema versions changed.
- The persistent OpenAI WebSocket provider already requested automatic summaries and is unchanged.
