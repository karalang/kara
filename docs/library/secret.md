# `Secret[T]`

**Status: post-v1 library.** `Secret[T]` is library code and needs no language rule beyond the trait impls it leaves out. It is not part of the v1 release surface.

`std.secret.Secret[T]` wraps credentials, tokens, keys and other sensitive values. It blocks the most common paths by which such values leak: formatting into logs, derived serialization, and variable-time `==`.

## Traits it does not implement

`Secret[T]` deliberately does **not** implement:

| Trait | Why |
|---|---|
| `Debug`, `Display` | Formatting writes the inner value into logs and panic messages. |
| `Serialize`, `Deserialize` | Blanket serialization moves the value onto the wire and into logs unnoticed. The explicit methods `serialize_expose` and `deserialize_wrap` are the only path. |
| `PartialEq`, `Eq`, `PartialOrd`, `Ord` | Variable-time comparison is a long-standing attack. `Secret[T]` implements `ConstantTimeEq` instead, so comparing tokens, HMACs and CSRF tokens is constant-time by default. |
| `Hash` | The default hashers take time that depends on the content. Index secrets by a non-secret digest, not by the secret. |
| `Copy` | Silent duplication, even of a `Secret[u64]`, defeats the audit trail. `.clone()` is the only way to duplicate. |

`Secret[T]` also never converts implicitly to `ref T`, which would bypass the audit trail. Writing `impl Debug for Secret[T]`, deriving any of these traits on `Secret[T]`, or implementing any of them for it, is a compile error whose diagnostic points to this section.

## Access

```kara
import std.secret.{Secret};

let mut tok: Secret[String] = Secret.new(raw_token);

println(f"token = {tok}");                        // compile error: Display is not implemented
let inner: ref String = tok.expose();             // read-only access
let inner_mut: mut ref String = tok.expose_mut(); // in-place change
```

| Method | Signature | Notes |
|---|---|---|
| `new` | `fn new(value: own T) -> Secret[T]` | Wraps a value |
| `expose` | `fn expose(self) -> ref T` | Read access to the inner value |
| `expose_mut` | `fn expose_mut(mut ref self) -> mut ref T` | Write access to the inner value |

`expose` and `expose_mut` are the only ways to reach `T`. Both are easy to search for and stand out in code review. Copying the value out (`tok.expose().clone()` into a plain `T`) gives a value that is no longer zeroized on drop: an explicit choice, visible at the call site.

## Types that contain a secret

A struct with a `Secret[T]` field can derive `Debug` or `Display`; the derived impl prints the field as `<redacted>`:

```kara
#[derive(Debug)]
struct User {
    name: String,
    token: Secret[String],
}

// prints:  User { name: "alice", token: <redacted> }
```

`#[derive(Serialize)]` on a type that contains a `Secret[T]` is a compile error. Serializing a secret needs an explicit decision: a manual `Serialize` impl that calls `serialize_expose`, or a non-secret mirror type for the wire.

## Clone, drop and zeroize

- **`Clone`** (`where T: Clone`) clones the inner value into a new `Secret`. Each clone is zeroized separately when it drops.
- **`Drop`** (`where T: Zeroize`) overwrites the inner value's bytes before the value is released. For `String` and `Vec[u8]` this zeros the heap buffer. Guarantees at the operating-system level (swap, memory mapping, inspection of process memory) are out of scope.
- **`Zeroize`** (`std.secret.Zeroize`, `fn zeroize(mut ref self)`) is the trait `Drop` uses. The library implements it for `String`, `Vec[u8]`, `Array[u8, N]` and the integer types. A user type that holds secret material implements it by hand.

## Not in the prelude

`Secret`, `ConstantTimeEq` and `Zeroize` live in `std.secret` and are not imported automatically. Each file that uses them starts with `import std.secret.{Secret};`, so the import line itself tells a reviewer that the file handles secrets.
