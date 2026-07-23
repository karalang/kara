# The 3am Runbook: Debugging Crashes

You have been paged. A Kāra service crashed, and there is a JSON file waiting
for you. This chapter is the short version of what to do next — enough to
triage the common cases without reading the whole book at 3am.

When a Kāra program panics, its `std.panic` handler writes a **structured crash
report** — a single JSON file — and prints a short summary plus the file's path
to stderr. The default location is `/tmp/kara-crash-{pid}-{timestamp}.json`
(configurable via `KARA_CRASH_DIR`). The JSON is the load-bearing artifact: it
is a stable, versioned wire format that tools can dedupe and group across
compiler versions.

## First move: render it

When you have a crash file, run:

```bash
karac debug /tmp/kara-crash-48213-20260723T024117.json
```

`karac debug` turns the JSON into a human-readable report. (Pass `-` to read
from stdin, e.g. `curl … | karac debug -`.) Everything below is read off that
rendered report. The single most useful habit: **read the effect set first** —
it usually tells you *which subsystem* to look at before you read a single line
of application code.

## Worked example 1 — a panic that names its blast radius

```text
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Kāra crash report
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  panic: index out of bounds
  at src/handlers.kara:42:18  (fetch_user_dashboard)
  index out of bounds: the len is 3 but the index is 99

  effects: reads(UserDB) + sends(Network)

  logical stack:
    ▸ fetch_user_dashboard
        src/handlers.kara:42:18  reads(UserDB) + sends(Network)
    ▸ handle_request
        src/server.kara:88:5  reads(UserDB)
    ▸ main
        src/main.kara:12:3  pure

  parallel context:
    panicked branch: par@spawn_site_42 (worker 3)
    still running: render_sidebar, load_notifications
    cancelled: fetch_activity at sends boundary

  provider stack:
    Db ← PostgresPool   bound at src/main.kara:8:3

  RC fallback:
    `session` became RC at src/handlers.kara:40:9
        reason: captured by closure with subsequent outer use

  ────────────────────────────────────────────────────────────
  kara 0.1.0 (abc1234) · x86_64-unknown-linux-gnu · release · 2026
```

**How to read it, top to bottom:**

- **`panic: index out of bounds`** — the *kind*. An index-out-of-bounds is a
  logic bug (a bad index), not an outage. You are looking for a length
  assumption that failed, not a down dependency.
- **`at src/handlers.kara:42:18`** — go straight here. Line 42 indexed a
  3-element collection with `99`.
- **`effects: reads(UserDB) + sends(Network)`** — the blast radius. This code
  path touches the user database and the network. If the bad index came from
  data, `UserDB` is the place that data came from.
- **`logical stack`** — the call chain, each frame with its own effect summary.
  Note `main` is `pure`: the effects are introduced deeper in.
- **`parallel context`** — this ran inside a `par` block. Two siblings were
  still running and one (`fetch_activity`) was cancelled by fail-fast. That is
  expected: one branch panicking cancels the rest.
- **`provider stack`** — the `Db` resource was bound to a `PostgresPool` at
  `main.kara:8`. If this were a connectivity problem, this is the provider you
  would check.

**Verdict:** logic bug at `handlers.kara:42`. Not a paging-worthy outage — file
a bug, patch the index.

## Worked example 2 — a crash *inside cleanup*

The famously-hard case: a panic that happens while the program is already
unwinding from another panic, during a destructor. C++ aborts here; Kāra
captures it.

```text
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Kāra crash report
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  panic: panic while unwinding (drop)
  at src/cache.kara:71:5  (Session::drop)
  channel closed: send on a disconnected receiver

  effects: sends(Metrics)

  logical stack:
    ▸ Session::drop
        src/cache.kara:71:5  sends(Metrics)
    ▸ handle_checkout
        src/checkout.kara:118:9  reads(OrderDB) + writes(OrderDB)

  RC fallback:
    `session` became RC at src/checkout.kara:96:13
        reason: captured by a spawned closure with subsequent outer use — RC keeps it alive across the task boundary

  caused by:
  panic: index out of bounds
  at src/checkout.kara:122:20  (handle_checkout)
  index out of bounds: the len is 0 but the index is 0

  effects: reads(OrderDB)

  logical stack:
    ▸ handle_checkout
        src/checkout.kara:122:20  reads(OrderDB)

  ────────────────────────────────────────────────────────────
  kara 0.1.0 (abc1234) · aarch64-apple-darwin · release · 2026
```

**How to read it:**

- **`panic: panic while unwinding (drop)`** — the *second* panic. This one fired
  in `Session::drop` while the stack was already unwinding.
- **`caused by:`** at the bottom — the **original** panic. Read this first: the
  root cause is the index-out-of-bounds at `checkout.kara:122`, not the channel
  error. The drop-time panic is a symptom of the cleanup running during an
  already-failing request.
- **`RC fallback`** — the report tells you *why* the `Session` was still alive to
  be dropped here: the compiler chose an RC representation for `session` at
  `checkout.kara:96` because a spawned closure captured it. When a panic crosses
  an implicit `Drop` of an RC value, this annotation is how you find it without
  guessing. The fix for the *secondary* crash is usually making `Session::drop`
  tolerate a closed channel; the fix for the *incident* is the `caused_by` bug.

## Worked example 3 — one branch takes down a parallel batch

```text
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Kāra crash report
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  panic: parallel fail-fast cancellation
  at src/ingest.kara:54:12  (parse_shard)
  unwrap on Err value: ParseError { line: 3, msg: "unexpected end of input" }

  effects: reads(Blob) + panics

  logical stack:
    ▸ parse_shard
        src/ingest.kara:54:12  reads(Blob) + panics
    ▸ ingest_batch
        src/ingest.kara:40:3  reads(Blob) + writes(WarehouseDB)

  parallel context:
    panicked branch: par@spawn_site_17 (worker 2)
    still running: parse_shard[0], parse_shard[1], parse_shard[3]
    cancelled: parse_shard[0] at writes boundary
    cancelled: parse_shard[1] at writes boundary
    cancelled: parse_shard[3] at reads boundary

  ────────────────────────────────────────────────────────────
  kara 0.1.0 (abc1234) · x86_64-unknown-linux-gnu · release · 2026
```

**How to read it:**

- **`panic: parallel fail-fast cancellation`** — a branch of a `par` block
  panicked, and Kāra cancelled its siblings (fail-fast). The panic itself is the
  `unwrap on Err value` at `ingest.kara:54`.
- **`parallel context`** is the important part here. Worker 2 hit a
  `ParseError` on its shard. Three sibling shards were mid-flight and were
  cancelled — note *where*: two were cancelled at a `writes` boundary (they had
  not yet committed to `WarehouseDB`) and one at a `reads` boundary. That tells
  you the batch did **not** partially write: cancellation caught the writers
  before their effect boundary. No cleanup of half-written warehouse rows is
  needed.
- **Verdict:** a single malformed shard (line 3, unexpected EOF) failed the whole
  batch. Fix the input or make `parse_shard` return a `Result` the batch can
  skip, rather than `unwrap`.

## Common patterns — what the effect set is telling you

The effect line is a triage shortcut. Some patterns worth memorizing:

| Effect set on the panic frame | Where to look first |
|---|---|
| `panics + reads(SomeDB)` | Data from that database — a bad value, an empty result indexed, a failed `unwrap` on a query. |
| `sends(Network)` / `receives(Network)` on the frame | A downstream dependency — timeouts, closed connections, a service that is down. Check the **provider stack** for which endpoint. |
| `writes(SomeDB)` present but the panic is `par_fail_fast_cancel` | Check the **parallel context** cancellation boundaries — did any sibling get past its `writes` boundary? If not, no partial write happened. |
| `drop_during_unwind` | Read **`caused_by`** first — the root cause is the original panic, not the drop. |
| `rc_fallback_borrow`, or an **RC fallback** annotation on the crash | A shared value's lifetime crossed a task or closure boundary; the annotation names where the compiler chose RC. |

## Escalation

If the rendered report is not enough — you want to attach it to a bug, feed it
to an AI agent, or diff two crashes — re-emit the structured form:

```bash
karac debug crash.json --output=json
```

This prints the parsed report as pretty JSON. Because the wire format is stable,
it is safe to store, diff (`karac debug a.json --output=json | diff - <(karac
debug b.json --output=json)`), or hand to tooling that keys on `panic_kind` and
`panic_site` for grouping. Tools dedupe on the `(panic_kind, panic_site)` pair,
so two reports with the same kind and site are "the same bug" even across
builds.

For the full field-by-field contract — every field, the panic-kind vocabulary,
the edge cases (concurrent panics, WASM, GPU) — see the language reference's
*Crash Report Format* section. This runbook is deliberately the short version.
