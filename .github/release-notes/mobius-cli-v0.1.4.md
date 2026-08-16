## Highlights

- Bundles `mobius-gateway` 0.1.3 and `mobius` 0.1.3, making DeepSeek available in the existing login
  and model setup screens with no CLI-specific provider code.
- Installs both `mobius` and `mobius-gateway` with the updated framework provider catalog.
- Shows the existing configurable Responses endpoint as "Local and Other" for local inference
  servers and third-party compatible APIs.

## Compatibility

- The client/gateway wire protocol and existing gateway state remain unchanged.
- Upgrade the CLI and its bundled gateway together; older CLIs do not recognize DeepSeek.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --locked mobius-cli
mobius
```

The GitHub archive remains the alternative for systems without a Rust toolchain and includes both
binaries, checksums, `LICENSE`, and `NOTICE`.
