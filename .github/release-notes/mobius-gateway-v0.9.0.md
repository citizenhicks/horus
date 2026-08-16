# möbius Gateway 0.9.0

- Bundles möbius 0.9.0 and serves paginated transcript history for long-running chats.
- Raises the gateway frame budget to 50 MiB for larger workspace inspection responses.
- Bumps the wire protocol to version 30; clients and gateways must upgrade together.
- Keeps gateway command, session, transport, and configuration ownership in focused modules.
