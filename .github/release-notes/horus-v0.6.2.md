## Highlights

- Hardens attachment hydration across compaction, replay, forks, and provider image-input limits.
- Keeps text attachments available on non-image models while failing unsupported current images
  clearly without poisoning later turns.
- Preserves attachment-only user messages and their neutral replay placeholders.

## Upgrade

- Provider capability metadata now names image input explicitly; upgrade gateway clients together.
