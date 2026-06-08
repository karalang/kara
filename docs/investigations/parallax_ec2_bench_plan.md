# Parallax EC2 bench plan (2026-06-07)

Execution plan for producing launch-grade Parallax throughput numbers
now that the cohort is **six impls** (kara, rust, go, node, phoenix,
java — the Java/Netty comparator shipped `cbf1579d`, phase-6 P1).

Tracks phase-6 entries: *P2 — Parallax x86 confirmation run* and
*P2 — Parallax Graviton confirmation run* (new). Bench source:
[`examples/parallax/bench/`](../../examples/parallax/bench/).

---

## Framing (read first — it governs how every number is reported)

The Parallax bench measures **throughput** (req/s, p99), but the
auto-par value is **ergonomic**, not raw speed. The two are different
axes and must not be conflated in the writeup:

- **Headline = "same performance class, a fraction of the concurrency
  code."** The proof of auto-par is the *source comparison* (Kāra: four
  plain `let` bindings vs Java `CompletableFuture.allOf` / Rust
  `tokio::join!` / Go goroutines+WaitGroup). The throughput table is a
  **defensive "no perf tax" backstop** to that — "...and it costs you
  nothing in throughput to get it."
- **Do NOT headline "Kāra beats Java/Rust on req/s."** A JIT-warmed JVM
  and raw Rust are mature; Kāra being *competitive* is the win, not
  Kāra being *fastest*. Claiming a throughput victory invites a tuned
  rebuttal and loses the real (ergonomic) argument.
- **Within-Kāra control is the cleanest auto-par demonstration.** Build
  a second Kāra binary with `KARAC_AUTO_PAR=0` and bench it alongside
  the default. The auto-par-on vs auto-par-off delta on the *same
  binary, same machine* shows the compiler's contribution directly and
  is same-lane by construction (see "Lane discipline" below).

Known workload caveat (already footnoted in the bench README): the
providers are **CPU-bound busy loops**, not I/O. The auto-par story is
really about independent *I/O* (4 DB/API calls); CPU work turns the
bench into a thread-pool-scheduling contest, which understates the
ergonomic message. The busy-loop is a stand-in only because Kāra's
stdlib has no `sleep_ms` in v1 (Phase 11). With real I/O the story is
stronger, not weaker — note this when presenting.

---

## The three runs

| # | Where | ISA | Impls | Purpose | Feeds README? |
|---|-------|-----|-------|---------|---------------|
| 0 | Local Mac (M5 Pro, 18-core) | arm64 | all 6 | Correctness gate + relative-ordering sanity. Confirms every impl builds, serves correct JSON, and produces non-zero numbers before paying for EC2. | **No** (laptop, not reproducible) |
| 1 | EC2 `c7i.4xlarge` (16 vCPU Intel) | x86-64 | all 6 (+ Kāra `AUTO_PAR=0` control) | Canonical comparison numbers — the launch artifact. | **Yes** — primary tables |
| 2 | EC2 `c7g.4xlarge` (16 vCPU Graviton3) | arm64 | Kāra only (+ `AUTO_PAR=0` control) | Second-ISA data point for Kāra → supports an "ISA-invariant" framing mirroring `ws_idle_holder`. Comparators run x86-only; no need to reinstall 6 toolchains on ARM just to re-confirm relative ordering the x86 box already establishes. | **Yes** — Kāra row + note |

Why x86 is the *primary* and Graviton is *Kāra-only*: the dev/test box
is already ARM (M5 Pro), so x86 is the missing ISA — the higher-info
confirmation and the deployment most readers assume. Graviton adds the
second Kāra data point cheaply (mirrors the dual-ISA story
`ws_idle_holder` used to claim ISA-invariance). Running the full
comparator cohort on *both* ISAs is possible but redundant for v1.

---

## Decisions to confirm / redline

| Decision | Proposed default | Notes |
|---|---|---|
| Region | `us-east-1` | Cheapest, widest AMI coverage. |
| x86 instance | `c7i.4xlarge` (16 vCPU, Sapphire Rapids) | Matches the existing tracker entry. |
| Graviton instance | `c7g.4xlarge` (16 vCPU, Graviton3) | Core-count-matched sibling so fan-out width is comparable. |
| OS / AMI | Ubuntu 24.04 LTS | Resolve AMI via the Canonical SSM public parameter, don't hardcode. Same family as the `ws_idle_holder` rig. |
| Disk | 60 GB gp3 | LLVM-18 + 6 toolchains + cargo target tree are bulky; karac `--features llvm` build alone wants a few GB. |
| Access | Ephemeral keypair + SG locked to my egress IP | No `.pem` present locally. SSM Session Manager is the keyless alternative if preferred. |
| Pricing | On-demand | Spot risks mid-build eviction; total cost is tiny (below) so on-demand is simpler. |
| Teardown | Terminate both immediately after results pulled | Hard checklist item — see Teardown. |

**Cost estimate (us-east-1 on-demand):** `c7i.4xlarge` ≈ $0.714/hr,
`c7g.4xlarge` ≈ $0.58/hr. Dominant time cost is building karac from
source (LLVM link) + toolchain installs (~30–45 min each), then the
bench (~12 min full / ~3 min Kāra-only). Budget ~2 hr/box → **under
$10 total.** No standing infra; both terminated same session.

---

## Per-run procedure

### Run 0 — Local Mac (do first, free)

Self-bootstrapping via `bench.sh` (it builds karac + runtime + every
impl). All six toolchains confirmed present locally
(`cargo`/`go`/`node`/`java`-via-Homebrew-JDK/`mvn`/`wrk`); Java needs
`JAVA_HOME` exported — see [`reference_local_jdk_via_maven`] memory.

```sh
export JAVA_HOME=/opt/homebrew/Cellar/openjdk/26.0.1/libexec/openjdk.jdk/Contents/Home
export PATH="$JAVA_HOME/bin:$PATH"
# correctness + first ordering read — 1 round is enough to gate
sh examples/parallax/bench/bench.sh --runs=1 | tee /tmp/parallax_mac_$(date +%s).log
```

Pass criteria: every impl prints real req/s (no `SKIP`/`BIND_FAIL`/
`WRK_MISSING`), JSON bodies correct (the kara smoke test already
asserts this). Numbers are **not** for the README — laptop, ARM,
18-core, thermals. Relative ordering only.

### Run 1 — x86 EC2 (canonical)

1. **Provision**: resolve Ubuntu 24.04 amd64 AMI via SSM param; launch
   `c7i.4xlarge`, 60 GB gp3, keypair + SG (SSH from my IP).
2. **Toolchains** (the heavy step): `apt update`; install build-essential,
   `clang`, **`llvm-18 llvm-18-dev libpolly-18-dev`** (inkwell/llvm-sys
   needs `llvm-config-18` on PATH — export `LLVM_SYS_181_PREFIX` if the
   versioned config isn't auto-found), Rust via rustup, `golang`,
   Node (NodeSource or nvm), Elixir+OTP (for phoenix), a JDK 11+ +
   `maven` (for java), and `wrk` (`apt install wrk`, universe).
3. **Source**: `git clone https://github.com/karalang/kara` (repo is
   public — verified HTTP 200). Or rsync the local worktree if there
   are unpushed commits the bench depends on (check `git log
   origin/main..main` before relying on clone).
4. **Light sysctl for -c5000 on localhost** (NOT the ws_idle_holder
   2M-tuple setup — this bench is throughput, not idle density):
   `net.core.somaxconn=65535`, `net.ipv4.tcp_max_syn_backlog=65535`,
   raise `nofile` ulimit to ~1M. `-c5000` on loopback needs the listen
   queue + fd headroom, nothing more.
5. **Run**:
   ```sh
   sh examples/parallax/bench/bench.sh | tee parallax_x86_$(date +%s).log
   ```
   Plus the auto-par control: build a Kāra binary with
   `KARAC_AUTO_PAR=0` and bench it as a manual extra lane (bench.sh has
   no built-in off-lane; build the binary, run wrk against it with the
   same `-t4 -c100/1000/5000` shape, record alongside).
6. **Pull** the log(s) back to the repo host (`scp`).
7. **Terminate.**

### Run 2 — Graviton EC2 (Kāra second-ISA point)

Same as Run 1 but: `c7g.4xlarge` + arm64 AMI; install **only** the
Kāra toolchain (Rust + LLVM-18 + clang) + `wrk` (skip go/node/
elixir/java); run `bench.sh --impls=k` (+ the `AUTO_PAR=0` control
lane). Pull log, terminate.

---

## Toolchain matrix

| Tool | Mac (run 0) | x86 (run 1) | Graviton (run 2) |
|---|---|---|---|
| Rust/cargo + **LLVM 18** (karac) | ✓ | ✓ | ✓ |
| wrk | ✓ | ✓ | ✓ |
| go | ✓ | ✓ | — |
| node | ✓ | ✓ | — |
| elixir/OTP (phoenix) | ✓ | ✓ | — |
| JDK 11+ / maven (java) | ✓ (via Homebrew JDK) | ✓ | — |

The **LLVM 18** dependency is the trickiest install — inkwell links
against it at karac build time. Ubuntu 24.04 ships it in apt
(`llvm-18`, `llvm-18-dev`). `bench.sh` then self-builds karac with
`--features llvm`.

---

## Lane discipline (per `feedback_bench_lane_discipline`)

The usual rule — never headline auto-par Kāra against single-threaded
comparators — is **satisfied by construction here**: every Parallax
comparator is an explicit *parallel* fan-out (`tokio::join!`,
goroutines, `Promise.all`, `Task.async`, `CompletableFuture.allOf`), so
the cross-impl comparison is multi-core-vs-multi-core, same lane. The
`KARAC_AUTO_PAR=0` control lane is the *within-Kāra* same-lane
demonstration of the compiler's contribution. No cross-lane hazard as
long as the writeup doesn't compare Kāra-auto-par to a hypothetical
single-threaded baseline.

---

## Results capture

Parallax has **no `results.json` → consolidate → graph pipeline** like
the kata benches do (`feedback_bench_json_pipeline_canonical` is about
katas, not this bench). `bench.sh` prints a stdout table only. So:

- Capture each run's stdout to a timestamped log (`tee`).
- Transcribe the **x86** table into the README's existing throughput
  tables (cold-start + steady-state), adding the `java` rows and the
  `kara (auto-par off)` control lane.
- Add a short **Graviton** note (Kāra x86 vs Kāra arm64) supporting the
  ISA-invariance framing.
- Update the README's `_v7 (... five impls)_` provenance line to a v8
  six-impl-on-x86 entry; keep the historical v7 line.
- Per `feedback_bench_kata_rerun_regression_check` / `kata_readme_sweep`:
  diff old→new for every metric that changed and classify
  (noise/load/improvement/regression) in the writeup, even though this
  is an impl *addition* not a karac change.

---

## Teardown checklist (do not skip)

- [ ] `scp` both run logs back before terminating anything.
- [ ] `aws ec2 terminate-instances` for both instance IDs.
- [ ] Delete the ephemeral keypair (`aws ec2 delete-key-pair`) + the
      temporary security group.
- [ ] `aws ec2 describe-instances` confirm both `terminated`.
- [ ] Confirm no leftover EBS volumes / EIPs.

---

## Risks / watch-items

- **karac LLVM-18 build on fresh Ubuntu** is the most failure-prone
  step (llvm-sys version detection). Budget time; it's the same build
  the `ws_idle_holder` rig did, so it's known-good on Ubuntu 24.04.
- **Repo freshness**: if `main` has unpushed local commits the bench
  needs, `git clone` gets a stale tree. Check `git log
  origin/main..main` first; rsync the worktree if non-empty.
- **wrk on the same box** as the server splits the 16 vCPU between load
  generator and server (matches the existing single-machine bench
  design — F4 fairness control — so it's consistent across impls, just
  note absolute numbers are single-box).
- **Cost leak**: forgetting teardown. The checklist above is the guard.
- **Don't fabricate Java rows** into the README from the Mac run — only
  x86 numbers are canonical.
