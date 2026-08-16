## Highlights

- Composes one deterministic Markdown system prompt from the chat instructions and concise,
  capability-owned sections in middleware declaration order.
- Gives root agents explicit TOML-backed subagent delegation guidance and keeps child identity
  guidance capability-local.
- Keeps standalone native compaction history-only, so current system instructions and tools stay
  top-level instead of being copied into compacted conversation history.
- Adds a minimal sandbox host-platform section and concise guidance for coding tools, discovered
  workspace instructions, and installed skills.
- Defines sandboxing, tools, sessions, and steering as the minimum middleware set, while scheduling
  becomes optional and contributes its own prompt section only when enabled.
- Keeps Scratchpad's stored-note surfaces available in read-only mode when agent access is off,
  without restoring its prompt or tools.

## Upgrade

- This requested patch release renames the public `Middleware::prompt_fragment` hook to
  `prompt_section` and adds the `PromptSection` contract.
- Checkpoint, transcript, and gateway wire formats are unchanged. The gateway's minimum-capability
  change requires configuration version 15 and chat specification version 7; preserve provider
  setup and recreate prior chat recipes.
