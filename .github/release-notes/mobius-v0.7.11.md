# möbius 0.7.11

- Keeps OpenAI Responses WebSocket connections healthy with an idle socket pump, Ping handling,
  bounded streaming, proactive connection recycling, and graceful idle eviction.
- Retries interrupted model streams as fresh model-step attempts with cancellation-aware jittered
  backoff, keeping partial output and tool calls isolated from the successful attempt.
- Falls back to full-context HTTPS streaming after five WebSocket failures and keeps that transport
  selected for the remainder of the session.
- Adds provider-neutral reconnect presentation and prevents raw WebSocket reset details from being
  surfaced as terminal user errors.
- Preserves streamed reasoning-part boundaries without requiring index metadata from compatible
  Responses endpoints.
- Includes terminal provider errors in errored subagent previews while keeping preview pages bounded.
- Repairs incorrect unified-diff hunk counts before applying otherwise valid workspace patches.
