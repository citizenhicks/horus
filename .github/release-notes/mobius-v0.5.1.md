## Highlights

- Adds `FrontendSymbol`, a frontend-neutral symbol vocabulary with a custom fallback for plugins.
- Uses semantic symbols for provider manifests, middleware widgets, and capability actions so each
  frontend can choose its own artwork without provider or icon-set coupling.

## Breaking changes

- `FrontendWidget::symbol` and `FrontendAction::symbol` now use `FrontendSymbol` instead of strings;
  `ProviderDefinition::symbol()` now returns `&FrontendSymbol`.
- This release adds no compatibility aliases or dual symbol representation.
