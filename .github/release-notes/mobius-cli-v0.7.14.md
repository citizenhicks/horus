# möbius CLI 0.7.14

- Bundles möbius 0.7.12 and möbius Gateway 0.7.14.
- Uses the simplified one-document `apply_patch` contract and safely renders hosted searches that
  complete without query metadata.
- New gateway configurations use the expanded concise coding-agent system prompt.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.7.14
mobius-gateway serve --background
```
