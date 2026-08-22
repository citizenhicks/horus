# möbius 0.9.7

- Accepts Agent Plugins v1 packages with root `plugin.json`, remote Streamable HTTP `mcp.json` servers, and the existing fixed `skills/` layout.
- Preserves the legacy `.codex-plugin` format for existing extensions.
- Lets gateway hosts contribute portable MCP tools without coupling the framework to a hosted catalog.
- Inherits host Git and SSH credentials inside the command sandbox while suppressing repository-local Git execution redirects.

Gateway protocol 44, configuration 19, and chat specification 9 are supported by the companion Gateway release.
