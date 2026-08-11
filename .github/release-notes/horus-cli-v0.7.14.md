# Horus CLI 0.7.14

- Bundles Horus 0.7.12 and Horus Gateway 0.7.14.
- Uses the simplified one-document `apply_patch` contract and safely renders hosted searches that
  complete without query metadata.
- New gateway configurations use the expanded concise coding-agent system prompt.

Upgrade both installed commands together:

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.14
horus-gateway serve --background
```
