# Horus CLI

`horus-cli` is the reference terminal coding agent built with the
[`horus`](https://crates.io/crates/horus) framework. It owns the executable, setup and local
configuration, and the Ratatui frontend; the agent loop and capabilities remain in the framework
crate.

## Run

Download the matching archive and checksum from
[GitHub Releases](https://github.com/citizenhicks/horus/releases):

- Apple Silicon macOS: `aarch64-apple-darwin`
- x86_64 Linux: `x86_64-unknown-linux-gnu`

Verify with `shasum -a 256 -c FILE.sha256`, extract the archive, and put `horus` on your
`PATH`. Rust users and other macOS or Linux architectures can build locally with Rust 1.89 or
newer:

```sh
cargo install horus-cli
```

Run `horus`. To run from a source checkout:

```sh
cargo run -p horus-cli
```

The first interactive launch opens setup for a provider, model, optional middleware, and sandbox
approval policy. Built-in providers are OpenAI by API key, OpenAI with ChatGPT login, Kimi,
OpenRouter, Anthropic, and a configurable OpenAI-compatible Responses endpoint.

API-key providers use their standard environment variables:

```sh
export OPENAI_API_KEY=...
export MOONSHOT_API_KEY=...
export OPENROUTER_API_KEY=...
export ANTHROPIC_API_KEY=...
```

If a key is pasted into setup, Horus stores it in its local TOML configuration. ChatGPT login
stores refreshable OAuth credentials in `auth.json`; it does not read or modify Codex CLI
credentials. On Unix, Horus writes configuration and authentication files with owner-only
permissions.

## Local state

The first launch creates `~/.horus`. It contains:

- `config.toml` — model routes, middleware, sandbox, and checkpoint settings.
- `auth.json` — ChatGPT credentials, when configured.
- `horus.sqlite3` — session checkpoints and catalog state.

The supported overrides are:

- `HORUS_CONFIG` — an alternate configuration file.
- `HORUS_STATE_DIR` — an alternate state directory.
- `HORUS_SESSION_ID` — a session to resume at startup.

Each normal launch starts a fresh session. Use the middleware-provided `/resume` command to select
an existing session when session middleware is enabled.

## Terminal contributions

The TUI is a thin subscriber to the framework capability catalog:

- Capabilities own their commands, status widgets, references, and capability-specific rendering.
- `/` opens both CLI shell commands and commands contributed by framework capabilities.
- `$` references are contributed by skills middleware.
- `@` workspace-file completion belongs to the CLI because it is terminal composer behavior.

The CLI itself owns only shell lifecycle and presentation commands: `/help`, `/login`, `/new`,
`/clear`, `/model`, `/reasoning`, `/status`, `/interrupt`, and `/exit`. `/new` starts a fresh
session; `/clear` also clears the underlying terminal scrollback. The menu changes with the
installed capabilities. Sending ordinary text while a turn is active steers the turn when steering
middleware is installed.

The Sora-themed TUI uses the full terminal. The mouse wheel and Page Up/Page Down scroll the chat;
Ctrl-T opens its full-screen transcript view. Arrow, page, and wheel input navigate an open preview;
Up/Down and Ctrl-P/Ctrl-N navigate composer history.

The local command sandbox uses `/usr/bin/sandbox-exec` on macOS and `bwrap` on Linux. The
`/permissions` command changes the sandbox policy between approval prompts, no-prompt execution
without network, and no-prompt execution with network. Bash may write only in the workspace and
its private temporary directory in every mode. Command execution fails closed when the platform
sandbox is unavailable.

## License

Licensed under [Apache-2.0](LICENSE). See [NOTICE](NOTICE) for upstream attribution.
