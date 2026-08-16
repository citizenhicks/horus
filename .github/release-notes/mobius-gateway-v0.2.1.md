## Highlights

- Uses an owner-only `gateway.toml` version 5 configuration with provider-manifest defaults and
  corrected live token-usage accounting.
- Delegates Git inspection to the shared read-only sandbox, isolates gateway state and TLS material,
  and keeps scheduled task bodies in private managed files.
- Splits session catalog, Git, and provider services under the gateway host without adding parallel
  registries or service lookup.
- Broadcasts session-resume requests live without retaining them for replay, preventing reopened
  chats from bouncing between stale navigation requests.

## Compatibility

- The gateway accepts protocol version 5, TOML configuration version 5, chat specification version
  2, and cron state version 2 exactly; every other version is rejected.
- JSON/version 4 gateway configuration is not read, converted, migrated, renamed, or discovered as
  fallback state. Delete prior gateway state and initialize this release directly.
