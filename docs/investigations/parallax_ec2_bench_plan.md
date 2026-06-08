# Parallax EC2 bench plan (2026-06-07)

Execution plan for producing launch-grade Parallax throughput numbers
now that the cohort includes the Java/Netty comparator (shipped
`cbf1579d`, phase-6 P1).

Tracks phase-6 entries: *P2 — Parallax Graviton confirmation run*
(canonical/headline) and *P2 — Parallax x86 confirmation run*
(cross-ISA). Bench source:
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

| # | Where | ISA | Cohort | Purpose | Feeds README? |
|---|-------|-----|--------|---------|---------------|
| 0 | Local Mac (M5 Pro, 18-core) | arm64 | **all 6** (k,r,g,n,p,j) | Correctness gate + free full-picture directional read. Confirms every impl builds, serves correct JSON, produces non-zero numbers before paying for EC2. Keeps Node/Phoenix/Go data alive (they're dropped from the paid runs). | **No** (laptop — not reproducible/citable; supplementary at most) |
| 1 | EC2 `c7g.4xlarge` (16 vCPU Graviton3) | arm64 | **K/R/J/G** (+ Kāra `AUTO_PAR=0` control) | **Canonical / headline** comparison numbers — the launch artifact. | **Yes** — primary tables |
| 2 | EC2 `c7i.4xlarge` (16 vCPU Intel) | x86-64 | **K/R/J/G** (+ Kāra `AUTO_PAR=0` control) | **Cross-ISA confirmation** — same 4-way cohort so the Kāra-vs-Rust/Java/Go *ratio* is shown ISA-invariant, not just Kāra's number. | **Yes** — confirmation table |

**Cohort = Kāra + Rust + Java + Go (`--impls=k,r,j,g`) on both paid
boxes.** Rust = perf ceiling / credibility; Java = enterprise-JVM
commercial foil; Go = concurrency-first language baseline (and a trivial
single-binary toolchain install). **Node and Phoenix are dropped from
the paid runs** — Node is a predictable loser on CPU-bound fan-out
(single event loop) and Phoenix carries the slowest runtime *and* the
fragile Elixir/OTP install *and* the untuned Bandit `-c5000` asterisk
(its own tracked tuning item). Both stay in the free Mac run so no data
is silently lost.

**Why Graviton is the *primary* and x86 is the *confirmation*:** ARM is
the server-compute growth curve (Graviton's price/performance is exactly
what the "saves money" commercial framing points at), and leading on
Graviton makes one coherent "we benchmark on modern ARM server
instances" story across both flagship demos (`ws_idle_holder` also led
arm64). aarch64 codegen for JVM/Go/Rust/LLVM is fully mature, so the
comparators are *not* handicapped on Graviton. x86 then proves the
result isn't ARM-cherry-picked — and because both paid boxes run the
*same* 4-way cohort, the cross-ISA claim covers the whole ratio, not
just Kāra.

---

## Decisions to confirm / redline

| Decision | Proposed default | Notes |
|---|---|---|
| Region | `us-east-1` | Cheapest, widest AMI coverage. |
| Primary instance (Graviton) | `c7g.4xlarge` (16 vCPU, Graviton3) | Headline / canonical box. |
| Confirmation instance (x86) | `c7i.4xlarge` (16 vCPU, Sapphire Rapids) | Core-count-matched so fan-out width is comparable. |
| OS / AMI | Ubuntu 24.04 LTS | Resolve AMI via the Canonical SSM public parameter, don't hardcode. Same family as the `ws_idle_holder` rig. |
| Disk | 60 GB gp3 | LLVM-18 + toolchains + cargo target tree are bulky; karac `--features llvm` build alone wants a few GB. |
| Access | Ephemeral keypair + SG locked to my egress IP | No `.pem` present locally. SSM Session Manager is the keyless alternative if preferred. |
| Pricing | On-demand | Spot risks mid-build eviction; total cost is tiny (below) so on-demand is simpler. |
| Teardown | Terminate both immediately after results pulled | Hard checklist item — see Teardown. |

**Cost estimate (us-east-1 on-demand):** `c7g.4xlarge` ≈ $0.58/hr,
`c7i.4xlarge` ≈ $0.714/hr. Dominant time cost is building karac from
source (LLVM link) + the (now lighter) toolchain install — only
rust/llvm + go + JDK/maven + wrk per box, **no Elixir/OTP, no Node**.
Bench itself is ~8–10 min (4-way). Budget ~1.5 hr/box → **well under
$10 total.** This whole bench is featherweight next to the
`ws_idle_holder` 1M-idle-connection runs. No standing infra; both
terminated same session.

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
# all 6 impls, correctness + first ordering read — 1 round is enough to gate
sh examples/parallax/bench/bench.sh --runs=1 | tee /tmp/parallax_mac_$(date +%s).log
```

Pass criteria: every impl prints real req/s (no `SKIP`/`BIND_FAIL`/
`WRK_MISSING`), JSON bodies correct (the kara smoke test already
asserts this). Numbers are **not** the launch artifact — laptop, 18
perf+efficiency cores, thermals. Relative ordering + dress-rehearsal for
the exact EC2 cohort/commands.

### Run 1 — Graviton EC2 (canonical / headline)

1. **Provision**: resolve Ubuntu 24.04 **arm64** AMI via SSM param;
   launch `c7g.4xlarge`, 60 GB gp3, keypair + SG (SSH from my IP).
2. **Toolchains** (lighter now — K/R/J/G only): `apt update`; install
   build-essential, `clang`, **`llvm-18 llvm-18-dev libpolly-18-dev`**
   (inkwell/llvm-sys needs `llvm-config-18` on PATH — export
   `LLVM_SYS_181_PREFIX` if the versioned config isn't auto-found), Rust
   via rustup, `golang-go`, a JDK 11+ + `maven`, and `wrk` (`apt install
   wrk`, universe). **No Node, no Elixir/OTP.**
3. **Source**: `git clone https://github.com/karalang/kara` (repo
   public — verified HTTP 200). If `main` has unpushed local commits the
   bench needs (`git log origin/main..main` non-empty), rsync the
   worktree instead.
4. **Light sysctl for -c5000 on localhost** (NOT the `ws_idle_holder`
   2M-tuple setup — this is throughput, not idle density):
   `net.core.somaxconn=65535`, `net.ipv4.tcp_max_syn_backlog=65535`,
   raise `nofile` ulimit to ~1M.
5. **Run**:
   ```sh
   sh examples/parallax/bench/bench.sh --impls=k,r,j,g \
     | tee parallax_graviton_$(date +%s).log
   ```
   Plus the auto-par control: build a Kāra binary with
   `KARAC_AUTO_PAR=0`, run wrk against it with the same
   `-t4 -c100/1000/5000` shape, record alongside (bench.sh has no
   built-in off-lane).
6. **Pull** the log(s) back to the repo host (`scp`).
7. **Terminate.**

### Run 2 — x86 EC2 (cross-ISA confirmation)

Identical to Run 1 but: `c7i.4xlarge` + Ubuntu 24.04 **amd64** AMI;
**same K/R/J/G toolchain set** (rust/llvm + go + JDK/maven + wrk); run
`bench.sh --impls=k,r,j,g` (+ the `AUTO_PAR=0` control lane). Log named
`parallax_x86_*`. Pull, terminate.

---

## Toolchain matrix

| Tool | Mac (run 0) | Graviton (run 1) | x86 (run 2) |
|---|---|---|---|
| Rust/cargo + **LLVM 18** (karac + rust comparator) | ✓ | ✓ | ✓ |
| wrk | ✓ | ✓ | ✓ |
| go | ✓ | ✓ | ✓ |
| JDK 11+ / maven (java) | ✓ (Homebrew JDK) | ✓ | ✓ |
| node | ✓ | — | — |
| elixir/OTP (phoenix) | ✓ | — | — |

The **LLVM 18** dependency is the trickiest install — inkwell links
against it at karac build time. Ubuntu 24.04 ships it in apt
(`llvm-18`, `llvm-18-dev`). `bench.sh` then self-builds karac with
`--features llvm`. Dropping Node + Elixir from the paid boxes removes
the slowest/most fragile installs.

---

## Lane discipline (per `feedback_bench_lane_discipline`)

The usual rule — never headline auto-par Kāra against single-threaded
comparators — is **satisfied by construction here**: every paid-cohort
comparator is an explicit *parallel* fan-out (`tokio::join!`,
goroutines+WaitGroup, `CompletableFuture.allOf`), so the cross-impl
comparison is multi-core-vs-multi-core, same lane. The
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
- **Canonical table = Graviton K/R/J/G** (cold-start + steady-state),
  including the `kara (auto-par off)` control lane. This supersedes the
  current README tables (which are laptop-era, five-impl k/r/g/n/p).
- **Confirmation table = x86 K/R/J/G** — present as "the ratio holds on
  x86 too" (Kāra-vs-comparators on both ISAs, side by side).
- Note the cohort change explicitly: Node + Phoenix are no longer in the
  headline tables (kept only in the free Mac run); Java + Go are the
  comparators alongside Rust.
- Update the README provenance line (currently `_v7 (... five impls)_`)
  to a v8 entry: K/R/J/G on Graviton (canonical) + x86 (confirmation);
  keep the historical v7 line.
- Per `feedback_bench_kata_rerun_regression_check` /
  `kata_readme_sweep`: diff old→new for every metric that changed and
  classify (noise/load/improvement/regression) in the writeup, even
  though this is a cohort/host change, not a karac change.

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
- **Canonical = EC2 only.** Don't promote Mac numbers into the headline
  tables — laptop arm64 is supplementary/directional at most. Graviton
  is the canonical arm64 number; x86 is the cross-ISA confirmation.
