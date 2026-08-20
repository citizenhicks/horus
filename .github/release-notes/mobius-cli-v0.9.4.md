# möbius CLI 0.9.4

- Shows context consumed instead of context remaining.
- Measures fill against the gateway's effective compaction boundary, or the model context window when compaction is disabled.
- Preserves the displayed fill across session refreshes without showing usage before the first token report.
- Bundles möbius 0.9.6 and möbius Gateway 0.9.8 using protocol 40.

Upgrade both installed commands together:

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli --version 0.9.4
mobius-gateway serve --background
```
