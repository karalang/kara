# secret.md sentence map

Each normative sentence of the page, split into units, and the cell or script that checks it. Cells run with `python3 conformance/cases.py <this dir>` on the interpreter and the compiled build. conformance/secret_model.py checks `ct_eq`, `expose` and `expose_mut` against Python's `==` on random pairs; conformance/ct_timing.py measures `ct_eq`'s timing. conformance/ct_timing.py also measures Secret.ct_eq.

| # | Section | Sentence | Cell(s) |
|---|---|---|---|
| sec.1 | `Secret[T]` | **Status: post-v1 library.** `Secret[T]` is library code and needs no language rule beyond the trait impls it leaves out. | status, no cell |
| sec.2 | `Secret[T]` | It is not part of the v1 release surface. | status, no cell |
| sec.3 | `Secret[T]` | `std.secret.Secret[T]` wraps credentials, tokens, keys and other sensitive values. | redacted_and_access; the rows below |
| sec.4 | `Secret[T]` | It blocks the most common paths by which such values leak: formatting into logs, derived serialization, and variable-time `==`. | redacted_and_access; the rows below |
| sec.5 | Traits it does not implement | `Secret[T]` deliberately does **not** implement: | the rows below |
| sec.6 | Traits it does not implement | row: Trait / Why | the rows below |
| sec.7 | Traits it does not implement | row: `Debug`, `Display` / Formatting writes the inner value into logs and panic messages. | debug_refused, display_refused, println_refused |
| sec.8 | Traits it does not implement | row: `Serialize`, `Deserialize` / Blanket serialization moves the value onto the wire and into logs unnoticed. The explicit methods `seriali | derive_serialize_refused (the explicit methods are not yet implemented: item sec.40) |
| sec.9 | Traits it does not implement | row: `PartialEq`, `Eq`, `PartialOrd`, `Ord` / Variable-time comparison is a long-standing attack. `Secret[T]` implements `ConstantTimeEq` in | eq_refused (`==` refused, pointing at `.ct_eq(`); secret_ordering_refused (`<` refused) |
| sec.10 | Traits it does not implement | row: `Hash` / The default hashers take time that depends on the content. Index secrets by a non-secret digest, not by the secret. | secret_not_a_map_key_refused (no Hash: a Map key refused, pointing at a digest) |
| sec.11 | Traits it does not implement | row: `Copy` / Silent duplication, even of a `Secret[u64]`, defeats the audit trail. `.clone()` is the only way to duplicate. | secret_not_copy_refused (a Secret[u64] moved twice is refused: not Copy) |
| sec.12 | Traits it does not implement | `Secret[T]` also never converts implicitly to `ref T`, which would bypass the audit trail. | secret_no_implicit_ref_refused (a Secret[String] where a ref String is expected is refused) |
| sec.13 | Traits it does not implement | Writing `impl Debug for Secret[T]`, deriving any of these traits on `Secret[T]`, or implementing any of them for it, is a compile error whos | user_debug_impl_refused (`impl Debug for Secret[T]`, E_SECRET_TRAIT_FORBIDDEN, pointing at the page); debug_refused, display_refused |
| sec.14 | Access | code: import std.secret.{Secret}; let mut tok: Secret[String] = Secret.new(raw_token); println(f"token = {tok}");                        //  | println_refused (the example's `println(f"token = {tok}")`); redacted_and_access (expose, expose_mut) |
| sec.15 | Access | row: `new` / `fn new(value: own T) -> Secret[T]` / Wraps a value | redacted_and_access (new, expose, expose_mut) |
| sec.16 | Access | row: `expose` / `fn expose(self) -> ref T` / Read access to the inner value | redacted_and_access (new, expose, expose_mut) |
| sec.17 | Access | row: `expose_mut` / `fn expose_mut(mut ref self) -> mut ref T` / Write access to the inner value | redacted_and_access (new, expose, expose_mut) |
| sec.18 | Access | row: `ct_eq` / `fn ct_eq(ref self, other: ref Secret[T]) -> bool` / Constant-time equality: every byte is compared, with no early exit at th | redacted_and_access (ct_eq on Secret[String] and Secret[Vec[u8]], equal, unequal, different lengths); ct_timing.py (Secret.ct_eq's time independent of where the bytes differ) |
| sec.19 | Access | `expose` and `expose_mut` are the only ways to reach `T`. | redacted_and_access; the refusals above |
| sec.20 | Access | Both are easy to search for and stand out in code review. | redacted_and_access; the refusals above |
| sec.21 | Access | Copying the value out (`tok.expose().clone()` into a plain `T`) gives a value that is no longer zeroized on drop: an explicit choice, visibl | secret_copy_out_and_derived_display (`a.expose().clone()` into a plain String outlives the secret) |
| sec.22 | Types that contain a secret | A struct with a `Secret[T]` field can derive `Debug` or `Display`; the derived impl prints the field as `<redacted>`: | redacted_and_access (derived Debug prints `<redacted>`); secret_copy_out_and_derived_display (derived Display prints `<redacted>`) |
| sec.23 | Types that contain a secret | code: import std.secret.{Secret}; #[derive(Debug)] struct User { name: String, token: Secret[String], } // prints:  User { name: "alice", to | redacted_and_access (derived Debug prints `<redacted>`); secret_copy_out_and_derived_display (derived Display prints `<redacted>`) |
| sec.24 | Types that contain a secret | `#[derive(Serialize)]` on a type that contains a `Secret[T]` is a compile error. | derive_serialize_refused |
| sec.25 | Types that contain a secret | Serializing a secret needs an explicit decision: a manual `Serialize` impl that calls `serialize_expose`, or a non-secret mirror type for th | derive_serialize_refused |
| sec.26 | Clone, drop and zeroize | **`Clone`** (`where T: Clone`) clones the inner value into a new `Secret`. | not yet implemented (sec.36): secret_clone_not_yet_refused |
| sec.27 | Clone, drop and zeroize | Each clone is zeroized separately when it drops. | not yet implemented (sec.36): secret_clone_not_yet_refused |
| sec.28 | Clone, drop and zeroize | **`Drop`** (`where T: Zeroize`) overwrites the inner value's bytes before the value is released. | zeroizing on drop is not observable from a program's output; not yet implemented as specified (sec.39), no cell |
| sec.29 | Clone, drop and zeroize | For `String` and `Vec[u8]` this zeros the heap buffer. | zeroizing on drop is not observable from a program's output; not yet implemented as specified (sec.39), no cell |
| sec.30 | Clone, drop and zeroize | Guarantees at the operating-system level (swap, memory mapping, inspection of process memory) are out of scope. | zeroizing on drop is not observable from a program's output; not yet implemented as specified (sec.39), no cell |
| sec.31 | Clone, drop and zeroize | **`Zeroize`** (`std.secret.Zeroize`, `fn zeroize(mut ref self)`) is the trait `Drop` uses. | not yet implemented (sec.37-38): secret_exports_only_secret_refused |
| sec.32 | Clone, drop and zeroize | The library implements it for `String`, `Vec[u8]`, `Array[u8, N]` and the integer types. | not yet implemented (sec.37-38): secret_exports_only_secret_refused |
| sec.33 | Clone, drop and zeroize | A user type that holds secret material implements it by hand. | not yet implemented (sec.37-38): secret_exports_only_secret_refused |
| sec.34 | Not in the prelude | `Secret`, `ConstantTimeEq` and `Zeroize` live in `std.secret` and are not imported automatically. | secret_without_import_refused; every cell imports std.secret |
| sec.35 | Not in the prelude | Each file that uses them starts with `import std.secret.{Secret};`, so the import line itself tells a reviewer that the file handles secrets | secret_without_import_refused; every cell imports std.secret |
| sec.36 | Not yet implemented | `Clone` on `Secret[T]`. | secret_clone_not_yet_refused (`.clone()` on a Secret refused) |
| sec.37 | Not yet implemented | `Zeroize` and `ConstantTimeEq` as importable, nameable items: | secret_exports_only_secret_refused (`import std.secret.{Zeroize}` refused: only Secret is exported) |
| sec.38 | Not yet implemented | `std.secret` exports only `Secret`, so a user `impl Zeroize` cannot be written yet (B-2026-11-07-24). | secret_exports_only_secret_refused (`import std.secret.{Zeroize}` refused: only Secret is exported) |
| sec.39 | Not yet implemented | Dropping a `Secret[String]`, or a `Secret[Vec[I]]` over an integer type `I`, zeros the whole buffer before it is freed (B-2026-11-16-192); a | B-2026-11-16-192's own tests; not observable from a program's output, no cell |
| sec.40 | Not yet implemented | `serialize_expose` and `deserialize_wrap`. | derive_serialize_refused (no serialize path exists yet) |
| sec.41 | Not yet implemented | These are tracked as the `std.secret` items of [phase-11-stdlib-longtail.md](../implementation_checklist/phase-11-stdlib-longtail.md). | cross-reference, no cell |
