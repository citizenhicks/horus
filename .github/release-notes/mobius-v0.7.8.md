## Highlights

- Grounds file edits in the tools middleware by telling agents to read the target's exact current
  contents and surrounding context before making changes.
- Guides patch calls to use raw unified diffs built from text the agent actually read, without
  Markdown fences, reducing malformed and unmatched patches.

## Upgrade

- Gateway protocol 25, checkpoint version 5, SQLite schema 5, configuration version 15, and chat
  specification version 7 are unchanged.
