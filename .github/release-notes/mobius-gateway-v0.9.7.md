# möbius Gateway 0.9.7

- Adds named, tinted provider instances so separate accounts and endpoints for the same provider can coexist.
- Stores API credentials per instance and keeps provider-wide browser login shared where appropriate.
- Removes non-default provider instances safely, reloading idle chats and falling dormant chats back to the gateway default when reopened.

Gateway protocol 39, configuration 18, and chat specification 9 are required.
