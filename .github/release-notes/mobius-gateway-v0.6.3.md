## Highlights

- Advances the wire protocol to 18 with separate modified and Git-filtered all-files workspace
  catalogs, plus reliable reads for the selected file.
- Rejects stale file-catalog responses when users switch scopes.

## Upgrade

- Upgrade clients and the gateway together.
- Gateway configuration remains version 11 and checkpoint storage remains schema 4.
