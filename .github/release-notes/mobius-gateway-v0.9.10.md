# möbius Gateway 0.9.10

- Adds portable Streamable HTTP MCP connections with OAuth PKCE or owner-only API-key storage.
- Requires approval for every remote MCP tool and keeps a failing remote server isolated from chat startup.
- Adds exact GitHub HTTPS credential discovery and host-helper setup without storing Git credentials in gateway configuration.
- Adds safe SSH public-identity inventory and non-overwriting Ed25519 setup for headless hosts.
- Inherits existing host Git helpers, SSH keys, and SSH agents while disabling repository-local Git execution redirects.

Gateway protocol 44 and configuration 19 are required. Chat specification 9 is unchanged.
