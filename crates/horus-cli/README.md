# Horus CLI

`horus-cli` is the reference Ratatui client for a `horus-gateway`. The gateway owns agent
composition, providers, sessions, sandboxing, usage, and scheduled work.

## Install the client

Download matching `horus-cli` and `horus-gateway` archives and checksums from
[GitHub Releases](https://github.com/citizenhicks/horus/releases):

- Apple Silicon macOS: `aarch64-apple-darwin`
- x86_64 Linux: `x86_64-unknown-linux-gnu`

Verify with `shasum -a 256 -c FILE.sha256`, extract both binaries into the same directory,
and put it on your `PATH`. Rust users and other macOS or Linux architectures can install both
with Rust 1.89 or newer:

```sh
cargo install horus-gateway horus-cli
```

## Gateway prerequisite

`horus-gateway` is the CLI's only extra runtime prerequisite; the core `horus` crate is linked
into the binaries. Run the CLI from the workspace the local gateway should own:

```sh
cd /path/to/repository
horus
```

With no explicit gateway endpoint or token, the first run initializes the default loopback
gateway for that directory, pairs the CLI, saves its token, and starts `horus-gateway` in the
background. Subsequent runs reconnect to that gateway or restart it without changing its
workspace. For a source checkout, build the sibling gateway binary first:

```sh
cargo build -p horus-gateway
cargo run -p horus-cli
```

Plaintext is restricted to loopback. A gateway reachable over the network must use TLS; point the
client at that exact endpoint before pairing and connecting. Explicit endpoint or token settings
disable automatic local management:

```sh
horus pair tls://gateway.example:443 <pairing-code>
export HORUS_GATEWAY_ENDPOINT=tls://gateway.example:443
horus
```

If local state already exists without a saved CLI token, stop the gateway and pair manually:

```sh
horus-gateway pair-code
horus-gateway serve # keep this running in another terminal
horus pair tcp://127.0.0.1:8741 <pairing-code>
```

Run one task file without the TUI:

```sh
horus run path/to/task.md
# From a source checkout:
cargo run -p horus-cli -- run path/to/task.md
```

The gateway workspace—not the CLI process—is the command and file boundary. An approval prompt
aborts a headless run, so scheduled work that edits files or runs commands needs an appropriate
gateway approval policy.

Register that task with the gateway scheduler, for example every day at 03:00:

```sh
horus cron --task /path/to/repository/task.md --schedule "0 3 * * *"
```

The schedule must be quoted so the shell does not expand `*`:

```sh
horus cron list
horus cron reschedule <task-id> --schedule "0 5 * * *"
horus cron delete <task-id>
horus cron run <task-id>
horus cron history [task-id]
```

Inside the TUI, `/cron` exposes the same add, list, reschedule, delete, run, and history operations.
Every scheduled execution is a normal durable gateway session.

`/providers` shows frontend-safe provider status. `/login <provider>` starts device login;
`/login <provider> env:NAME` securely sends the named environment variable as an API key. Secrets
are never returned by the gateway. `/agent` prints the current composition, and `/agent <json>`
validates, persists, and restarts that composition on the gateway while preserving the session.

API-key providers use their standard environment variables:

```sh
export OPENAI_API_KEY=...
export MOONSHOT_API_KEY=...
export OPENROUTER_API_KEY=...
export ANTHROPIC_API_KEY=...
```

The CLI stores only an owner-readable endpoint-to-token map at
`~/.horus/gateway-tokens.json`. `HORUS_GATEWAY_TOKEN` overrides it explicitly;
`HORUS_GATEWAY_TOKEN_FILE` changes its path.

## Terminal contributions

The TUI is a thin subscriber to the framework capability catalog:

- Capabilities own their commands, status widgets, references, and capability-specific rendering.
- `/` opens both CLI shell commands and commands contributed by framework capabilities.
- `$` references are contributed by skills middleware.
- `@` workspace-file completion is available for a local plaintext gateway; TLS gateways do not
  scan similarly named paths on the client machine.

The CLI owns only shell lifecycle and presentation commands: `/help`, `/agent`, `/providers`,
`/login`, `/pair`, `/profile`, `/artifacts`, `/new`, `/clear`, `/model`, `/reasoning`, `/cron`,
`/status`, `/interrupt`, and `/exit`. The menu changes with the installed gateway capabilities.

The Sora-themed TUI uses the full terminal. The mouse wheel and Page Up/Page Down scroll the chat;
Ctrl-T opens its full-screen transcript view. Arrow, page, and wheel input navigate an open preview;
Up/Down and Ctrl-P/Ctrl-N navigate composer history.

Sandboxing runs on the gateway host and fails closed when its platform sandbox is unavailable.

## License

Licensed under [Apache-2.0](LICENSE). See [NOTICE](NOTICE) for upstream attribution.
