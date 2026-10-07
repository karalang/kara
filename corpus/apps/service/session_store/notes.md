lines: 399

build: ok
interp: same (`karac run --interp` stdout is byte-identical to `./prog`)
mirror: agrees (`python3 session_store.py` stdout is byte-identical to `./prog`)

stmt-par: none (the program does no file I/O and no sleep_ms)

shared types: `shared enum Expiry` (L22) and `shared enum TokenRef` (L34). Both are there only because of a compiler limit, not because they need reference semantics. See workarounds 1 and 2.

workarounds:
1. L22 `enum Expiry` was changed to `shared enum Expiry`. The plain enum is the payload of `SessionError.Expired(String, Expiry)`, and the build failed with `error[typecheck]: error[E_ENUM_NESTED_ENUM_PAYLOAD]: enum variant 'SessionError.Expired' has a payload of nested enum type 'Expiry' — v1 only supports up to one level of enum nesting; either flatten the variant, mark the inner enum as `shared` ...`. I used the fix the compiler suggested.
2. L34 `enum TokenRef` was changed to `shared enum TokenRef` for the same reason. `Op.Logout(TokenRef)`, `Op.Refresh(TokenRef)` and `Op.Check(TokenRef)` hit the same E_ENUM_NESTED_ENUM_PAYLOAD error.
3. L354: the natural form `Ok(out) => { issued.push(out.token.clone()); match out.evicted { Some(old) => ...{out.token}... } }` built cleanly, but the native binary aborted with `free(): double free detected in tcache 2` at the first login that evicts a session (t=20). Valgrind showed an invalid free in `main` of a string allocated by `karac_string_clone`: the evicted token, which `oldest_of` created with `tok.clone()`. Because of the abort, no stdout was printed. `karac run --interp` was correct.
   - The fix: destructure in the arm pattern (`Ok(LoginOutcome { token, evicted }) => { ... match evicted { ... } }`). Valgrind then reported 0 errors.
   - The bug reproduces in a ~55-line standalone program: a `mut ref self` method fills `let mut evicted: Option[String] = None` inside a `match self.oldest() { Some(old) => { self.sessions.remove(old); evicted = Some(old); } ... }`, returns `Ok(Outcome { token, evicted })`, and the caller does `match out.evicted` inside `Ok(out) =>`.
   - Some smaller variants of that program did not reproduce. Binding `let ev = out.evicted; match ev` also avoids the bug. This is a codegen double free and should be filed as a compiler bug.
4. L118 (type error, minor): `let code = c as u32;` followed by `h * HASH_BASE + code` with `h: i64` was rejected with `cannot mix integer types 'i64' and 'u32' in arithmetic`. The spec's widening table says u32 -> i64 is implicit, so I wrote `(c as u32) as i64`. (`char` -> `i64` directly is not a legal cast per the spec.)
5. Not a compiler bug: `seq` is a reserved keyword (`seq { ... }` expression form), so the sequence counter is named `serial`.

ignored diagnostics: 0 (`karac check source.orig.kara` prints no `error[` lines; it prints one warning, `warning[prelude_shadow]` for `struct Request`. `karac build` also prints one `warning[borrow_projection_copy]` at `match req.op`.)
