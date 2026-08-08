<p align="center">
  <img src="https://raw.githubusercontent.com/citizenhicks/horus/main/horus-app/apple/Sources/HorusApp/Assets.xcassets/HorusLogo.imageset/HorusLogo.svg" width="160" height="160" alt="Horus">
</p>

# Horus

Horus is a small, frontend-neutral Rust framework for coding agents. Its shipped
runtime has one headless composition root: `horus-gateway`. The terminal and
Apple apps are thin clients of that gateway; they do not own agent behavior.

| Component | Boundary |
| --- | --- |
| [`horus`](https://crates.io/crates/horus) | Embeddable agent loop, provider and storage interfaces, sandbox policy, middleware, and frontend-neutral protocol |
| [`horus-gateway`](https://crates.io/crates/horus-gateway) | The only shipped owner of an `Agent`; composes capabilities and owns authentication, chats, workspaces, artifacts, usage, and cron |
| [`horus-cli`](https://crates.io/crates/horus-cli) | Ratatui gateway client and local gateway launcher |
| [Apple app](horus-app/apple/README.md) | SwiftUI gateway client for macOS, iPhone, and iPad |

## Install

Download `horus-<version>-<target>.tar.gz` and its checksum from
[GitHub Releases](https://github.com/citizenhicks/horus/releases). The archive
contains `horus`, `horus-gateway`, and `cloudflared`. Rust users can install the
two Horus commands with Rust 1.89 or newer:

```sh
cargo install --locked horus-cli
```

Cargo does not install `cloudflared`; Quick Connect requires it beside
`horus-gateway` or on `PATH`.

Users upgrading from the earlier split packages should run
`cargo install --force --locked horus-cli` once so Cargo transfers both commands to the CLI
package.

Then run `horus` from the workspace it should own:

```sh
cd /path/to/repository
horus
```

On first use, the CLI initializes the machine-wide gateway with a loopback listener
and Cloudflare Quick Tunnel, provisions its local credential, starts the gateway in
the background, and opens `/login` when no provider is configured. The first
configured model becomes the gateway default for new chats. Each CLI invocation
creates an independent chat for its current directory; other terminal and app
frontends can connect to the same gateway and open separate or shared chats.
Workspace, model, reasoning, agent features, approval policy, and prompt are
chat-scoped. The gateway owns the available-model catalog and new-chat default;
a chat only selects from that catalog. The core `horus` crate is linked into the
binaries and is not a separate runtime prerequisite.

Plaintext remains limited to loopback. Run `horus-gateway connect` to advertise
both that local TCP endpoint and the Quick Tunnel's public WSS endpoint with one
single-use pairing code; pairing through either exchanges it for a per-client
token used on later connections. A direct TLS listener remains available as an advanced
alternative. See the
[gateway guide](https://github.com/citizenhicks/horus/blob/main/crates/horus-gateway/README.md),
the [CLI guide](https://github.com/citizenhicks/horus/blob/main/crates/horus-cli/README.md),
and the [Apple guide](https://github.com/citizenhicks/horus/blob/main/horus-app/apple/README.md)
for manual and remote setup.

To run the Rust binaries from this checkout:

```sh
cargo build -p horus-cli
cargo run -p horus-cli --bin horus
```

## Framework

Horus requires Rust 1.89 or newer.

```toml
[dependencies]
horus = "0.6"
```

The caller owns composition:

```rust,no_run
use std::path::Path;
use std::sync::Arc;

use horus::Result;
use horus::agent::{Agent, AgentConfig, create_agent};
use horus::backend::checkpoint::{CheckpointStore, sqlite::SqliteCheckpoint};
use horus::backend::model::{Model, ModelRouter, openai::OpenAi};
use horus::backend::sandbox::{ApprovalPolicy, Sandbox, local::LocalSandbox};
use horus::middleware::{Middleware, MiddlewareStack};
use horus::middleware::tools::Tools;

async fn build_agent(
    workspace: &Path,
    api_key: String,
    model_id: &str,
) -> Result<Agent> {
    let model: Arc<dyn Model> = Arc::new(OpenAi::new(
        api_key,
        "https://api.openai.com/v1",
        model_id,
    )?);
    let models = Arc::new(ModelRouter::new("default", model));
    let sandbox = Arc::new(Sandbox::new(
        Arc::new(LocalSandbox::new(workspace)?),
        ApprovalPolicy::Ask,
    ));
    let checkpoints: Arc<dyn CheckpointStore> =
        Arc::new(SqliteCheckpoint::new(workspace.join("horus.sqlite3"))?);
    let middleware: Vec<Arc<dyn Middleware>> = vec![Arc::new(Tools::coding())];

    create_agent(AgentConfig::new(
        models,
        sandbox,
        checkpoints,
        MiddlewareStack::new(middleware)?,
        "You are a concise coding agent.",
    ))
    .await
}
```

Frontends submit
[`protocol::Op`](https://docs.rs/horus/latest/horus/protocol/enum.Op.html) values and consume
events from `Agent`. Framework capabilities may also contribute frontend-neutral commands,
references, widgets, and rendered blocks. A frontend decides how those contributions look;
capability implementations do not depend on terminal code. Interrupts target a specific turn, and
events carry an optional submission ID so command-driven and unsolicited system events remain
distinct.

## Architecture

```text
Terminal client ─┐
Apple client ────┼── versioned gateway protocol ⇄ horus-gateway ──> Agent
Other clients ───┘                                     │
                                                        ├─ model router
                                                        ├─ sandbox
                                                        ├─ checkpoints
                                                        └─ middleware stack
```

The gateway is the shipped composition root. It creates one `Agent` per active
chat and translates authenticated wire operations into frontend-neutral core
operations. Frontends render gateway events and capability contributions; they
do not dispatch tools or implement middleware behavior.

| Path | Owns |
| --- | --- |
| `src/agent/` | The linear session, model, and tool loop |
| `src/backend/model/` | Provider transports, provider manifests, and model routing |
| `src/backend/sandbox/` | File and command boundaries, approval policy, and background processes |
| `src/backend/checkpoint/` | Durable checkpoints, journals, transcript pages, and the session catalog |
| `src/middleware/` | Vertical optional capabilities, including their tools, hooks, state, settings, and UI contributions |
| `src/protocol/` | Frontend-neutral operations, events, approvals, usage, and presentation records |
| `crates/horus-gateway/` | Runtime composition, authentication, client sessions, workspaces, artifacts, Git, usage, cron, and the wire protocol |
| `crates/horus-cli/src/frontend/` | Terminal lifecycle, input, and rendering only |
| `horus-app/apple/` | Apple lifecycle, platform storage, and SwiftUI rendering only |

Middleware declaration order is observable hook and prompt-fragment order. Each
capability owns its complete vertical slice; adding one to the shipped runtime
changes its module and the explicit gateway registry, not the agent loop or a
frontend-specific dispatcher. Static prompt fragments are composed once at
agent creation, while dynamic state enters through runtime hooks.

Checkpoint backends expose cursor-paginated session catalogs and
sequence-bounded transcript pages. Context offloading and compaction remain
middleware policy, and provider adapters normalize private wire formats before
the agent loop sees them.

The sandbox enforces approval policy for every approval-required tool. The local
backend uses Seatbelt on macOS and Bubblewrap on Linux and fails closed when the
platform sandbox is unavailable. Linux must permit the selected `bwrap` binary
to create user, PID, and network namespaces; AppArmor-restricted hosts need a
matching Bubblewrap profile. Filesystem confinement remains active under every
approval policy.

## Contributing

Read [AGENTS.md](https://github.com/citizenhicks/horus/blob/main/AGENTS.md) before changing the framework. It defines module ownership,
capability extension points, required checks, and the no-compatibility rule while
the public contract remains under active development.

Release tags are intentionally separate:

- `horus-vX.Y.Z` publishes the framework crate and creates its GitHub Release.
- `horus-gateway-vX.Y.Z` publishes the gateway crate and attaches server binaries.
- `horus-cli-vX.Y.Z` publishes the CLI crate and attaches downloadable binaries to a GitHub
  Release.

Publish `horus`, then `horus-gateway`, then `horus-cli`, waiting for each dependency to appear in
the crates.io index. Creating a tag is a release action; ordinary pushes and pull requests only
run CI. The release workflow expects a `CARGO_REGISTRY_TOKEN` repository secret for
crates.io publication.

## License

Licensed under [Apache-2.0](LICENSE). See [NOTICE](NOTICE) for third-party
attributions.
