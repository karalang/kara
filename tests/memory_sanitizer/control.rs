//! control flow, loops, bindings, assignment, scopes -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer control::
//!
//! New fixtures about control flow, loops, bindings, assignment, scopes belong in this file.

use super::*;

/// B-2026-09-06-26 — the MEMORY half of
/// `e2e_scalar_leaf_return_keeps_the_arg_walk` (tests/codegen.rs). The fix
/// narrows the payload-escape scanner both backends consult for an enum
/// argument (a scalar-typed leaf never counts as leaving, a lowered
/// primitive operator never takes an argument over), so the caller-side
/// walk over `E3.S { k, r }` now runs `r`'s body where it used to be
/// masked, and the struct cells that only the interpreter lost are pinned
/// alongside. Bodies only; this pins that no free moved with them. The
/// cells that bind a leaf and never consume it (`bare`, `ren`, `iflet`,
/// `bf`, `s`) are deliberately absent: that unconsumed enum leaf leaks its
/// heap at -O0 on the by-value param spelling (B-2026-09-06-28), before and
/// after this fix.
#[test]
fn asan_scalar_leaf_return_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H2 { e: E, n: i64 }
struct Hs { e: E, s: String }
struct Hb { e: E, b: bool, f: f64 }
enum E2 { A(R), B(i64) }
impl Drop for E2 { fn drop(mut ref self) { println("  dE2") } }
enum E3 { A(R), S { k: i64, r: R } }
impl Drop for E3 { fn drop(mut ref self) { println("  dE3") } }
fn consume(x: R) -> i64 { return x.id }

fn p_sum(h: H2) -> i64 { match h { H2 { e, n } => { match e { E.A(r) => { return r.id + n; } E.B => { return n; } } } } }
fn p_bare(h: H2) -> i64 { match h { H2 { e, n } => { return n; } } }
fn p_ren(h: H2) -> i64 { match h { H2 { e, n: k } => { return k; } } }
fn p_iflet(h: H2) -> i64 { if let H2 { e, n } = h { return n; } else { return 0; } }
fn p_alias(h: H2) -> i64 { match h { H2 { e, n } => { let z = n * 2; match e { E.A(r) => { return r.id + z; } E.B => { return 0; } } } } }
fn p_bf(h: Hb) -> f64 { match h { Hb { e, b, f } => { if b { return f; } return 0.0; } } }
fn p_s(h: Hs) -> String { match h { Hs { e, s } => { return s; } } }
fn p_slen(h: Hs) -> i64 { match h { Hs { e, s } => { return s.len(); } } }
fn p_read(h: H2) -> i64 { match h { H2 { e, n } => { match e { E.A(r) => { return r.id; } E.B => { return 0; } } } } }
fn e_call(b: E2) -> i64 { match b { E2.A(r) => { return consume(r); } E2.B(k) => { return k; } } }
fn e_arith(b: E2) -> i64 { match b { E2.A(r) => { return r.id * 2; } E2.B(k) => { return k + 1; } } }
fn s_k(b: E3) -> i64 { match b { E3.A(r) => { return r.id; } E3.S { k, r } => { return k; } } }

fn main() {
    println("sum/local"); let a1 = H2 { e: E.A(mk(1)), n: 100 }; let x1 = p_sum(a1); println(f"  x{x1}");
    println("sum/temp"); let x2 = p_sum(H2 { e: E.A(mk(2)), n: 100 }); println(f"  x{x2}");
    println("alias/local"); let a7 = H2 { e: E.A(mk(7)), n: 100 }; let x7 = p_alias(a7); println(f"  x{x7}");
    println("read/local"); let a11 = H2 { e: E.A(mk(11)), n: 100 }; let x11 = p_read(a11); println(f"  x{x11}");
    println("ecall/A"); let b1 = E2.A(mk(21)); let y1 = e_call(b1); println(f"  x{y1}");
    println("ecall/B"); let b2 = E2.B(22); let y2 = e_call(b2); println(f"  x{y2}");
    println("earith/A"); let b3 = E2.A(mk(23)); let y3 = e_arith(b3); println(f"  x{y3}");
    println("earith/B"); let b4 = E2.B(24); let y4 = e_arith(b4); println(f"  x{y4}");
    println("sk/A"); let c1 = E3.A(mk(31)); let z1 = s_k(c1); println(f"  x{z1}");
    println("sk/S"); let c2 = E3.S { k: 32, r: mk(132) }; let z2 = s_k(c2); println(f"  x{z2}");
    println("sk/S-temp"); let z3 = s_k(E3.S { k: 33, r: mk(133) }); println(f"  x{z3}");
    println("end");
}
"#,
        &[
            "sum/local",
            "  dE",
            "  dR1",
            "  x101",
            "sum/temp",
            "  dE",
            "  dR2",
            "  x102",
            "alias/local",
            "  dE",
            "  dR7",
            "  x207",
            "read/local",
            "  dE",
            "  dR11",
            "  x11",
            "ecall/A",
            "  dE2",
            "  dR21",
            "  x21",
            "ecall/B",
            "  dE2",
            "  x22",
            "earith/A",
            "  dE2",
            "  dR23",
            "  x46",
            "earith/B",
            "  dE2",
            "  x25",
            "sk/A",
            "  dE3",
            "  dR31",
            "  x31",
            "sk/S",
            "  dE3",
            "  dR132",
            "  x32",
            "sk/S-temp",
            "  dE3",
            "  dR133",
            "  x33",
            "end",
        ],
        "asan_scalar_leaf_return_is_balanced",
    );
}

/// A value-position BLOCK or BRANCH whose tail MINTS a fresh owned temp is
/// itself a fresh owned temp (B-2026-08-29-27).
///
/// The sibling above is the same merge, one producer over: there an arm
/// tail READS a container element and deep-clones it, here an arm tail
/// CALLS something that allocates. The clone case was answered with a
/// merge-point owner keyed on `vec_elem_field_clone_slots`; a fresh call
/// temp is not a container-element clone, so that machinery never reached
/// this shape and `if c { mk(n) } else { mk(n + 1) }.contains("p")` simply
/// lost the taken arm's buffer — once per evaluation, unbounded in a loop.
///
/// THE BRANCH IS NOT THE SUBJECT, which is why the bare-block rows are here
/// and not treated as filler. `{ mk(n) }.contains("p")` has no branch in it
/// at all and leaked identically, so the population is "a value-position
/// construct whose TAIL mints a fresh owned temp"; `if` and `match` are two
/// members of it. Reading the branch rows alone sends a fix to `compile_if`
/// / `compile_match`, which would miss the block.
///
/// SEVEN CONSUMING POSITIONS, each its own gate in codegen and each blind
/// to a wrapper before the fix:
///   - a method receiver (`a1`–`a3`, `a7`, `a8`)
///   - the `len`/`is_empty` fast path, a SEPARATE receiver gate (`a4`–`a6`)
///   - a by-value call argument (`b1`–`b4`)
///   - an f-string interpolation (`c1`, `c2`)
///   - a `for` iterable (`h1`, `h2`)
///   - a borrow-consuming operand, via `free_fresh_owned_str_arg` (`h3`)
///
/// The unwrapped call is clean at every one of them, which is what localizes
/// this to the predicate rather than to the frees.
///
/// THE `f`-ROWS ARE THE TRIPWIRE, not decoration: a `let`, a `Vec.push`
/// argument, a `return` and an assignment all OWN the merged buffer, so each
/// is a DOUBLE FREE if the widened predicate fires where a destination has
/// already taken the value over. The two discarded statements are the fourth
/// corner — there the statement frame owns it, and a second free would abort
/// rather than leak.
///
/// `e4` takes the ELSE arm (`d` is false), so the fixture does not prove
/// only the then-path; `g` runs the leaking shape three times, which is what
/// makes "unbounded in a loop" an assertion rather than a claim.
///
/// Measured pre-fix on this exact fixture: 378 B in 14 blocks at `-O2`
/// (555 B in 20 at `-O0`), with byte-identical stdout — a leak-only class.
/// B-2026-08-29-51 — a self-assignment whose RHS is a BLOCK ending in a
/// passthrough call frees the value it overwrites.
///
/// `assign_rhs_is_owned_user_call` matched only a bare `Call` at the RHS
/// root, so `e = { let z = 1; pass(e) };` arrived as an `ExprKind::Block`
/// and answered false. That made `roundtrip_frees_old` false, and with
/// `rhs_mentions_lhs` true the caller's whole overwrite-cleanup branch was
/// skipped: the old value's field heap was never freed. The unwrapped
/// `e = pass(e);` was clean, which is what localizes it to the wrapper
/// rather than to the roundtrip.
///
/// INVISIBLE TO THE A/B GATE TWICE OVER, which is why it needs a leak test
/// specifically: both backends agree, AND the printed output is correct on
/// both — one `Drop` body for one object. Only the heap is wrong.
///
/// The four shapes are the ones measured to leak pre-fix under
/// `valgrind --leak-check=full` at `KARAC_OPT_LEVEL=0`, and they pin
/// different halves of the fix:
///
///   `one`    one `String` field       13 allocs / 12 frees, 2 B lost
///   `two`    two `String` fields      14 / 12, 2 blocks — one PER FIELD
///   `en`     enum `String` payload    10 / 9
///   `nod`    no `impl Drop` at all    11 / 10 — not limited to Drop types
///
/// `nest` and `uns` cover the peel being recursive and reaching the other
/// two value-position block spellings; a plain `Seq`/`Unsafe` tail is the
/// same shape to the assignment.
///
/// COVERAGE, stated because it is weaker than the assertion looks: this
/// leak is OPTIMIZATION-DEPENDENT. Measured on the pre-fix compiler, the
/// fixture loses 7 blocks at `KARAC_OPT_LEVEL=0` (36 allocs / 29 frees) and
/// is CLEAN at the default `-O2` (22 / 22), where LLVM deletes the
/// redundant allocation outright. So the MEMORY half of this fixture is
/// carried by the `-O0` leg (`scripts/asan-o0-leg.sh`, B-2026-08-04-17),
/// which is what that leg exists for; verified red there pre-fix with
/// `ERROR: LeakSanitizer: detected memory leaks` and green after. What the
/// default `-O2` leg still asserts is the OUTPUT — a leak traded for a lost
/// or doubled `Drop` body fails here at either level.
///
/// That dependence is also why the row could call this low severity and why
/// it survived so long: a release build never showed it.
///
/// NOT covered here, deliberately: an `if` / `match` RHS
/// (`e = if c { pass(e) } else { pass(e) }`) leaks identically and is NOT
/// fixed — measured at 12 allocs / 11 frees both before and after this
/// change, so it is untouched rather than regressed. Those have several
/// tails that need not agree, which is a different question; filed
/// separately.
#[test]
fn asan_self_assign_block_tail_frees_the_overwritten_value() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.id}") } }

struct W { id: i64, a: String, b: String }
impl Drop for W { fn drop(mut ref self) { println(f"dropW {self.id}") } }

struct P { id: i64, name: String }

enum E { A(String), B }
impl Drop for E { fn drop(mut ref self) { println("dropE") } }

fn pass(r: R) -> R { return r; }
fn passw(w: W) -> W { return w; }
fn passp(p: P) -> P { return p; }
fn passe(x: E) -> E { return x; }

fn main() {
    let n: i64 = env.args().len();

    let mut one = R { id: 1, name: f"one-aaaaaaaaaaaaaaaaaaaaaaaa" };
    one = { let z: i64 = n; pass(one) };
    println(f"one {one.id}");

    let mut two = W { id: 2, a: f"a-aaaaaaaaaaaaaaaaaaaaaaaa", b: f"b-bbbbbbbbbbbbbbbbbbbbbbbb" };
    two = { let z: i64 = n; passw(two) };
    println(f"two {two.id}");

    let mut en: E = E.A(f"en-aaaaaaaaaaaaaaaaaaaaaaaa");
    en = { let z: i64 = n; passe(en) };
    println("en done");

    let mut nod = P { id: 4, name: f"nod-aaaaaaaaaaaaaaaaaaaaaaaa" };
    nod = { let z: i64 = n; passp(nod) };
    println(f"nod {nod.id}");

    let mut nest = R { id: 5, name: f"nest-aaaaaaaaaaaaaaaaaaaaaaaa" };
    nest = { let z: i64 = n; { pass(nest) } };
    println(f"nest {nest.id}");

    let mut uns = R { id: 6, name: f"uns-aaaaaaaaaaaaaaaaaaaaaaaa" };
    // Safety: no unsafe operation here — the block spelling is the point.
    uns = unsafe { pass(uns) };
    println(f"uns {uns.id}");
}
"#,
        // Each body fires at its binding's LAST USE, not at scope exit, so
        // the drops interleave with the printlns. Both backends produce
        // this sequence identically — checked, since the fix moves where a
        // free happens and an order change would be the interesting kind
        // of regression.
        &[
            "one 1", "drop 1", "two 2", "dropW 2", "dropE", "en done", "nod 4", "nest 5", "drop 5",
            "uns 6", "drop 6",
        ],
        "asan_self_assign_block_tail_frees_the_overwritten_value",
    );
}

/// B-2026-09-01-1 — a self-assignment whose RHS is an `if` / `if let` /
/// `match` frees the value it overwrites, the branch sibling of
/// `asan_self_assign_block_tail_frees_the_overwritten_value` above.
///
/// That fix taught `assign_rhs_is_owned_user_call` to peel value-position
/// BLOCK wrappers to their single tail. A branch has several tails, so it
/// stayed unpeeled and answered false: `roundtrip_frees_old` was false, and
/// with `rhs_mentions_lhs` true the caller's whole overwrite-cleanup branch
/// was skipped and the old value's field heap was never freed. Measured
/// UNTOUCHED by that fix at the time — 12 allocs / 11 frees before and
/// after — so it was filed separately rather than folded in.
///
/// WHY THE ARMS NEVER HAVE TO AGREE, which is what the row expected to be
/// the hard part: the cleanup is emitted AFTER `compile_expr(value)` has
/// already produced the branch's value, so the arm has run by then. Only
/// "may this old value be freed at all" is left, and requiring EVERY arm to
/// qualify answers it per path. `bmix` / `bmix2` are the pair that pins
/// this: one runs the arm that roundtrips, the other the arm that mints a
/// fresh value, and the displaced old value is orphaned identically by both.
///
/// The shapes, each measured at 12 allocs / 11 frees pre-fix and 12 / 12
/// after:
///
///   `bif`     `if` / `else`, both arms roundtrip
///   `bmatch`  the `match` spelling
///   `bmix`    MIXED arms, roundtripping arm taken
///   `bmix2`   MIXED arms, fresh-value arm taken
///   `bnest`   a BLOCK whose tail is an `if` — the two peels composing
///   `bil`     the `if let` spelling
///   `belif`   an else-if CHAIN, i.e. an `if` nested in an else branch
///   `ben`     enum `String` payload — the value-enum leg, not just structs
///   `bnod`    no `impl Drop` at all — not limited to Drop types
///
/// `bnest`, `bil` and `belif` were all listed NOT MEASURED on the row and
/// were confirmed to leak identically while fixing it.
///
/// THE IDENTITY ARM was left out of this fixture and is now FIXED on its own
/// row (B-2026-09-05-32), with its own fixture directly below —
/// `asan_self_assign_identity_arm_frees_only_the_distinct_value`. The note
/// that stood here said the shape was declined because freeing would trade a
/// leak for a use-after-free; that was true of an UNGUARDED free, and the
/// fix guards it on the old and incoming values being distinct rather than
/// widening the predicate blindly. It is still absent from THIS fixture
/// because this one pins the strict predicate's reach, which deliberately
/// does not include it.
///
/// COVERAGE, weaker than it looks and stated for the same reason the
/// sibling states it: the leak is OPTIMIZATION-DEPENDENT. Measured pre-fix
/// at 12 allocs / 11 frees under `KARAC_OPT_LEVEL=0` and CLEAN at the
/// default `-O2` (10 / 10), where LLVM deletes the redundant allocation. So
/// the MEMORY half here is carried by the `-O0` leg
/// (`scripts/asan-o0-leg.sh`, B-2026-08-04-17); the default leg asserts the
/// OUTPUT, and a leak traded for a lost or doubled `Drop` body fails at
/// either level. That dependence is why the row is low severity and why a
/// release build never showed it.
#[test]
fn asan_self_assign_branch_arms_free_the_overwritten_value() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.id}") } }

struct P { id: i64, name: String }

enum E { A(String), B }
impl Drop for E { fn drop(mut ref self) { println("dropE") } }

fn pass(r: R) -> R { return r; }
fn passp(p: P) -> P { return p; }
fn passe(x: E) -> E { return x; }
fn mk(n: i64) -> R { return R { id: n, name: f"mk-aaaaaaaaaaaaaaaaaaaaaaaa" }; }

fn main() {
    let n: i64 = env.args().len();
    let c = n > 0;
    let d = n > 5;

    let mut bif = R { id: 1, name: f"bif-aaaaaaaaaaaaaaaaaaaaaaaa" };
    bif = if c { pass(bif) } else { pass(bif) };
    println(f"bif {bif.id}");

    let mut bmatch = R { id: 2, name: f"bmatch-aaaaaaaaaaaaaaaaaaaaaaaa" };
    bmatch = match c { true => pass(bmatch), false => pass(bmatch) };
    println(f"bmatch {bmatch.id}");

    let mut bmix = R { id: 3, name: f"bmix-aaaaaaaaaaaaaaaaaaaaaaaa" };
    bmix = if c { pass(bmix) } else { mk(30) };
    println(f"bmix {bmix.id}");

    let mut bmix2 = R { id: 4, name: f"bmix2-aaaaaaaaaaaaaaaaaaaaaaaa" };
    bmix2 = if d { pass(bmix2) } else { mk(40) };
    println(f"bmix2 {bmix2.id}");

    let mut bnest = R { id: 5, name: f"bnest-aaaaaaaaaaaaaaaaaaaaaaaa" };
    bnest = { let z: i64 = n; if c { pass(bnest) } else { pass(bnest) } };
    println(f"bnest {bnest.id}");

    let mut bil = R { id: 6, name: f"bil-aaaaaaaaaaaaaaaaaaaaaaaa" };
    let o: Option[i64] = Option.Some(n);
    bil = if let Option.Some(k) = o { pass(bil) } else { pass(bil) };
    println(f"bil {bil.id}");

    let mut belif = R { id: 7, name: f"belif-aaaaaaaaaaaaaaaaaaaaaaaa" };
    belif = if d { pass(belif) } else if c { mk(70) } else { pass(belif) };
    println(f"belif {belif.id}");

    let mut ben: E = E.A(f"ben-aaaaaaaaaaaaaaaaaaaaaaaa");
    ben = if c { passe(ben) } else { passe(ben) };
    println("ben done");

    let mut bnod = P { id: 9, name: f"bnod-aaaaaaaaaaaaaaaaaaaaaaaa" };
    bnod = if c { passp(bnod) } else { passp(bnod) };
    println(f"bnod {bnod.id}");
}
"#,
        // Each body fires at its binding's LAST USE, not at scope exit, so
        // the drops interleave with the printlns. Both backends produce
        // this sequence identically — checked, since the fix moves where a
        // free happens and an order change would be the interesting kind of
        // regression. `bmix2` and `belif` print the MINTED id (40, 70)
        // because the fresh-value arm is the one taken there.
        &[
            "bif 1", "drop 1", "bmatch 2", "drop 2", "bmix 3", "drop 3", "bmix2 40", "drop 40",
            "bnest 5", "drop 5", "bil 6", "drop 6", "belif 70", "drop 70", "dropE", "ben done",
            "bnod 9",
        ],
        "asan_self_assign_branch_arms_free_the_overwritten_value",
    );
}

/// A value-position block or branch whose tail hands out a BINDING THAT
/// OUTLIVES THE HAND-OUT owns what escapes (B-2026-08-30-2).
///
/// `suppress_block_tail_cleanup` zeroes the source's `cap` on the premise
/// its own doc states — "the consumer's binding remains the sole owner" —
/// which holds for a `let`, a by-value argument, a `return` and an
/// assignment, and does NOT hold for the read-only positions: a
/// `.contains()` receiver, a `println` argument, a concat operand, an
/// f-string part. There the value is read and dropped, so zeroing the source
/// stranded its buffer.
///
/// FIXING IT AT THE CONSUMER IS NOT AVAILABLE, which is why this needed an
/// owner rather than one more arm on B-2026-08-29-27's predicate. The source
/// binding is still READABLE after the hand-out — every `a*` / `b*` / `c*`
/// row reads it back on the same line and it prints correctly — while the
/// gates that predicate feeds free IMMEDIATELY at the use site. Admitting a
/// binding tail there would dangle that read: a use-after-free, strictly
/// worse than the leak. The owner is registered in the frame that held the
/// SOURCE's own cleanup instead, which is after every such read.
///
/// EVERY RECEIVER ROW USES `.contains()`, NOT `.len()`, and that is not a
/// style choice. `.len()` reads a word out of the already-loaded header and
/// touches no buffer, so a row written that way passes whether or not the
/// buffer is owned by anyone — three rows of an earlier draft were exactly
/// that, and reported clean against a compiler still leaking 54 B on the
/// same shapes. A consumer that reads the BYTES is what makes it an
/// assertion.
///
/// EVERY BRANCH ROW TAKES THE ARM THAT NAMES THE BINDING, for a different
/// reason: the sibling MINTING arm of a mixed branch is a shape this fix
/// does NOT cover (see `asan_branch_tail_declined_shapes_do_not_double_free`),
/// so a row that took it would be pinning a leak in the test that asserts
/// there are none.
///
/// The rows separate the four things that can go wrong:
///
/// - `a*` — a bare block, plain and nested. `{ { x } }` is not decoration:
///   `suppress_block_tail_cleanup` recurses through a block tail and
///   re-emits the cap-zero, which the outer block cannot tell from a first
///   disarm, so it registered a SECOND owner and double-freed. The outer now
///   adopts the inner's slots — COPIED under its own key, not moved, because
///   a consumer looks up whichever span IT holds and that is not always the
///   outermost construct.
/// - `b*` / `c*` — all three branch compilers. Ownership is genuinely
///   per-path: exactly one arm's value escapes and the other's binding dies
///   in place, which is why the owner is stored in the arm's own basic block
///   rather than once at the merge. `b3` takes a binding from EITHER arm and
///   reads both back. The `if let` row (`c3`) is here because `compile_expr`
///   never stashed a branch span for `IfLet`, so its arms had no takeover key
///   and registered nothing at all.
/// - `d*` / `e*` — the consumer axis. `d*` are read-only (a `println`
///   argument, a concat operand, an f-string part, and `d4`, a DISCARDED
///   block) and must leave the owner alone; `e*` are the OWNING destinations
///   — a `let`, a by-value argument, a `Vec.push`, a function tail — and must
///   take it OVER. Without that half this trades the leak for a double free,
///   and `e1`-`e5` are what catch it.
/// - `f1` — three iterations. A per-arm owner slot is written on one path
///   only, so on the next pass an arm that did not run still holds the
///   previous pass's already-freed header unless the slot is reset in the
///   block that dominates the arms. Measured as a double free before that
///   reset existed.
///
/// `g*` and `h1` are TRIPWIRES for the two neighbouring fixes this one sits
/// between and must not disturb: an all-mint construct (B-2026-08-29-27),
/// whose value a consuming gate still frees at the use site, and a DISCARDED
/// branch (B-2026-08-29-5), whose source is deliberately left armed. Both
/// were measured double-freeing against an earlier draft.
///
/// `i*` are the tripwire that matters most, because they are the shape this
/// fix had to be NARROWED to exclude. A tail naming a binding declared
/// INSIDE the block it is handed out of is what every codegen-internal
/// desugar synthesizes — `Vec.sorted()` lowers to `{ let mut __srt =
/// recv.clone(); __srt.sort(); __srt }`, and the iterator adaptors (`i5`),
/// `collect` into a non-Vec target, the nested-Vec index bind and the
/// generic-enum payload bind (`i4`) all build the same thing. Each already
/// arranges its own ownership at the producer, so a second owner is a double
/// free: an earlier draft took out 11 codegen and 12 memory-sanitizer tests
/// at once, `let s: Vec[i64] = v.sorted()` segfaulting among them. The owner
/// now registers only when the source's own cleanup frame is still LIVE,
/// which such a tail's never is. These rows assert that the decline stays a
/// decline.
#[test]
fn asan_branch_tail_binding_handout_frees_once() {
    assert_clean_asan_run(
        r#"
enum E { A(String), B }
fn mkA(n: i64) -> String { return f"A{n}-aaaaaaaaaaaa"; }
fn mkB(n: i64) -> String { return f"B{n}-bbbbbbbbbbbbbbbbbbbbbbbb"; }
fn use_s(s: String) -> i64 { return s.len(); }
fn tail_block(n: i64) -> String { let t = mkB(n); { t } }
fn tail_branch(n: i64, c: bool) -> String { let t = mkB(n); if c { t } else { mkA(n) } }

fn main() {
    let n: i64 = env.args().len();
    let c = n > 0;

    let a1 = mkB(n);
    let ra1 = { a1 }.contains("bbb");
    println(f"a1={ra1} {a1}");
    let a2 = mkB(n);
    let ra2 = { { a2 } }.contains("bbb");
    println(f"a2={ra2} {a2}");

    let b1 = mkB(n);
    let rb1 = if c { b1 } else { mkA(n) }.contains("bbb");
    println(f"b1={rb1} {b1}");
    let b2 = mkB(n);
    let rb2 = if n < 0 { mkA(n) } else { b2 }.contains("bbb");
    println(f"b2={rb2} {b2}");
    let b3 = mkA(n);
    let b4 = mkB(n);
    let rb3 = if c { b3 } else { b4 }.contains("aaa");
    println(f"b3={rb3} {b3} {b4}");

    let c1 = mkB(n);
    let rc1 = match n { 0 => mkA(n), _ => c1 }.contains("bbb");
    println(f"c1={rc1} {c1}");
    let c2 = mkB(n);
    let rc2 = match n { 1 => c2, _ => mkA(n) }.contains("bbb");
    println(f"c2={rc2} {c2}");
    let c3 = mkB(n);
    let o3: Option[i64] = Option.Some(n);
    let rc3 = if let Option.Some(_x) = o3 { c3 } else { mkA(n) }.contains("bbb");
    println(f"c3={rc3} {c3}");

    let d1 = mkB(n);
    println({ d1 });
    println(f"d1={d1}");
    let d2 = mkB(n);
    let rd2 = "x" + { d2 };
    println(f"d2={rd2}");
    let d3 = mkB(n);
    let rd3 = f"[{if c { d3 } else { mkA(n) }}]";
    println(f"d3={rd3}");
    let d4 = mkB(n);
    { d4 };
    println(f"d4={d4}");

    let e1 = mkB(n);
    let re1 = { e1 };
    println(f"e1={re1}");
    let e2 = mkB(n);
    let re2 = if n < 0 { mkA(n) } else { e2 };
    println(f"e2={re2}");
    let e3 = mkB(n);
    let re3 = use_s({ e3 });
    println(f"e3={re3}");
    let e4 = mkB(n);
    let re4 = use_s(if n < 0 { mkA(n) } else { e4 });
    println(f"e4={re4}");
    let e5 = mkB(n);
    let mut ev: Vec[String] = Vec.new();
    ev.push(if n < 0 { mkA(n) } else { e5 });
    println(f"e5={ev[0]}");
    let re6 = tail_block(n);
    println(f"e6={re6}");
    let re7 = tail_branch(n, c);
    println(f"e7={re7}");

    let mut i = 0;
    while i < 3 {
        let f1 = mkB(n + i);
        let rf1 = if i < 3 { f1 } else { mkA(n) }.contains("bbb");
        println(f"f1={rf1}");
        i = i + 1;
    }

    let rg1 = if c { mkA(n) } else { mkB(n) }.contains("aaa");
    println(f"g1={rg1}");
    let rg2 = { mkA(n) }.contains("aaa");
    println(f"g2={rg2}");
    let rg3 = use_s(if c { mkA(n) } else { mkB(n) });
    println(f"g3={rg3}");
    let h1 = mkB(n);
    if c { mkA(n) } else { h1 };
    println(f"h1={h1}");

    let i1 = { let t = mkB(n); t };
    println(f"i1={i1}");
    let e = E.A("k");
    let i2 = match e { E.A(x) => { let p = f"[{x}]"; p }, E.B => "z" };
    println(f"i2={i2}");
    let v: Vec[String] = ["b", "a"];
    let i3: Vec[String] = v.sorted();
    println(f"i3={i3[0]} {v[0]}");
    let o4: Option[String] = Option.Some(mkB(n));
    let i4 = match o4 { Option.Some(x) => x, Option.None => mkA(n) };
    println(f"i4={i4}");
    let vv: Vec[i64] = Vec[1i64, 2i64, 3i64];
    let i5: Vec[Vec[i64]] = vv.iter().chunks(2i64).collect();
    println(f"i5={i5.len()}");
}
"#,
        &[
            "a1=true B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "a2=true B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "b1=true B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "b2=true B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "b3=true A1-aaaaaaaaaaaa B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "c1=true B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "c2=true B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "c3=true B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "d1=B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "d2=xB1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "d3=[B1-bbbbbbbbbbbbbbbbbbbbbbbb]",
            "d4=B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "e1=B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "e2=B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "e3=27",
            "e4=27",
            "e5=B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "e6=B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "e7=B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "f1=true",
            "f1=true",
            "f1=true",
            "g1=true",
            "g2=true",
            "g3=15",
            "h1=B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "i1=B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "i2=[k]",
            "i3=a b",
            "i4=B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "i5=2",
        ],
        "asan_branch_tail_binding_handout_frees_once",
    );
}

/// B-2026-08-30-12 — a value-position block whose tail names a binding IT
/// DECLARED is a minting tail, and the seven consuming gates now free it.
///
/// `suppress_block_tail_cleanup` disarms the local before the block's frame
/// drains, which is required — the frame is about to free it and the value
/// must survive to its consumer — and nothing then owned it unless the
/// consumer did. So a READ-ONLY consumer stranded the buffer while the
/// OWNING spelling was clean. Measured (valgrind, one fixture per row,
/// `KARAC_OPT_LEVEL=0` and `2` identical):
///
/// | shape | pre |
/// |---|---|
/// | `{ let t = mkB(n); t }.contains("bbb")` | 27 B |
/// | `println({ let u = mkB(n); u })` | 27 B |
/// | `{ let t = mkB(n); t }.len()` | 27 B |
/// | `let s = { let t = mkB(n); t }` (OWNING) | clean |
///
/// WHY THE GATES AND NOT B-2026-08-30-2'S OWNER. That owner registers a
/// replacement in the frame that held the SOURCE's cleanup, and here that
/// frame has already drained — see
/// `asan_branch_tail_declined_shapes_do_not_double_free`, where this shape
/// used to be pinned as a leak. The gates work instead because a
/// block-local binding has NO LATER READER: the block ends at the tail, so
/// the immediate free those gates emit cannot dangle. That is the whole
/// difference from the outer-binding tail the predicate still declines, and
/// `o1` below is the control for it — `loc` is read AFTER the wrapper hands
/// it out, and must still print.
///
/// `s1` is the tripwire for the shape the row warned about: a
/// codegen-internal desugar builds exactly this block
/// (`Vec.sorted()` lowers to `{ let mut __srt = recv.clone(); __srt.sort();
/// __srt }`) and arranges its own ownership at the producer, so a second
/// owner here would be a double free rather than a leak. Both the read-only
/// and the owning spelling are exercised.
#[test]
fn asan_block_local_tail_frees_once() {
    assert_clean_asan_run(
        r#"
fn mkB(n: i64) -> String { return f"B{n}-bbbbbbbbbbbbbbbbbbbbbbbb"; }

fn main() {
    let n: i64 = env.args().len();

    let r1 = { let t = mkB(n); t }.contains("bbb");
    println(f"r1={r1}");
    println({ let u = mkB(n); u });
    let r3 = { let t = mkB(n); t }.len();
    println(f"r3={r3}");

    let e1 = { let t = mkB(n); t };
    println(f"e1={e1}");

    let loc = mkB(n);
    let o1 = { loc }.contains("bbb");
    println(f"o1={o1} {loc}");

    let v: Vec[i64] = vec![3, 1, 2];
    let s1 = v.sorted().len();
    println(f"s1={s1}");
    let s2: Vec[i64] = v.sorted();
    println(f"s2={s2[0]}");
}
"#,
        &[
            "r1=true",
            "B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "r3=27",
            "e1=B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "o1=true B1-bbbbbbbbbbbbbbbbbbbbbbbb",
            "s1=3",
            "s2=1",
        ],
        "asan_block_local_tail",
    );
}

// ── Early-return cleanup (2026-05-13) ─────────────────────────
// `ExprKind::Return` historically built the LLVM return instruction
// directly without draining `scope_cleanup_actions`, so early returns
// (`if cond { return v; }` inside a function with tracked heap
// locals) leaked every tracked binding's heap content. Fixed by
// calling `emit_scope_cleanup()` before `build_return` and applying
// the same `suppress_source_vec_cleanup_for_arg` move-aware
// suppression on the return value that the function-end tail-return
// path already applies. ASAN catches both halves: leak (no free
// emitted on return path) and double-free (cleanup fires on the
// moved-out buffer the caller now owns).

#[test]
fn asan_early_return_cleans_up_tracked_locals() {
    // The function has a tracked `Vec[i64]` local and exits via
    // `return 0` inside a conditional. Without the cleanup-on-return
    // fix, `v`'s data buffer would leak; ASAN reports it on exit.
    assert_clean_asan_run(
        r#"
fn process(short_circuit: bool) -> i64 {
    let mut v: Vec[i64] = Vec.new();
    v.push(1i64);
    v.push(2i64);
    v.push(3i64);
    if short_circuit {
        return 0;
    }
    v.len()
}
fn main() {
    let mut s = 0i64;
    let mut i = 0i64;
    while i < 5 {
        s = s + process(true);
        i = i + 1;
    }
    println(s);
}
"#,
        &["0"],
        "early_return_cleans_up_tracked_locals",
    );
}

// ── 491: tail-expression temp drops before block-local lets ──
//
// phase-6-runtime.md line 491 — "Tail-expression temporary scope —
// drop before block locals." The ordering rule is structural, not a
// special case: a block's let-bindings and the materialized temp of
// its tail expression share ONE scope-cleanup frame, pushed in
// program order (the lets first, the tail-expr temp last because it
// is later in source order). LIFO drain therefore frees the tail-
// expr temp BEFORE every block-local let — the same unified-stack
// mechanism pinned at IR level by
// `test_ir_defer_drop_interleave_emission_order` (tests/codegen.rs).
//
// The only mid-expression temporary codegen tracks today is the
// `ref T` Vec/String call-arg materialization (the `asan_ref_arg_*`
// family above). This test puts one in TAIL position (`slen(make())`
// with no trailing `;`) alongside a heap block-local `let v`, and
// asserts ASAN-clean. A regression that hoisted the tail temp to the
// outer scope, freed it against the wrong cap, or double-freed it
// against the block-local `v` would surface here (leak arm on Linux;
// UAF / double-free on macOS). The canonical MutexGuard *drop-order*
// observation from the spec's test plan awaits a `MutexGuard` type
// (mutex.kara is type-shape-only) and general method-chain temp
// tracking; this pins the rule for every temporary tracked today.
#[test]
fn asan_tail_expr_temp_coexists_with_block_local_let() {
    assert_clean_asan_run(
        r#"
fn make() -> String {
    let a = "tail ";
    let b = "temp";
    return a + b;
}

fn slen(s: ref String) {
    println(s.len());
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    v.push(3_i64);
    println(f"v={v.len()}");
    slen(make())
}
"#,
        &["v=3", "9"],
        "tail_expr_temp_coexists_with_block_local_let",
    );
}

#[test]
fn asan_fresh_return_builders_repeat() {
    // Phase C1b fresh-return transfer under ASAN: both sanctioned
    // tail shapes (SomeRoot `Some(head)` and RootLink `dummy.next`)
    // hand the b2 count-free chain to the caller at rc==1 per node.
    // A missed suppression (tail compensation inc / Some transfer
    // inc) leaks every chain head; an over-eager root cleanup
    // (free-walk instead of root-only / none) is an immediate ASAN
    // double-free when the caller's dec-drop walks the chain.
    // 100 iterations x two 100-node chains; sum(1..=100) = 5050.
    assert_clean_asan_run(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build_someroot(n: i64) -> Option[ListNode] {
    let head = ListNode { val: 1, next: None };
    let mut tail = head;
    let mut i = 2;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    Some(head)
}
fn build_rootlink(n: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 1;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.next
}
fn sum_chain(head: Option[ListNode]) -> i64 {
    let mut sum = 0;
    let mut cur = head;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 100 {
        total = total + sum_chain(build_someroot(100));
        total = total + sum_chain(build_rootlink(100));
        iter = iter + 1;
    }
    println(total);
}
"#,
        &["1010000"],
        "fresh_return_builders_repeat",
    );
}

// B-2026-07-31-11 — early `return` out of a provider body. The
// `karac_provider_pop` is now a cleanup action drained on every exit
// path; this gate exercises the RETURN edge with a heap-owning body
// local: the body's own frame must free the f-string String before the
// ProviderPop drains (a missed free is an LSan leak; a re-entrant or
// mis-ordered drain would double-free under ASAN). Looped so both the
// early-return and fall-through paths re-push onto a healthy provider
// stack — a dangling frame head from a skipped pop asserts in the
// runtime on the next push/dispatch. Pre-fix this program did not
// compile at all (verifier error), so the gate cannot pass vacuously.
#[test]
fn asan_provider_early_return_frees_body_local() {
    assert_clean_asan_run(
        r#"
trait Counter { fn get(ref self) -> i64; }
effect resource Ctr: Counter;
struct InMem { n: i64 }
impl Counter for InMem { fn get(ref self) -> i64 { self.n } }
fn read() -> i64 with reads(Ctr) { Ctr.get() }
fn heapy(flag: bool) -> i64 with reads(Ctr) {
    with_provider[Ctr](InMem { n: 5 }, || {
        let s = f"local-heap-payload-{read()}";
        if flag { return s.len(); }
        read()
    })
}
fn blocky(flag: bool) -> i64 with reads(Ctr) {
    providers { Ctr => InMem { n: 6 } } in {
        let s = f"block-heap-payload-{read()}";
        if flag { return s.len(); }
        read()
    }
}
fn main() with reads(Ctr) {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        n = n + heapy(true) + heapy(false) + blocky(true) + blocky(false);
        i = i + 1;
    }
    println(n);
}
"#,
        // (20 + 5 + 20 + 6) x 200 = 10200.
        &["10200"],
        "provider_early_return_frees_body_local",
    );
}

/// The Identifier carrier, and the LEAK side of the same coin: a fresh
/// String is allocated on every iteration and only the last one breaks, so
/// the iterations that fall through must still free theirs. Suppressing
/// unconditionally (or retracting the queued cleanup at compile time,
/// which is flow-insensitive) would leak those.
#[test]
fn asan_loop_break_binding_value_frees_unbroken_iterations() {
    assert_clean_asan_run_min_allocs(
        "fn pick() -> String {\n\
             \x20   let mut i: i64 = env.args().len() - 1;\n\
             \x20   loop {\n\
             \x20       i = i + 1;\n\
             \x20       let s = f\"row{i}\";\n\
             \x20       if i == 3 { break s }\n\
             \x20   }\n\
             }\n\
             fn main() { println(pick()); }\n",
        &["row3"],
        "loop-break-binding",
        5,
    );
}

/// B-2026-08-25-6 — a BRANCHING rvalue carrier. Both tails allocate, one
/// runs, and the per-iteration `scratch` node still has to be freed on the
/// breaking path as well as the two that fall through.
///
/// The recursion admits this branch only because EVERY tail manufactures
/// ownership; the mixed-tail counterpart stays refused at compile time, so
/// it cannot be exercised here and is pinned in `tests/codegen.rs`
/// instead.
#[test]
fn asan_loop_break_branching_rvalue_single_owner() {
    assert_clean_asan_run_min_allocs(
            "shared struct Node { v: i64 }\n\
             fn pick() -> Node {\n\
             \x20   let mut i: i64 = env.args().len() - 1;\n\
             \x20   loop {\n\
             \x20       i = i + 1;\n\
             \x20       let scratch = Node { v: i };\n\
             \x20       if i == 3 { break if scratch.v > 2 { Node { v: i * 11 } } else { Node { v: i } } }\n\
             \x20   }\n\
             }\n\
             fn main() { println(pick().v); }\n",
            &["33"],
            "loop-break-branching-rvalue",
            4,
        );
}

/// B-2026-08-28-22 — the MEMORY half of the conditionally-returned-param
/// body fix. The codegen and interpreter twins pin the body COUNTS; this
/// pins that recovering those bodies moved no free.
///
/// The registration is deliberately BODIES-ONLY
/// (`emit_struct_user_drop_bodies_only_fn`): an owned by-value param is
/// caller-drops for MEMORY whoever runs the body, so the callee must run
/// the body and free nothing. Installing the binding's own `Drop` wrapper
/// instead — which frees the fields too — double-freed a heap-carrying
/// param, measured as `free(): double free detected in tcache 2` under the
/// JIT on the `cond` shape below.
///
/// That is why every struct here carries a `String` rather than the
/// `i64`-only struct the body tests use: an `i64` payload has nothing to
/// double-free, so the plain-wrapper version of this fix passed every
/// body-count case in the tree while corrupting the heap. Both directions
/// of each branch are exercised — the path that RETURNS the param must
/// free at the caller and nowhere else, the path that does not must free
/// inside the callee having just run the body there.
///
/// `agg` is the aggregate-literal route. It was the DECLINED one when this
/// was written (admitting it then ran one object's body twice), so the
/// `false` cell expected NO body for the dying param; B-2026-09-02-4
/// admits it now that the return-site retraction is aggregate-aware and
/// per path, so that cell runs `drop k1` inside the callee exactly once.
/// It must stay memory-clean either way.
#[test]
fn asan_conditionally_returned_param_bodies_are_memory_balanced() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.name}"); } }
struct Wrap { r: R }
fn cond(r: R, k: bool) -> R { if k { r } else { R { id: 99, name: f"n99" } } }
fn cond_match(r: R, k: i64) -> R { match k { 1 => { r } _ => { R { id: 98, name: f"n98" } } } }
fn two(a: R, b: R, k: bool) -> R { if k { a } else { b } }
fn agg(r: R, k: bool) -> Wrap { if k { Wrap { r: r } } else { Wrap { r: R { id: 95, name: f"n95" } } } }
fn main() {
    let base: i64 = env.args().len();
    let a = cond(R { id: base, name: f"a{base}" }, false);
    println(f"{a.id}");
    let b = cond(R { id: base, name: f"b{base}" }, true);
    println(f"{b.id}");
    let c = cond_match(R { id: base, name: f"c{base}" }, 2);
    println(f"{c.id}");
    let d = cond_match(R { id: base, name: f"d{base}" }, 1);
    println(f"{d.id}");
    let g = two(R { id: base, name: f"g{base}" }, R { id: base, name: f"h{base}" }, true);
    println(f"{g.id}");
    let i = agg(R { id: base, name: f"j{base}" }, true);
    println(f"{i.r.id}");
    let j = agg(R { id: base, name: f"k{base}" }, false);
    println(f"{j.r.id}");
    cond(R { id: base, name: f"l{base}" }, false);
    println("end");
}
"#,
        &[
            "drop a1", "99", "drop n99", "1", "drop b1", "drop c1", "98", "drop n98", "1",
            "drop d1", "drop h1", "1", "drop g1", "1", "drop j1", "drop k1", "95", "drop n95",
            "drop l1", "drop n99", "end",
        ],
        "conditionally-returned-param-bodies",
    );
}

/// B-2026-08-29-14 — a METHOD that hands its owned param back with
/// `return r;` rather than as a BLOCK TAIL is memory-balanced.
///
/// The behaviour twin is
/// `e2e_return_spelling_method_returned_param_body_runs_once`. This side
/// pins the claim that made removing a body safe: pre-fix the `return`
/// spelling ran the user `Drop` body TWICE on ONE object while the heap
/// payload was still freed exactly once, so the defect was a spurious body
/// and not a double free. Measured under valgrind at `KARAC_OPT_LEVEL=0`,
/// the `return` spelling went from 13 allocs / 13 frees to 12 / 12 — the
/// SAME count as the block-tail and free-function forms of the identical
/// program, which is the shape of a removed body rather than a new leak.
///
/// EVERY param here is named DISTINCTLY (`ra`, `rb`, ... `rg`) on purpose.
/// The interpreter shares one moved-out name space across method frames
/// (B-2026-08-29-11), so reusing `r` across these methods makes an EARLIER
/// method's legitimate move-out suppress a LATER frame's body — measured as
/// a lost `drop` under `--interp` that renaming one param alone restores.
/// That is a different row's defect, and naming around it keeps this
/// fixture measuring this one.
#[test]
fn asan_return_spelling_method_param_is_memory_balanced() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.name}"); } }
struct T1 { n: i64 }
struct Hh { r: R }
impl T1 {
    fn take(ref self, ra: R) -> R { return ra; }
    fn keep(ref self, rb: R) -> R { rb }
    fn wrap(ref self, rc: R) -> Hh { return Hh { r: rc }; }
    fn gtake[X](ref self, rd: R, x: X) -> R { return rd; }
    fn swapout(ref self, re: R) -> R { println(f"saw {re.name}"); return R { id: 99, name: f"z9" }; }
    fn eat(ref self, rf: R, k: bool) { if k { return; } println(f"kept {rf.name}"); }
}
fn take_free(rg: R) -> R { return rg; }
fn main() {
    let base: i64 = env.args().len();
    let t = T1 { n: 1 };
    let a = t.take(R { id: base, name: f"a{base}" });
    println(f"{a.id}");
    let b = t.keep(R { id: base, name: f"b{base}" });
    println(f"{b.id}");
    let c = t.wrap(R { id: base, name: f"c{base}" });
    println(f"{c.r.id}");
    let d = t.gtake(R { id: base, name: f"d{base}" }, 5);
    println(f"{d.id}");
    let e = t.swapout(R { id: base, name: f"e{base}" });
    println(f"{e.id}");
    t.eat(R { id: base, name: f"f{base}" }, true);
    let g = take_free(R { id: base, name: f"g{base}" });
    println(f"{g.id}");
    t.take(R { id: base, name: f"h{base}" });
    println("end");
}
"#,
        &[
            "1", "drop a1", "1", "drop b1", "1", "drop c1", "1", "drop d1", "saw e1", "drop e1",
            "99", "drop z9", "drop f1", "1", "drop g1", "drop h1", "end",
        ],
        "return-spelling-method-param",
    );
}

/// B-2026-08-29-15, extended by B-2026-08-29-50 — a NAMED binding passed
/// by value to a callee that hands it back, AS ITSELF or wrapped in a
/// returned aggregate, runs one body and is memory-balanced.
///
/// The behaviour twin is `e2e_named_arg_returned_bare_user_drop_body_runs_once`.
/// This side pins the claim that made removing a body safe rather than a
/// new leak, for both spellings:
///
///  * `take`/`keep`/`takef` — the param handed back AS ITSELF. The result
///    binding owns the very same object, so the caller's own body is a
///    duplicate.
///  * `wrap` — the param moved into a RETURNED AGGREGATE. This case
///    asserted TWO bodies until B-2026-08-29-50, on the reasoning that the
///    aggregate is a NEW owner while the caller still owns the original, so
///    retracting the caller's `karac_drop_<T>` wrapper would orphan a
///    buffer — a 3-byte definite leak that had actually been measured.
///    What that reasoning missed is that the stand-down no longer retracts
///    the wrapper: `suppress_user_drop_body_keeping_memory` DOWNGRADES it
///    to the field-cleanup-only `__karac_drop_struct_<T>`, so the body goes
///    and the free stays. Re-measured with the downgrade in place, this
///    file's own ASAN+LSan gate passes and valgrind reports `13 allocs / 13
///    frees, 0 errors` — identical to the bare spelling. The two owners are
///    PARALLEL, not nested: an own-heap struct passed by value is already
///    deep-copied, so the caller's slot has a distinct buffer to free.
///
/// The pin still runs in the leak direction: if the stand-down is ever
/// widened back to the REMOVING form, `wrap` orphans its buffer and LSan
/// fails this test rather than the output comparison.
///
/// Every param is named DISTINCTLY (`ra`..`rf`) for the reason the
/// `return`-spelling sibling above gives: the interpreter shares one
/// moved-out name space across method frames, so reusing `r` would let one
/// frame's move-out suppress another's body and change what is measured.
#[test]
fn asan_named_arg_returned_bare_is_memory_balanced() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.name}"); } }
struct T3 { n: i64 }
struct Hx { r: R }
impl T3 {
    fn take(ref self, ra: R) -> R { return ra; }
    fn keep(ref self, rb: R) -> R { rb }
    fn wrap(ref self, rc: R) -> Hx { return Hx { r: rc }; }
    fn eat(ref self, rd: R) -> i64 { println(f"saw {rd.name}"); return 0; }
}
fn takef(re: R) -> R { return re; }
fn keepf(rf: R) -> R { rf }
fn main() {
    let base: i64 = env.args().len();
    let t = T3 { n: 1 };
    let a1 = R { id: base, name: f"a{base}" };
    let a = t.take(a1);
    println(f"{a.id}");
    let b1 = R { id: base, name: f"b{base}" };
    let b = t.keep(b1);
    println(f"{b.id}");
    let c1 = R { id: base, name: f"c{base}" };
    let c = t.wrap(c1);
    println(f"{c.r.id}");
    let d1 = R { id: base, name: f"d{base}" };
    let d = t.eat(d1);
    println(f"{d}");
    let e1 = R { id: base, name: f"e{base}" };
    let e = takef(e1);
    println(f"{e.id}");
    let g1 = R { id: base, name: f"g{base}" };
    let g = keepf(g1);
    println(f"{g.id}");
    println("end");
}
"#,
        &[
            "1", "drop a1", "1", "drop b1", "1", "drop c1", "saw d1", "drop d1", "0", "1",
            "drop e1", "1", "drop g1", "end",
        ],
        "named-arg-returned-bare",
    );
}

/// B-2026-08-29-50 — a named binding passed to a CONDITIONAL callee, one
/// that hands the argument back on some paths and lets it die inside on the
/// rest, is memory-balanced with exactly one body per object on both paths.
///
/// This is the shape where getting the gate wrong fails in BOTH directions,
/// which is why it earns a memory pin of its own rather than a row in the
/// fixture above. Pre-fix the caller fired on every path, so `k = true`
/// doubled against the CALLEE's own registration
/// (`cond_returned_param_drop_names`) and `k = false` doubled against the
/// RESULT BINDING — two different second owners, so neither "stand the
/// caller down" nor "stop the callee registering" fixes it alone. Stand the
/// caller down UNCONDITIONALLY instead and `k = true` loses its body
/// entirely, the regression B-2026-08-28-22 measured. The gate that works
/// is `fn_conditionally_returns_param_bare`, the same predicate that hands
/// the body to the callee — so the caller stands down exactly when someone
/// else has picked the body up.
///
/// `T4.pick`'s non-param exit interpolates `self.n` deliberately. That made
/// `may_mention` answer conservatively `true` and the predicate decline, and
/// the two backends then reached different answers by different routes: the
/// interpreter ran two bodies for `a1` where the compiled backends ran one,
/// a RUN-VS-BUILD divergence that predates this row (measured on the
/// pre-fix compiler, where the free-function rows below still doubled on
/// both backends alike). Teaching `may_mention` that a method's receiver is
/// never one of its `params` closes it, so this row pins that too — drop
/// the `SelfValue` arm and the interpreter and AOT legs disagree again.
#[test]
fn asan_conditionally_returned_arg_is_memory_balanced() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.name}"); } }
struct T4 { n: i64 }
impl T4 {
    fn pick(ref self, ra: R, k: bool) -> R { if k { return R { id: 98, name: f"z{self.n}" }; } ra }
}
fn pickf(rb: R, k: bool) -> R { if k { return R { id: 98, name: f"yy" }; } rb }
fn main() {
    let base: i64 = env.args().len();
    let t = T4 { n: 1 };
    let a1 = R { id: base, name: f"a{base}" };
    let x = t.pick(a1, true);
    println(f"{x.id}");
    let b1 = R { id: base, name: f"b{base}" };
    let y = t.pick(b1, false);
    println(f"{y.id}");
    let c1 = R { id: base, name: f"c{base}" };
    let z = pickf(c1, true);
    println(f"{z.id}");
    let d1 = R { id: base, name: f"d{base}" };
    let w = pickf(d1, false);
    println(f"{w.id}");
    println("end");
}
"#,
        &[
            "drop a1", "98", "drop z1", "1", "drop b1", "drop c1", "98", "drop yy", "1", "drop d1",
            "end",
        ],
        "conditionally-returned-arg",
    );
}

/// B-2026-08-30-13 — an assignment whose RHS is a VALUE-POSITION BLOCK did
/// not free the value it overwrote. `s = { t };` stores `t`'s buffer into
/// `s` exactly as the bare `s = t;` spelling does, but the Assign arm's two
/// alias predicates matched on the RHS `ExprKind` directly, so a block
/// wrapper made the statement neither a self- nor a moved-alias and
/// `trigger_eager_free` never fired.
///
/// The reason it is one RHS SHAPE rather than a missing mechanism, and the
/// reason the fix is three lines: `rhs_yields_fresh_ref` ALREADY peels the
/// same wrappers, so `s = { mkB(n) }` was clean the whole time and only the
/// alias spelling leaked. The neighbours `s = mkB(n)` and `s = t` are both
/// clean at both opt levels and are pinned below so a fix that widens the
/// wrong way is caught here rather than in a kata.
///
/// `s3 = { s3 }` IS THE LOAD-BEARING CASE, and it is a control, not a leak:
/// widening the moved-alias predicate through the block WITHOUT widening the
/// self-alias predicate alongside it frees the very buffer the statement is
/// about to store back — a use-after-free traded for a leak, and the one way
/// this fix can go wrong. It is clean before and after; only a half-fix
/// breaks it.
///
/// COVERAGE, stated because it is narrower than the assertion looks:
/// measured against the pre-fix compiler this leaks **138 B in 7 blocks** at
/// `KARAC_OPT_LEVEL=0` (30 allocs / 23 frees) — 15 B for the displaced
/// `s1`, 40 B for the displaced `s2`, and 83 B across the loop's five
/// displaced generations — and is CLEAN at the default `-O2` BOTH before and
/// after, where the optimizer folds every one of those allocations away (20
/// allocs, 20 frees, identical on both compilers). The loop was added
/// specifically to try to defeat that fold and does not: an opaque loop
/// bound was tried too and LLVM still constant-folds the whole thing. So the
/// memory half of this fixture is carried ENTIRELY by the `-O0` leg
/// (`scripts/asan-o0-leg.sh`, B-2026-08-04-17); at `-O2` it asserts only the
/// accumulated output, which still catches a leak traded for a wrong value
/// or for a use-after-free.
#[test]
fn asan_block_rhs_assignment_frees_the_value_it_overwrites() {
    assert_clean_asan_run(
        r#"
fn mkA(n: i64) -> String { return f"AAAAAAAAAAAAAA{n}"; }
fn mkB(n: i64) -> String { return f"BBBBBBBBBBBBBB{n}"; }

fn main() {
    let t1 = mkB(1);
    let mut s1 = mkA(2);
    s1 = { t1 };
    println(f"s1={s1}");

    let t2: Vec[i64] = vec![10, 20, 30];
    let mut s2: Vec[i64] = vec![1, 2, 3, 4, 5];
    s2 = { t2 };
    println(f"s2len={s2.len()} s2head={s2[0]}");

    let mut s3 = mkA(3);
    s3 = { s3 };
    println(f"s3={s3}");

    let mut s4 = mkA(4);
    s4 = mkB(4);
    let t5 = mkB(5);
    let mut s5 = mkA(5);
    s5 = t5;
    println(f"s4={s4} s5={s5}");

    let mut acc = mkA(0);
    let mut i = 1;
    while i < 6 {
        let ti = mkA(i * 100);
        acc = { ti };
        i = i + 1;
    }
    println(f"acc={acc}");
}
"#,
        &[
            "s1=BBBBBBBBBBBBBB1",
            "s2len=3 s2head=10",
            "s3=AAAAAAAAAAAAAA3",
            "s4=BBBBBBBBBBBBBB4 s5=BBBBBBBBBBBBBB5",
            "acc=AAAAAAAAAAAAAA500",
        ],
        "b30-13-block-rhs-assign-old-value-free",
    );
}

/// B-2026-09-24-29 — an `if let` whose value is an f-string. Its THEN arm
/// hand-rolls its frame instead of going through `compile_block_with_frame`,
/// so nothing zeroed the tail accumulator's `cap`: the arm's drain freed the
/// buffer the construct's value had just loaded, and the consumer freed it
/// again. On `main` that double freed on every compiled surface whatever the
/// scrutinee (`Some(7)` included), as a `let`, a function tail, a block
/// tail, an `else if` chain, a loop body, a `Vec.push` argument and before
/// an early `return`. With the arm disarmed, an `if let` in argument or
/// receiver position was left owned by nobody, so it is now admitted as a
/// fresh owned branch wrapper beside `if` and `match`. The discarded
/// spellings (a statement, `let _ =`) must stay clean.
#[test]
fn asan_if_let_fstring_value_is_freed_once() {
    assert_clean_asan_run(
        r#"enum E { A(String), B(i64) }
fn mk(n: i64) -> String { f"heap-string-longer-than-sso-{n}" }
fn tail(doc: Option[String]) -> String { if let Some(s) = doc { f"x {s}" } else { f"none" } }
fn kind(e: E) -> String { if let E.A(s) = e { f"a {s}" } else { f"b" } }
fn early(o: Option[i64]) -> String { let r = if let Some(s) = o { f"x {s}" } else { return f"early" }; r }
fn show(s: String) { println(s) }
fn main() {
    let doc = Some(7);
    let r = if let Some(s) = doc { f"x" } else { f"none" };
    println(r);
    let h = Some(f"heap-string-longer-than-sso-1");
    let r2 = if let Some(s) = h { f"x {s}" } else { f"none" };
    println(r2);
    println(tail(Some(mk(2))));
    println(tail(None));
    let r3 = if let Some(s) = doc { let k = s + 1; f"x {k}" } else { f"none" };
    println(r3);
    if let Some(s) = doc { f"x {s}" } else { f"none" };
    let _ = if let Some(s) = doc { f"x {s}" } else { f"none" };
    let mut n = 0;
    for i in 0..5 { let o = if i % 2 == 0 { Some(i) } else { None }; let q = if let Some(s) = o { mk(s) } else { f"none" }; n = n + q.len(); }
    println(n);
    println(if let Some(s) = doc { f"x {s}" } else { f"none" });
    let no: Option[i64] = None;
    println(if let Some(s) = no { f"x {s}" } else { f"none" });
    println((if let Some(s) = doc { f"x {s}" } else { f"none" }).len());
    show(if let Some(s) = doc { f"x {s}" } else { f"none" });
    println(if let Some(s) = no { f"x {s}" } else if let Some(t) = Some(3) { f"y {t}" } else { f"none" });
    let mut v: Vec[String] = [];
    for i in 0..3 { let o = Some(i); v.push(if let Some(s) = o { mk(s) } else { f"none" }); }
    println(v[2]);
    println(kind(E.A(mk(3))));
    println(kind(E.B(2)));
    println(early(Some(1)));
    println(early(None));
    println((if let Some(s) = doc { mk(s) } else { mk(0) }).len());
    println("end")
}
"#,
        &[
            "x",
            "x heap-string-longer-than-sso-1",
            "x heap-string-longer-than-sso-2",
            "none",
            "x 8",
            "95",
            "x 7",
            "none",
            "3",
            "x 7",
            "y 3",
            "heap-string-longer-than-sso-2",
            "a heap-string-longer-than-sso-3",
            "b",
            "x 1",
            "early",
            "29",
            "end",
        ],
        "asan_if_let_fstring_value_is_freed_once",
    );
}
