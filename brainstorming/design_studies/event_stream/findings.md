# Brick #4 — Event stream (push-model source)

**Program.** Read JSON events line-by-line from stdin until EOF; print a summary per event; log malformed events to stderr.

**What this brick stress-tests.**
- Push-model (data arrives) vs. pull-model (you ask for data) — different from every prior brick
- Unbounded source handling
- Per-item robustness (one bad event shouldn't kill the stream)
- Stdin + stdout + stderr in the same program (multiple I/O resources)

**Note on the source.** Stdin line-streaming is the minimum viable push source that every language handles without external infrastructure. A "real" event stream (Kafka, Kinesis, SSE) would add ecosystem complexity that obscures the language-level comparison. Same shape, cheaper to run. Findings below generalize.

---

## Per-axis scoring

| Axis | Java | Python | Rust | Kāra (current) |
|---|---|---|---|---|
| Robustness | Medium | Low-Med | High | High |
| Locality | High | High | High | High |
| Composability | Medium | High | Medium | High |
| Ceremony | Medium | Very low | Medium | Low |

### Robustness
- **Java** — unchecked cast via `event.get("event").asText()`; null on missing field silently becomes `"null"` string unless you check.
- **Python** — `KeyError` on missing field caught by the single `except` clause. No schema check; every access is dynamic.
- **Rust** — `#[derive(Deserialize)]` enforces schema at parse time. Malformed events fail fast at the JSON layer, never reach business logic with invalid state.
- **Kāra** — same as Rust on schema; the effect signature makes every I/O operation visible (`reads(Stdin)`, `writes(Stdout)`, `writes(Stderr)`).

### Locality
All four tie. One file, one main loop, one data type. Stream processing is inherently local at this scale.

### Composability
- **Java** — per-event `try/catch` is the mechanism; no way to compose "retry on parse error but abort on I/O error" without restructuring.
- **Python** — for loops over iterables compose naturally; `for line in sys.stdin` reads cleanly as "stream of lines"; adding filtering/mapping via generators is idiomatic.
- **Rust** — `stdin.lock().lines()` returns an iterator; composable but needs explicit `match` arms for the `Result<String, io::Error>` each yield returns.
- **Kāra** — `loop` + `io.read_line()` + `match` is the shape. **Per-event effect tracking** — a handler that only reads + writes Stdout is clearly distinct at the signature from one that touches the network or a DB. Composability via effect-typed boundaries rather than generic iterator traits.

### Ceremony
- **Python** shortest at ~15 lines.
- **Kāra** next, ~20 lines — the `match` on `Err(IoError.UnexpectedEof)` adds a line vs. Python, but buys explicit EOF handling.
- **Rust** ~25 lines with explicit stdout/stderr locks (needed to avoid the implicit lock-per-println slowdown on tight streams).
- **Java** ~30 lines with reader/writer plumbing.

---

## What each language forced

- **Java** — explicit `BufferedReader` + `InputStreamReader` + charset specification; reader-level exception handling.
- **Python** — iterator protocol does all the work; `for line in sys.stdin` is idiomatic without ceremony.
- **Rust** — explicit handle locking on stdout/stderr for performance; `lines()` returns `Result`-wrapped strings, each unwrapped explicitly.
- **Kāra** — `io.read_line()` returns `Result[String, IoError]` with `reads(Stdin)` effect; EOF is a named variant (`IoError.UnexpectedEof`) rather than a special-cased condition.

---

## Where Kāra stands

**Parity with Rust on the in-function shape.** Neither language has a special "stream" type; both use loop + read.

**Soft win via effect signature.** `reads(Stdin), writes(Stdout), writes(Stderr)` appears in the inferred signature. A reader of the main function knows which I/O resources are touched without reading the body.

**No D1 tension.** This brick has zero trait/impl blocks. The `Event` type is pure data with a derive. Exactly matches bricks #1 (money) and #2 (http): for flow-level programs, the trait/impl shape is invisible.

---

## Observations across bricks

This is the **4th brick that did not stress D1**. The full stress comes only when:
- A type has 5+ trait conformances (not tested — unlikely to change the result, but honest to note)
- Conformance is retroactive (not tested — would require a third-party type, not present in our study set)
- A module graph crosses files (not tested)

The four bricks we did run **consistently put concurrency + effect system at the center of Kāra's distinctiveness**, and **consistently put the trait/impl shape at the edge**. This is the shape of the answer.

---

## Provisional implication

**Brick #4 adds no new signal on D1.** It reinforces the pattern: the trait/impl syntax is the least interesting question in the language; the effect system + concurrency model are the most interesting.

**For v45:** the programs-first approach has done its job. The enumeration I propose in the top-level `design_studies/findings.md` captures the aggregate verdict.
