# shortener — a URL shortener in Kāra

A complete HTTP service in one file: routing, mutable module-level state,
JSON responses, and a real `302` redirect.

```bash
karac run examples/shortener/shortener.kara
```

| Route | Behavior |
|---|---|
| `POST /shorten` | body = the long URL → `201`, `{"code","short"}` |
| `GET /<code>` | `302` + `Location: <long URL>`, bumps the hit counter |
| `GET /stats` | `200`, link count + per-code hit counts |
| `GET /` | `200`, usage text |
| anything else | `404` |

```console
$ curl -s -d 'https://example.com/one' localhost:8080/shorten
{"code":"a","short":"/a"}
$ curl -si localhost:8080/a | head -2
HTTP/1.1 302 Found
location: https://example.com/one
$ curl -s localhost:8080/stats
{"links":1,"hits":{"a":1}}
```

## Response headers

The other `std_net` examples declare `struct Response { status, body }`.
That shape cannot set a header, so it cannot express a redirect. The
header channel is an **optional third field** on the program's own
`Response`:

```kara
struct Response {
    status: i64,
    body: String,
    headers: Vec[(String, String)],
}
```

Codegen detects the third field and lowers each pair through
`karac_runtime_http_response_set_header` (`src/codegen/http.rs`). Pairs
are applied in order; `content-type` defaults to `application/json`
unless a pair overrides it. The two-field form keeps working unchanged.

## Oracle

`tests/http_server.rs::test_shortener_example_end_to_end` compiles this
file, spawns it, and pins every route above — including the `302`'s
`location` header and the `/stats` hit counts. It substitutes
`127.0.0.1:0` for the hardcoded `:8080` so it binds an ephemeral port
like every other test in that file; the routing and responses are this
example's own code.

The `/stats` assertion is the one with teeth: it is the automated form
of the manual read that surfaced `B-2026-07-31-30`. Neuter the `HITS`
loop and the test fails with `"hits":{}` — the bug's exact symptom.

`examples/mend/examples/link_registry/` is the Mend task+oracle pair for
the same state layer with the HTTP surface removed, so it has the
deterministic stdout oracle a server cannot.

## Why it exists

This was written as a dogfooding exercise, developed through the Mend
loop (`karac check` → `karac fix` → verify against an oracle) rather
than hand-fixed. What that turned up:

- **`B-2026-07-31-30`** — `for (code, n) in HITS` over a **module-level**
  `Map` compiled to a **zero-iteration loop**, so `/stats` reported
  `"hits":{}` while `LINKS.len()` on the sibling map was correct and the
  interpreter iterated it fine. Module-level containers are the ordinary
  way to hold service state, so the miss was load-bearing. Fixed.
- `karac fix` applied the `.clone()` repairs for the advisory
  use-after-move diagnostics (three iterations, one per use).
