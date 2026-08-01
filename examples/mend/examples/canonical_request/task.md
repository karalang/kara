# Task: canonical_request

## Prompt fed to the LLM

> Write a Kāra program that builds the **canonical request** string of AWS
> Signature Version 4 — stage 1 of the signing process. No hashing is needed;
> the payload hash is supplied to you as a string.
>
> Define two structs, `Header { name, value }` and `QueryParam { key, value }`,
> and these functions:
>
> - `lower(s)` — ASCII lowercase.
> - `trimall(s)` — strip leading/trailing spaces and collapse each run of
>   internal spaces to one.
> - `uri_encode(s, keep_slash)` — RFC 3986 percent-encoding. `A-Z a-z 0-9 - _ .
>   ~` pass through; everything else becomes `%XX` with UPPERCASE hex digits.
>   When `keep_slash` is true a `/` passes through unencoded — that is the only
>   difference between encoding a path and encoding a query value.
> - `sort_headers(headers)` — sort by lowercased name.
> - `sort_params(params)` — sort by *encoded* key.
> - `canonical_query(params)` — `key=value` pairs joined by `&`, sorted, both
>   halves encoded.
> - `canonical_request(method, path, params, headers, payload_hash)` — join
>   with newlines: the method; the encoded path (slashes kept); the canonical
>   query; then one `name:value` line per sorted header with the name lowercased
>   and the value trimall'd; then a blank line; then the signed-header names
>   joined by `;`; then the payload hash.
>
> In `main`, build a request with query params `Zed=1`, `alpha=a b`, `Mid=x/y`
> supplied in that order, headers `X-Amz-Date`, `Host`, `My-Header` (whose value
> is `"  a  b  "`) also out of order, method `GET`, path `/my path/doc`, and the
> SHA-256 of the empty string as the payload hash. Print the canonical request,
> then `---`, then `uri_encode("a/b c~d_e.f-g", true)`, the same with `false`,
> then `trimall("  a  b  ")` wrapped in `[` `]`, then `lower("X-Amz-DATE")`.
>
> The compiler will check your code with `karac check --output=json` and report
> structured diagnostics. If any carry a `replacement` field, run `karac fix`;
> patch descriptive errors yourself and re-check.

## Why this task

Stage 1 of `examples/sigv4.kara`, extracted so the unit is writable from a
prompt and has a deterministic stdout oracle without dragging in 220 lines of
SHA-256.

It is a **specification-conformance** task, which is rare in this corpus: every
output line is fixed by a published spec rather than by taste, so "compiles and
looks reasonable" and "correct" are far apart. The two sorts use *different*
comparison keys (lowercased name vs encoded key), the `/` rule inverts between
path and value, and `trimall` collapses interior runs but the encoder must not.
Each is a place a plausible implementation is silently wrong.

It also carries the shape that produced `B-2026-08-01-24`: `sort_headers` moves
elements out of a by-value `Vec[Header]` parameter into a fresh one, which
codegen double-freed. The oracle is checked interp == compiled, which is the
gate that catches that class — the interpreter was correct while the binary
aborted.

## The mistakes it surfaces

See `notes.md`. Recorded as shapes the task is designed to provoke, not as a
machine-fix-rate claim — the honesty rule in `TASK_FORMAT.md` excludes authoring
by anyone who already knows the answers.
