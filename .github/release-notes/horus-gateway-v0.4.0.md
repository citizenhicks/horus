## Highlights

- Introduces gateway protocol version 8 and TOML configuration version 9 as the only accepted
  contracts.
- Publishes schema-backed middleware settings, generic message actions, and exact durable message
  targets for safe per-message forks.
- Adds the live gateway dashboard, direct provider setup, active and inactive device/chat history,
  and immediate credential revocation when a device is unpaired.
- Supports native WSS connections and supervised Cloudflare quick tunnels with copyable setup text
  and an iPhone/iPad pairing QR.
- Lets `horus-gateway connect` issue another one-time code through an already-running gateway
  without interrupting active clients.
- Keeps local CLI and dashboard access paired automatically while remote clients use independent
  revocable credentials.

## Upgrade

- Initialize fresh version-9 state. Earlier state is rejected; there is no discovery, conversion,
  migration, fallback, or dual-read path.
