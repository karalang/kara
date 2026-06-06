# welcome_emails — what Python misses, what Kāra catches

## Python (`python_buggy.py`)

- **mypy / pyright pass.** Every type annotation is correct.
- **Single-threaded run is correct.** The bug is invisible without
  contention.
- **Multi-threaded run undercounts.** `self.sent += 1` is two
  bytecodes; a thread switch between read and store drops the
  increment. Output is non-deterministic, typically 95-99% of the
  expected count.
- **No Python static checker can flag this.** The types are right.
  The control flow is right. The bug is a property of *shared mutable
  state under concurrency* — outside the type system's remit.

## Kāra (`solution.kara` — current scaffold)

The current scaffold is the **simplest end of the loop**: a sequential
program with two resolver-level typos, both auto-fixed by `karac fix`.
It demonstrates the mechanism — JSON diagnostics with `replacement`
spans → mechanical application → clean build — without yet engaging
the effect system.

## The concurrency punchline — see `concurrent_emails`

The sibling `concurrent_emails` example poses the same task with a
parallelism ask. The LLM's natural attempt — increment a shared
mutable counter from inside a `par { }` block — is rejected at compile
time. The real diagnostic (verified against the in-tree `karac`, not a
mock) is `E0408` from the effect phase:

```
module-level let mut 'SENT_COUNT' cannot be written from inside
par { } — wrap in Atomic[T], Mutex[T], or use #[thread_local] for
per-task state (binding declared at line 1)
```

(Note: a write/write conflict in *ordinary* sequential code is not an
error — the compiler silently serializes it. The hard error fires
specifically for a shared mutable binding written from inside an
explicit `par { }` region, where serializing is almost never intended.)

This is the demo's punchline: *the same shape that races silently in
Python — and that `go build`/`go vet` wave through in Go — is rejected
at compile time in Kāra*. The LLM lifts the counter into an
`Atomic[i64]` and the build goes clean with the parallelism preserved.
Rust would reject this class too, so the contrast is against Python and
Go; see `concurrent_emails/notes.md` for the precise, verified
comparison.

## Why the contrast matters

> "A language designed to be written by AI" isn't a slogan if the
> compiler can't keep an LLM honest. The LLM is incentivized to write
> code that *looks* like what works in the languages it was trained on
> (Python, JavaScript, Go). Kāra's compiler is the part that catches
> the patterns that *look right* but aren't.
