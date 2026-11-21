# concurrent_emails — what Python and Go miss, what Kāra catches

> **Rewritten 2026-11-20.** `task.md`, `solution.kara` and
> `canned_responses.json` return each welcome line and print the three
> lines after the `par` block, because two branches that both print
> conflict (`E0418`, core-semantics.md §11.2). The compiler reports the
> shared-counter race as `E0418`; `E0408` is the historical code, and a
> module-level `let mut` is refused earlier as `E_MODULE_LET`. The recorded
> harness runs and `casts/concurrent_emails.cast` predate the rewrite.
> `expected.txt`, produced by `mirror.py`, is the oracle.

The task: send three welcomes **concurrently** while keeping a running
count. The naive shape — a shared mutable counter incremented from
concurrent tasks — is a data race. Here is who catches it, verified on
the toolchains noted.

## Python (`python_buggy.py`)

- **mypy / pyright pass.** Every annotation is correct.
- **`self.sent += 1` is load-add-store** (multiple bytecodes); a thread
  switch mid-sequence drops an increment. Output is non-deterministic,
  typically under-counting under contention.
- **No Python static checker can flag it.** The types and control flow
  are right; the bug is a property of shared mutable state under
  concurrency, outside the type system's remit.

## Go (`go_buggy.go`) — verified on go1.26.3

- **`go build ./...` → compiles clean.**
- **`go vet ./...` → reports nothing.**
- **`go run -race .` → `WARNING: DATA RACE`** — but only via the
  *opt-in runtime* detector, on a scheduling interleaving that happens
  to expose it. There is **no compile-time error**. Ship without
  `-race` (the default) and the race goes to production silently.

This is the important contrast for the microservices audience: Go is
the dominant language in that niche, and its concurrency safety net is
a runtime flag you have to remember to turn on, not a compile gate.

## Kāra (`solution.kara`) — verified on the in-tree `karac`

The same shape is a **compile-time error**. The iter-0 attempt
(`canned_responses.json`), a plain counter each branch bumps:

```
fn send_welcome(user_id: i64, sent: mut ref i64) -> String {
    sent = sent + 1;
    f"welcome user {user_id}"
}

fn main() {
    let mut sent = 0;
    let (first, second, third) = par {
        send_welcome(1, mut ref sent),
        send_welcome(2, mut ref sent),
        send_welcome(3, mut ref sent),
    };
    // ...
}
```

The Rust spellings come back first, each with a `replacement`, and
`karac fix` repairs them: `mut ref sent` at a call site is `mut sent`
(`E0001`), and the store through the reference is `*sent = sent + 1`.
What is left is the race:

```
$ karac check concurrent_emails.kara --output=json
{"diagnostics":[{"code":"E0418","phase":"effect","line":8,"column":34,
  "message":"branches 1 and 2 of the `par` conflict: `mutable borrow of
    sent` and `mutable borrow of sent` (core semantics §11.2: branches may
    run at once, so none may write, move, mutably borrow or drop a place
    another uses, or write a resource another uses)"}]}
```

Each branch mutably borrows `sent`, and `par` branches may run at once,
so the conflict is a hard error rather than the serialization it would
be in sequential code. It carries no `replacement`: choosing a
concurrency primitive is the author's call.

The fix lifts the counter into a concurrency primitive and **keeps the
parallelism**:

```
sync struct Counter { count: Atomic[i64] }
// counter.count.fetch_add(1) inside each branch
```

`karac check` → clean, and the output matches `expected.txt`. The
compiler didn't push the programmer back to sequential code; it forced
the counter to be race-free while the three sends still run
concurrently.

## Where this sits relative to Rust — be precise

**Rust catches this class too.** Sharing a plain `&mut` counter across
`thread::spawn` is a borrow-checker / `Send + Sync` error; you are
forced to `Arc<Mutex<_>>` or an atomic, exactly as Kāra forces
`Atomic`/`Mutex`. So this example is a clean win over **Python and Go**
— **not** over Rust.

That is deliberate. Data-race freedom is Rust's home turf; claiming a
win there would be dishonest. Kāra's advantage over Rust is not
*catching this bug* — it is:

1. **Auto-parallelization with no ceremony.** Non-conflicting effects
   (e.g. `reads(R)` siblings) are scheduled concurrently by the
   compiler with no `par`, no `spawn`, no `Arc`/`join` plumbing. Rust
   never auto-parallelizes; the programmer wires every thread by hand.
2. **No colored functions.** The concurrent path is written in
   blocking style; there is no `async`/`.await` split.
3. **The structured fix-loop itself** — `E0418` arrives as JSON with
   span and message the agent consumes mechanically (this corpus).

So the honest framing for the corpus: **concurrent_emails is the
Go/Python-beating case.** The Rust-beating story is the auto-par
ergonomics demo (`examples/parallax`), not a caught-race.

## Why the contrast matters

> "A language designed to be written by AI" only means something if the
> compiler keeps the agent honest. An LLM writes what worked in its
> training corpus — a shared counter incremented from concurrent tasks,
> which is *fine to write* in Python and Go and only bites at runtime.
> Kāra's effect checker turns that latent race into a compile error the
> agent must resolve before the build is green.
