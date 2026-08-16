## Highlights

- Advances the wire protocol to 19 with the gateway machine name and a durable history cursor in
  each opened-session snapshot.
- Keeps history pagination complete when the live replay buffer is truncated.
- Bundles `mobius` 0.6.3.

## Upgrade

- Upgrade clients and the gateway together.
- Gateway configuration remains version 11 and checkpoint storage remains schema 4.
