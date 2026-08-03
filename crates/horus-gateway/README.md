# Horus Gateway

`horus-gateway` is the headless Horus runtime. One process owns machine
credentials, usage, and scheduled tasks while hosting up to 32 independent
chat agents. Every chat owns its canonical workspace, model, reasoning, agent
features, approval policy, and prompt. The terminal, macOS, iPhone, and iPad
clients can independently open different chats or subscribe to the same one.

Install `horus-cli` to get both the client and gateway commands:

```sh
cargo install --locked horus-cli
```

The separately versioned `horus-gateway` crate is the runtime library used by those binaries.

Initialize and serve a local gateway:

```sh
horus-gateway init
horus-gateway serve
```

Initialization prints a ten-minute, one-use pairing code. Plaintext listeners
and clients are restricted to loopback. A remote macOS or Linux host requires
TLS:

```sh
horus-gateway init --listen 0.0.0.0:8741 \
  --tls-cert /absolute/path/fullchain.pem \
  --tls-key /absolute/path/private-key.pem
```

By default, owner-only state is stored under `~/.horus/gateway`. Set
`HORUS_GATEWAY_STATE_DIR` or pass `--state-dir` to use another location.
Provider secrets remain on the gateway and are never returned to clients.
The configured-model catalog and new-chat default live in gateway configuration.
The first configured model becomes the default. A new chat copies that default,
then stores its own selected model and runtime recipe beside its durable
checkpoint; changing one chat never changes the catalog, another chat, or its
workspace.

On macOS or Linux, inspect or gracefully stop the configured gateway from
another terminal:

```sh
horus-gateway status
horus-gateway exit
```

Status prints the configured endpoint together with `running` or `stopped`.
Exit verifies the gateway's locked process record before sending
SIGINT and waits up to five seconds for shutdown.

Pair the terminal client with the printed code:

```sh
horus pair tcp://127.0.0.1:8741 <pairing-code>
```

If every client token is lost, stop the gateway and issue another one-use code;
existing paired clients remain valid:

```sh
horus-gateway pair-code
horus-gateway serve
```

The internal scheduler accepts standard five-field cron expressions. Scheduled
runs use durable Horus sessions and never install system crontab entries or
spawn a child CLI. A frontend starts assisted setup with the protocol's cron
setup operation (`/cron new [task]` in the terminal client); ordinary chat is
not authorized to create schedules. Model-confirmed task files are owner-only
under the gateway state directory. With no clients and no registered cron tasks,
the gateway exits after 72 hours; any scheduled task disables that idle timer.
Stopping the gateway manually also stops scheduled work, and missed runs are not
replayed after restart.
