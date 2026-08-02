# Horus Gateway

`horus-gateway` is the headless Horus runtime. It owns one workspace's agent,
provider credentials, sandbox, sessions, usage, artifacts, and scheduled tasks.
The terminal, macOS, iPhone, and iPad clients all use the same authenticated
framed protocol; localhost is not a special execution path.

Initialize and serve a local gateway:

```sh
horus-gateway init --workspace /path/to/repository
horus-gateway serve
```

Initialization prints a ten-minute, one-use pairing code. Plaintext listeners
and clients are restricted to loopback. A remote macOS or Linux host requires
TLS:

```sh
horus-gateway init --workspace /path/to/repository \
  --listen 0.0.0.0:8741 \
  --tls-cert /absolute/path/fullchain.pem \
  --tls-key /absolute/path/private-key.pem
```

By default, owner-only state is stored under `~/.horus/gateway`. Set
`HORUS_GATEWAY_STATE_DIR` for both `init` and `serve` to use another location.
Provider secrets remain on the gateway and are never returned to clients.

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
spawn a child CLI.
