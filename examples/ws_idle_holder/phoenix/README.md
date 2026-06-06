# `ws_idle_holder/phoenix` — Phoenix/Elixir reference impl (comparator)

A minimal **Phoenix Channels + Presence** server that mirrors the
`ws_idle_holder` flagship demo: it holds N idle WebSocket-over-TLS
connections so the shared bench harness can measure per-connection memory
(density), connect latency, and churn against Kāra.

This is the **commercial-tier comparator #67** in the Phase 3 bench-day
plan, and the *rhetorically critical* one: BEAM/Phoenix is the runtime
most often cited as the gold standard for holding millions of idle
connections. Beating — or even matching — it on density is the headline
this comparator exists to produce.

## Why a Phoenix comparator

Elixir/Phoenix is *the* reference point for connection-density at scale:
Discord, Pinterest, and Bleacher Report all run Phoenix Channels for
exactly this workload. If Kāra's per-connection footprint lands at or
below Phoenix's, the density claim is settled against the toughest
incumbent. If Phoenix matches or beats Kāra, the framing shifts to the
**combination claim** (see below) — never density in isolation.

## Real-world-vs-purist caveat (READ THIS)

Per the bench-day discipline (`apples-to-apples = mimic real-world prod
config, not a purist protocol match`), this comparator runs **Phoenix
Channels with Presence tracking enabled** — the real-world Elixir default
— **not** raw Cowboy WebSocket. That means the per-connection number
**includes the framework's overhead**:

- a Phoenix **transport process** + a **channel process** per connection
  (two BEAM processes, each with its own heap), and
- a **Presence** CRDT entry per connection, replicated over PubSub.

This is deliberate and is called out so critics cannot dismiss the
comparison as cherry-picking. Kāra's bare WS holds neither a channel
abstraction nor a presence layer — it is a lower-level primitive. To keep
the framework cost *quantified* rather than hidden, every scale is run
twice:

- **presence ON** (`PRESENCE` unset) — the headline, real-world config.
- **presence OFF** (`PRESENCE=off`) — the sidebar; subtracting it from
  the headline isolates what Presence alone costs per connection.

## The "matches/beats Kāra → combination claim" framing (pre-written)

If Phoenix's presence-on density comes in **at or below** Kāra's, do not
retreat to "but it has framework overhead" — that is the apples-to-apples
caveat already conceded above. Instead lead with the **combination**:

> Phoenix reaches this density on a garbage-collected VM with per-process
> heaps and no ahead-of-time native codegen — you buy density with an
> interpreter/JIT runtime and a GC pause budget. Kāra reaches comparable
> density while compiling to a **native AOT binary** with **static
> ownership + effect checking** and **no GC**. The claim was never
> "denser than BEAM in isolation" — it is "BEAM-class density *and*
> native performance *and* static safety, in one language." Phoenix
> matching Kāra on the first axis leaves the other two standing.

If Phoenix comes in **above** Kāra (the expected ~10–20 KB/conn with
presence vs Kāra's ~12 KB), the density headline stands on its own and
this framing is the backstop, not the lede.

## Design choices

- **Phoenix Channels, not raw Cowboy.** A `UserSocket` mounts the
  `room:*` topic to `BenchChannel`; the bench client joins `room:bench`.
  This is what exercises the channel + presence machinery a real Phoenix
  app runs.
- **Presence on by default, toggleable.** `BenchChannel.join/3` calls
  `Presence.track/3` (via an `after_join` message) unless `PRESENCE=off`.
  The toggle is read at runtime, so one compiled artifact serves both
  runs.
- **TLS 1.2 + 1.3, single self-signed cert, no client auth.** Same
  posture as the bare-WS comparators. The cert/key are the shared
  `tests/fixtures/tls/{cert,key}.pem` fixture (CN=localhost), copied into
  `priv/` so this directory is self-contained when scp'd to a rig
  (Phoenix resolves `certfile: "priv/cert.pem"` relative to the project
  root, which `run_server.sh` `cd`s into).
- **Cowboy transport (via the Cowboy2 adapter).** See "Transport choice".
- **Ephemeral port → `BOUND_PORT`.** The endpoint binds `127.0.0.1:0`;
  `Bench.Application.start/2` reads the real port back with
  `:ranch.get_port(Bench.Endpoint.HTTPS)` and prints `BOUND_PORT=<n>` on
  stdout, satisfying the harness's `--server-bin` contract.
- **`run_server.sh` `exec`s the BEAM.** Because `exec` replaces the shell
  with `beam.smp` in place, the PID the harness spawned *is* the BEAM
  PID, so `ps -o rss=` measures the VM node directly — no wrapper/child
  indirection.
- **`+Q 2000000 +P 8000000`.** The BEAM caps ports (one per TCP conn) at
  65536 and processes at ~262K by default — both below 250K-conn scale.
  `run_server.sh` raises them.

### Transport choice (Cowboy vs Bandit)

Phoenix 1.7's `mix phx.new` defaults to **Bandit**, but this comparator
uses **Cowboy** (`plug_cowboy` + `Phoenix.Endpoint.Cowboy2Adapter`) for
two reasons: (1) Cowboy/ranch exposes `:ranch.get_port/1`, giving the
bound ephemeral port for the `BOUND_PORT` contract with no scraping; and
(2) Cowboy is the long-standing Channels transport that the high-scale
deployments ran. It remains a fully real-world, widely-deployed choice.
If a Bandit delta is ever wanted, it's a one-line adapter swap — noted
here so the transport is an explicit, defensible decision rather than an
accident.

### Bench accommodations (density-neutral)

- **`websocket: [timeout: :infinity]`.** A real Phoenix client sends a
  heartbeat every ~30s; the server closes a socket that misses one within
  `timeout` (default 60s). Rather than have the harness drive 250K
  heartbeat timers through the idle-hold + RSS-settle window, the idle
  timeout is disabled. This is purely a liveness setting — it changes no
  per-connection allocation — so the density number is unaffected.

## Usage

```bash
# One-time, inside this dir:
mix deps.get
mix compile            # MUST precede the bench run — see note below

# Bench it through the shared harness (presence ON, the headline):
cd ../bench
BENCH_EXTRA_ARGS="--ws-path /socket/websocket?vsn=2.0.0 --phx-join room:bench" \
  cargo run --release -- \
    --server-bin ../phoenix/run_server.sh \
    -n 200 --concurrency 64 --churn-rounds 0 --hold-secs 3

# Presence OFF (sidebar): prefix the same command with PRESENCE=off.
```

> **Pre-compile first.** The harness waits 15s for `BOUND_PORT`; a cold
> `mix compile` inside that window would time out. `run_server.sh` runs
> `mix run --no-halt` (no compile), so `mix compile` must have run first.

## At-scale results

**NOT YET RUN.** This comparator is prepped + locally validated; the
50K / 250K / 250K-no-presence rig runs await a user-provisioned box (per
the bench-day rig-spend sign-off discipline). Expected ~10–20 KB/conn
with presence. Results table lands here after the rig run, mirroring the
Go comparator's format.

Scope for #67: **idle density only** — 50K (linearity) + 250K (headline)
+ 250K with `PRESENCE=off` (framework-overhead sidebar). Active-traffic
echo is out of scope here (the harness's echo path speaks raw WS frames,
not Phoenix's channel-event wire format).

### Reproduce — turnkey rig recipe

```bash
# 0. Kernel + nofile + loopback-alias setup (idempotent; needs a fresh
#    login afterward so the systemd nofile cap actually lifts). The BEAM
#    inherits the 3M nofile hard cap for its own listen-side fds.
examples/ws_idle_holder/bench/scripts/ec2_setup.sh
# re-login here

# 1. Toolchains. ec2_setup.sh does NOT install compilers. Phoenix needs
#    Erlang/OTP + Elixir; the harness needs cargo. (No karac/runtime
#    build — this comparator is self-contained.)
sudo apt-get update && sudo apt-get install -y erlang elixir   # or asdf/kerl for a pinned OTP
mix local.hex --force && mix local.rebar --force
# rustup for the harness if absent: curl ... | sh

# 2. Build the comparator deps + the bench harness on-box:
( cd examples/ws_idle_holder/phoenix && mix deps.get && mix compile )
( cd examples/ws_idle_holder/bench && cargo build --release )

# 3. Run all three (JSON tee'd to ./<basename>-{250k,50k}.json):
cd examples/ws_idle_holder/bench/scripts
PHX="$(cd ../../phoenix && pwd)/run_server.sh"
PHX_ARGS="--ws-path /socket/websocket?vsn=2.0.0 --phx-join room:bench"

# Headline (presence ON): 250K + 50K linearity
BENCH_EXTRA_ARGS="$PHX_ARGS" ./run_250k.sh "$PHX" phoenix_idle_250k.json
BENCH_EXTRA_ARGS="$PHX_ARGS" ./run_50k.sh  "$PHX" phoenix_idle_50k.json
# Sidebar (presence OFF): 250K
PRESENCE=off BENCH_EXTRA_ARGS="$PHX_ARGS" ./run_250k.sh "$PHX" phoenix_idle_250k_nopresence.json

# 4. scp the JSONs off-box to docs/investigations/ and `git add` them
#    BEFORE terminating the instance (scp != tracked).
```

> **Linearity gate:** if 50K→250K `per_conn_bytes` drift exceeds 5%,
> escalate to a 1M run (`run_1m.sh`). Phoenix is the comparator most
> likely to trip this — BEAM grows its allocator carriers in large chunks
> and pre-allocates heap, so small-N per-conn is inflated by fixed costs
> that amortize only at scale (the local N=200 smoke saw ~290 KiB/conn,
> pure slab-prealloc noise — do **not** read small-N as density).

## Local validation (macOS, Elixir 1.19.5 / OTP 29)

- `mix deps.get` + `mix compile` clean (the only warnings are from the
  Phoenix dep itself under Elixir 1.19's new type-checker).
- `run_server.sh` boots, prints `BOUND_PORT`, applies `+Q/+P`.
- Harness via `--server-bin`, N=200: **200/200 established, 0 failed** in
  both presence modes (every `phx_join` was acked — the bench counts a
  conn established only after the `ok` `phx_reply`).
- **Presence tracking confirmed:** with 200 connections held,
  `Bench.Presence.list("room:bench")` reported **200** entries via RPC —
  `track` populated the CRDT for every joined connection, not just acked
  the join.
- Presence-on RSS delta > presence-off delta at N=200, confirming the
  toggle has measurable cost (the real magnitude needs scale).

## What this impl deliberately omits

- **No active-traffic echo** through this comparator (out of #67 scope;
  the harness echo path is raw-WS, not channel-event framed).
- **No clustering / distributed Presence** — single node, which is the
  per-box density question. (Multi-node Presence would *add* CRDT
  replication cost, not reduce per-conn memory.)
- **No auth / no per-socket identity** (`id/1` returns nil).
- **No DB / Ecto / LiveView** — just the Channels + Presence surface the
  density measurement needs.
