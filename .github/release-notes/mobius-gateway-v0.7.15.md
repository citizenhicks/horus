# möbius Gateway 0.7.15

- Bundles möbius 0.7.14 and exposes automatic-review and interrupted-search events through gateway
  protocol 27.
- Validates session IDs before checkpoint access and preserves resident session hosts when a tree
  deletion is rejected.
- Reports failed scheduled runs instead of discarding their errors.
