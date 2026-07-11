# Parallax bench — Phoenix/Elixir reference impl

Phoenix 1.8 + Bandit + `Task.async`/`Task.await` fan-out, matching the
wire shape of the other Parallax comparators (`../kara/`, `../rust/`,
`../go/`, `../node/`).

Phoenix is the natural foil for Kāra's auto-parallelization claim:
the BEAM gives you concurrency, but the developer writes the fan-out
wiring by hand — four `Task.async` calls, four `Task.await` joins —
where Kāra infers the same fan-out from the four disjoint `reads(R_i)`
effect declarations.

## Toolchain prerequisites

| Tool          | Version tested            |
|---------------|---------------------------|
| Erlang/OTP    | 29.0.1 (JIT, 18 schedulers) |
| Elixir        | 1.19.5                    |
| Phoenix       | 1.8.7                     |
| Bandit (HTTP) | 1.11.1                    |
| wrk           | 4.2.0                     |

### macOS (Homebrew)

```sh
brew install elixir            # pulls in erlang as a dep
mix local.hex --force
mix local.rebar --force
mix archive.install hex phx_new --force   # Phoenix project generator
```

### Linux

Use [asdf](https://asdf-vm.com/) or your distro's package manager;
Elixir 1.15+ and Erlang/OTP 26+ are required by Phoenix 1.8. After
installing the toolchain, the same three `mix` commands above set up
hex + rebar + the phx_new generator.

## Build + run

```sh
cd examples/parallax/bench/phoenix
mix deps.get
MIX_ENV=prod mix compile         # one-time warm-up — bin/server uses
                                 # incremental compile, but pre-compiling
                                 # makes cold-start measurements honest.
./bin/server                     # prints `BOUND_PORT=<n>` on stdout,
                                 # serves `GET /dashboard/:user_id` on
                                 # 127.0.0.1:<n>. Bind on 0 by default,
                                 # OS picks a free port — same convention
                                 # as the other comparators. Set PORT to
                                 # pin to a specific number.
```

The server stays foregrounded; send `SIGINT` (Ctrl-C) or `kill` the
process to stop it. Subsequent runs reuse the existing `_build/`
cache; delete `_build/` and `deps/` for a fully fresh state.

## What it listens on

| Param          | Value                                  |
|----------------|----------------------------------------|
| Path           | `GET /dashboard/:user_id` (JSON body)  |
| Bind address   | `127.0.0.1`                            |
| Port           | OS-picked (`PORT=0`); printed as `BOUND_PORT=<n>` on stdout |
| Response       | `application/json`; same nested shape as `rust/`, `go/`, `node/` |
| Compile profile| `MIX_ENV=prod` (no code reloader, prod logger level) |

The `BOUND_PORT=<n>` stdout line mirrors the convention every other
comparator follows so the existing `bench.sh::launch_and_get_port`
helper picks it up unchanged — only `bench.sh`'s impl table would
need a fifth row to drive Phoenix automatically. See "Bench harness
integration" below.

## What the busy_loop measures

Same hash-mix kernel as the other comparators
(`x = (x*31 + i) mod 1_073_741_789`), same iteration counts (700 K /
4 M / 1.7 M / 2.7 M for profile / orders / notifs / recommendations),
same fetch-result-folding pattern that prevents BEAM-JIT DCE from
eliding the work. Each of the four `fetch_*` functions returns its
busy_loop result; `get_dashboard/1` folds three of them into the
JSON body (`order_id`, `notif_kind`, `item_id`), so DCE can't kill
their corresponding spawns. The fourth (`fetch_profile_name`) returns
`"Alice"` literal — same 3-of-4 observable-fold pattern as
`kara/server.kara` and the other impls. See `lib/parallax_bench/providers.ex`.

## How the fan-out works

```elixir
def get_dashboard(user_id) do
  profile_task   = Task.async(fn -> fetch_profile_name(user_id) end)
  order_task     = Task.async(fn -> fetch_latest_order_id(user_id) end)
  notif_task     = Task.async(fn -> fetch_top_notification_kind(user_id) end)
  recommend_task = Task.async(fn -> fetch_top_recommendation_id(user_id) end)

  %{
    profile:            %{user_id: user_id, name: Task.await(profile_task, 15_000)},
    latest_order:       %{order_id: Task.await(order_task, 15_000)},
    top_notification:   %{kind: Task.await(notif_task, 15_000)},
    top_recommendation: %{item_id: Task.await(recommend_task, 15_000)}
  }
end
```

Each `Task.async/1` spawns a BEAM process (~338 bytes per spawn) on
one of the schedulers; `Task.await/2` joins it back. The BEAM
scheduler distributes the four processes across its 18 schedulers
(matching the M5 Pro's 18 logical CPUs by default). Contrast with
`kara/server.kara`'s `get_dashboard`, where the four `let` bindings
look like sequential code and the compiler infers the fan-out from
the disjoint `reads(R_i)` effects on each `fetch_*` function — no
`Task.async` boilerplate at the call site.

## Deviations from the other comparators

1. **Bandit, not Cowboy.** Phoenix 1.8's default HTTP adapter is
   Bandit (pure Elixir, no Erlang Cowboy/ranch). This is the
   out-of-the-box choice a modern Elixir shop would ship; Cowboy is
   reachable via one config line but isn't the Phoenix default
   anymore. No tuning knobs are pre-set — same F4 fairness rule as
   the other impls.

2. **No static-asset / session / method-override plugs in the
   endpoint pipeline.** `mix phx.new`'s default pipeline includes
   `Plug.Static`, `Plug.Session`, `Plug.MethodOverride`, `Plug.Head`,
   `Phoenix.CodeReloader`. The Parallax demo is a pure JSON API with
   no cookies, no static files, no HTML form-method overrides, and
   the bench compiles under `MIX_ENV=prod` so the reloader is dead
   weight. Kept the canonical `Plug.RequestId`, `Plug.Telemetry`,
   `Plug.Parsers` (incl. JSON decoder) since every real Phoenix API
   ships with those. See `lib/parallax_bench_web/endpoint.ex` for the
   exact set. Net: measures what an Elixir-shop's idiomatic Phoenix
   API server pays per request, minus the obviously-irrelevant
   static-file plumbing.

3. **15-second `Task.await` timeout.** Default is 5 s; bumped to 15 s
   so a per-branch saturation tail doesn't kill the request under
   `wrk -c1000+` load. Matches the "don't kill the request on slow
   fan-out" behavior of `tokio::join!` and Go's `WaitGroup.Wait()`.

4. **Sleep substitute (F5 deviation).** Same busy-loop substitute
   as the other impls; see `../README.md` § "Sleep substitute" for
   the rationale.

## Bench numbers — first run

Measured 2026-05-30, Apple M5 Pro (10P + 8E, 18 logical CPUs), 64 GB
RAM, macOS 26.4.1, Elixir 1.19.5 + Erlang/OTP 29 JIT, Phoenix 1.8.7,
Bandit 1.11.1, wrk 4.2.0. Same invocation shape as `bench.sh`:
cold-start = `wrk -t1 -c1 -d1s --latency`; steady-state per
connection-count = three rounds of `wrk -t4 -c<conn> -d10s --latency`,
median req/s with [min..max] across the three rounds. Full raw output
captured in `bench-results.json`.

### Cold start (first ~1 s after spawn)

| Impl    | req/s | p50 ms | p75 ms | p90 ms | p99 ms | max ms |
|---------|-------|--------|--------|--------|--------|--------|
| Phoenix | 44    | 22.0   | 22.1   | 22.3   | 40.9   | 40.9   |

For context (from `../README.md`'s 2026-05-10 numbers): Kāra
cold-start was 86 req/s / 17.4 ms p99; Rust 85 / 18.2; Go 80 / 19.7;
Node 5 / 184. Phoenix sits roughly halfway between the three
multi-core impls and Node — about 2× the per-request latency floor of
Kāra/Rust/Go (the BEAM pays one process-spawn per fan-out branch,
where tokio + goroutine + Kāra's `karac_par_run` all use long-lived
worker pools).

### Steady-state

| Impl    | -c   | req/s (med [min..max]) | p50 ms | p75 ms | p90 ms | p99 ms | max ms |
|---------|------|------------------------|--------|--------|--------|--------|--------|
| Phoenix | 100  | 274 [259..294]         | 353    | 423    | 479    | 588    | 723    |
| Phoenix | 1000 | 198 [165..230]         | 1170*  | 1210*  | 1260*  | 1300*  | 1300*  |
| Phoenix | 5000 | 0 [0..3]               | —      | —      | —      | —      | —      |

*c1000 latencies from the one round that produced a full distribution
(two of three rounds saw enough wrk-side timeouts that wrk suppressed
the distribution table; req/s was still captured).

c5000: **highly variable** on this single-box macOS setup — some rounds
settle around ~180–230 req/s, others complete near-zero within the 10 s
window (the 2026-05-30 first run above happened to catch the near-zero
end). **This is not an acceptor-pool limit** (see the resolved follow-up
below): re-measured 2026-07-11, `wrk` reports **zero `connect` errors**
at -c5000, so Bandit accepts all 5000 connections; the failures are
downstream `read`/`timeout`. The cause is BEAM scheduler + CPU saturation
from the four-process busy-loop fan-out at 5000-way concurrency,
compounded by macOS loopback socket-state (TIME_WAIT / ephemeral-port
pressure with `wrk` co-resident on the same box). It is the same
CPU-bound wall the native impls hit at -c5000 (Go collapses to ~86 in the
v8 canonical Graviton run), not a Phoenix misconfiguration.

### Headline numbers vs the other comparators (`-c100`)

For context, the existing comparators' 2026-05-10 -c100 numbers:

| Impl    | req/s (med) | p50 ms | p99 ms |
|---------|-------------|--------|--------|
| Kāra    | 720         | 134    | 300    |
| Rust    | 718         | 120    | 803    |
| Go      | 667         | 143    | 449    |
| Phoenix | 274         | 353    | 588    |
| Node    | 6           | 1140   | 1960   |

Phoenix is **2.6× lower than Kāra in throughput** at -c100 and **~2×
in p50 latency** — the four-process-per-request fan-out cost is real.
p99 (588 ms) sits between Go and Rust, which is interesting on its
own — Phoenix's per-request tail is *better* than Rust's
`spawn_blocking` tail under saturation, the BEAM's preemptive
scheduler smooths bursts in a way tokio's blocking-pool doesn't.

(Re-running every comparator in the same session would be needed for
the table to be truly apples-to-apples — the existing numbers are
from 2026-05-10 and the user may want a fresh sweep before the
commercial framing lands. Tracked as a follow-up.)

## Bench harness integration

`bench.sh` builds + runs Phoenix as the fifth impl alongside kara /
rust / go / node. The 2026-05-30 numbers above were captured by
running `wrk` directly against `./bin/server` (before the harness
wiring landed), matching `bench.sh`'s invocation shape exactly so the
numbers are comparable to the existing table. From 2026-05-30 onward,
`bench.sh --impls=k,r,g,n,p` (the new default) sweeps all five impls
in one session — see the `prepare_phoenix` runner and `PHOENIX_CMD_HOLDER`
wiring in `bench.sh`. The `tests/parallax_bench.rs::test_bench_script_dry_run`
CI smoke also checks Phoenix is present in the dry-run table.

## Project layout

```
phoenix/
├── README.md                       # this file
├── bench-results.json              # raw bench numbers (2026-05-30)
├── bin/server                      # convenience launcher; prints BOUND_PORT=<n>
├── config/                         # generated by `mix phx.new`, unmodified
├── lib/
│   ├── parallax_bench.ex
│   ├── parallax_bench/
│   │   ├── application.ex          # supervision tree (added BoundPortReporter)
│   │   ├── bound_port_reporter.ex  # prints `BOUND_PORT=<n>` after Bandit binds
│   │   └── providers.ex            # busy_loop + four fetch_* + get_dashboard
│   ├── parallax_bench_web.ex
│   └── parallax_bench_web/
│       ├── controllers/
│       │   ├── dashboard_controller.ex  # GET /dashboard/:user_id
│       │   └── error_json.ex            # 404/500 JSON; generator default
│       ├── endpoint.ex             # pared plug pipeline (see "Deviations")
│       ├── router.ex               # one route — /dashboard/:user_id
│       └── telemetry.ex            # generator default; logs nothing custom
├── mix.exs                         # mix.exs generated by `mix phx.new`
├── mix.lock
└── test/                           # generator default; not the bench surface
```

## Follow-ups surfaced

- **Re-run all four other comparators in the same session** before
  the Phoenix-vs-Kāra commercial framing ships; the existing table
  is from 2026-05-10 and three weeks of compiler work may have moved
  the Kāra numbers.
- **Bandit acceptor pool tuning at -c5000 — RESOLVED 2026-07-11 (no
  config change needed).** The original hypothesis (default `num_acceptors`
  too small, ~10) does not hold on the current deps: ThousandIsland 1.4.3
  (via Bandit 1.11.1) defaults `num_acceptors` to **100**, with
  `num_connections: 16_384` per acceptor — ample headroom for 5000
  connections. Measured directly: `wrk` reports **zero `connect` errors**
  at -c5000 regardless of acceptor count, and sweeping `num_acceptors`
  across 100 / 400 / 800 (via the Endpoint's `:thousand_island_options`)
  leaves both the -c5000 result and the stable -c100 (~335–340 req/s)
  unchanged. The -c5000 ceiling is downstream BEAM/CPU saturation + macOS
  loopback socket-state, **not** the acceptor pool, so the default config
  is left unchanged (a clean acceptor-count A/B is anyway confounded by
  loopback TIME_WAIT accumulation across back-to-back high-conn runs; the
  zero-`connect`-error invariant is what rules the pool out). Phase-6
  line 47.
- **Cowboy adapter cross-check.** Phoenix lets you swap Bandit for
  Cowboy with one config line; would be informative to capture both
  numbers, since Cowboy is the historical default and some Elixir
  shops still ship it.
