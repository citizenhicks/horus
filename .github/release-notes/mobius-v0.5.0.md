## Highlights

- Adds a Scratchpad middleware for bounded session notes and explicit, approval-aware promotion to
  durable global lessons.
- Adds independent automatic approval review with inherited or selected model routes, configurable
  strictness, exact-call authority, network-enabled execution, and fail-to-Ask escalation.
- Moves built-in middleware metadata and configurable policy into core-owned manifests so hosts can
  expose settings without capability-specific frontend branches.
- Adds generic navigation and chat-menu surfaces, action lists, editable capability input, and live
  widget replacement to the frontend-neutral protocol.

## Breaking changes

- `ApprovalPolicy::On` is replaced by `ApprovalPolicy::Ask`; `AutoApprove` is the new default.
- Frontend capability commands now include optional editable input, and middleware presentation gains
  new slots and action-list records.
- This release adds no compatibility aliases, fallback dispatch, or automatic state migration.
