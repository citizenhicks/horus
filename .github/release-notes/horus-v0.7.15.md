# Horus 0.7.15

- Adds the `full_access` approval policy for shell commands that need host filesystem and network
  access without per-command approval; workspace file tools remain workspace-scoped.
- Preserves sandbox authority across background execution and checkpoints, and reliably reaps
  full-access process groups after cancellation or timeout.
- Runs compatible tool calls concurrently while preserving model order around exclusive and
  unknown-tool barriers.
