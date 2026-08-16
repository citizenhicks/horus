# möbius 0.7.18

- Uses Responses compaction v2 for ChatGPT Codex providers instead of the retired `/responses/compact` endpoint.
- Reuses the active Responses WebSocket for native compaction, with normal Responses HTTP fallback when WebSockets are unavailable.
- Aligns Codex request metadata with Codex CLI 0.147.0 and removes timer-forced WebSocket reconnects.
- Retries interrupted compaction streams and retains a bounded recent user-message tail around opaque compaction state.
