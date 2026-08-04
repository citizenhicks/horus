# Horus Gateway

`horus-gateway` is the headless Horus runtime. One process owns machine
credentials, usage, and scheduled tasks while hosting up to 32 independent
chat agents. Every chat owns its canonical workspace, model, reasoning, agent
features, approval policy, and prompt. The terminal, macOS, iPhone, and iPad
clients can independently open different chats or subscribe to the same one.
Chats store only enabled optional middleware IDs. The gateway advertises the
ordered catalog to clients and always installs scheduling and durable sessions.

Install `horus-cli` to get both the client and gateway commands:

```sh
cargo install --locked horus-cli
```

The separately versioned `horus-gateway` crate is the runtime library used by those binaries.

Initialize and pair a local gateway:

```sh
horus-gateway init
horus-gateway connect
```

`connect` starts the gateway, prints its client endpoint and a ten-minute,
one-use code, then waits. Enter both values in a client. Once a client pairs,
the command returns and the gateway keeps running in the background.

Plaintext listeners and clients are restricted to loopback. An iPhone, iPad,
or another machine therefore needs a routable TLS endpoint with a
publicly trusted certificate whose hostname matches that endpoint:

```sh
horus-gateway init --listen 0.0.0.0:8741 \
  --tls-cert /absolute/path/fullchain.pem \
  --tls-key /absolute/path/private-key.pem
horus-gateway connect --endpoint tls://gateway.example:8741
```

On iPhone, iPad, or Mac, choose **Add gateway** and enter the displayed
**Gateway address** and **One-time code**. On another terminal client, run the
displayed `horus pair` command. Pairing consumes the code and returns a unique
bearer token; Apple clients keep it in Keychain and `horus` keeps it in its
owner-only gateway account file. Later connections use that token, not the
one-time code.

To add another device while the gateway is already running, an authenticated
Apple client can open **Gateway → Pair another device → Create one-time code**;
an authenticated terminal client can run `/pair`. `horus-gateway connect` is
the host-side recovery flow for a stopped gateway.

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

Status prints the configured listener together with `running` or `stopped`.
Exit verifies the gateway's locked process record before sending
SIGINT and waits up to five seconds for shutdown.
`serve --background` starts a detached process on macOS or Linux and returns
only after that process owns the gateway process record. Foreground `serve`
continues to run until interrupted. Use `serve --background` for ordinary
restarts after at least one client is paired.

If every client token is lost, stop the gateway and run the supervised pairing
flow again; existing paired clients remain valid:

```sh
horus-gateway exit
horus-gateway connect # add --endpoint tls://HOST:PORT for TLS
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
