# concurrent_emails — what Python and Go miss, what Kāra catches

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

The same shape is a **compile-time error**. The iter-0 attempt:

```
let mut SENT_COUNT: i64 = 0;
// ...
fn main() {
    par {
        send_welcome(1);   // each branch writes SENT_COUNT
        send_welcome(2);
        send_welcome(3);
    }
}
```

```
$ karac check concurrent_emails.kara --output=json
{"diagnostics":[{"code":"E0408","phase":"effect","line":9,"column":5,
  "message":"module-level let mut 'SENT_COUNT' cannot be written from
    inside par { } — wrap in Atomic[T], Mutex[T], or use #[thread_local]
    for per-task state (binding declared at line 1)"}]}
```

The effect checker knows each `par` branch's transitive effects include
`writes(SENT_COUNT)`, and a plain shared binding written from a
parallel region is rejected — the conflict is upgraded from the silent
"serialize" it would be in sequential code to a hard error, because
serializing inside a `par { }` is almost never what was meant.

The fix lifts the counter into a concurrency primitive and **keeps the
parallelism**:

```
par struct Counter { count: Atomic[i64] }
// counter.count.fetch_add(1) inside each branch
```

`karac check` → clean. The compiler didn't push the programmer back to
sequential code; it forced the counter to be race-free while the three
sends still run concurrently.

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
3. **The structured fix-loop itself** — `E0408` arrives as JSON with
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
