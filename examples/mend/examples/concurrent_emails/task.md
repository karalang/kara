# Task: concurrent_emails

## Prompt fed to the LLM

> Write a Kāra program that sends a welcome message to users 1, 2, and 3
> **concurrently**, keeping a running count of how many were sent. Define:
>
> - `send_welcome(user_id: i64, ...)` — prints a welcome line and increments
>   the shared sent-count.
> - `main()` — runs the three sends concurrently in a `par { }` block, then
>   prints `"done"`.
>
> The compiler will check your code with `karac check --output=json` and report
> structured diagnostics. If any errors carry a `replacement` field, run
> `karac fix` to apply them mechanically. Patch any descriptive errors yourself
> and re-check.

## Why this task

This is the corpus's concurrency punchline. The obvious first attempt —
a module-level `let mut` counter incremented from inside a `par { }`
block — is the exact shape of a data race. Kāra rejects it at **compile
time** with `E0408`: a `par { }` branch may not write a plain shared
mutable binding; the value must be a concurrency primitive
(`Atomic[T]` / `Mutex[T]`) or per-task state.

The LLM reads the `E0408` message, lifts the counter into a
`par struct Counter { count: Atomic[i64] }`, and the build goes clean —
*with the parallelism preserved*. The compiler didn't force the
programmer back to sequential code; it forced the counter to be
race-free.

This is the descriptive end of the Mend loop (no machine-applicable
`replacement` — convergence relies on the LLM reading the message and
restructuring), exercising the **effect checker's `par`-conflict path**
— a fourth compiler axis beyond `welcome_emails` (ownership),
`order_status` (exhaustiveness), and `user_lookup` (type).

## Comparator note

The side-by-side versions (`python_buggy.py`, `go_buggy.go`) are the
same task in the two dominant microservices incumbents. Both **compile
and ship** the race; see `notes.md` for the verified contrast (Go's
race is caught only by the opt-in *runtime* `-race` detector, never by
`go build` / `go vet`). Rust would reject this class too — so this
example is a clean win over **Python and Go**, not over Rust; `notes.md`
is explicit about where Kāra's advantage over Rust actually lies.
