## Highlights

- Adds optional attachment middleware with protected, session-scoped storage and bounded tools for
  listing and reading uploaded UTF-8 files.
- Adds provider-neutral image input for OpenAI-compatible providers, Anthropic, and Kimi while
  allowing providers such as DeepSeek to advertise that attachments are unavailable.
- Preserves attachment references through durable transcripts, replay, compaction, and subagent
  boundaries.

## Breaking changes

- User-input and model-choice records now carry attachment data and provider capability metadata.
  Downstream frontends and model adapters must compile against the 0.6 contracts.
- Checkpoint JSON and SQLite schema versions remain unchanged.
