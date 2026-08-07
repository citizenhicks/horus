## Highlights

- Adds authenticated, chunked attachment upload, listing, and download with private per-chat
  storage, bounded quotas, atomic publication, and exact reference validation.
- Advertises each selected model's attachment capability to frontends and supplies uploaded raster
  images to compatible providers through the optional attachment middleware.
- Adds staged, unstaged, committed, and combined Git diff scopes plus bounded workspace-file
  browsing for native clients.

## Upgrade

- Upgrade clients and the gateway together; the wire protocol advances from 15 to 16.
- Gateway configuration remains version 11 and checkpoint storage remains schema 4, so no data
  migration is required.
