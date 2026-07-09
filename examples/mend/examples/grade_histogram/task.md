# Task: grade_histogram

## Prompt fed to the LLM

> Write a Kāra program with a `GradeBook` struct that stores student `names`
> (`Vec[String]`), their `scores` (`Vec[i64]`), and a `Map[String, i64]` index
> from name to score. Give it methods to:
>
> - `add(name, score)` — record a student in all three fields.
> - `bucket(score)` — return a letter-grade bucket index: `0`=A (>=90),
>   `1`=B (>=80), `2`=C (>=70), `3`=D (>=60), `4`=F (otherwise).
> - `histogram()` — return a `Vec[i64]` of length 5 counting students per
>   bucket. Start it zero-initialised with `Vec.filled(5, 0)`.
> - `total_name_chars()` — sum `names[i].len()` across all students.
> - `score_of(name)` — look the score up in the map, or `-1` if absent.
>
> In `main`, add six students (ada 95, bo 83, cy 72, di 68, ez 55, fi 91),
> print the histogram as `LETTER: count` lines (labels `["A","B","C","D","F"]`),
> then print `total name chars: N` and `score of cy: N`.
>
> The compiler will check your code with `karac check --output=json` and report
> structured diagnostics. If any carry a `replacement` field, run `karac fix`;
> patch descriptive errors yourself and re-check.

## Why this task

A correctness dogfood over the surfaces that saw recent codegen churn:
`Vec.filled(n, 0)` zero-init, indexed-receiver method calls
(`self.names[i].len()` — the B-2026-07-09-1 shape), struct + impl, a
`Map[String, i64]`, and range bucketing. The oracle is stdout equality; the
build is additionally checked interp == compiled, which is the gate that
catches silent codegen divergences a compile-only oracle would miss.

## The mistakes it surfaces (see notes.md)

1. An escaped quote inside an f-string interpolation (`{f(\"x\")}`) — a parse
   error with a precise fix hint.
2. A use-after-move: the `name` argument used as both the map key and a `Vec`
   element without a `clone()` (E0500).
3. (Compiler gap, not a task mistake) `String.len()` / `Vec.len()` return
   `i64`, so `u64` loop counters trip E0200 in arithmetic — which led to
   discovering that implicit integer conversions at `let`/arg/return
   boundaries are silently accepted (filed separately in the bug ledger).
