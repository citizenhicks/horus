## Highlights

- Bundles `horus-gateway` 0.7.7 and `horus` 0.7.6.
- Recovers ChatGPT OAuth access-token revocation without requiring a manual provider login when the
  saved refresh token remains valid.
- Uses fresh context as the documented default for subagent delegation.

## Install or upgrade

```sh
horus-gateway exit
cargo install --force --locked horus-cli --version 0.7.7
horus-gateway serve --background
```

- Gateway protocol 24 and all persisted state formats remain unchanged.
