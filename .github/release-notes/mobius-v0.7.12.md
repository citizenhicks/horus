# möbius 0.7.12

- Normalizes hosted-search completions without usable query text so frontends always receive a
  valid provider-neutral web-search event.
- Accepts both plural and singular OpenAI search-query fields while preserving every non-empty
  query.
- Aligns `apply_patch` with the familiar patch envelope through one strict JSON `patch` argument,
  while retaining `diffy`, ordered matching, context anchors, line endings, and sandbox limits.
