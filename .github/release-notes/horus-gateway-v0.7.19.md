# Horus Gateway 0.7.19

- Bundles Horus 0.7.18 with Responses compaction v2 for ChatGPT Codex models.
- Prevents long transcripts from repeatedly aborting on authenticated `/responses/compact` 404 responses.
- Keeps healthy Codex WebSockets open until the server closes them and reconnects interrupted compaction streams with bounded retries.
- Sends the Codex wire-compatibility and request identity metadata required by current ChatGPT Codex routes.
