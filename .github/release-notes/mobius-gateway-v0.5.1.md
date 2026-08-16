## Highlights

- Carries typed, semantic provider and capability symbols through the gateway without naming
  frontend-specific glyphs.
- Advances the gateway wire protocol to version 12 for the updated frontend presentation contract.

## Upgrade

- Upgrade clients and the gateway together; protocol versions before 12 are rejected.
- Gateway configuration remains version 10, so existing 0.5 gateway state can be restarted in place.
