# möbius 0.7.16

- Preserves Codex Responses transport failures instead of switching to the HTTP fallback after
  unrelated WebSocket interruptions.
- Uses HTTP fallback only after an explicit `426 Upgrade Required` WebSocket response and preserves
  provider status codes such as `404` for actionable diagnostics.
