## Highlights

- Advances the wire protocol to 20 with durable active-input presentation and editing.
- Adds validated model and reasoning catalogs for configurable providers, including multiple
  comma-separated reasoning efforts and collision-safe route validation.
- Bundles `horus` 0.6.4 and keeps capability policy in middleware rather than the agent loop.

## Upgrade

- Gateway configuration advances from version 11 to 12 and requires each configured provider's
  reasoning catalog explicitly.
- Upgrade clients and the gateway together; older configuration and wire versions are rejected.
- There is no automatic state migration. Back up the gateway state directory first; the supported
  fresh-start path is `horus-gateway init`, whose explicit reinitialization confirmation permanently
  deletes the existing configuration, chats, providers, and paired devices.
