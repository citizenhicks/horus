## Highlights

- Initializes new non-TLS gateways with both a loopback listener and an account-free Cloudflare
  Quick Tunnel.
- Makes `mobius-gateway connect` advertise the public WSS and local TCP endpoints with the same
  ten-minute, one-use pairing code while the gateway remains available to active clients.
- Removes the plaintext-only bootstrap command; first-run CLI setup now uses the normal supervised
  gateway path.
- Adds protocol v9 epoch-bound replay cursors and an explicit replay-complete boundary so remote
  frontends can restore cached chats atomically across reconnects and gateway restarts.

## Upgrade

- Existing 0.4.0 local-only state remains local. Stop and reinitialize it once to enable both
  endpoints; there is no config migration or fallback path.
