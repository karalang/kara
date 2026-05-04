# Brick #2 — HTTP API call

**Program.** HTTP GET a JSON endpoint, deserialize to `Vec[User]`, print rows.

**What this brick stress-tests.**
- Async / blocking story across languages
- `reads(Network)` effect visibility (Kāra-unique)
- Error-type diversity (network error, HTTP status error, parse error — three distinct failure modes)
- Dep-footprint per language for a "simple" HTTP call

---

## Per-axis scoring

| Axis | Java | Python | Rust | Kāra (current) |
|---|---|---|---|---|
| Robustness | Medium | Low | High | High-plus |
| Locality | High | High | High | High |
| Composability | Medium | Medium | High | High |
| Ceremony | Medium (~45) | Very low (~35) | Low (~30) | Low (~30) |

### Robustness
- **Java** — checked `Exception` on `client.send`; one giant `catch (Exception e)` lumps network + status + parse failures together. Status-code check is manual.
- **Python** — `requests.RequestException` covers network + status (via `raise_for_status()`), but parse errors are separate (`KeyError`, `json.JSONDecodeError`). No compile-time guarantee anything is checked.
- **Rust** — `reqwest::Error` chained with `error_for_status()` + `serde_json::Error` via `?` + `anyhow::Context`. Every failure mode is in the signature (`Result<(), anyhow::Error>`).
- **Kāra** — same as Rust on error handling **plus** `reads(Network)` appears in main's inferred effect set. *The compiler knows this function touches the network.* No other language surfaces this at the type level.

### Locality
All four win — one file, one short `main`, one data type. No cross-file navigation required. HTTP calls don't exercise the D1 tension at all; this is a flow-level task, not a type-level one.

### Composability
- **Java / Python** — fine but the HTTP client and JSON parser are separate libraries you glue together. No type-level link between "I got a 200 response body" and "this body is valid JSON of type X."
- **Rust** — `reqwest` + `serde` compose via `.json().await?` which returns `Result<T, reqwest::Error>`. One chain, three failure modes unified.
- **Kāra** — same shape as Rust, plus the effect system lets a parent function see `reads(Network)` in the signature without any import-time scanning.

### Ceremony
- **Java** has the most — `HttpClient.newHttpClient()`, request builder, response handler, `TypeReference<List<User>>` generic gymnastics.
- **Python** is shortest at ~35 lines.
- **Rust** is ~30 lines but carries `#[tokio::main]` + async/await machinery.
- **Kāra** matches Rust in line count with simpler chain (no explicit async) — but the cost is hidden in the `blocks` effect (or `suspends` if the call is re-entrant).

---

## What each language forced

- **Java** — explicit client construction; no compile-time JSON schema check; one `catch-all` exception.
- **Python** — no schema check at all; the `User(**u)` dance is manual; runtime `KeyError` if fields missing.
- **Rust** — async propagation forces `#[tokio::main]` even for one-shot CLI; `error_for_status()` + `.context()` chains manage error breadcrumbs; type-checked JSON via `serde`.
- **Kāra** — same shape as Rust minus the visible async plumbing; effect system surfaces `reads(Network)` to callers without any annotation beyond the function call.

---

## Where Kāra stands

**Pure wins over Java/Python:** typed errors, type-checked JSON, effect-visible network access.

**Parity with Rust on the flow-level code.** The D1 question is untouched by this brick — there's no trait/impl block to compare.

**One Kāra-distinctive win surfaced:** `reads(Network)` in the inferred effect signature. A caller looking at `main` knows it touches the network without opening any file. This is *not* something the D1 debate affects — it's a different axis entirely (effect system, not trait syntax). It's worth a separate note because v44/v45 could accidentally decide D1 in a way that tangles with effects; keeping them orthogonal matters.

---

## Provisional implication

Brick #2 **confirms brick #1's headline** — current D1a is adequate, not excellent, and unchanged by HTTP-shaped programs. The genuinely interesting Kāra property that showed up here is the *effect system*, not the *trait/impl shape*.

**Takeaway for v45:** keep effect-system evolution and trait/impl-shape evolution on separate tracks. A D1 change should not perturb how `reads(Network)` surfaces in signatures, and effect-system changes should not require a new `impl` form.

Move on to brick #3 (`parallel_fanout`) — the one most likely to actually break something.
