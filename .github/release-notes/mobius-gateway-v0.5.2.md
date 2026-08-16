## Highlights

- Exposes authenticated, paginated session history and recent durable run summaries.
- Reports aggregate execution statistics and active-run details for observability frontends.
- Makes configurable provider model IDs part of the backend catalog, including comma-separated
  setup for providers without a fixed manifest.

## Upgrade

- Upgrade clients and the gateway together; the wire protocol advances from 12 to 14.
- Gateway configuration advances from version 10 to 11 and checkpoint storage from schema 3 to 4;
  both require explicit migration before startup.
