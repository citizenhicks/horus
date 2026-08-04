## Highlights

- Adds complete provider manifests with model descriptions, reasoning choices, hosted-search
  support, symbols, and strict validation of advertised selections.
- Extends frontend-neutral capability records with counts, symbols, progress, and popup content so
  thin frontends can render tasks and subagents without capability-specific branches.
- Reports the registered tool count, makes frontend update backpressure fail visibly instead of
  dropping middleware state, and propagates subagent lifecycle persistence failures.
- Rejects nonempty unversioned checkpoint databases and adds a no-network Git-mutation sandbox path
  that keeps non-Git agent metadata protected.

## Contract

- This release adds no aliases, fallback dispatch, compatibility adapters, or state migration.
