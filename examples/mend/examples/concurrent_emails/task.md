# Task: concurrent_emails

## Prompt fed to the LLM

> Write a Kāra program that sends a welcome message to users 1, 2, and 3
> **concurrently**, keeping a running count of how many were sent. Define:
>
> - `send_welcome(user_id: i64, ...) -> String` — returns the welcome line
>   for that user (`welcome user <id>`) and increments the shared sent-count.
> - `main()` — runs the three sends concurrently in a `par { }` block, then
>   prints the three lines in user order, then `sent <count>`, then `"done"`.
>
> The compiler will check your code with `karac check --output=json` and report
> structured diagnostics. If any errors carry a `replacement` field, run
> `karac fix` to apply them mechanically. Patch any descriptive errors yourself
> and re-check.

## Why this task

This is the corpus's concurrency punchline. The obvious first attempt —
a plain counter (a `let mut` the branches update through `mut ref`)
incremented from inside a `par { }` block — is the exact shape of a data
race. Kāra rejects it at **compile time**: two `par` branches may not
write, or mutably borrow, the same place (`E0418`, core-semantics.md
§11.2); the value must be a concurrency primitive (`Atomic[T]` /
`Mutex[T]`) or per-task state. (A module-level `let mut` counter is
refused before that, as `E_MODULE_LET`: Kāra has no mutable globals.)
Earlier versions of this task asked each branch to print its own line;
that first attempt now also meets `E0418`, since two branches writing
`Stdout` conflict the same way. It is the same lesson: a shared effect in
`par` branches is refused.

The LLM reads the `E0418` message, lifts the counter into a
`sync struct Counter { count: Atomic[i64] }`, and the build goes clean —
*with the parallelism preserved*. The compiler didn't force the
programmer back to sequential code; it forced the counter to be
race-free. `expected.txt` (from `mirror.py`, the Python mirror) is the
oracle.

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
