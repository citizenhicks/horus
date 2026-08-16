# möbius 0.8.0

- Adds durable prompt-cache diagnostics, provider-owned cost estimates, stable cache identities, and explicit Anthropic cache breakpoints.
- Tracks context rewrite epochs and compaction counts while preserving attachment materialization across compaction and removing disabled scratchpad projections.
- Moves session files to private content-addressed blob storage with integrity validation, deduplication, garbage collection, and 50 MiB file support.
- Strengthens model-step lifecycle diagnostics, replay fidelity, provider continuation handling, and sandboxed background execution.
