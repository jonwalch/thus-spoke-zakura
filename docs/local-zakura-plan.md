# Plan: test local Zakura changes with ths

Let a developer rebuild Zakura in their own checkout and exercise that build
through the existing wallet, explorer, faucet, and mining controls. Support both
a native process launched by `ths` and a node the developer launches themselves.
Keep Docker as the default node mode; continue using the app and lightwalletd
containers in both local modes.

This records the original design. Both local workflows are now implemented;
see the [README](../README.md#test-a-local-zakura-build) for usage and the
implementation status below for remaining validation/hardening work.

## Developer workflow

Make the first implementation support a local executable:

```console
ths start --zakura-bin /absolute/path/to/zakura/target/debug/zakurad
ths logs zakura -f
```

`ths` generates an isolated Regtest configuration, starts that executable, and
opens the dashboard. After changing Zakura, stop `ths`, rebuild in the Zakura
checkout, and rerun the same command. No Zakura image build or pull is needed.
The launcher prints the resolved binary path and its reported version so it is
clear which build is under test. Custom development versions are allowed.

The second implementation supports manually launching the binary, including
under a debugger:

```console
ths --name local prepare --zakura-rpc http://127.0.0.1:18232
/absolute/path/to/zakura/target/debug/zakurad --config <printed-config-path> start
ths --name local start --zakura-rpc http://127.0.0.1:18232
```

`prepare` creates the development wallet and a host configuration with its
treasury address before the developer starts Zakura. `start` then reuses that
prepared wallet. This preserves the faucet and initial 5 ZEC funding when the
developer controls the node process. An already running node can attach if its
network settings and mining destination match the prepared instance; otherwise
report the required configuration and restart step.

| Mode | Node owner | Node chain on Ctrl+C / stop / reset |
| --- | --- | --- |
| Default Docker image | ths | Deleted, as today |
| `--zakura-bin PATH` | ths | Deleted from the instance's dedicated native data directory |
| `--zakura-rpc URL` | Developer | Preserved; node remains running |

Docker is still required for the app and lightwalletd. Running the entire stack
without Docker, live binary replacement, and arbitrary network upgrade schedules
are outside this change. All modes remain Regtest-only.

## Baseline constraints (before implementation)

- [`runtime.rs`](../crates/tsz-cli/src/runtime.rs) requires all three images,
  starts a Zakura container, derives RPC/P2P ports through Docker inspection,
  and deletes resources before every start and on shutdown. `StartHost` and
  `RecordingHost` already provide a seam for testing lifecycle changes.
- [`main.rs`](../crates/tsz-server/src/main.rs) initializes a random wallet and
  writes the hidden treasury's address into `zakurad.toml`. That configuration
  hardcodes container addresses and `/data` as the chain directory.
- [`rpc.rs`](../crates/tsz-server/src/rpc.rs) calls `generate(blocks)` without
  a destination. Startup funding and treasury replenishment in
  [`api.rs`](../crates/tsz-server/src/api.rs) depend on mining rewards belonging
  to this wallet. Changing only the RPC URL would break that assumption.
- [`wallet.rs`](../crates/tsz-server/src/wallet.rs) fixes activation heights
  through NU6 at block 1 and initializes accounts with a block-2 birthday.
  Attaching to an existing chain needs validation of both assumptions.
- Status output and the Network page currently assume Docker-owned endpoints
  and a chain that is fresh on every run.

The pinned Zakura v1.4.0 RPC surface exposes `generate` with a block count and
does not expose `generatetoaddress`. Do not base treasury integration on an
unavailable address-selecting mining RPC.
([Zakura RPC source](https://github.com/zakura-core/zakura/blob/v1.4.0/crates/zakura-rpc/src/methods.rs))

## 1. Model node selection and resource ownership

Add mutually exclusive `--zakura-bin PATH` and `--zakura-rpc URL` options to
`Start` in [`CLI main.rs`](../crates/tsz-cli/src/main.rs). No option selects the
existing Docker behavior. Introduce a small `NodeSource` enum with `Docker`,
`LocalBinary`, and `ExternalRpc` variants, passed into runtime allocation.

Separate node lifecycle operations from orchestration of the app and
lightwalletd. Resolve a node connection once, including its host RPC URL,
container RPC URL, optional P2P address, and ownership. Pass that connection
into container creation, readiness checks, and endpoint reporting instead of
assuming a `zakura` container exists.

Version `instance.json` and retain support for version-1 Docker instances. Store
the mode, lifecycle state (`prepared` or `running`), run identifier, owned
resources, and native process identity where applicable. Write enough metadata
before allocation to clean up a partially started instance. Use an instance
lock and run identifier so concurrent commands and an old cleanup guard cannot
delete a newer run.

Validate an executable's canonical path, permissions, architecture errors, and
`--version` invocation before replacing an existing environment. Local modes
must never require or silently fall back to the Zakura image. Add
`--without-zakura` to `pull` and `build`; document the existing installer
`TSZ_SKIP_IMAGE_PULL=1` option followed by `ths pull --without-zakura`.

## 2. Generate a host configuration and supervise the local binary

Parameterize server initialization with the node's RPC/P2P listen addresses and
chain directory. Use structured TOML serialization for paths and values. Keep
the Regtest parameters and generated treasury destination shared across modes.
Return public initialization metadata, including the treasury address, through
a machine-readable file; do not scrape the output that prints development keys.

For a local executable, allocate a dedicated directory under the named instance
for its chain, configuration, and logs. Keep wallet and lightwalletd data in their
existing Docker volumes. Export the generated node configuration from the init
container to the instance directory; its chain path must be a host path rather
than the init container's `/data`. Select distinct loopback RPC/P2P ports, with
bounded retries if another process wins a port allocation race.

Launch the executable directly with `Command`, using `--config <path> start`.
Use an instance working directory so incidental files stay out of the Zakura
checkout. Preserve useful development environment settings such as `RUST_LOG`
and `RUST_BACKTRACE`. Capture stdout/stderr for `ths logs zakura`, including
follow mode, and expose the configuration path for reproducing a failure.

Replace container-only node readiness with an RPC probe plus a mode-specific
liveness check. Bound individual RPC requests and the overall startup wait.
After startup, monitor the child alongside shutdown signals so a crashed custom
build produces its exit status and log tail immediately.

On Ctrl+C or `ths stop`, stop dependents, gracefully terminate the owned native
process, wait for exit, and escalate only after a timeout. Retain ownership
metadata if termination fails; never delete a database that is still in use.
Separate `stop` invocations and crash recovery must verify process identity
(including its start identity and instance config), rather than signaling a
stored PID alone. Cleanup must never target the source checkout or an arbitrary
user-supplied data directory.

## 3. Connect the containers to the native node

Treat host access as an explicit runtime configuration:

| Docker environment | Connection strategy for local modes |
| --- | --- |
| Docker Desktop on macOS | Keep the instance bridge network; app and lightwalletd use `host.docker.internal:<rpc-port>` |
| Native Docker Engine on Linux | Run the companion containers with host networking; bind the app, lightwalletd, and node explicitly to distinct `127.0.0.1` ports |

Docker documents `host.docker.internal` for Desktop host access.
([Desktop networking](https://docs.docker.com/desktop/features/networking/networking-how-tos/))
Linux host networking shares the host network namespace and ignores published
port mappings, so that path must select ports itself, omit `-p`, and replace
container DNS names with explicit loopback endpoints.
([Host network driver](https://docs.docker.com/engine/network/drivers/host/))

Validate these paths with a loopback-bound node during the first integration
spike. Probe from the same network context as the consumers, using a small
diagnostic subcommand in the app image. Host-side readiness alone is insufficient.
Keep the publicly displayed RPC URL as `http://127.0.0.1:<port>`; container routing
addresses are internal details.

Detect the Docker environment rather than branching only on the OS: Linux can
also use Docker Desktop. Initially support local Docker Desktop and local native
Docker Engine; report unsupported remote daemons or rootless networking with a
specific diagnostic. Do not change the node's listener to `0.0.0.0` as an
automatic workaround. Cover simultaneous named instances and port collisions.

## 4. Attach to a developer-owned process

Implement `prepare` using the same wallet/config generation as local executable
mode, but without starting Zakura. Store the prepared wallet identity and
connection details. Make repeated preparation reuse the instance; require an
explicit reset to replace it. Reject conflicting preparation or startup for a
running instance.

In this mode, keep the external chain, node config, and logs in a separate node
directory outside the launcher metadata directory and all recursive cleanup
targets. The generated config may refer to a dedicated chain directory chosen
during preparation, but it remains developer-owned. `start --zakura-rpc` must
transition the prepared instance to running without the current unconditional
pre-start deletion. No node container, native child, or owned chain volume is
created by attach mode.

Before provisioning funds or allowing mutations, validate:

- The RPC is reachable from the host and companion containers and reports
  Regtest with the supported upgrade schedule and required RPC capabilities.
- The configured mining payout matches the prepared treasury. Use a supported
  non-mutating template/configuration check; validate that check against the
  pinned node during implementation. A mismatch must fail before attempting
  the current 102-block treasury replenishment.
- The chain history and tree states required for wallet sync are available.
  Replace the synthetic block-1 birthday state with the actual state for an
  existing chain; handle a genesis-only chain through the explicit bootstrap
  flow. Give indexing/scanning progress a separate startup timeout.

Repeat the network check in the server, so a manually configured server cannot
skip the launcher validation. Document that attaching with this full development
mode permits startup funding, mining, and transaction submission to the existing
Regtest chain. Those changes remain after detaching.

Initially support the generated configuration's unauthenticated, local RPC
connection. Detect authentication failures and explain the supported setup;
never rewrite an existing node's authentication settings. Cookie-authenticated
attachment and arbitrary existing wallets can follow separately.

For attach mode, Ctrl+C and `stop` detach: stop/remove the companion containers
but retain the prepared wallet, metadata, and indexing data so the next attach
uses the same treasury. A failed start must also leave prepared state reusable.
`reset --force` deletes those ths-owned volumes and metadata. All of these paths
preserve the external process, chain, node configuration, and logs. Resetting
the wallet creates a new treasury, requiring preparation and a node
configuration update before the next attachment.

Record a known block hash/height with reused wallet state and detect an external
chain reset, accounting for normal reorgs; a shared Regtest genesis hash alone
does not identify the same chain history. Give an actionable resync/reset error
instead of serving stale balances. `logs zakura` should identify externally
managed logs rather than invoke `docker logs` against a nonexistent container.

## 5. Make status and documentation reflect the selected mode

Update `status`, `doctor`, `endpoints`, and `list` to use stored mode and resolved
connections. Report service health separately from process ownership. Include
the binary path/version for a managed local build, and mark attached nodes as
externally managed. External P2P information is optional; never invent a default
address when the launcher does not know it.

Extend the server status contract and
[`web API schemas`](../web/src/lib/api/schemas.ts) together. Update the
[`Network page`](../web/src/features/network/NetworkPage.tsx) to identify the
node mode, handle an absent P2P endpoint, and explain persistence accurately.
An attached chain is not guaranteed to start at block 0, have no peers, or belong
to only one ths instance.

Add both local workflows to the README, including prerequisites, the edit /
rebuild / restart loop, debugger attachment, cleanup behavior, and diagnostics
for a missing executable, incompatible RPC, payout mismatch, and an unreachable
host node. Keep the default Docker quick start intact.

## Delivery and acceptance

Implement in three reviewable increments:

1. Node selection, connection resolution, metadata migration, and a connectivity
   spike on macOS and Linux. Preserve the existing Docker lifecycle tests.
2. Local executable launch, generated host config, process supervision, logs,
   cleanup, status/UI updates, and documentation. This delivers the core local
   Zakura development loop with full wallet/faucet behavior.
3. Prepared external attachment, chain/treasury validation, existing-chain wallet
   initialization, and external-resource preservation.

Use focused unit tests for CLI conflicts, version-1 metadata, config paths with
spaces, network-specific endpoint resolution, and resource ownership. Extend
the recording lifecycle tests to cover partial startup, child exit, Ctrl+C,
timeouts, concurrent stop, and stale process identities. Add contract/UI tests
for mode reporting and missing P2P information.

Integration acceptance on macOS Docker Desktop and Linux Docker Engine:

- Start without a Zakura image using a locally modified executable. Verify its
  resolved path/version and an observable change from that build.
- Reach a healthy dashboard with five development accounts and Account 1's
  initial 5 Orchard ZEC. Exercise faucet, send, mining, and wallet synchronization.
- Rebuild Zakura and restart with the same command; confirm the new build runs
  without rebuilding or pulling companion images.
- Run two named local instances without port, process, or data collisions.
- On failure or shutdown, clean up only the owned resources and leave no managed
  native child behind. Preserve useful failure diagnostics before cleanup.
- Prepare a wallet, manually launch Zakura, and attach successfully. Verify that
  stopping/resetting ths leaves its process, chain, configuration, and logs intact.
- Detach and reattach without changing the node's mining address or losing the
  development wallet. Detect an externally reset chain before reusing old state.
- Reject a wrong network, incompatible upgrade schedule, or wrong treasury
  before any mining or funding. Verify attachment to a compatible nonempty chain.
- Run the existing Rust and web CI checks and installer tests; plain `ths` retains
  its current Docker behavior.

## Implementation status

Implemented native executable supervision and prepared external attachment,
mode-aware cleanup/status/logs, instance locking, Regtest genesis/upgrade/payout
validation, real external wallet birthdays, and persistent chain anchoring.
Both local modes passed live startup on macOS with OrbStack without a Zakura
image installed. Native mode also passed faucet, Orchard transfer, mining, and
sync; external mode passed detach/reattach without refunding, plus rejection of
a mismatched treasury and a reset chain without mining. Resetting the prepared
wallet preserved the external process, configuration, and chain. Default Docker
startup remains working.

Remaining hardening: live Docker Desktop and Linux Engine coverage, automated
end-to-end regression coverage, bounded port-allocation retries, and expanded
mode-specific `doctor` diagnostics. Port collisions currently fail startup and
clean up owned resources; they are not automatically retried. Metadata uses an
exclusive instance lock and unique native config directory rather than an
explicit lifecycle phase field.
