## Highlights

- Normalizes output-only Responses metadata before replay, including reasoning `format` and
  `status` plus message and function-call `status`, while preserving valid status fields on hosted
  tool records.
- Makes the per-turn primary model-step budget configurable, raises its default from 64 to 256,
  and removes the previous product-level upper bound.

## Upgrade

- The framework API adds `AgentConfig::max_model_steps`; checkpoint JSON and SQLite storage are
  unchanged.
