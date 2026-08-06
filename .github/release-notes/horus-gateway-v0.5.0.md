## Highlights

- Advertises every configurable built-in middleware option through one generic manifest contract,
  including approval reviewer model and strictness, subagent routing and limits, context thresholds,
  compaction, steering, and session page size.
- Makes strict automatic approval with network access the default for new agents while preserving
  human approval whenever the independent reviewer is uncertain or fails.
- Carries Scratchpad navigation, chat-menu action lists, edits, deletion, and explicit global
  promotion through protocol v10 without Scratchpad-specific gateway operations.
- Validates all advertised model-route settings and persists the generic middleware configuration as
  the sole agent-policy source.

## Upgrade

- Gateway protocol and configuration are version 10; durable chat specifications are version 5.
- Version-9 gateway state is rejected intentionally. Back it up, stop the old gateway, and initialize
  and configure fresh 0.5 state; there is no automatic migration or legacy fallback.
