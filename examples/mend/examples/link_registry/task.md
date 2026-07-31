# Task: link_registry

## Prompt fed to the LLM

> Write a Kāra program that implements the state layer of a URL shortener,
> using **mutable module-level bindings** for its storage:
>
> - `LINKS: Map[String, String]` — code → long URL.
> - `HITS: Map[String, i64]` — code → times followed.
> - `ORDER: Vec[String]` — codes in the order they were minted.
> - `SEQ: i64` — the counter the next code is derived from.
> - `CODE_ALPHABET: StringSlice` — `"abcdefghijklmnopqrstuvwxyz0123456789"`.
>
> Then these functions:
>
> - `encode_code(n)` — base-36 encode `n` over `CODE_ALPHABET`, most
>   significant digit first. `0` encodes as `"a"`.
> - `shorten(url)` — mint the code for the current `SEQ`, advance `SEQ`,
>   record the URL, seed the hit count at 0, append the code to `ORDER`, and
>   return the code.
> - `follow(code)` — return the URL for `code` and bump its hit count;
>   return an empty string when the code is unknown.
>
> In `main`: shorten `https://example.com/one`, `/two`, `/three`; print
> `codes: <a> <b> <c>` and `seq 37: <encode_code(37)>`. Follow the first code
> twice and the second once, printing each returned URL. Follow `"zz"` and
> print `missing is empty: <bool>`. Then **iterate `HITS`** to count entries
> and sum the hit counts, printing
> `links: <LINKS.len()> entries: <n> hits: <n>`; **iterate `ORDER`**
> concatenating the codes and print `order: <codes>`. Finish with
> `unused: <bool>` for whether the third code is non-empty.
>
> Fold the map iteration into totals rather than printing per entry — the
> interpreter and the compiled binary walk a `Map` in different orders.
>
> The compiler will check your code with `karac check --output=json` and report
> structured diagnostics. If any carry a `replacement` field, run `karac fix`;
> patch descriptive errors yourself and re-check.

## Why this task

It is the state layer of `examples/shortener/`, with the HTTP surface removed
so the unit has a deterministic stdout oracle a server cannot have.

The target shape is **mutable module-level containers read, written, and
iterated from functions** — which is how any service holds state, and which is
exactly what `B-2026-07-31-30` got wrong: `for x in <module-level Vec/Map/Set/
String>` compiled to a **zero-iteration loop**. The shortener reported
`"hits":{}` while `LINKS.len()` on the sibling map was correct and the
interpreter iterated fine. A compile-only check cannot see that, which is the
whole argument for the oracle: `entries`, `hits`, and `order` all collapse to
empty under the bug and the stdout diff fails.

`encode_code` carries the second half — a `while` loop over integer division
with a `char_at` returning `Option[char]`, prepending to build the result. The
`seq 37: bb` line pins the multi-digit path, which a single-digit-only test
would miss.

## The mistakes it surfaces

Recorded per run rather than asserted here — the honesty rule in
`TASK_FORMAT.md` means the machine-fix rate is only meaningful over blind LLM
authorship, and this file was written by someone who already knows the
language. See `notes.md` for the shapes it is designed to provoke.
