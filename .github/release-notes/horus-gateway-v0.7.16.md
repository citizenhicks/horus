# Horus Gateway 0.7.16

- Bundles Horus 0.7.15 and supports full-access shell execution for approved gateway sessions.
- Keeps gateway state, TLS keys, provider credentials, and gateway control tokens unavailable to
  full-access commands while permitting access to ordinary host paths.
- Preserves command cleanup and sandbox authority across foreground and background execution.
