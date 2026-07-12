# Handling Secrets

Credentials leak in boring, repeatable ways. A token ends up in a log line because someone `println`'d a struct for debugging. An API key rides into a crash report because the type derived `Serialize`. A session compare uses `==`, and an attacker times the responses to recover the token one byte at a time. None of these are exotic — they are the *default* behavior of ordinary types, and that is the problem.

Kāra's answer is `std.secret.Secret[T]`: a wrapper that makes the leaky paths *not compile*. You still hold the value, you still use it — but printing it, serializing it, and comparing it with `==` are compile errors, and the one comparison you're allowed is constant-time.

## Wrapping a value

`Secret` is not in the prelude. You import it explicitly:

```kara
import std.secret.{Secret};

fn main() {
    let api_key = Secret.new("sk-live-abc123");
    // ...
}
```

That import line is deliberate. `std.secret` is a **gated module** — code that never asks for it never sees the name — so an `import std.secret` at the top of a file is itself a signal in code review: *this file handles sensitive material.* You want that signal loud.

## Reading it back

The wrapped value is reachable through exactly one read path, `.expose()`:

```kara
import std.secret.{Secret};

fn main() {
    let api_key = Secret.new("sk-live-abc123");
    let raw = api_key.expose();
    println(f"Authorization: Bearer {raw}");
}
```

```text
Authorization: Bearer sk-live-abc123
```

`.expose()` is intentionally ugly and intentionally unique. It is not an operator, not a `Deref`, not an implicit coercion — it is a method with a name you can `grep` for. Every place a secret becomes an ordinary value is a `.expose()` call site, so "where do we touch the raw key?" is a text search, not an audit.

## The accidents that no longer compile

The point of the wrapper is what it *refuses* to do. Comparing two secrets with `==` doesn't type-check:

```kara
let a = Secret.new("x");
let b = Secret.new("y");
println(a == b);
```

```text
error[typecheck]: type 'Secret<String>' does not implement Eq; add #[derive(Eq)] to use == or !=
```

The suggestion to `#[derive(Eq)]` is a dead end *on purpose* — deriving it is itself blocked:

```kara
impl Display for Secret[String] {
    fn fmt(ref self) -> String { "leak" }
}
```

```text
error[E_SECRET_TRAIT_FORBIDDEN]: cannot implement `Display` for `Secret[T]` — the
wrapper deliberately withholds this trait so a secret cannot be printed,
serialized, or structurally compared by accident (see design.md § Secret Type).
Read the value with `.expose()` where you genuinely need it; for equality use
the constant-time `.ct_eq(...)`, and rely on the built-in `Zeroize`/`Drop` for wiping
```

The same rejection covers `Debug`, `Display`, `Serialize`, `Deserialize`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`, `Deref`, `Borrow`, `AsRef`, and `Copy` — every trait that would let a secret slip out through printing, wire transit, structural comparison, transparent unwrapping, or silent bit-duplication. There is no `#[derive]` that turns them back on.

## Secrets inside structs

Most secrets don't travel alone — they sit in a config or a session struct. Deriving `Display` on a struct that *contains* a `Secret` field is fine; the field just renders as `<redacted>`:

```kara
import std.secret.{Secret};

#[derive(Display)]
struct Config { host: String, token: Secret[String] }

fn main() {
    let c = Config { host: "db.internal", token: Secret.new("hunter2") };
    println(c);
}
```

```text
Config { host: db.internal, token: <redacted> }
```

So the reflexive "log the whole config" debugging move is safe by construction: the ordinary fields print, the secret doesn't. This holds everywhere the struct renders — `println`, `.to_string()`, an f-string — and through nesting, because each struct re-checks its own `Secret` fields.

## Comparing in constant time

The one comparison a `Secret` permits is `.ct_eq()`:

```kara
import std.secret.{Secret};

fn main() {
    let expected = Secret.new("sk-live-abc123");
    let presented = Secret.new("sk-live-abc123");

    if expected.ct_eq(presented) {
        println("tokens match");
    } else {
        println("tokens differ");
    }
}
```

```text
tokens match
```

Why not just `==`? Because a normal string compare returns the instant it finds a differing byte. An attacker who can time your token check learns *how many leading bytes were correct* from the response latency, and walks the secret out one byte per round of guesses. `.ct_eq()` compares the whole length every time — it accumulates the differences and only then decides — so the timing carries no information about *where* two values diverge. This is the right primitive for tokens, HMAC tags, and CSRF values, and it's the reason `Secret` withholds `==` in the first place.

(Today `.ct_eq()` covers `Secret[String]` — the token/HMAC/CSRF case. Byte-array secrets are on the way.)

## The posture: wrap at the type, not at the call site

The habit Kāra pushes you toward is to make the secret a `Secret[T]` *at the boundary where it's born* — the moment you read the env var, parse the request header, or load the key file — and to keep it wrapped for its whole lifetime. Every downstream function takes a `Secret[String]`, not a `String`; the type flows through your program carrying its guarantees, and the only holes are the `.expose()` calls you can enumerate.

The opposite habit — passing a raw `String` around and being *careful* at each use — is the one that fails, because "be careful everywhere" is not a property a compiler can check. Wrapping at the type makes the safe path the default and the unsafe path a visible, greppable exception.

> **A note on zeroization.** The language reference (`design.md § Secret Type`) also specifies that a `Secret` *wipes its bytes when it's dropped*, so a freed buffer doesn't linger in memory. That behavior is being wired through the compiler's drop paths and is **not complete yet** — so don't rely on a dropped `Secret`'s memory being zeroed today. Everything else in this chapter — the wrapper, `.expose()`, the trait blocklist, `<redacted>` rendering, and constant-time `.ct_eq()` — is enforced now.
