//! match, arms, if/while let, destructuring -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer patterns::
//!
//! New fixtures about match, arms, if/while let, destructuring belong in this file.

use super::*;

/// B-2026-09-03-25 — A WILDCARD TUPLE LEAF OVER AN `Option`/`Result`
/// ELEMENT TAKES THE PAYLOAD'S `Drop` BODY WITHOUT TAKING ITS MEMORY.
///
/// `run_discarded_leaf_user_drop_bodies` selects every walker by the type's
/// NAME, and the built-in `Option`/`Result` carry their payload in a
/// generic argument, so the leaf found no walker and the payload's body was
/// owned by nobody: `let t = (mk(10), Option.Some(mk(110))); let (r, _) = t;`
/// ran `dR110` under `--interp` and on no compiled surface.
///
/// WHY THIS CASE IS IN THE SANITIZER SUITE AND NOT ONLY IN THE TRANSCRIPT
/// PINS. The fix ADDS an owner for the body, and the neighbouring row
/// B-2026-09-03-15 measured what that costs when the memory half is missing
/// — its `Result` leaf ran the due body and leaked 272 bytes in 9
/// allocations. This arm avoids that by construction rather than by luck:
/// on a PLACE source it takes the body only, and the source aggregate's own
/// drop still frees the element (this arm never cap-zeroes). That is a
/// claim about ownership, and LSan is what checks it — a transcript pin
/// cannot tell a balanced free from a leaked one, and the `dR` lines are
/// identical either way.
///
/// THE `resw` CELL IS THE ONE THAT WOULD BREAK FIRST if the arm ever took
/// memory too: `Result` has no `track_inline_result_payload_var` peer for a
/// boxed payload, which is exactly why B-2026-09-03-15 declined it on the
/// BINDING arm. It is safe HERE only because `free_memory` is false.
///
/// THE CONTROLS MUST NOT MOVE, and the risk they cover is a DOUBLE free
/// rather than a leak — the source's walk reaching an element the leaf also
/// took:
/// - `bind`  — the binding leaf B-2026-09-03-15 fixed, through different
///   machinery; it must keep its single body and stay balanced.
/// - `bothstruct` — two plain struct wildcards, which already worked and
///   proves the source's own walk does not double-fire here.
/// - `optstr` — `Option[String]`, an INLINE payload with no user `Drop`.
///   Nothing is owed, and it stayed clean across the fix, which is what
///   isolates the boxed-payload arm rather than the hand-off itself.
///
/// Every body renders `s` and `xs.len()`, so a body run against a
/// cap-zeroed husk prints `dR110::0` and fails the transcript rather than
/// passing as a bare count.
#[test]
fn asan_wildcard_tuple_leaf_over_optres_takes_body_not_memory() {
    assert_clean_asan_run(
            "struct R { id: i64, s: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}:{self.s}:{self.xs.len()}\") } }\n\
             fn mk(n: i64) -> R { return R { id: n, s: f\"s{n}\", xs: [n, n] }; }\n\
             fn wr() { let t = (mk(10), Option.Some(mk(110))); let (r, _) = t; println(f\"rd{r.id}\") }\n\
             fn wboth() { let t = (mk(12), Option.Some(mk(112))); let (_, _) = t; println(\"wb\") }\n\
             fn resw() { let t: (R, Result[R, String]) = (mk(40), Result[R, String].Ok(mk(140))); let (r, _) = t; println(f\"rw{r.id}\") }\n\
             fn nestw() { let t = ((mk(70), Option.Some(mk(170))), 3); let ((_, _), n) = t; println(f\"nw{n}\") }\n\
             fn bind() { let t = (mk(21), Option.Some(mk(221))); let (a, b) = t; println(f\"bd{a.id}\") }\n\
             fn bothstruct() { let t = (mk(50), mk(150)); let (_, _) = t; println(\"bs\") }\n\
             fn optstr() { let t = (mk(60), Option.Some(f\"x60\")); let (r, _) = t; println(f\"os{r.id}\") }\n\
             fn main() { wr(); wboth(); resw(); nestw(); bind(); bothstruct(); optstr(); }\n",
            &[
                "dR110:s110:2",
                "rd10",
                "dR10:s10:2",
                "dR12:s12:2",
                "dR112:s112:2",
                "wb",
                "dR140:s140:2",
                "rw40",
                "dR40:s40:2",
                "dR70:s70:2",
                "dR170:s170:2",
                "nw3",
                "dR221:s221:2",
                "bd21",
                "dR21:s21:2",
                "dR50:s50:2",
                "dR150:s150:2",
                "bs",
                "os60",
                "dR60:s60:2",
            ],
            "b25-wildcard-optres-leaf",
        );
}

/// B-2026-09-04-1's probe (B-2026-09-03-22 follow-up) — a `Result` tuple
/// destructure leaf whose consuming arm MOVES the binding out.
///
/// The B-2026-09-03-22 suppressor zeroed one word of the leaf's payload — the
/// box pointer, for the seven-word payload its own fixture used — and `R { id,
/// tag: String }` is four words, laid inline, so `id` was cleared and the
/// String's `{ptr,len,cap}` stayed live: `call`, `inner` and `esc` all aborted
/// with glibc's double-free on an ordinary build; `read` was clean only because
/// the borrow classifier skips the suppressor for a read-only arm. `errc` is
/// the `Err` side; the `w*` cells are the boxed twin, which was always correct
/// and pins that the wider zero does not disturb it.
///
/// ASAN twin of `e2e_result_agg_leaf_moving_arm_zeroes_the_whole_payload` (tests/codegen.rs): the cells that aborted did so
/// on a double free, and the fresh-source cells leaked, so the pin here is
/// the balance itself; the stdout expectation is the same as the E2E's.
#[test]
fn asan_result_agg_leaf_moving_arm_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct W { id: i64, x: String, y: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.x}{self.y}") } }
fn mkw(n: i64) -> W { return W { id: n, x: f"x{n}", y: f"y{n}" }; }
fn eat(x: R) { println(f"  eat{x.id}") }
fn eatw(w: W) { println(f"  eatw{w.id}") }

fn read()   { let t: (R, Result[R, String]) = (mk(1), Result.Ok(mk(101))); let (a, b) = t;
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn call()   { let t: (R, Result[R, String]) = (mk(2), Result.Ok(mk(102))); let (a, b) = t;
              match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") } }
fn inner()  { let t: (R, Result[R, String]) = (mk(3), Result.Ok(mk(103))); let (a, b) = t;
              match b { Result.Ok(r) => { let g = r; println(f"  got{g.id}") }, Result.Err(e) => println(f"  er{e}") } }
fn esc()    { let t: (R, Result[R, String]) = (mk(4), Result.Ok(mk(104))); let (a, b) = t;
              let g = match b { Result.Ok(r) => r, Result.Err(_) => mk(0) }; println(f"  got{g.id}") }
fn errc()   { let t: (R, Result[String, R]) = (mk(5), Result.Err(mk(105))); let (a, b) = t;
              match b { Result.Ok(s) => println(f"  ok{s}"), Result.Err(r) => eat(r) } }
fn wread()  { let t: (R, Result[W, String]) = (mk(6), Result.Ok(mkw(106))); let (a, b) = t;
              match b { Result.Ok(w) => println(f"  ok{w.id}"), Result.Err(e) => println(f"  er{e}") } }
fn winner() { let t: (R, Result[W, String]) = (mk(7), Result.Ok(mkw(107))); let (a, b) = t;
              match b { Result.Ok(w) => { let g = w; println(f"  got{g.id}") }, Result.Err(e) => println(f"  er{e}") } }
fn wesc()   { let t: (R, Result[W, String]) = (mk(8), Result.Ok(mkw(108))); let (a, b) = t;
              let g = match b { Result.Ok(w) => w, Result.Err(_) => mkw(0) }; println(f"  got{g.id}") }

fn main() {
  println("read");   read()
  println("call");   call()
  println("inner");  inner()
  println("esc");    esc()
  println("errc");   errc()
  println("wread");  wread()
  println("winner"); winner()
  println("wesc");   wesc()
  println("done")
}
"#,
        &[
            "read",
            "dR1/t1",
            "  ok101",
            "dR101/t101",
            "call",
            "dR2/t2",
            "  eat102",
            "dR102/t102",
            "inner",
            "dR3/t3",
            "  got103",
            "dR103/t103",
            "esc",
            "dR4/t4",
            "  got104",
            "dR104/t104",
            "errc",
            "dR5/t5",
            "  eat105",
            "dR105/t105",
            "wread",
            "dR6/t6",
            "  ok106",
            "dW106/x106y106",
            "winner",
            "dR7/t7",
            "  got107",
            "dW107/x107y107",
            "wesc",
            "dR8/t8",
            "  got108",
            "dW108/x108y108",
            "done",
        ],
        "asan_result_agg_leaf_moving_arm_is_balanced",
    );
}

/// B-2026-09-06-32 — the MEMORY half, and the load-bearing pin, of
/// `e2e_enum_leaf_handed_back_out_of_a_destructure` (tests/codegen.rs): the
/// row is a double free (`free(): double free detected in tcache 2` on the
/// JIT and at -O0) of an enum leaf's payload, freed once by the source's
/// drop and once by the returned value's owner, on the `let`-destructure and
/// bare-tuple-arm hand-out paths. Both paths now transfer the leaf, and this
/// pins every cell of the fixture balanced under ASAN/LSan — the handed-out
/// leaf, the unconsumed one (which must now free itself exactly once), the
/// rebound and call-consumed ones, the struct-leaf twins, and the `if let` /
/// `let (e, n) = t` spellings. valgrind measured 0 errors at -O0 and -O2
/// before this landed as a test.
#[test]
fn asan_enum_leaf_handed_back_out_of_a_destructure_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H2 { e: E, n: i64 }
struct Hr { r: R, n: i64 }
fn consume_e(x: E) -> i64 { match x { E.A(r) => { return r.id; } E.B => { return 0; } } }

fn p_let(h: H2) -> E { let H2 { e, n } = h; return e; }
fn p_let_unused(h: H2) -> i64 { let H2 { e, n } = h; return n; }
fn p_let_rebind(h: H2) -> E { let H2 { e, n } = h; let k = e; return k; }
fn p_let_call(h: H2) -> i64 { let H2 { e, n } = h; return consume_e(e) + n; }
fn p_let_r(h: Hr) -> R { let Hr { r, n } = h; return r; }
fn p_match(h: H2) -> E { match h { H2 { e, n } => { return e; } } }
fn p_tuple(t: (E, i64)) -> E { match t { (e, n) => { return e; } } }
fn p_tuple_unused(t: (E, i64)) -> i64 { match t { (e, n) => { return n; } } }
fn p_tuple_call(t: (E, i64)) -> i64 { match t { (e, n) => { return consume_e(e) + n; } } }
fn p_tuple_r(t: (R, i64)) -> R { match t { (r, n) => { return r; } } }
fn p_tuple_iflet(t: (E, i64)) -> E { if let (e, n) = t { return e; } else { return E.B; } }
fn p_tuple_let(t: (E, i64)) -> E { let (e, n) = t; return e; }

fn main() {
    println("let/local"); let a1 = H2 { e: E.A(mk(1)), n: 10 }; let x1 = p_let(a1); println("  got"); let _ = x1;
    println("let/temp"); let x2 = p_let(H2 { e: E.A(mk(2)), n: 10 }); println("  got"); let _ = x2;
    println("let_unused/local"); let a3 = H2 { e: E.A(mk(3)), n: 10 }; let x3 = p_let_unused(a3); println(f"  got{x3}");
    println("let_rebind/local"); let a4 = H2 { e: E.A(mk(4)), n: 10 }; let x4 = p_let_rebind(a4); println("  got"); let _ = x4;
    println("let_call/local"); let a5 = H2 { e: E.A(mk(5)), n: 10 }; let x5 = p_let_call(a5); println(f"  got{x5}");
    println("let_r/local"); let a6 = Hr { r: mk(6), n: 10 }; let x6 = p_let_r(a6); println(f"  got{x6.id}");
    println("match/local"); let a7 = H2 { e: E.A(mk(7)), n: 10 }; let x7 = p_match(a7); println("  got"); let _ = x7;
    println("tuple/local"); let b1 = (E.A(mk(11)), 10); let y1 = p_tuple(b1); println("  got"); let _ = y1;
    println("tuple/temp"); let y2 = p_tuple((E.A(mk(12)), 10)); println("  got"); let _ = y2;
    println("tuple_unused/local"); let b3 = (E.A(mk(13)), 10); let y3 = p_tuple_unused(b3); println(f"  got{y3}");
    println("tuple_call/local"); let b4 = (E.A(mk(14)), 10); let y4 = p_tuple_call(b4); println(f"  got{y4}");
    println("tuple_r/local"); let b5 = (mk(15), 10); let y5 = p_tuple_r(b5); println(f"  got{y5.id}");
    println("tuple_iflet/local"); let b6 = (E.A(mk(16)), 10); let y6 = p_tuple_iflet(b6); println("  got"); let _ = y6;
    println("tuple_let/local"); let b7 = (E.A(mk(17)), 10); let y7 = p_tuple_let(b7); println("  got"); let _ = y7;
    println("end");
}
"#,
        &[
            "let/local",
            "  got",
            "  dE",
            "  dR1",
            "let/temp",
            "  got",
            "  dE",
            "  dR2",
            "let_unused/local",
            "  dE",
            "  dR3",
            "  got10",
            "let_rebind/local",
            "  got",
            "  dE",
            "  dR4",
            "let_call/local",
            "  dE",
            "  dR5",
            "  got15",
            "let_r/local",
            "  got6",
            "  dR6",
            "match/local",
            "  got",
            "  dE",
            "  dR7",
            "tuple/local",
            "  got",
            "  dE",
            "  dR11",
            "tuple/temp",
            "  got",
            "  dE",
            "  dR12",
            "tuple_unused/local",
            "  dE",
            "  dR13",
            "  got10",
            "tuple_call/local",
            "  dE",
            "  dR14",
            "  got24",
            "tuple_r/local",
            "  got15",
            "  dR15",
            "tuple_iflet/local",
            "  got",
            "  dE",
            "  dR16",
            "tuple_let/local",
            "  got",
            "  dE",
            "  dR17",
            "end",
        ],
        "asan_enum_leaf_handed_back_out_of_a_destructure_is_balanced",
    );
}

/// B-2026-09-16-12 — the MEMORY half of
/// `e2e_mixed_bind_and_wildcard_arm_keeps_the_unbound_payload_body`
/// (tests/codegen.rs), pinning the half the row says was never broken.
///
/// That row is BODIES-only: the husk's payload-bodies walker was retracted
/// wholesale when an arm consumed any Drop-bearing position, so a
/// wildcarded position's `Drop` body ran nowhere. The MEMORY channel beside
/// it — `suppress_destructured_enum_payload_cleanup`'s per-field cap-zeroing
/// — was already per-position and stayed correct throughout: 52 allocs / 52
/// frees, valgrind-clean at `KARAC_OPT_LEVEL=0`, measured on this exact
/// program both before and after the fix.
///
/// It is pinned anyway because the fix hands the husk back positions it had
/// stopped walking, and "run a body over a payload whose buffer the arm's
/// binding already freed" is the use-after-free that mistake would produce.
/// This is the cell that would catch it; the output assertion above it is
/// not, because a body reading freed bytes still prints something.
#[test]
fn asan_mixed_bind_and_wildcard_arm_payload_ownership_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}" } }
enum W2 { Two(R, R), None2 }
enum W3 { A(R, R), B(R), None3 }
fn take(r: R) -> i64 { return r.id; }
fn first_bound() -> i64 { let w: W2 = W2.Two(mk(1), mk(2)); match w { W2.Two(a, _) => { return a.id; } W2.None2 => { return 0; } } }
fn second_bound() -> i64 { let w: W2 = W2.Two(mk(3), mk(4)); match w { W2.Two(_, b) => { return b.id; } W2.None2 => { return 0; } } }
fn moved_out() -> i64 { let w: W2 = W2.Two(mk(5), mk(6)); match w { W2.Two(a, _) => { return take(a); } W2.None2 => { return 0; } } }
fn rebound() -> i64 { let w: W2 = W2.Two(mk(7), mk(8)); match w { W2.Two(a, _) => { let m: R = a; return m.id; } W2.None2 => { return 0; } } }
fn iflet() -> i64 { let w: W2 = W2.Two(mk(9), mk(10)); if let W2.Two(a, _) = w { return a.id; } return 0; }
fn both_wild() -> i64 { let w: W2 = W2.Two(mk(11), mk(12)); match w { W2.Two(_, _) => { return 1; } W2.None2 => { return 0; } } }
fn both_bound() -> i64 { let w: W2 = W2.Two(mk(13), mk(14)); match w { W2.Two(a, b) => { return a.id + b.id; } W2.None2 => { return 0; } } }
fn other_variant_live() -> i64 { let w: W3 = W3.B(mk(22)); match w { W3.A(a, _) => { return a.id; } W3.B(_) => { return 99; } W3.None3 => { return 0; } } }
fn each_variant_takes() -> i64 { let w: W3 = W3.A(mk(30), mk(31)); match w { W3.A(a, _) => { return a.id; } W3.B(c) => { return c.id; } W3.None3 => { return 0; } } }
fn main() {
    println("first_bound"); let a: i64 = first_bound(); println(f"  ={a}");
    println("second_bound"); let b: i64 = second_bound(); println(f"  ={b}");
    println("moved_out"); let c: i64 = moved_out(); println(f"  ={c}");
    println("rebound"); let d: i64 = rebound(); println(f"  ={d}");
    println("iflet"); let e: i64 = iflet(); println(f"  ={e}");
    println("both_wild"); let f: i64 = both_wild(); println(f"  ={f}");
    println("both_bound"); let g: i64 = both_bound(); println(f"  ={g}");
    println("other_variant_live"); let h: i64 = other_variant_live(); println(f"  ={h}");
    println("each_variant_takes"); let i: i64 = each_variant_takes(); println(f"  ={i}");
    println("end");
}
"#,
        &[
            "first_bound",
            "  dR2",
            "  dR1",
            "  =1",
            "second_bound",
            "  dR4",
            "  dR3",
            "  =4",
            "moved_out",
            "  dR5",
            "  dR6",
            "  =5",
            "rebound",
            "  dR7",
            "  dR8",
            "  =7",
            "iflet",
            "  dR10",
            "  dR9",
            "  =9",
            "both_wild",
            "  dR12",
            "  dR11",
            "  =1",
            "both_bound",
            "  dR14",
            "  dR13",
            "  =27",
            "other_variant_live",
            "  dR22",
            "  =99",
            "each_variant_takes",
            "  dR31",
            "  dR30",
            "  =30",
            "end",
        ],
        "mixed_bind_and_wildcard_arm_payload_ownership_is_balanced",
    );
}

/// B-2026-09-06-37 — the MEMORY half of
/// `e2e_wildcard_arm_over_owned_enum_receiver_runs_payload_body`
/// (tests/codegen.rs). The lowering-pass rewrite turns a wildcard payload
/// position of a `match` over an owned enum `self` into a never-read
/// binding, which moves the payload's MEMORY as well as its body: the
/// position becomes a consumed one, the receiver's payload words are zeroed
/// (`suppress_destructured_enum_payload_cleanup_at`), and the minted binding
/// frees them at the arm's end. This pins that hand-off balanced under
/// ASAN/LSan for every cell — one wildcard, two wildcards, a wildcard beside a
/// bound payload, the `if let` spelling, an enum without its own `Drop`, and
/// the local-scrutinee / by-value-param controls the rewrite must not touch.
/// valgrind measured 0 errors at -O0 and -O2 before this landed as a test.
///
/// B-2026-09-06-39 — the ASAN twin of
/// `tests/codegen.rs`'s `e2e_read_only_arm_on_owned_enum_receiver_orders_payload_after_shell`:
/// the row is an ORDER row, so what this adds is the proof that the re-homing
/// moved no free. Bodies changed hands (the arm's view-bound payload is now the
/// caller's to run), and two registrations are new — the caller's payload walk
/// on a named receiver, and the fresh-temp registrar's walk on a CHAIN-LINK
/// receiver — either of which would show here as a double free or a leak if it
/// had been paired with a memory action by mistake. Both are bodies-only.
#[test]
fn asan_read_only_arm_on_owned_enum_receiver_orders_payload_after_shell() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn eat(r: R) -> i64 { return r.id; }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
enum N { A(R), B }
struct W { e: E }
impl E {
    fn read(self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn wild(self) -> i64 { match self { E.A(_) => { return 1; } E.B => { return 0; } } }
    fn iflet(self) -> i64 { if let E.A(r) = self { return r.id; } return 0; }
    fn call(self) -> i64 { match self { E.A(r) => { return eat(r); } E.B => { return 0; } } }
    fn me(self) -> E { return self; }
    fn none(self) -> i64 { return 5 }
    fn letself(self) -> i64 { let e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn ret_self(self) -> E { return self }
    fn wrap(self) -> W { return W { e: self } }
    fn refm(ref self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
}
impl N {
    fn read(self) -> i64 { match self { N.A(r) => { return r.id; } N.B => { return 0; } } }
    fn out(self) -> R { match self { N.A(r) => { return r; } N.B => { return mk(0); } } }
}
fn f_read(e: E) -> i64 { match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
fn main() {
    println("named/read");   { let a: E = E.A(mk(1)); println(f"  x{a.read()}") }
    println("temp/read");    { println(f"  x{E.A(mk(2)).read()}") }
    println("named/wild");   { let a: E = E.A(mk(3)); println(f"  x{a.wild()}") }
    println("named/iflet");  { let a: E = E.A(mk(4)); println(f"  x{a.iflet()}") }
    println("chain/read");   { println(f"  x{E.A(mk(5)).me().read()}") }
    println("named/call");   { let a: E = E.A(mk(6)); println(f"  x{a.call()}") }
    println("named/none");   { let a: E = E.A(mk(7)); println(f"  x{a.none()}") }
    println("named/letself");{ let a: E = E.A(mk(8)); println(f"  x{a.letself()}") }
    println("ret_self");     { let a: E = E.A(mk(9)); let b: E = a.ret_self(); println("  got") }
    println("wrap");         { let a: E = E.A(mk(10)); let w: W = a.wrap(); println("  got") }
    println("refm");         { let a: E = E.A(mk(11)); println(f"  x{a.refm()}") }
    println("plain");        { let a: E = E.A(mk(12)); println("  x12") }
    println("param");        { let a: E = E.A(mk(13)); println(f"  x{f_read(a)}") }
    println("nodrop/read");  { let a: N = N.A(mk(14)); println(f"  x{a.read()}") }
    println("nodrop/out");   { let a: N = N.A(mk(15)); let r: R = a.out(); println(f"  x{r.id}") }
    println("end");
}
"#,
        &[
            "named/read",
            "  x1",
            "  dE",
            "  dR1",
            "temp/read",
            "  dE",
            "  dR2",
            "  x2",
            "named/wild",
            "  x1",
            "  dE",
            "  dR3",
            "named/iflet",
            "  x4",
            "  dE",
            "  dR4",
            "chain/read",
            "  dR5",
            "  x5",
            "named/call",
            "  dR6",
            "  x6",
            "  dE",
            "named/none",
            "  x5",
            "  dE",
            "  dR7",
            "named/letself",
            "  dE",
            "  dR8",
            "  x8",
            "ret_self",
            "  dE",
            "  dR9",
            "  dE",
            "  got",
            "wrap",
            "  dE",
            "  dR10",
            "  dE",
            "  got",
            "refm",
            "  x11",
            "  dE",
            "  dR11",
            "plain",
            "  dE",
            "  dR12",
            "  x12",
            "param",
            "  x13",
            "  dE",
            "  dR13",
            "nodrop/read",
            "  dR14",
            "  x14",
            "nodrop/out",
            "  x15",
            "  dR15",
            "end",
        ],
        "asan_read_only_arm_on_owned_enum_receiver_orders_payload_after_shell",
    );
}

/// B-2026-09-06-39 — REPINNED (ASAN itself unchanged: the mismatch this caught
/// was stdout only, with memory balanced before and after). The shell body now
/// precedes the payload bodies — `dE dR1`, and `dT` ahead of both `dR`s in
/// `half`/`both` — because a read-only bare-`self` arm over an enum with its own
/// `Drop` binds VIEWS and the caller owns the payload's body.
#[test]
fn asan_wildcard_arm_over_owned_enum_receiver_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
enum P { A(R), B }
enum T { A(R, R), B }
impl Drop for T { fn drop(mut ref self) { println("  dT") } }
impl E {
    fn m_wild(self) -> i64 { match self { E.A(_) => { return 1; } E.B => { return 0; } } }
    fn m_unit(self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn m_ifwild(self) -> i64 { if let E.A(_) = self { return 1; } else { return 0; } }
}
impl P { fn m_wild(self) -> i64 { match self { P.A(_) => { return 1; } P.B => { return 0; } } } }
impl T {
    fn m_half(self) -> i64 { match self { T.A(_, r) => { return r.id; } T.B => { return 0; } } }
    fn m_both(self) -> i64 { match self { T.A(_, _) => { return 2; } T.B => { return 0; } } }
}
fn f_wild(e: E) -> i64 { match e { E.A(_) => { return 1; } E.B => { return 0; } } }
fn main() {
    println("wild/local"); let a = E.A(mk(1)); let x = a.m_wild(); println(f"  x{x}");
    println("wild/temp"); let x2 = E.A(mk(2)).m_wild(); println(f"  x{x2}");
    println("bound/local"); let b = E.A(mk(3)); let y = b.m_unit(); println(f"  y{y}");
    println("ifwild/local"); let c = E.A(mk(4)); let z = c.m_ifwild(); println(f"  z{z}");
    println("ifwild/temp"); let z2 = E.A(mk(5)).m_ifwild(); println(f"  z{z2}");
    println("noshell/local"); let d = P.A(mk(6)); let w = d.m_wild(); println(f"  w{w}");
    println("noshell/temp"); let w2 = P.A(mk(7)).m_wild(); println(f"  w{w2}");
    println("half/local"); let g = T.A(mk(8), mk(108)); let v = g.m_half(); println(f"  v{v}");
    println("both/local"); let h = T.A(mk(9), mk(109)); let v2 = h.m_both(); println(f"  v{v2}");
    println("free/local"); let i = E.A(mk(10)); let u = f_wild(i); println(f"  u{u}");
    println("free/temp"); let u2 = f_wild(E.A(mk(11))); println(f"  u{u2}");
    println("localmatch"); let j = E.A(mk(12)); match j { E.A(_) => { println("  arm"); } E.B => { } }
    println("end");
}
"#,
        &[
            "wild/local",
            "  dE",
            "  dR1",
            "  x1",
            "wild/temp",
            "  dE",
            "  dR2",
            "  x1",
            "bound/local",
            "  dE",
            "  dR3",
            "  y3",
            "ifwild/local",
            "  dE",
            "  dR4",
            "  z1",
            "ifwild/temp",
            "  dE",
            "  dR5",
            "  z1",
            "noshell/local",
            "  dR6",
            "  w1",
            "noshell/temp",
            "  dR7",
            "  w1",
            "half/local",
            "  dT",
            "  dR108",
            "  dR8",
            "  v108",
            "both/local",
            "  dT",
            "  dR109",
            "  dR9",
            "  v2",
            "free/local",
            "  dE",
            "  dR10",
            "  u1",
            "free/temp",
            "  dE",
            "  dR11",
            "  u1",
            "localmatch",
            "  arm",
            "  dE",
            "  dR12",
            "end",
        ],
        "asan_wildcard_arm_over_owned_enum_receiver_is_balanced",
    );
}

/// B-2026-09-06-28 — an enum leaf bound out of an owned-struct-pattern
/// match arm and NEVER consumed frees its payload. These are the `shell`
/// cells the parent test (B-2026-09-06-15) deliberately omitted: a
/// `match self { H1 { e } => 9 }` binds `e` but the arm returns without
/// touching it, so the source field's cap-zero (needed for a consumed leaf)
/// left nobody freeing the payload — 10 B/call at `-O0`, before this fix, on
/// the by-value PARAM and free-function-param spellings alike. The fix
/// registers the leaf's memory-only `EnumDrop` from the struct-pattern
/// suppressor (where the leaf is already bound in the match path), so an
/// unconsumed leaf frees itself; a consumed one is retracted by the arm
/// body's move hooks, and the `let`-destructure path is untouched (its
/// suppressor runs BEFORE its bind, so the leaf is not yet there).
///
/// Method-receiver `self` (local and temp receiver) and the free-function
/// param, results bound to locals so the drop order is stable (an inline
/// receiver temp in a larger expression drains at a different point on the
/// two backends — B-2026-09-04-36, unrelated). The LOCAL-scrutinee spelling
/// is memory-clean too but still loses the leaf's Drop BODY on the compiled
/// backends, a separate run-vs-build split filed on its own row.
#[test]
fn asan_unconsumed_enum_leaf_of_owned_param_struct_pattern_freed() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H1 { e: E }
impl H1 { fn shell(self) -> i64 { match self { H1 { e } => { return 9; } } } }
fn p_shell(h: H1) -> i64 { match h { H1 { e } => { return 9; } } }
fn main() {
    println("recv/local"); let a1 = H1 { e: E.A(mk(31)) }; let x1 = a1.shell(); println(f"  r{x1}");
    println("recv/temp"); let x2 = H1 { e: E.A(mk(32)) }.shell(); println(f"  r{x2}");
    println("param/local"); let b1 = H1 { e: E.A(mk(33)) }; let y1 = p_shell(b1); println(f"  r{y1}");
    println("end");
}
"#,
        &[
            "recv/local",
            "  dE",
            "  dR31",
            "  r9",
            "recv/temp",
            "  dE",
            "  dR32",
            "  r9",
            "param/local",
            "  dE",
            "  dR33",
            "  r9",
            "end",
        ],
        "unconsumed_enum_leaf_of_owned_param_struct_pattern_freed",
    );
}

/// B-2026-09-03-22 — the BALANCE half of
/// `e2e_result_agg_destructure_leaf_owns_its_payload_body`, and the
/// assertion the row was actually blocked on.
///
/// Its `Option` sibling B-2026-09-03-15 measured what taking a `Result`
/// leaf costs without a memory owner: the due body ran and 272 bytes leaked
/// in 9 allocations, against an ASAN-clean run with the leaf declined. That
/// is why the leaf was declined for four months of rows rather than taken.
/// `track_inline_result_agg_payload_var` supplies the owner, and this case
/// is what says the owner is balanced rather than merely present.
///
/// `consok` AND `conserr` CARRY THE NEW MECHANISM. `Result` has no empty
/// tag, so an arm that binds the payload out cannot be neutralized the way
/// the `Option` twin is (store `None`); the suppressor zeroes the PAYLOAD
/// area instead and relies on `emit_result_drop_fn`'s null-box guard. Both
/// tag sides are exercised because the guard is per-arm.
///
/// `moved`, `ret` and `field` exercise the whole-value transfer disarm on
/// the new set — the failure B-2026-09-04-10 fixed for `Option`, which this
/// set would otherwise reproduce — and `arg` is the counter-case that must
/// NOT be disarmed: it does not transfer, so disarming it loses a body
/// instead. `loopr` runs two iterations so a per-iteration imbalance shows
/// as a multiple.
#[test]
fn asan_result_agg_destructure_leaf_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}:{self.s}:{self.xs.len()}") } }
struct W { p: Result[R, String] }
fn mk(n: i64) -> R { return R { id: n, s: f"s{n}", xs: [n, n] }; }
fn eat(xa: Result[R, String]) -> i64 { match xa { Result.Ok(ra) => { ra.id }, Result.Err(sa) => { 0 } } }
fn give() -> Result[R, String] { let (_, ob) = (mk(20), Result[R, String].Ok(mk(120))); return ob }

fn annres()  { let tc: (R, Result[R, String]) = (mk(7), Result[R, String].Ok(mk(77)));
               let (rc, oc) = tc; println(f"  a{rc.id}") }
fn unann()   { let td = (mk(1), Result[R, String].Ok(mk(11))); let (rd, od) = td; println(f"  u{rd.id}") }
fn fresh()   { let (re, oe) = (mk(3), Result[R, String].Ok(mk(33))); println(f"  h{re.id}") }
fn errside() { let tf: (R, Result[String, R]) = (mk(5), Result[String, R].Err(mk(55)));
               let (rf, of) = tf; println(f"  r{rf.id}") }
fn consok()  { let (_, og) = (mk(22), Result[R, String].Ok(mk(122)));
               match og { Result.Ok(rg) => { println(f"  k{rg.id}") }, Result.Err(sg) => { println("  e") } } }
fn conserr() { let (_, oh) = (mk(24), Result[String, R].Err(mk(124)));
               match oh { Result.Ok(sh) => { println("  o") }, Result.Err(rh) => { println(f"  c{rh.id}") } } }
fn moved()   { let (_, oi) = (mk(26), Result[R, String].Ok(mk(126))); let qi = oi; println("  m") }
fn ret()     { let zj = give(); println("  t") }
fn arg()     { let (_, ok2) = (mk(28), Result[R, String].Ok(mk(128))); println(f"  g{eat(ok2)}") }
fn field()   { let (_, ol) = (mk(30), Result[R, String].Ok(mk(130))); let wl = W { p: ol }; println("  f") }
fn loopr()   { let mut i = 0; while i < 2 { let (_, om) = (mk(32), Result[R, String].Ok(mk(132))); i = i + 1; } println("  l") }
fn nested()  { let ((_, on), nn) = ((mk(36), Result[R, String].Ok(mk(136))), 4); println(f"  n{nn}") }
fn wildres() { let (ro, _) = (mk(15), Result[R, String].Ok(mk(115))); println(f"  w{ro.id}") }
fn strres()  { let (rp, op) = (mk(17), Result[String, String].Ok(f"p17")); println(f"  s{rp.id}") }

fn main() {
  println("annres");  annres()
  println("unann");   unann()
  println("fresh");   fresh()
  println("errside"); errside()
  println("consok");  consok()
  println("conserr"); conserr()
  println("moved");   moved()
  println("ret");     ret()
  println("arg");     arg()
  println("field");   field()
  println("loopr");   loopr()
  println("nested");  nested()
  println("wildres"); wildres()
  println("strres");  strres()
  println("done")
}
"#,
        &[
            "annres",
            "dR77:s77:2",
            "  a7",
            "dR7:s7:2",
            "unann",
            "dR11:s11:2",
            "  u1",
            "dR1:s1:2",
            "fresh",
            "dR33:s33:2",
            "  h3",
            "dR3:s3:2",
            "errside",
            "dR55:s55:2",
            "  r5",
            "dR5:s5:2",
            "consok",
            "dR22:s22:2",
            "  k122",
            "dR122:s122:2",
            "conserr",
            "dR24:s24:2",
            "  c124",
            "dR124:s124:2",
            "moved",
            "dR26:s26:2",
            "dR126:s126:2",
            "  m",
            "ret",
            "dR20:s20:2",
            "dR120:s120:2",
            "  t",
            "arg",
            "dR28:s28:2",
            "  g128",
            "dR128:s128:2",
            "field",
            "dR30:s30:2",
            "dR130:s130:2",
            "  f",
            "loopr",
            "dR32:s32:2",
            "dR132:s132:2",
            "dR32:s32:2",
            "dR132:s132:2",
            "  l",
            "nested",
            "dR36:s36:2",
            "dR136:s136:2",
            "  n4",
            "wildres",
            "dR115:s115:2",
            "  w15",
            "dR15:s15:2",
            "strres",
            "  s17",
            "dR17:s17:2",
            "done",
        ],
        "b22-result-agg-leaf",
    );
}

/// B-2026-09-04-10 — the BALANCE assertion for the disarm that
/// `e2e_option_agg_destructure_leaf_move_disarms_its_source` pins as a
/// transcript.
///
/// The fix ZEROES a source slot so its `EnumDrop` skips, which is exactly
/// the shape that trades a double free for a leak if the reasoning about
/// who owns the payload afterwards is wrong. The transcript cannot tell
/// those apart — a leaked box and a handed-off one both print the same `dR`
/// lines — so the ownership claim is checked here.
///
/// The `arg` cell is the one to watch. Its payload is NOT transferred by
/// being passed, so the caller's slot must stay armed; the first draft of
/// this fix disarmed it too and lost `dR114:s114:2` outright. It is a
/// transcript failure rather than a leak, and it is pinned in the codegen
/// twin — but the reverse mistake, disarming too little, is a double free
/// that only this suite reports.
///
/// `loopreb` runs two iterations so a per-iteration imbalance shows as a
/// multiple rather than as one block.
#[test]
fn asan_option_agg_destructure_leaf_move_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}:{self.s}:{self.xs.len()}") } }
struct H { a: R, b: Option[R] }
fn mk(n: i64) -> R { return R { id: n, s: f"s{n}", xs: [n, n] }; }

fn eat(xa: Option[R]) -> i64 { match xa { Option.Some(ra) => { ra.id }, Option.None => { 0 } } }
fn giveback() -> Option[R] { let tb = (mk(3), Option.Some(mk(103))); let (_, ob) = tb; return ob }

fn rebind()  { let tc = (mk(2), Option.Some(mk(102))); let (_, oc) = tc; let qc = oc; println(f"  q{qc.is_some()}") }
fn ret()     { let zd = giveback(); println(f"  z{zd.is_some()}") }
fn twohop()  { let te = (mk(6), Option.Some(mk(106))); let (_, oe) = te; let qe = oe; let we = qe; println(f"  w{we.is_some()}") }
fn loopreb() { let mut i = 0;
               while i < 2 { let tf = (mk(7), Option.Some(mk(107))); let (_, of) = tf; let qf = of; i = i + 1; }
               println("  lr") }
fn slot0()   { let tg = (Option.Some(mk(8)), 5); let (og, kg) = tg; let qg = og; println(f"  k{kg}{qg.is_some()}") }
fn matched() { let th = (mk(4), Option.Some(mk(104))); let (_, oh) = th; match oh { Option.Some(rh) => { println(f"  g{rh.id}") }, Option.None => { println("  n") } } }
fn arg()     { let ti = (mk(14), Option.Some(mk(114))); let (_, oi) = ti; println(f"  e{eat(oi)}") }
fn stay()    { let tj = (mk(5), Option.Some(mk(105))); let (_, oj) = tj; println(f"  y{oj.is_some()}") }
fn plain()   { let (_, ok) = (mk(9), mk(109)); let qk = ok; println(f"  p{qk.id}") }
fn instr()   { let tl = (mk(15), Option.Some(f"p15")); let (_, ol) = tl; let ql = ol; println(f"  i{ql.is_some()}") }
fn fld()     { let hm = H { a: mk(13), b: Option.Some(mk(113)) }; let H { a, b } = hm; let qm = b; println(f"  f{qm.is_some()}") }
fn loc()     { let on = Option.Some(mk(11)); let qn = on; println(f"  o{qn.is_some()}") }

fn main() {
  println("rebind");  rebind()
  println("ret");     ret()
  println("twohop");  twohop()
  println("loopreb"); loopreb()
  println("slot0");   slot0()
  println("matched"); matched()
  println("arg");     arg()
  println("stay");    stay()
  println("plain");   plain()
  println("instr");   instr()
  println("fld");     fld()
  println("loc");     loc()
  println("done")
}
"#,
        &[
            "rebind",
            "dR2:s2:2",
            "  qtrue",
            "dR102:s102:2",
            "ret",
            "dR3:s3:2",
            "  ztrue",
            "dR103:s103:2",
            "twohop",
            "dR6:s6:2",
            "  wtrue",
            "dR106:s106:2",
            "loopreb",
            "dR7:s7:2",
            "dR107:s107:2",
            "dR7:s7:2",
            "dR107:s107:2",
            "  lr",
            "slot0",
            "  k5true",
            "dR8:s8:2",
            "matched",
            "dR4:s4:2",
            "  g104",
            "dR104:s104:2",
            "arg",
            "dR14:s14:2",
            "  e114",
            "dR114:s114:2",
            "stay",
            "dR5:s5:2",
            "  ytrue",
            "dR105:s105:2",
            "plain",
            "dR9:s9:2",
            "  p109",
            "dR109:s109:2",
            "instr",
            "dR15:s15:2",
            "  itrue",
            "fld",
            "dR13:s13:2",
            "  ftrue",
            "dR113:s113:2",
            "loc",
            "  otrue",
            "dR11:s11:2",
            "done",
        ],
        "b10-optres-agg-leaf-move",
    );
}

/// B-2026-09-06-24 — the read-only `if let` / `while let` payload binding
/// over a borrow projection is now a VIEW on the compiled side: this pins
/// that suppressing its slot frees nothing twice and leaks nothing (the
/// caller's original still owns the `String` / `Vec` buffers), and that
/// the escaping controls still take a real copy with one owner.
#[test]
fn asan_readonly_if_let_over_borrow_projection_view_is_not_freed() {
    assert_clean_asan_run(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, tag: f\"t{i}\", xs: [i] } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"  dE\") } }\n\
             struct S { e: E }\n\
             struct H { e: E }\n\
             struct H2 { s: S }\n\
             fn consume(x: R) -> i64 { return x.id }\n\
             \n\
             fn p_iflet(h: ref H) -> i64 { if let E.A(r) = h.e { return r.id; } else { return 0; } }\n\
             fn p_iflet_mut(h: mut ref H) -> i64 { if let E.A(r) = h.e { return r.id; } else { return 0; } }\n\
             fn p_iflet_assign(h: ref H) -> i64 { let mut t = 0; if let E.A(r) = h.e { t = r.id + 1; } return t; }\n\
             fn p_whilelet(h: ref H) -> i64 { while let E.A(r) = h.e { return r.id; } return 0; }\n\
             fn p_iflet2(h: ref H2) -> i64 { if let E.A(r) = h.s.e { return r.id; } else { return 0; } }\n\
             fn p_match(h: ref H) -> i64 { match h.e { E.A(r) => { return r.id; } E.B => { return 0; } } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn p_iflet_move(h: ref H) -> i64 { if let E.A(r) = h.e { let m = r; return m.id; } else { return 0; } }\n\
             fn p_iflet_consume(h: ref H) -> i64 { if let E.A(r) = h.e { return consume(r); } return 0; }\n\
             impl H {\n\
             \x20   fn m_iflet(ref self) -> i64 { if let E.A(r) = self.e { return r.id; } else { return 0; } }\n\
             \x20   fn m_iflet_mut(mut ref self) -> i64 { if let E.A(r) = self.e { return r.id; } else { return 0; } }\n\
             \x20   fn m_whilelet(ref self) -> i64 { while let E.A(r) = self.e { return r.id; } return 0; }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn m_iflet_move(ref self) -> i64 { if let E.A(r) = self.e { let m = r; return m.id; } else { return 0; } }\n\
             }\n\
             \n\
             fn main() {\n\
             \x20   println(\"p_iflet\"); let a1 = H { e: E.A(mk(1)) }; let x1 = p_iflet(a1); println(f\"  got{x1}\");\n\
             \x20   println(\"p_iflet_mut\"); let mut a2 = H { e: E.A(mk(2)) }; let x2 = p_iflet_mut(mut a2); println(f\"  got{x2}\");\n\
             \x20   println(\"p_iflet_assign\"); let a3 = H { e: E.A(mk(3)) }; let x3 = p_iflet_assign(a3); println(f\"  got{x3}\");\n\
             \x20   println(\"p_whilelet\"); let a4 = H { e: E.A(mk(4)) }; let x4 = p_whilelet(a4); println(f\"  got{x4}\");\n\
             \x20   println(\"p_iflet2\"); let a5 = H2 { s: S { e: E.A(mk(5)) } }; let x5 = p_iflet2(a5); println(f\"  got{x5}\");\n\
             \x20   println(\"p_match\"); let a6 = H { e: E.A(mk(6)) }; let x6 = p_match(a6); println(f\"  got{x6}\");\n\
             \x20   println(\"p_iflet_move\"); let a7 = H { e: E.A(mk(7)) }; let x7 = p_iflet_move(a7); println(f\"  got{x7}\");\n\
             \x20   println(\"p_iflet_consume\"); let a8 = H { e: E.A(mk(8)) }; let x8 = p_iflet_consume(a8); println(f\"  got{x8}\");\n\
             \x20   println(\"m_iflet\"); let a9 = H { e: E.A(mk(9)) }; let x9 = a9.m_iflet(); println(f\"  got{x9}\");\n\
             \x20   println(\"m_iflet_mut\"); let mut a10 = H { e: E.A(mk(10)) }; let x10 = a10.m_iflet_mut(); println(f\"  got{x10}\");\n\
             \x20   println(\"m_whilelet\"); let a11 = H { e: E.A(mk(11)) }; let x11 = a11.m_whilelet(); println(f\"  got{x11}\");\n\
             \x20   println(\"m_iflet_move\"); let a12 = H { e: E.A(mk(12)) }; let x12 = a12.m_iflet_move(); println(f\"  got{x12}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "p_iflet",
                "  dE",
                "  dR1",
                "  got1",
                "p_iflet_mut",
                "  dE",
                "  dR2",
                "  got2",
                "p_iflet_assign",
                "  dE",
                "  dR3",
                "  got4",
                "p_whilelet",
                "  dE",
                "  dR4",
                "  got4",
                "p_iflet2",
                "  dE",
                "  dR5",
                "  got5",
                "p_match",
                "  dE",
                "  dR6",
                "  got6",
                "p_iflet_move",
                "  dR7",
                "  dE",
                "  dR7",
                "  got7",
                "p_iflet_consume",
                "  dR8",
                "  dE",
                "  dR8",
                "  got8",
                "m_iflet",
                "  dE",
                "  dR9",
                "  got9",
                "m_iflet_mut",
                "  dE",
                "  dR10",
                "  got10",
                "m_whilelet",
                "  dE",
                "  dR11",
                "  got11",
                "m_iflet_move",
                "  dR12",
                "  dE",
                "  dR12",
                "  got12",
                "end"
            ],
            "readonly_if_let_over_borrow_projection",
        );
}

/// B-2026-09-10-14 — the MEMORY half of `tests/codegen.rs`'s
/// `e2e_whole_payload_arm_binding_over_a_tuple_payload_runs_element_bodies`
/// under ASAN + LSan, same fifteen cells and same transcript.
///
/// The row itself is a BODIES-channel gap — memory was balanced before the
/// fix (0 valgrind errors, nothing lost), which is precisely why no
/// sanitizer leg could see it. This fixture is here for the OTHER
/// direction: the fix re-homes who runs the payload walk, and the two
/// rejected alternatives each cost a free rather than a body. Funding the
/// BINDING instead of the place ran the walk once per arm over one payload
/// (`twice`); leaving the place armed through a materializing arm ran it
/// beside the caller's owner (`ret`, measured `dR30 dR31 dR30 dR31`). Both
/// are double frees of the element interiors, and only a sanitizer holds
/// them closed.
#[test]
fn asan_whole_payload_arm_binding_over_a_tuple_payload_keeps_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             struct W { r: R, n: i64 }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"  dW{self.n}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"n{i}\" } }\n\
             fn eat(t: (R, R)) -> i64 { println(\"  eat\"); return 7 }\n\
             fn giveback() -> (R, R) {\n\
             \x20\x20\x20\x20let o: Option[(R, R)] = Some((mk(30), mk(31)));\n\
             \x20\x20\x20\x20match o { Some(t) => { return t } None => { return (mk(90), mk(91)) } }\n\
             }\n\
             \n\
             fn main() {\n\
             \x20\x20\x20\x20println(\"bind\");    { let o: Option[(R, R)] = Some((mk(1), mk(2))); println(\"  x\"); match o { Some(t) => { println(\"  hit\") } None => { println(\"  n\") } } }\n\
             \x20\x20\x20\x20println(\"read\");    { let o: Option[(R, R)] = Some((mk(3), mk(4))); println(\"  x\"); match o { Some(t) => { println(f\"  hit{t.0.id}\") } None => { println(\"  n\") } } }\n\
             \x20\x20\x20\x20println(\"iflet\");   { let o: Option[(R, R)] = Some((mk(5), mk(6))); println(\"  x\"); if let Some(t) = o { println(\"  hit\") } }\n\
             \x20\x20\x20\x20println(\"whilet\");  { let mut o: Option[(R, R)] = Some((mk(7), mk(8))); println(\"  x\"); while let Some(t) = o { println(\"  hit\"); o = None; } println(\"  end\") }\n\
             \x20\x20\x20\x20println(\"result\");  { let o: Result[(R, R), i64] = Ok((mk(9), mk(10))); println(\"  x\"); match o { Ok(t) => { println(\"  hit\") } Err(e) => { println(\"  n\") } } }\n\
             \x20\x20\x20\x20println(\"twice\");   { let o: Option[(R, R)] = Some((mk(11), mk(12))); println(\"  x\"); match o { Some(t) => { println(\"  a\") } None => { println(\"  n\") } } match o { Some(t) => { println(\"  b\") } None => { println(\"  n\") } } }\n\
             \x20\x20\x20\x20println(\"after\");   { let o: Option[(R, R)] = Some((mk(13), mk(14))); println(\"  x\"); match o { Some(t) => { println(\"  hit\") } None => { println(\"  n\") } } println(\"  end\") }\n\
             \x20\x20\x20\x20println(\"wild\");    { let o: Option[(R, R)] = Some((mk(15), mk(16))); println(\"  x\"); match o { Some(_) => { println(\"  hit\") } None => { println(\"  n\") } } }\n\
             \x20\x20\x20\x20println(\"struct\");  { let o: Option[W] = Some(W { r: mk(17), n: 18 }); println(\"  x\"); match o { Some(t) => { println(\"  hit\") } None => { println(\"  n\") } } }\n\
             \x20\x20\x20\x20println(\"single\");  { let o: Option[R] = Some(mk(19)); println(\"  x\"); match o { Some(t) => { println(\"  hit\") } None => { println(\"  n\") } } }\n\
             \x20\x20\x20\x20println(\"ret\");     { println(\"  x\"); let v = giveback(); println(\"  hit\") }\n\
             \x20\x20\x20\x20println(\"eat\");     { let o: Option[(R, R)] = Some((mk(20), mk(21))); println(\"  x\"); match o { Some(t) => { println(f\"  hit{eat(t)}\") } None => { println(\"  n\") } } }\n\
             \x20\x20\x20\x20println(\"nomatch\"); { let o: Option[(R, R)] = Some((mk(22), mk(23))); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"none\");    { let o: Option[(R, R)] = None; println(\"  x\"); match o { Some(t) => { println(\"  hit\") } None => { println(\"  n\") } } }\n\
             \x20\x20\x20\x20println(\"tlocal\");  { let t: (R, R) = (mk(24), mk(25)); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"end\")\n\
             }\n\
",
            &[
                "bind",
                "  x",
                "  hit",
                "  dR1",
                "  dR2",
                "read",
                "  x",
                "  hit3",
                "  dR3",
                "  dR4",
                "iflet",
                "  x",
                "  hit",
                "  dR5",
                "  dR6",
                "whilet",
                "  x",
                "  hit",
                "  dR7",
                "  dR8",
                "  end",
                "result",
                "  x",
                "  hit",
                "  dR9",
                "  dR10",
                "twice",
                "  x",
                "  a",
                "  b",
                "  dR11",
                "  dR12",
                "after",
                "  x",
                "  hit",
                "  dR13",
                "  dR14",
                "  end",
                "wild",
                "  x",
                "  hit",
                "  dR15",
                "  dR16",
                "struct",
                "  x",
                "  hit",
                "  dW18",
                "  dR17",
                "single",
                "  x",
                "  hit",
                "  dR19",
                "ret",
                "  x",
                "  dR30",
                "  dR31",
                "  hit",
                "eat",
                "  x",
                "  eat",
                "  hit7",
                "nomatch",
                "  dR22",
                "  dR23",
                "  x",
                "none",
                "  x",
                "  n",
                "tlocal",
                "  dR24",
                "  dR25",
                "  x",
                "end",
            ],
            "tuple_payload_arm_binding_one_owner",
        );
}

/// B-2026-09-07-7 — the MEMORY half, which IS this row: the same program
/// under ASAN + LSan. Pre-fix it lost 93 B in 3 blocks (one per discarded
/// literal over a named local); the buffer now has exactly one owner in
/// every cell, and the `let`-bound literal inside the arm still has its own.
#[test]
fn asan_discarded_arm_literal_over_a_named_local() {
    assert_clean_asan_run(
            "struct D { s: String }\n\
             struct R { id: i64, s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"p{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn main() {\n\
               let n = seed();\n\
               println(\"named_field\"); let b = payload(); let _ = if n >= 0 { D { s: b } };\n\
               println(\"stmt_in_arm\"); let c = payload(); if n >= 0 { let q = D { s: c }; println(f\"  q={q.s.len()}\"); }\n\
               println(\"mint_field\"); let _ = if n >= 0 { R { id: 2, s: payload() } };\n\
               println(\"bare_stmt_named\"); let d = payload(); if n >= 0 { D { s: d } };\n\
               println(\"not_taken\"); let e = payload(); let _ = if n > 900 { D { s: e } };\n\
               println(\"end\");\n\
             }\n",
            &[
                "named_field",
                "stmt_in_arm",
                "  q=31",
                "mint_field",
                "  dR2",
                "bare_stmt_named",
                "not_taken",
                "end"
            ],
            "discarded_arm_literal_over_a_named_local",
        );
}

/// B-2026-09-05-34 — THE `if let` / `while let` / `let … else` LEGS MUST
/// STAGE A BARE-TUPLE ELEMENT BINDING EXACTLY AS THE `match` ARM DOES.
///
/// `fn t(t: (R, i64)) -> i64 { if let (r, k) = t { k } else { 0 } }`
/// registered `__karac_drop_struct_R` on `r` — a bit-copy of `t.0` — AND ran
/// the tuple's own drop on `t` at the merge, so the element's `String` and
/// `Vec` buffers were freed twice. B-2026-09-02-23 had closed exactly this for
/// the `match` arm (`current_bare_tuple_bindings`, the memory half) and its
/// three single-pattern siblings never grew the staging.
///
/// The row read "JIT-only" because the JIT executes RAW IR while `karac build`
/// runs `default<O2>` first, and the optimizer folded one of the two frees
/// away for every cell measured; `KARAC_OPT_LEVEL=0 karac build` aborted on
/// all of them. That is why this lives in the ASAN suite: the double free is
/// caught at any optimization level, and an `-O2` transcript pin would pass
/// vacuously. Measured before the fix: `free(): double free detected in
/// tcache 2` on the JIT and at `-O0` for a PARAM, a LOCAL and a STRUCT-FIELD
/// tuple scrutinee, on `if let`, `while let` and `let … else` alike; every
/// cell valgrind-clean after.
///
/// THE CONTROLS ARE THE POINT, because the fix REMOVES an owner and the
/// failure mode of over-reaching is a leak (which LSan catches here):
/// - `m_read` — the `match` spelling, correct before and after.
/// - `letd` — the `let (r, k) = t` destructure, which goes through
///   `finish_place_source_tuple_destructure` and must stay untouched.
/// - `p_out` — the element HANDED OUT of the then-block. The tuple drop must
///   skip the source slot (`zero_bare_tuple_elem_source_for_moved`, the
///   match arm's tail hook) while the caller's `d` frees the buffers once.
/// - `p_rebind` / `l_rebind` — the element REBOUND inside the block
///   (`record_bare_tuple_elem_sources`, so the move-out neutralizes both
///   the view and the source).
#[test]
fn asan_iflet_bare_tuple_element_binding_is_not_a_second_owner() {
    assert_clean_asan_run(
            "struct H { id: i64, xs: Vec[i64], name: String }\n\
             fn mk(id: i64) -> H {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(id);\n\
             \x20   return H { id: id, xs: v, name: f\"n{id}\" }\n\
             }\n\
             struct W { t: (H, i64) }\n\
             fn p_read(t: (H, i64)) -> i64 { if let (r, k) = t { k } else { 0 } }\n\
             fn p_noread(t: (H, i64)) { if let (r, k) = t { println(\"  noread\") } else { println(\"  miss\") } }\n\
             fn p_rebind(t: (H, i64)) -> i64 { if let (r, k) = t { let m = r; m.id } else { 0 } }\n\
             fn p_out(t: (H, i64)) -> H { if let (r, k) = t { r } else { mk(0) } }\n\
             fn p_field(w: W) -> i64 { if let (r, k) = w.t { r.xs.len() + k } else { 0 } }\n\
             fn p_letelse(t: (H, i64)) -> i64 { let (r, k) = t else { return 0 }; r.id + k }\n\
             fn p_while(t: (H, i64)) -> i64 { while let (r, k) = t { return r.id } 0 }\n\
             fn l_read() -> i64 { let t = (mk(21), 0); if let (r, k) = t { r.id } else { 0 } }\n\
             fn l_rebind() -> i64 { let t = (mk(22), 0); if let (r, k) = t { let m = r; m.id } else { 0 } }\n\
             fn m_read(t: (H, i64)) -> i64 { match t { (r, k) => { r.id } } }\n\
             fn letd(t: (H, i64)) -> i64 { let (r, k) = t; r.id }\n\
             fn main() {\n\
             \x20   println(f\"  a{p_read((mk(1), 0))}\");\n\
             \x20   p_noread((mk(2), 0));\n\
             \x20   println(f\"  c{p_rebind((mk(3), 0))}\");\n\
             \x20   let d = p_out((mk(4), 0));\n\
             \x20   println(f\"  d{d.name}\");\n\
             \x20   println(f\"  e{p_field(W { t: (mk(5), 0) })}\");\n\
             \x20   println(f\"  f{p_letelse((mk(6), 0))}\");\n\
             \x20   println(f\"  g{p_while((mk(7), 0))}\");\n\
             \x20   println(f\"  h{l_read()}\");\n\
             \x20   println(f\"  i{l_rebind()}\");\n\
             \x20   println(f\"  m{m_read((mk(8), 0))}\");\n\
             \x20   println(f\"  l{letd((mk(9), 0))}\");\n\
             \x20   println(\"end\")\n\
             }\n",
            &[
                "a0",
                "  noread",
                "  c3",
                "  dn4",
                "  e1",
                "  f6",
                "  g7",
                "  h21",
                "  i22",
                "  m8",
                "  l9",
                "end",
            ],
            "b34-iflet-bare-tuple-element-single-owner",
        );
}

/// B-2026-09-02-38 — the STRUCT-PATTERN sibling of the case above, under
/// ASAN + LSan.
///
/// `let S { r, k } = s; let m = r;` over an owned struct param ran the
/// element's `Drop` body TWICE on all four surfaces. The body count is
/// pinned by `e2e_struct_pattern_destructure_of_owned_param_is_a_view`;
/// what this case pins is that withholding the second body did not strand
/// or double-free the MEMORY behind it, which a transcript test cannot see.
///
/// The heap matters here more than in the tuple sibling, because the fix
/// marks the leaf a param VIEW while the arm chain below the mark still
/// transfers Vec/String buffers to that same leaf — so mark and transfer
/// have to coexist. `heapstr` and `heapvec` are the two shapes where they
/// meet: a `String` field alongside the destructured one puts the param in
/// `owned_struct_params`, and a `Vec` field does the same by the other arm
/// of that gate.
///
/// Each `Drop` body READS both heap fields, so a premature free is a
/// use-after-free ASAN catches rather than a silent leak — and a body run
/// against a cap-zeroed husk (the B-2026-09-02-43 class) shows up in the
/// transcript as an empty name and a zero length rather than passing.
#[test]
fn asan_struct_pattern_destructure_leaf_has_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, name: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}:{self.name}:{self.xs.len()}\") } }\n\
             struct S { r: R, k: i64 }\n\
             struct Hs { r: R, name: String }\n\
             struct Vs { r: R, ys: Vec[i64] }\n\
             struct In { r: R }\n\
             struct Ou { inner: In, k: i64 }\n\
             fn mk(id: i64) -> R {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(id);\n\
             \x20   return R { id: id, name: f\"n{id}\", xs: v }\n\
             }\n\
             fn plain(s: S) { let S { r, k } = s; let m = r; println(f\"  p{m.xs.len()}:{m.name}\") }\n\
             fn heapstr(h: Hs) { let Hs { r, name } = h; let m = r; println(f\"  o:{m.name}:{name}\") }\n\
             fn heapvec(v: Vs) { let Vs { r, ys } = v; let m = r; println(f\"  v:{m.name}:{ys.len()}\") }\n\
             fn nested(o: Ou) { let Ou { inner, k } = o; let m = inner; println(f\"  t:{m.r.name}\") }\n\
             fn norebind(s: S) { let S { r, k } = s; println(f\"  nr{r.xs.len()}:{r.name}\") }\n\
             fn main() {\n\
             \x20   plain(S { r: mk(1), k: 0 });\n\
             \x20   heapstr(Hs { r: mk(2), name: \"s\" });\n\
             \x20   heapvec(Vs { r: mk(3), ys: [7, 8] });\n\
             \x20   nested(Ou { inner: In { r: mk(4) }, k: 0 });\n\
             \x20   norebind(S { r: mk(5), k: 0 });\n\
             \x20   println(\"end\")\n\
             }\n",
            &[
                "p1:n1",
                "dR1:n1:1",
                "  o:n2:s",
                "dR2:n2:1",
                "  v:n3:2",
                "dR3:n3:1",
                "  t:n4",
                "dR4:n4:1",
                "  nr1:n5",
                "dR5:n5:1",
                "end",
            ],
            "b38-struct-pattern-destructure-single-owner",
        );
}

/// B-2026-09-05-32 — the IDENTITY-ARM spelling of the row above:
/// `e = if c { pass(e) } else { e }`, where one arm hands the binding back
/// unchanged.
///
/// Its parent's predicate declines this outright — a bare identifier is not
/// a `Call` — and that was correct while the overwrite cleanup was
/// UNGUARDED: the `else` arm's value IS the old value, so freeing the old
/// slot's heap before the store would free the buffer about to be written
/// back, trading a 2-byte leak for a use-after-free. The fix guards the free
/// on the old and incoming values being DISTINCT, then admits the shape.
///
/// WHY A BIT-IDENTITY TEST IS THE EXACT DISCRIMINATOR, and not a heuristic:
/// `pass(r) { return r }` reads as an identity function, so both arms look
/// like they alias. They do not. Owned args are DEEP-COPIED at callee entry
/// (caller-retains), so the roundtripping arm returns a buffer at a
/// different address and the old one is genuinely orphaned by the store,
/// while `else { e }` yields the old value itself. The comparison therefore
/// no-ops on precisely the arm that would have become a use-after-free and
/// fires on precisely the one that leaks.
///
/// FOUR CELLS, and the middle two are the ones that matter:
///   `a`  the ROUNDTRIPPING arm taken — the leak, 12 allocs / 11 frees
///        at `-O0` pre-fix, 12 / 12 after.
///   `b`  the IDENTITY arm taken — the use-after-free hazard. It prints its
///        `name` field AFTER the assignment, so a freed buffer surfaces as
///        garbage or a sanitizer report rather than passing quietly.
///   `c`  a 20-iteration loop ALTERNATING the two arms, which is where the
///        leak is unbounded and where a wrong guard would fail on one
///        iteration in two.
///   `d`  MIXED arms (`else { mk(9) }`), the shape the parent already
///        covers, kept here so a regression that narrowed the guard too far
///        shows up beside the widening rather than only in the parent.
///
/// Every cell prints its `name` after the store for the reason cell `b`
/// does: the output assertion is what makes this a use-after-free test and
/// not only a leak test, and it fails at BOTH opt levels if the guard ever
/// inverts. The memory half is `-O0`-carried as the parent's is — at `-O2`
/// LLVM deletes the redundant allocation.
#[test]
fn asan_self_assign_identity_arm_frees_only_the_distinct_value() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.id}") } }
fn pass(r: R) -> R { return r; }
fn mk(n: i64) -> R { return R { id: n, name: f"e{n}" } }
fn main() {
    let mut a = mk(1);
    let t: bool = true;
    a = if t { pass(a) } else { a };
    println(f"A use {a.id} {a.name}");

    let mut b = mk(2);
    let f: bool = false;
    b = if f { pass(b) } else { b };
    println(f"B use {b.id} {b.name}");

    let mut c = mk(3);
    let mut i: i64 = 0;
    while i < 20 { c = if i % 2 == 0 { pass(c) } else { c }; i = i + 1; }
    println(f"C use {c.id} {c.name}");

    let mut d = mk(4);
    d = if t { pass(d) } else { mk(9) };
    println(f"D use {d.id} {d.name}");
    println("end");
}
"#,
        &[
            "A use 1 e1",
            "drop 1",
            "B use 2 e2",
            "drop 2",
            "C use 3 e3",
            "drop 3",
            "D use 4 e4",
            "drop 4",
            "end",
        ],
        "self_assign_identity_arm_frees_only_the_distinct_value",
    );
}

/// B-2026-08-30-11 — the MINTING arm of a MIXED branch now has an owner
/// too, borrowed from the sibling arm that reported one.
///
/// B-2026-08-29-27's consuming gates require EVERY tail to mint, so one
/// binding arm declines the whole construct and no gate frees the merged
/// value; B-2026-08-30-2's owner replaces the cleanup a disarmed SOURCE
/// BINDING gave up, and a minting arm has no source binding to replace. The
/// minted buffer therefore reached the merge owned by nobody. Measured
/// (valgrind, `KARAC_OPT_LEVEL=0` and `2` identical; per-arm payload sizes
/// chosen so the leaked block identifies which arm produced it):
///
/// | shape | pre |
/// |---|---|
/// | `if c { mkA(n) } else { t }.contains("aaa")` | 15 B |
/// | `match n { 1 => mkA(n), _ => t }.contains("aaa")` | 15 B |
/// | `f"[{if c { mkA(n) } else { t }}]"` | 15 B |
///
/// The sibling's frame is the one to borrow and the INNERMOST live frame is
/// not: when the branch is itself wrapped, innermost is the WRAPPER's,
/// which drains before the value escapes — a draft that used it double-freed
/// on the self-host seed-run oracle. `w1` is the tripwire for that: the
/// branch is inside a block inside a function whose caller consumes the
/// result, which is the nesting a single-level fixture misses.
///
/// `e1` (an OWNING destination), `a1` (every arm mints, where the consuming
/// gates free at the use site and an arm-level owner would be the second
/// free) and `b1` (both arms hand out bindings) are the controls that a
/// second owner would turn into a double free.
///
/// `o1` reads the sibling binding AFTER the branch, which is what makes the
/// borrowed frame's lifetime observable: the value is freed at that frame's
/// exit, never earlier.
#[test]
fn asan_mixed_branch_minting_arm_frees_once() {
    assert_clean_asan_run(
        r#"
fn mkA(n: i64) -> String { return f"A{n}-aaaaaaaaaaaa"; }
fn mkB(n: i64) -> String { return f"B{n}-bbbbbbbbbbbbbbbbbbbbbbbb"; }
fn wrapped(n: i64, c: bool, t: String) -> String { { if c { mkA(n) } else { t } } }

fn main() {
    let n: i64 = env.args().len();
    let c = n > 0;

    let t1 = mkB(n);
    let r1 = if c { mkA(n) } else { t1 }.contains("aaa");
    println(f"r1={r1}");

    let t2 = mkB(n);
    let r2 = match n { 1 => mkA(n), _ => t2 }.contains("aaa");
    println(f"r2={r2}");

    let t3 = mkB(n);
    let r3 = f"[{if c { mkA(n) } else { t3 }}]";
    println(f"r3={r3.len()}");

    let t4 = mkB(n);
    let w1 = wrapped(n, c, t4);
    println(f"w1={w1.len()}");

    let t5 = mkB(n);
    let e1 = if c { mkA(n) } else { t5 };
    println(f"e1={e1.len()}");

    let a1 = if c { mkA(n) } else { mkB(n) }.contains("aaa");
    println(f"a1={a1}");

    let t6 = mkB(n);
    let u6 = mkB(n + 1);
    let b1 = if c { u6 } else { t6 }.contains("bbb");
    println(f"b1={b1}");

    let t7 = mkB(n);
    let o1 = if not c { mkA(n) } else { f"keep" }.contains("aaa");
    println(f"o1={o1} {t7}");
}
"#,
        &[
            "r1=true",
            "r2=true",
            "r3=17",
            "w1=15",
            "e1=15",
            "a1=true",
            "b1=true",
            "o1=false B1-bbbbbbbbbbbbbbbbbbbbbbbb",
        ],
        "asan_mixed_branch_minting_arm",
    );
}

/// B-2026-09-01-23 — the arm-owner slot is ONE PER CONSTRUCT and reset each
/// pass, so a construct inside a loop whose owner frame lives OUTSIDE the
/// loop used to free only the LAST pass's value. Pinned CLEAN.
///
/// LEAK-CHECKED, and the switch of runner is the point. This fixture ran
/// under `run_under_asan_no_leak_check` while the shape still leaked, so it
/// could only assert the remainder had not become a memory ERROR. The
/// remainder is gone: each pass's escaping value is now reclaimed at the
/// reset — the last instant it is still reachable — when the borrowed owner
/// frame lives below `LoopFrame::cleanup_depth` and therefore cannot drain
/// between two passes. So the leak-checking runner is the honest one, and a
/// regression to the old behaviour fails here rather than passing quietly.
///
/// The history it pins: B-2026-08-30-11 improved this shape without closing
/// it (three iterations 57 B in 3 blocks before, 42 B in 2 after; five
/// iterations 72 B in 4), because the owner slot existed for the minting arm
/// but `reset_vec_slot_at_block_end` made it mean "THIS pass's escaping
/// value" while the frame holding its cleanup drained once, after the loop.
/// The sibling binding's own value was stranded the same way, which is why
/// the count was `iterations - 1` rather than `iterations - 2`.
///
/// `s1` is the CONTROL that isolated the frame choice as the cause and is
/// kept: the same branch with the sibling binding declared INSIDE the loop
/// body was always clean, because the borrowed frame is then the body's and
/// drains every pass. It must stay clean, so it also guards against the
/// reclaim firing where a per-pass drain already freed the value — the
/// double free B-2026-08-30-2 measured.
#[test]
fn asan_loop_arm_owner_slot_reclaims_every_pass() {
    if !asan_available() {
        eprintln!("[asan_loop_arm_owner_slot] ASAN unavailable — skipping");
        return;
    }
    let src = r#"
fn mkA(n: i64) -> String { return f"A{n}-aaaaaaaaaaaa"; }
fn mkB(n: i64) -> String { return f"B{n}-bbbbbbbbbbbbbbbbbbbbbbbb"; }

fn main() {
    let n: i64 = env.args().len();

    let t = mkB(n);
    let mut i: i64 = 0;
    while i < 3 {
        let k = if i > 0 { mkA(n) } else { t }.contains("aaa");
        println(f"L{i}={k}");
        i = i + 1;
    }
    println(f"t={t.len()}");

    let mut j: i64 = 0;
    while j < 3 {
        let u = mkB(n);
        let s1 = if j > 0 { mkA(n) } else { u }.contains("aaa");
        println(f"S{j}={s1}");
        j = j + 1;
    }
}
"#;
    let Some((stdout, status)) = run_under_asan(src, "asan_loop_arm_owner_slot") else {
        eprintln!("[asan_loop_arm_owner_slot] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[asan_loop_arm_owner_slot] ASAN reported an error (exit {:?}). A LEAK \
             here means a pass's escaping value was stranded again; a double free \
             means the reclaim fired where a per-pass drain already took it.\n\
             stdout:\n{stdout}",
        status.code()
    );
    for want in [
        "L0=false", "L1=true", "L2=true", "t=27", "S0=false", "S1=true", "S2=true",
    ] {
        assert!(
            stdout.contains(want),
            "[asan_loop_arm_owner_slot] missing {want:?}\nstdout:\n{stdout}"
        );
    }
}

/// B-2026-09-03-15 — a destructured tuple's `Option` leaf takes its payload's
/// MEMORY along with the body, and takes it exactly once.
///
/// The correctness half is pinned by
/// `e2e_tuple_destructure_optres_leaf_owns_its_payload_body` in
/// `tests/codegen.rs`; this is the memory half, and the fix is precisely the
/// kind that trades a lost body for a double free if the ownership hand-off
/// is one-sided. The leaf now registers a tag-guarded payload free AND
/// `zero_tuple_elem_cap_at` neutralizes the source, so exactly one of them
/// must reclaim each payload: leave the source armed and it is a double
/// free, cap-zero without registering the leaf and it is a leak.
///
/// B-2026-09-03-22 — the `annres` cell's expectation CHANGED here, and
/// deliberately. It pinned a `Result` leaf running no payload body on any
/// surface, which was that row's whole subject: the body half generalized
/// and the MEMORY half had no peer, so taking the leaf leaked 272 bytes.
/// `track_inline_result_agg_payload_var` supplies the owner, so the leaf
/// now takes the body — `dR9`, `dR19`, `dR29`, one per iteration — and this
/// case's own question, whether it takes the payload exactly ONCE, is what
/// says the new owner is balanced rather than merely present.
///
/// All four cells are heap-bearing (`String` + `Vec[i64]` per `R`), and the
/// `used` cell additionally MOVES the payload out through a `match` arm —
/// the shape where the leaf's registration must be suppressed rather than
/// fire alongside the arm. `strp` is the inline-heap payload (`Option[String]`)
/// whose buffer has no `Drop` body to observe, so only ASAN can see whether
/// it was reclaimed. Three iterations, so a per-iteration imbalance
/// accumulates instead of hiding in a single pass.
#[test]
fn asan_tuple_destructure_optres_leaf_takes_its_payload_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
struct Ho { pe: (R, Option[R]) }
fn mk(id: i64) -> R { let mut xs = Vec[i64].new(); xs.push(id); return R { id: id, tag: f"t{id}", xs: xs } }

fn loc(b: i64)  { let t = (mk(b + 0), Option.Some(mk(b + 1))); let (r, o) = t; println(f"rd{r.id}") }
fn proj(b: i64) { let h = Ho { pe: (mk(b + 3), Option.Some(mk(b + 4))) }; let (r, o) = h.pe; println(f"rd{r.id}") }
fn used(b: i64) { let t = (mk(b + 5), Option.Some(mk(b + 6)));
                  let (r, o) = t;
                  match o { Option.Some(x) => println(f"got{x.id}/{x.tag}"), Option.None => println("none") }
                  println(f"rd{r.id}") }
fn strp(b: i64) { let t = (mk(b + 7), Option.Some("heapstr")); let (r, o) = t; println(f"rd{r.id}") }
fn annres(b: i64) { let t: (R, Result[R, String]) = (mk(b + 8), Result[R, String].Ok(mk(b + 9)));
                    let (r, o) = t; println(f"rd{r.id}") }

fn main() {
    let mut i = 0;
    while i < 3 {
        let b = i * 10;
        loc(b); proj(b); used(b); strp(b); annres(b);
        i = i + 1;
    }
}
"#,
        &[
            "dR1/t1/1",
            "rd0",
            "dR0/t0/1",
            "dR4/t4/1",
            "rd3",
            "dR3/t3/1",
            "got6/t6",
            "dR6/t6/1",
            "rd5",
            "dR5/t5/1",
            "rd7",
            "dR7/t7/1",
            "dR9/t9/1",
            "rd8",
            "dR8/t8/1",
            "dR11/t11/1",
            "rd10",
            "dR10/t10/1",
            "dR14/t14/1",
            "rd13",
            "dR13/t13/1",
            "got16/t16",
            "dR16/t16/1",
            "rd15",
            "dR15/t15/1",
            "rd17",
            "dR17/t17/1",
            "dR19/t19/1",
            "rd18",
            "dR18/t18/1",
            "dR21/t21/1",
            "rd20",
            "dR20/t20/1",
            "dR24/t24/1",
            "rd23",
            "dR23/t23/1",
            "got26/t26",
            "dR26/t26/1",
            "rd25",
            "dR25/t25/1",
            "rd27",
            "dR27/t27/1",
            "dR29/t29/1",
            "rd28",
            "dR28/t28/1",
        ],
        "asan_tuple_destructure_optres_leaf_takes_its_payload_once",
        // Floor guards the vacuous direction: were these allocations ever
        // folded away the fixture would pass over memory it never touched.
        // Three iterations x four cells x (String + Vec) per `R` is far above
        // this.
        30,
    );
}

/// B-2026-09-03-33 — the memory side of the `Result` husk fix. The defect
/// was a BODY that should not have run, and the fix masks the source's
/// field-bodies walk rather than removing the payload-area zero that walk
/// was reading; this pins that the zero is still doing its job.
///
/// Getting that backwards is the failure this guards against. The zero is
/// what keeps the SOURCE's struct drop off buffers the destructure leaf has
/// taken, so "fixing" the husk by deleting it would have traded a phantom
/// body for a double free -- which is why the surgery went to the walk. The
/// pre-fix program was already memory-clean (`12 allocs, 12 frees, 0 bytes
/// in use at exit` under valgrind), so this fixture is a NO-REGRESSION
/// gate, not a repair: it must stay clean, and the payload `String`s make a
/// dropped free visible as a leak rather than as nothing.
///
/// `used` WAS the one cell whose expected output was not what `--interp`
/// prints: the interpreter ran `dR5` after `got5` and the compiled
/// backends did not — the `Result` deferral B-2026-09-03-33 left in place,
/// filed as B-2026-09-04-1 and closed there. The leaf now carries the
/// payload's body walk on every source shape this fixture uses, so `loc`
/// and `awild` gain the unused leaf's body at the destructure (`dR1`,
/// `dR3`) and `used` gains `dR5` after `got5`, on all four surfaces. It is
/// kept here for the reason it always was: a leaf CONSUMED by a match is
/// where a mis-placed mask would double-free, and the ASAN half is the
/// point of the cell.
///
/// `awild` mirrors `loc` with the fields swapped (`a` wildcarded, `b`
/// bound), the shape that husked identically before the fix; `errh` is the
/// `Err` half, which never husked because it has no payload to walk.
/// Three iterations, so a per-iteration imbalance accumulates rather than
/// hiding in a single pass.
///
/// B-2026-09-03-32 — the `awild` cells (`dR2 dR3`, `dR12 dR13`, `dR22 dR23`)
/// pinned codegen's old order, leaf before discard; the discard is now
/// destroyed inside the statement, ahead of the unread leaf, on every
/// surface, and the strings here carry that one order.
#[test]
fn asan_struct_field_destructure_result_leaf_frees_its_payload_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
struct HoRes { a: R, b: Result[R, String] }
fn mk(id: i64) -> R { return R { id: id, tag: f"tag-value-{id}" } }

fn loc(k: i64)   { let h = HoRes { a: mk(k + 0), b: Result.Ok(mk(k + 1)) }; let HoRes { a, b } = h; println(f"rd{a.id}") }
fn awild(k: i64) { let h = HoRes { a: mk(k + 2), b: Result.Ok(mk(k + 3)) }; let HoRes { a: _, b } = h; println(f"rb{k}") }
fn used(k: i64)  { let h = HoRes { a: mk(k + 4), b: Result.Ok(mk(k + 5)) }; let HoRes { a, b } = h;
                   match b { Result.Ok(x) => println(f"got{x.id}/{x.tag}"), Result.Err(e) => println(f"err{e}") }
                   println(f"rd{a.id}") }
fn errh(k: i64)  { let h = HoRes { a: mk(k + 6), b: Result.Err(f"errstring-{k}") }; let HoRes { a, b } = h; println(f"rd{a.id}") }

fn main() {
    let mut i = 0;
    while i < 3 {
        let k = i * 10;
        loc(k); awild(k); used(k); errh(k);
        i = i + 1;
    }
}
"#,
        &[
            "dR1/tag-value-1",
            "rd0",
            "dR0/tag-value-0",
            "dR2/tag-value-2",
            "dR3/tag-value-3",
            "rb0",
            "got5/tag-value-5",
            "dR5/tag-value-5",
            "rd4",
            "dR4/tag-value-4",
            "rd6",
            "dR6/tag-value-6",
            "dR11/tag-value-11",
            "rd10",
            "dR10/tag-value-10",
            "dR12/tag-value-12",
            "dR13/tag-value-13",
            "rb10",
            "got15/tag-value-15",
            "dR15/tag-value-15",
            "rd14",
            "dR14/tag-value-14",
            "rd16",
            "dR16/tag-value-16",
            "dR21/tag-value-21",
            "rd20",
            "dR20/tag-value-20",
            "dR22/tag-value-22",
            "dR23/tag-value-23",
            "rb20",
            "got25/tag-value-25",
            "dR25/tag-value-25",
            "rd24",
            "dR24/tag-value-24",
            "rd26",
            "dR26/tag-value-26",
        ],
        "b33_result_leaf_husk",
        30,
    );
}

/// B-2026-09-03-24 — a destructured STRUCT's `Option` FIELD leaf takes its
/// payload's MEMORY exactly once, alongside the body it now runs.
///
/// The correctness half is pinned by
/// `e2e_struct_field_destructure_option_leaf_owns_its_payload_body` in
/// `tests/codegen.rs`; this is the half that would show up as a double free
/// or a leak rather than a wrong line. The hand-off has two sides and
/// exactly one of them must reclaim each payload: the leaf's own tag-guarded
/// free (`track_inline_option_agg_payload_var` / `track_boxed_enum_var`) and
/// the source's `zero_struct_field_move_cap`. Leave the source armed and it
/// is a double free; cap-zero without the leaf and it is a leak.
///
/// What this fix ADDS to that pair is a bodies walker registered AFTER the
/// memory, which is where the ordering claim becomes a memory-safety claim:
/// the frame drains LIFO, so registering second is what makes the body read
/// a live buffer. Registered the other way round it prints from freed memory
/// — a use-after-free ASAN sees, and one a `Drop` body that renders its
/// fields would otherwise report as garbage. Every `R` here carries BOTH a
/// `String` and a `Vec[i64]`, and every body renders both, so a stale read
/// is observable rather than silent.
///
/// `used` MOVES the payload out through a `match` arm — the shape where the
/// leaf's registration must be consumed by the arm rather than fire beside
/// it. `ostr` is the inline-heap payload (`Option[String]`) that is never
/// read, so nothing but ASAN can see whether its buffer was reclaimed.
/// Three iterations, so a per-iteration imbalance accumulates instead of
/// hiding in a single pass.
#[test]
fn asan_struct_field_destructure_option_leaf_takes_its_payload_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
struct Ho2 { a: R, b: Option[R] }
struct HoS { a: R, b: Option[String] }
fn mk(id: i64) -> R { let mut xs = Vec[i64].new(); xs.push(id); return R { id: id, tag: f"t{id}", xs: xs } }

fn loc(k: i64)  { let h = Ho2 { a: mk(k + 0), b: Option.Some(mk(k + 1)) }; let Ho2 { a, b } = h; println(f"rd{a.id}") }
fn lit(k: i64)  { let Ho2 { a, b } = Ho2 { a: mk(k + 2), b: Option.Some(mk(k + 3)) }; println(f"rd{a.id}") }
fn used(k: i64) { let h = Ho2 { a: mk(k + 4), b: Option.Some(mk(k + 5)) }; let Ho2 { a, b } = h;
                  match b { Option.Some(x) => println(f"got{x.id}/{x.tag}"), Option.None => println("none") }
                  println(f"rd{a.id}") }
fn ostr(k: i64) { let h = HoS { a: mk(k + 6), b: Option.Some("heapstr") }; let HoS { a, b } = h; println(f"rd{a.id}") }

fn main() {
    let mut i = 0;
    while i < 3 {
        let k = i * 10;
        loc(k); lit(k); used(k); ostr(k);
        i = i + 1;
    }
}
"#,
        &[
            "dR1/t1/1",
            "rd0",
            "dR0/t0/1",
            "dR3/t3/1",
            "rd2",
            "dR2/t2/1",
            "got5/t5",
            "dR5/t5/1",
            "rd4",
            "dR4/t4/1",
            "rd6",
            "dR6/t6/1",
            "dR11/t11/1",
            "rd10",
            "dR10/t10/1",
            "dR13/t13/1",
            "rd12",
            "dR12/t12/1",
            "got15/t15",
            "dR15/t15/1",
            "rd14",
            "dR14/t14/1",
            "rd16",
            "dR16/t16/1",
            "dR21/t21/1",
            "rd20",
            "dR20/t20/1",
            "dR23/t23/1",
            "rd22",
            "dR22/t22/1",
            "got25/t25",
            "dR25/t25/1",
            "rd24",
            "dR24/t24/1",
            "rd26",
            "dR26/t26/1",
        ],
        "asan_struct_field_destructure_option_leaf_takes_its_payload_once",
        // Floor guards the vacuous direction: were these allocations ever
        // folded away the fixture would pass over memory it never touched.
        // Three iterations x four cells x (String + Vec) per `R` is far
        // above this.
        30,
    );
}

/// B-2026-09-02-15 — the `if let` family over an indexed element, which is
/// a MEMORY defect end to end: before the fix these programs aborted with
/// `free(): double free detected in tcache 2`, so the guard is that they
/// run at all under ASAN and leave nothing behind under LSan.
///
/// `leg_option_elem` is the ANTI-OVER-REACH CONTROL and the reason the new
/// `clone_owned_vec_index_element` call is gated rather than unconditional.
/// `compile_match` calls the cloner for every scrutinee, and over a
/// `Vec[Option[R]]` element that clone is owned by nobody — the type-erased
/// `Option` layout carries no droppable payload, so
/// `materialize_freshtemp_enum_scrutinee` declines it. The `match` spelling
/// leaks 40 B there today; an ungated call here reproduced that leak in the
/// `if let` spelling, which had been clean. LSan is what caught it, and this
/// leg is what keeps it caught.
#[test]
fn asan_if_let_family_over_an_indexed_element_is_memory_clean() {
    let Some((out, status)) = run_under_asan(
        r#"struct R { id: i64, v: Vec[String] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }

fn mk(n: i64) -> E {
    let mut v: Vec[String] = Vec.new();
    v.push("abcdefghijklmnopqrstuvwxyz0123456789");
    v.push(f"tag{n}");
    return E.A(R { id: n, v: v })
}

fn mko(n: i64) -> Option[R] {
    let mut v: Vec[String] = Vec.new();
    v.push("abcdefghijklmnopqrstuvwxyz0123456789");
    v.push(f"opt{n}");
    return Option.Some(R { id: n, v: v })
}

fn leg_iflet() {
    let mut xs: Vec[E] = Vec.new();
    xs.push(mk(1));
    let mut i = 0;
    while i < 3 {
        if let E.A(r) = xs[0] { println(f"n{r.id} {r.v.len()}"); }
        i = i + 1;
    }
}

fn leg_whilelet() {
    let mut xs: Vec[E] = Vec.new();
    xs.push(mk(2));
    while let E.A(r) = xs[0] {
        println(f"n{r.id} {r.v.len()}");
        break
    }
}

fn leg_letelse() {
    let mut xs: Vec[E] = Vec.new();
    xs.push(mk(3));
    let E.A(r) = xs[0] else { println("none"); return }
    println(f"n{r.id} {r.v.len()}");
}

fn leg_option_elem() {
    let mut xs: Vec[Option[R]] = Vec.new();
    xs.push(mko(4));
    if let Option.Some(r) = xs[0] { println(f"n{r.id} {r.v.len()}"); }
}

fn main() {
    leg_iflet();
    leg_whilelet();
    leg_letelse();
    leg_option_elem();
    println("end");
}
"#,
        "asan_if_let_family_over_an_indexed_element_is_memory_clean",
    ) else {
        return;
    };
    assert!(status.success(), "ASAN/LSan reported a problem:\n{out}");
    assert!(
        out.contains("end"),
        "the program did not run to the end:\n{out}"
    );
    // Three passes over the same element, each reading a live clone.
    assert_eq!(
        out.matches("n1 2").count(),
        3,
        "the if-let clone did not carry a live payload on every pass:\n{out}"
    );
    assert_eq!(
        out.matches("n4 2").count(),
        1,
        "the Option-element control did not read its payload:\n{out}"
    );
}

/// B-2026-09-01-26 — a `match` in a NESTED EXPRESSION position leaked the
/// heap payload its arm moved out, while the `let`-bound spelling of the
/// same match was clean. 15 B per evaluation, at BOTH opt levels, and
/// unbounded in a loop.
///
/// The owner exists and never got registered. A `match` defers its arm
/// owners until every arm has drained (B-2026-08-30-11, so a minting arm
/// can borrow a sibling's frame), and an arm handing out a PATTERN BINDING
/// records that binding's own arm-local frame — which by then is gone, so
/// `own_escaping_tail_value_at`'s drained-frame guard declined. Under a
/// `let` the destination owns the result and nothing is lost; in a nested
/// position nothing downstream registers an owner either.
///
/// FIVE LEGS PER ITERATION, three that leaked and two controls:
///   - `fstr` — the match interpolated into an f-string, the row's shape.
///   - the `sink(match …)` CALL ARGUMENT, which contributes to `total`.
///   - `named` — the same nested position over a NAMED enum local, which
///     is what shows the defect is the position and not a fresh-temp
///     scrutinee.
///   - `let` — THE ORIGINAL CONTROL, clean before and after.
///   - `whole` — `match plain { t => { t } }`, a WHOLE-VALUE rebind of a
///     plain `String` local. It is the double-free control: the value
///     already has an owner downstream, and an earlier version of this fix
///     that re-homed every arm turned this leg into
///     `free(): double free detected in tcache 2` at both opt levels.
#[test]
fn asan_nested_match_owns_its_moved_out_payload() {
    let Some((out, status)) = run_under_asan(
        r#"enum Ve { A(String), B }
impl Drop for Ve { fn drop(mut ref self) { println("dVe") } }
fn mkVe(n: i64) -> Ve { return Ve.A(f"payloadpayload{n}") }
fn sink(s: String) -> i64 { return s.len() }

#[allow(partial_move_of_drop_enum)]
fn main() {
    let mut i = 0;
    let mut total = 0;
    while i < 3 {
        println(f"fstr[{match mkVe(i) { Ve.A(s) => { s } Ve.B => { "none".to_string() } }}]");
        total = total + sink(match mkVe(i) { Ve.A(s) => { s } Ve.B => { "none".to_string() } });
        let v = mkVe(i);
        println(f"named[{match v { Ve.A(s) => { s } Ve.B => { "none".to_string() } }}]");
        let out = match mkVe(i) { Ve.A(s) => { s } Ve.B => { "none".to_string() } };
        println(f"let[{out}]");
        let plain = f"plainplainplain{i}";
        println(f"whole[{match plain { t => { t } }}]");
        i = i + 1;
    }
    println(f"total {total}");
}
"#,
        "asan_nested_match_owns_its_moved_out_payload",
    ) else {
        return;
    };
    assert!(status.success(), "ASAN/LSan reported a problem:\n{out}");
    assert_eq!(
            out,
            "dVe\nfstr[payloadpayload0]\ndVe\nnamed[payloadpayload0]\ndVe\ndVe\nlet[payloadpayload0]\nwhole[plainplainplain0]\ndVe\nfstr[payloadpayload1]\ndVe\nnamed[payloadpayload1]\ndVe\ndVe\nlet[payloadpayload1]\nwhole[plainplainplain1]\ndVe\nfstr[payloadpayload2]\ndVe\nnamed[payloadpayload2]\ndVe\ndVe\nlet[payloadpayload2]\nwhole[plainplainplain2]\ntotal 45\n",
            "unexpected transcript:\n{out}"
        );
}

/// B-2026-09-02-31 — the BARE-ARM spelling of the row directly above, split
/// out at that fix's close because its block-bodied condition could not
/// reach this one. The two programs differ by two characters per arm
/// (`=> s,` against `=> { s }`) and leaked the same 15 B per evaluation,
/// unbounded in a loop.
///
/// WHY THE PARENT FIX EXCLUDED IT. Re-homing a declined arm owner to the
/// innermost LIVE frame was gated on the arm being BLOCK-BODIED, because
/// that condition — and nothing else in the record — kept the GENERIC-ENUM
/// DEBOX out. `fn get[T](o: Opt[T], d: T) -> T { match o { Opt.Yes(v) => v, .. } }`
/// is bare-armed too, and its value escapes the function entirely, so an
/// owner in the innermost live frame frees it before the caller reads it.
/// Both populations arrived through the same bare-tail channel, and the
/// record they arrived with could not tell them apart.
///
/// The discriminator is now `compute_fn_escaping_branch_spans`: does this
/// match's value BECOME the function's return value? The debox's does; a
/// match consumed inside the statement that built it does not.
///
/// FIVE LEGS PER ITERATION, three that leaked and two controls:
///   - `fstr` — the match interpolated into an f-string, the row's shape.
///   - the `sink(match …)` CALL ARGUMENT, which contributes to `total`.
///   - `named` — the same nested position over a NAMED enum local.
///   - `let` — the destination-owns-it control, clean before and after.
///   - `escape` — `deboxBare`, the ESCAPING control: the debox shape itself,
///     which must keep declining the re-home. It is the leg that fails if
///     the discriminator is dropped for a looser gate, and it fails as
///     `Instruction does not dominate all uses` on `%branchown` — the module
///     verifier catching the premature free before it can run — rather than
///     as a sanitizer report.
///
/// Measured pre-fix at HEAD: 135 B in 9 blocks at `-O0` (three leaking legs
/// × three iterations × 15 B) and 90 B in 6 blocks at `-O2`, LLVM having
/// elided one leg's allocation there.
#[test]
fn asan_nested_bare_arm_match_owns_its_moved_out_payload() {
    let Some((out, status)) = run_under_asan(
        r#"enum Ve { A(String), B }
impl Drop for Ve { fn drop(mut ref self) { println("dVe") } }
fn mkVe(n: i64) -> Ve { return Ve.A(f"payloadpayload{n}") }
fn sink(s: String) -> i64 { return s.len() }

#[allow(partial_move_of_drop_enum)]
fn deboxBare(v: Ve) -> String { match v { Ve.A(s) => s, Ve.B => "none".to_string() } }

#[allow(partial_move_of_drop_enum)]
fn main() {
    let mut i = 0;
    let mut total = 0;
    while i < 3 {
        println(f"fstr[{match mkVe(i) { Ve.A(s) => s, Ve.B => "none".to_string() }}]");
        total = total + sink(match mkVe(i) { Ve.A(s) => s, Ve.B => "none".to_string() });
        let v = mkVe(i);
        println(f"named[{match v { Ve.A(s) => s, Ve.B => "none".to_string() }}]");
        let out = match mkVe(i) { Ve.A(s) => s, Ve.B => "none".to_string() };
        println(f"let[{out}]");
        println(f"escape[{deboxBare(mkVe(i))}]");
        i = i + 1;
    }
    println(f"total {total}");
    println("end");
}
"#,
        "asan_nested_bare_arm_match_owns_its_moved_out_payload",
    ) else {
        return;
    };
    assert!(status.success(), "ASAN/LSan reported a problem:\n{out}");
    assert_eq!(out, "dVe\nfstr[payloadpayload0]\ndVe\nnamed[payloadpayload0]\ndVe\ndVe\nlet[payloadpayload0]\ndVe\nescape[payloadpayload0]\ndVe\nfstr[payloadpayload1]\ndVe\nnamed[payloadpayload1]\ndVe\ndVe\nlet[payloadpayload1]\ndVe\nescape[payloadpayload1]\ndVe\nfstr[payloadpayload2]\ndVe\nnamed[payloadpayload2]\ndVe\ndVe\nlet[payloadpayload2]\ndVe\nescape[payloadpayload2]\ntotal 45\nend\n", "unexpected transcript:\n{out}");
}

/// B-2026-09-15-29 — the `if let` sibling of the two rows above, split out
/// at B-2026-09-02-31's close because that fix's only consuming read site
/// is inside `compile_match`.
///
/// WHAT WAS ACTUALLY MISSING, which is not what the row guessed. The row
/// asks whether `compile_if_let` needs the same discriminator or a
/// different one, and warns that "something else is declining the owner
/// here". It is the same discriminator, and nothing else declines it: the
/// then-arm DOES produce an owner record, and
/// `own_escaping_tail_value_at` then throws it away because the frame the
/// record names is the arm's OWN frame, which the hand-rolled
/// `drain_top_frame_with_emit` a few lines earlier has already popped. The
/// `match` path reaches the identical state and survives it only because
/// B-2026-09-02-31 gave it `rehome_drained_frame`; the `if let` site passes
/// that argument as a hard `false`. So the fix is the re-home gate, not a
/// new record and not a new channel.
///
/// THE GATE'S FIRST DISJUNCT DOES NOT TRANSPLANT. The match's
/// `arm_pending_is_block || !match_escapes_fn` cannot be copied verbatim:
/// this arm compiles through a plain `compile_block`, never through
/// `compile_block_with_frame`, so its record always arrives on the bare
/// `vecstr_source_disarmed` channel and `arm_pending_is_block` would be a
/// constant `false`. What remains is the escaping-set lookup itself.
///
/// AND THAT LOOKUP IS LOAD-BEARING — measured, not argued. Replacing it
/// with `true` leaves EVERY leg of this fixture clean, including
/// `deboxIfLet`, so a single-level escaping control proves nothing here.
/// The program that catches it is the GENERIC one: with the gate dropped,
/// `getIfLet[T]` fails to compile at all, with
/// `Module verification failed: "Instruction does not dominate all uses!"`
/// on `%branchown` — the verifier catching the premature free before it can
/// run, exactly as B-2026-09-02-31 records for its own monomorph hole. That
/// is why `generic` and `genericvec` are legs of this fixture rather than a
/// note in its prose.
///
/// LEGS. Three that leaked (`fstr`, the `sink(…)` call argument, `named`),
/// the `Vec`-payload leg folded into `total`, and four controls: `let`
/// (destination owns it, always clean), `escape` (`deboxIfLet`), `step`
/// (the `while let` shape, which the row lists as unmeasured and which was
/// already clean), and the two `getIfLet` monomorphs above.
///
/// NON-VACUITY, measured against the parent commit's `src/` with `tests/`
/// kept at HEAD and a `grep -c` of this row's id printed as a guard either
/// side (0 -> 1): `AddressSanitizer: 186 byte(s) leaked in 9 allocation(s)`.
/// Nine is the three `String` legs times the three iterations; the aggregate
/// is quoted rather than split per leg because LSan reports it as one
/// number and this fixture is what produces it.
#[test]
fn asan_nested_if_let_owns_its_moved_out_payload() {
    let Some((out, status)) = run_under_asan(
        r#"enum Ve { A(String), B }
impl Drop for Ve { fn drop(mut ref self) { println("dVe") } }
fn mkVe(n: i64) -> Ve { return Ve.A(f"payloadpayload{n}") }
fn sink(s: String) -> i64 { return s.len() }

enum Vv { A(Vec[i64]), B }
fn mkVv(n: i64) -> Vv { let mut v: Vec[i64] = Vec.new(); v.push(n); v.push(n + 1); v.push(n + 2); v.push(n + 3); return Vv.A(v) }

fn mkStep(n: i64) -> Ve { if n >= 3 { return Ve.B } return Ve.A(f"steppayload{n}") }

#[allow(partial_move_of_drop_enum)]
fn deboxIfLet(v: Ve) -> String { if let Ve.A(s) = v { s } else { "none".to_string() } }

enum Opt[T] { Yes(T), No }
fn getIfLet[T](o: Opt[T], d: T) -> T { if let Opt.Yes(v) = o { v } else { d } }

#[allow(partial_move_of_drop_enum)]
fn main() {
    let mut i = 0;
    let mut total = 0;
    while i < 3 {
        println(f"fstr[{if let Ve.A(s) = mkVe(i) { s } else { "none".to_string() }}]");
        total = total + sink(if let Ve.A(s) = mkVe(i) { s } else { "none".to_string() });
        let v = mkVe(i);
        println(f"named[{if let Ve.A(s) = v { s } else { "none".to_string() }}]");
        let out = if let Ve.A(s) = mkVe(i) { s } else { "none".to_string() };
        println(f"let[{out}]");
        println(f"escape[{deboxIfLet(mkVe(i))}]");
        total = total + (if let Vv.A(w) = mkVv(i) { w } else { Vec.new() }).len();
        i = i + 1;
    }
    let mut j = 0;
    while let Ve.A(s) = mkStep(j) {
        println(f"step[{s}]");
        j = j + 1;
    }
    let g = getIfLet(Opt.Yes("genericpayload".to_string()), "d".to_string());
    println(f"generic[{g}]");
    let mut gv: Vec[i64] = Vec.new();
    gv.push(7);
    gv.push(8);
    println(f"genericvec[{getIfLet(Opt.Yes(gv), Vec.new()).len()}]");
    println(f"total {total}");
    println("end");
}
"#,
        "asan_nested_if_let_owns_its_moved_out_payload",
    ) else {
        return;
    };
    assert!(status.success(), "ASAN/LSan reported a problem:\n{out}");
    assert_eq!(out, "dVe\nfstr[payloadpayload0]\ndVe\nnamed[payloadpayload0]\ndVe\ndVe\nlet[payloadpayload0]\ndVe\nescape[payloadpayload0]\ndVe\nfstr[payloadpayload1]\ndVe\nnamed[payloadpayload1]\ndVe\ndVe\nlet[payloadpayload1]\ndVe\nescape[payloadpayload1]\ndVe\nfstr[payloadpayload2]\ndVe\nnamed[payloadpayload2]\ndVe\ndVe\nlet[payloadpayload2]\ndVe\nescape[payloadpayload2]\nstep[steppayload0]\ndVe\nstep[steppayload1]\ndVe\nstep[steppayload2]\ndVe\ndVe\ngeneric[genericpayload]\ngenericvec[2]\ntotal 57\nend\n", "unexpected transcript:\n{out}");
}

/// B-2026-08-28-12 — the MEMORY half of the wildcard-leaf fix.
///
/// The behavioural twins prove the discarded value's user `Drop` body now
/// runs exactly once at a scalar `R`. This asks the question they cannot:
/// at a heap-carrying `R`, does adding that body change what gets FREED?
///
/// It must not, and the reason is the fix's central design choice. A
/// wildcard leaf's memory was never the missing part — codegen's tuple
/// `Wildcard` arm already freed a `{ptr,len,cap}` element, and a struct
/// element's buffers are freed by whichever drop covers the source
/// aggregate. Only the BODY was absent. So the fix emits
/// `emit_struct_user_drop_bodies_only_fn` — body plus field-body walk, no
/// frees — rather than the full `karac_drop_<T>` wrapper. Reaching for the
/// wrapper would have looked more natural and would have freed every one of
/// those buffers a second time; this fixture is what makes that difference
/// observable, since both choices produce identical stdout.
///
/// `both-wildcards` is the sharpest row: two heap objects, both discarded,
/// so a wrapper-instead-of-bodies mistake double-frees twice over.
///
/// The param control pins the other edge — there the fix deliberately fires
/// nothing, so the row proves the caller-side owner still releases the
/// buffer exactly once and that the gate did not suppress a free along with
/// the body it was suppressing.
#[test]
fn asan_wildcard_destructure_leaf_drop_body_is_memory_balanced() {
    const DROPPER: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n";
    for (label, body, want) in [
        (
            "tuple-place",
            "fn main() { let p = (R { id: 41, name: f\"n{41}\" }, 1);\n\
                 \x20            let (_, n) = p; println(f\"{n}\"); }\n",
            vec!["drop 41 n41", "1"],
        ),
        (
            "tuple-fresh-literal",
            "fn main() { let (_, n) = (R { id: 41, name: f\"n{41}\" }, 1);\n\
                 \x20            println(f\"{n}\"); }\n",
            vec!["drop 41 n41", "1"],
        ),
        (
            "struct-fresh-call",
            "struct W { r: R, n: i64 }\n\
                 fn mk() -> W { W { r: R { id: 41, name: f\"n{41}\" }, n: 1 } }\n\
                 fn main() { let W { r: _, n } = mk(); println(f\"{n}\"); }\n",
            vec!["drop 41 n41", "1"],
        ),
        // Two discarded heap objects — the row a full-wrapper registration
        // would double-free twice.
        (
            "both-wildcards",
            "fn main() { let p = (R { id: 41, name: f\"n{41}\" },\n\
                 \x20                    R { id: 42, name: f\"n{42}\" });\n\
                 \x20            let (_, _) = p; println(\"x\"); }\n",
            vec!["drop 41 n41", "drop 42 n42", "x"],
        ),
        // An ENUM element's live payload, which carries the heap here.
        // The payload walker runs BODIES only and deliberately frees
        // nothing — the source aggregate's own drop still owns the buffer —
        // so this row is what catches getting that half wrong in either
        // direction: a leak if the free moved, a double free if it were
        // added.
        (
            "enum-payload-place",
            "enum E { A(R), B }\n\
                 fn main() { let p = (E.A(R { id: 41, name: f\"n{41}\" }), 1);\n\
                 \x20            let (_, n) = p; println(f\"{n}\"); }\n",
            vec!["drop 41 n41", "1"],
        ),
        (
            "enum-payload-fresh-literal",
            "enum E { A(R), B }\n\
                 fn main() { let (_, n) = (E.A(R { id: 41, name: f\"n{41}\" }), 1);\n\
                 \x20            println(f\"{n}\"); }\n",
            vec!["drop 41 n41", "1"],
        ),
        // A NESTED wildcard, whose element the recursion now reaches on
        // both the place and the fresh path.
        (
            "nested-wildcard-place",
            "fn main() { let p = ((R { id: 41, name: f\"n{41}\" }, 2), 1);\n\
                 \x20            let ((_, m), n) = p; println(f\"{m + n}\"); }\n",
            vec!["drop 41 n41", "3"],
        ),
        (
            "nested-wildcard-fresh-literal",
            "fn main() { let ((_, m), n) = ((R { id: 41, name: f\"n{41}\" }, 2), 1);\n\
                 \x20            println(f\"{m + n}\"); }\n",
            vec!["drop 41 n41", "3"],
        ),
        // CONTROL — a by-value param source, where the fix fires nothing.
        // B-2026-08-28-19 moved the caller-side body ahead of the `1`.
        (
            "param-tuple-control",
            "fn take(p: (R, i64)) -> i64 { let (_, n) = p; n }\n\
                 fn main() { let x = take((R { id: 41, name: f\"n{41}\" }, 1));\n\
                 \x20            println(f\"{x}\"); }\n",
            vec!["drop 41 n41", "1"],
        ),
    ] {
        assert_clean_asan_run(&format!("{DROPPER}{body}"), &want, label);
    }
}

/// B-2026-08-28-63 — the memory half of running a consuming arm's bound
/// ENUM payload's `Drop` body.
///
/// The registration is bodies-only (`__karac_dropbodies_only_<E>` runs the
/// own body plus the live variant's payload walk and frees nothing), while
/// the payload's MEMORY stays with `track_enum_var`'s `EnumDrop` on the
/// inline slot or with the box drop on the wide one. Two actions over one
/// slot is the arrangement that double-frees if the bodies leg also freed,
/// so every row carries a heap `String` the newly-running body reads.
///
/// `escaping-tail` is the row that holds the consumption gate: the arm's
/// value IS the binding, so the match result owns it and the arm must
/// register nothing. Without the gate this is a double free, not just a
/// doubled line.
#[test]
fn asan_consuming_arm_enum_payload_bodies_are_memory_balanced() {
    const H: &str = "enum G { A(String), B }\n\
             impl Drop for G { fn drop(mut ref self) { println(\"drop G\") } }\n";
    // BOXED payload — wider than the `Option` area, so the box owns the
    // memory and the bodies walk must only read it.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let o: Option[G] = Some(G.A(f\"z{{9}}\"));\n\
             \x20            match o {{ Some(g) => {{ println(\"got\") }}\n\
             \x20                       None => {{ println(\"none\") }} }} }}\n"
        ),
        &["got", "drop G"],
        "boxed-heap-payload",
    );
    // INLINE payload whose PAYLOAD STRUCT owns the heap — the other
    // registration site, reached through `track_enum_var`'s slot.
    assert_clean_asan_run(
        "enum H2 { A(R2), B }\n\
             struct R2 { id: i64, name: String }\n\
             impl Drop for R2 { fn drop(mut ref self) { println(f\"drop R{self.name}\") } }\n\
             fn main() { let r: Result[H2, i64] = Ok(H2.A(R2 { id: 5, name: f\"n{5}\" }));\n\
             \x20            match r { Ok(h) => { println(\"got\") }\n\
             \x20                      Err(n) => { println(\"err\") } } }\n",
        &["got", "drop Rn5"],
        "inline-nested-struct-heap",
    );
    // ESCAPING — the arm hands the payload to the match's result. The
    // consumption gate must keep the arm from registering, or the body and
    // the free both run twice.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let o: Option[G] = Some(G.A(f\"z{{9}}\"));\n\
             \x20            let k = match o {{ Some(g) => {{ g }} None => {{ G.B }} }};\n\
             \x20            println(\"kept\"); }}\n"
        ),
        &["drop G", "kept"],
        "escaping-tail",
    );
    // MOVED to a sink — the callee's owned param owns it.
    assert_clean_asan_run(
        &format!(
            "{H}fn sink(g: G) {{ println(\"sank\") }}\n\
             fn main() {{ let o: Option[G] = Some(G.A(f\"z{{9}}\"));\n\
             \x20            match o {{ Some(g) => {{ sink(g) }}\n\
             \x20                       None => {{ println(\"none\") }} }} }}\n"
        ),
        &["sank", "drop G"],
        "moved-to-sink",
    );
    // IF LET — the second registration path, same claim.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let o: Option[G] = Some(G.A(f\"z{{9}}\"));\n\
             \x20            if let Some(g) = o {{ println(\"got\") }} }}\n"
        ),
        &["got", "drop G"],
        "if-let",
    );
}

#[test]
fn asan_own_drop_enum_discarded_by_wildcard_leaf_is_balanced() {
    const H: &str = "enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
             struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    // FRESH tuple literal source — nothing else owns the temp, so the
    // discard site frees the payload's buffer.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let (_, n) = (E.A(R {{ id: 41, name: f\"n{{41}}\" }}), 1);\n\
             \x20            println(f\"{{n}}\"); }}\n"
        ),
        &["drop E", "drop R41", "1"],
        "fresh-tuple",
    );
    // PLACE source — the local `p`'s own drop frees it; freeing here too
    // would be a second free.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let p = (E.A(R {{ id: 41, name: f\"n{{41}}\" }}), 1);\n\
             \x20            let (_, n) = p; println(f\"{{n}}\"); }}\n"
        ),
        &["drop E", "drop R41", "1"],
        "place-source",
    );
    // CONTROL — the same enum BOUND rather than discarded, which runs both
    // bodies on every backend and is the shape the discard path is measured
    // against.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let e = E.A(R {{ id: 41, name: f\"n{{41}}\" }});\n\
             \x20            println(\"mid\"); }}\n"
        ),
        &["drop E", "drop R41", "mid"],
        "bound-control",
    );
    // The STRUCT-pattern spelling of the same discard. B-2026-08-28-40
    // routes it through a different walker than the two tuple rows above
    // (`emit_user_drop_field_bodies_fn_skipping`, not the bodies-only
    // whole-value one), so its balance is a separate question, not a
    // corollary of theirs.
    assert_clean_asan_run(
            &format!(
                "{H}struct W {{ e: E, n: i64 }}\n\
             \x20            fn main() {{ let w = W {{ e: E.A(R {{ id: 41, name: f\"n{{41}}\" }}), n: 1 }};\n\
             \x20            let W {{ e: _, n }} = w; println(f\"{{n}}\"); }}\n"
            ),
            &["drop E", "drop R41", "1"],
            "struct-field-sibling",
        );
}

#[test]
fn asan_iflet_pop_shared_struct_no_leak() {
    // B-2026-07-21-18: `if let Some(n) = st.pop()` over a `Vec[shared T]`
    // leaked the popped node. `Vec.pop` is an INLINED builtin: its result
    // is an owned `Option[shared T]` rvalue (the container relinquishes the
    // slot via `len--`, transferring its +1 to the returned Option), but it
    // never rides the general owned-temp-drop registration a real fn/method
    // return does. The `match` scrutinee chain registered
    // `track_freshtemp_shared_option_scrutinee` for exactly this shape; the
    // `if let` / `while let` / `let…else` paths stopped at the boxed-enum
    // tracker, so the pattern bind's inc + scope-exit dec cancelled and the
    // transferred +1 orphaned → every popped node leaked (kata #94 iterative
    // inorder, #23 merge-k heap). Must print 6 and be LSan-clean. `match`
    // (already clean pre-fix) is exercised alongside as a guard.
    assert_clean_asan_run(
        r#"
shared struct N { val: i64 }
fn main() {
    let mut st: Vec[N] = Vec.new();
    st.push(N { val: 1i64 });
    st.push(N { val: 2i64 });
    st.push(N { val: 3i64 });
    let mut sum = 0i64;
    // if let form
    if let Some(a) = st.pop() { sum = sum + a.val; }
    // while let form drains the rest
    while let Some(b) = st.pop() { sum = sum + b.val; }
    println(sum.to_string());
}
"#,
        &["6"],
        "asan_iflet_pop_shared_struct_no_leak",
    );
}

#[test]
fn asan_freshtemp_call_match_option_shared_no_leak() {
    // B-2026-07-12-23 — a direct `match <call returning Option[shared]>`
    // fresh-temp scrutinee leaked the extracted node once per match (6,400 B
    // / 200 iters pre-fix). The callee (`take`) rebuilds its Option through a
    // returning arm (`Some(n) => Some(n)`), so it hands back an owned +1 the
    // caller's match never dropped — the fresh-temp scrutinee's drop was
    // resolved from the erased generic Option layout (all-None drop-kinds),
    // so has_droppable was false. The lowering pass now rewrites the direct
    // call form into a let-bound scrutinee whose concrete-type cleanup
    // releases the rc (sibling of the B-21 index-read rewrite). Looped 200x
    // so any per-iteration leak accumulates well past noise; prints 1400.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Option[Node] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => None,
        Some(n) => Some(n),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        match take() {
            None => {}
            Some(n) => { t = t + n.val; }
        }
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["1400"],
        "freshtemp_call_match_option_shared_no_leak",
    );
}

#[test]
fn asan_freshtemp_literal_call_match_option_shared_no_double_free() {
    // B-2026-07-12-23 discriminator (b) — a fresh-LITERAL `match make()`
    // (callee returns a bare `Some(Node{..})`, no returning-arm rebuild) was
    // already CLEAN pre-fix. The B-23 lowering rewrite widens to all `Call`
    // scrutinees, so this case is now rewritten too; it must STAY clean — a
    // spurious extra release here would double-free the freshly-built node.
    // Guards the widened gate against over-release.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn make() -> Option[Node] {
    Some(Node { val: 7, left: None, right: None })
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        match make() {
            None => {}
            Some(n) => { t = t + n.val; }
        }
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["1400"],
        "freshtemp_literal_call_match_option_shared_no_double_free",
    );
}

#[test]
fn asan_freshtemp_call_match_result_shared_no_leak() {
    // B-2026-07-12-24 — the `Result[shared]` sibling of the Option B-23
    // leak. A direct `match take()` where `take` returns
    // `Result[shared Node, i64]` via a returning arm (`Some(n) => Ok(n)`)
    // leaked the node once per match (6,400 B / 200 iters pre-fix): the
    // B-21/B-23 lowering rewrite routes it through a synthetic let-bound
    // scrutinee, but `Result` had no rc cleanup for that scrutinee (the
    // Option-only `track_rc_option_var` had no Result sibling). The new
    // `track_rc_result_var` registers a tag-guarded RcDecOption for the
    // synthetic `__karac_msc_*` scrutinee. Looped 200x; prints 1400.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        match take() {
            Err(e) => { t = t + e; }
            Ok(n) => { t = t + n.val; }
        }
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["1400"],
        "freshtemp_call_match_result_shared_no_leak",
    );
}

#[test]
fn asan_freshtemp_call_match_result_shared_err_arm_no_leak() {
    // B-2026-07-12-24 — the `Err`-shared arm: `Result[i64, shared Node]`
    // where the payload node lives in `Err`. `track_rc_result_var`
    // registers a tag-guarded RcDecOption for BOTH arms, so the live `Err`
    // node is released. Guards the two-arm registration. Prints 1800.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[i64, Node] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 9, left: None, right: None }));
    match src[0] {
        None => Ok(0),
        Some(n) => Err(n),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        match take() {
            Ok(v) => { t = t + v; }
            Err(n) => { t = t + n.val; }
        }
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["1800"],
        "freshtemp_call_match_result_shared_err_arm_no_leak",
    );
}

#[test]
fn asan_freshtemp_call_match_result_shared_rebuild_arm_no_double_free() {
    // B-2026-07-12-24 guard — a returning-arm rebuild `Ok(n) => Ok(n)` that
    // hands the node out through a consumer `match relay()` (whose own
    // synthetic scrutinee releases it) must not double-free: the scrutinee
    // release and the rebuilt value's ownership are independent. Prints 1400.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn relay() -> Result[Node, i64] {
    match take() {
        Err(e) => Err(e),
        Ok(n) => Ok(n),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        match relay() {
            Err(e) => { t = t + e; }
            Ok(n) => { t = t + n.val; }
        }
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["1400"],
        "freshtemp_call_match_result_shared_rebuild_arm_no_double_free",
    );
}

#[test]
fn asan_let_bound_match_result_shared_no_leak() {
    // B-2026-07-12-24 (residual, escape-gated): a USER `let d = take(); match
    // d { … }` — the idiomatic bind-then-match, `d` consumed in place — used
    // to leak (only the synthetic direct-`match` scrutinee was released). The
    // conservative escape analysis (`crate::result_escape`) recognizes `d` as
    // non-escaping (used solely as a match scrutinee) and `track_rc_result_var`
    // releases it. Looped 200x; prints 1400.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn caller() -> i64 {
    let d = take();
    match d {
        Err(e) => e,
        Ok(n) => n.val,
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + caller();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["1400"],
        "let_bound_match_result_shared_no_leak",
    );
}

#[test]
fn asan_if_let_result_shared_no_leak() {
    // B-2026-07-12-24 (residual): `if let Ok(n) = d { … }` is match-sugar —
    // a consume-in-place use of `d`, exactly like `match d`. The escape
    // analysis now recognizes if-let (and while-let / let-else) scrutinees
    // as consume points (not just `match`), so `d` is released. Looped 200x;
    // prints 1400.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn caller() -> i64 {
    let d = take();
    let mut r = 0;
    if let Ok(n) = d {
        r = n.val;
    }
    r
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + caller();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["1400"],
        "if_let_result_shared_no_leak",
    );
}

#[test]
fn asan_if_let_result_shared_move_out_body_no_double_free() {
    // B-2026-07-12-24 (residual) guard: an if-let whose body MOVES the
    // payload out (`if let Ok(n) = d { Ok(n) }`) must not double-free — the
    // scrutinee `d`'s scope-exit dec and the rebuilt value's ownership are
    // independent (same balance as the `match` returning-arm case). The
    // rebuilt `Ok(n)` flows to a consumer (`match relay()`) that releases
    // it. Prints 1400.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn relay() -> Result[Node, i64] {
    let d = take();
    if let Ok(n) = d {
        Ok(n)
    } else {
        Err(0)
    }
}
fn caller() -> i64 {
    match relay() {
        Err(e) => e,
        Ok(n) => n.val,
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + caller();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["1400"],
        "if_let_result_shared_move_out_body_no_double_free",
    );
}

/// B-2026-09-06-50 — A BOXED STRUCT PAYLOAD DESTRUCTURED OUT OF A BY-VALUE
/// PARAM ABORTED WITH A DOUBLE FREE ON EVERY COMPILED BACKEND.
///
/// `fn show(x: Option[P])` over an 8-word `P` boxes its payload. The box's
/// interior belongs to the CALLER (`owned_boxed_option_param_struct` arms
/// box AND interior), which is why the callee's owned-param loop
/// deliberately registers nothing for a struct payload. A destructuring arm
/// then gave every field it BOUND a second owner — `bind_pattern_values`
/// hands each heap leaf its own drop — and both freed the same buffers:
/// `free(): double free detected in tcache 2`, exit 134, under `karac run`
/// and `karac build` alike at `-O0` and `-O2`, 2 invalid frees per call
/// under valgrind, while the interpreter printed the right answer.
///
/// The disarm that exists for this (`suppress_boxed_payload_struct_-
/// destructure`, which zeroes the bound fields' `cap` words INSIDE the box
/// so the owner's walk skips exactly them) was gated on
/// `boxed_enum_payload_vars` — a set of things the frame OWNS. A param owns
/// nothing, so the gate bailed on its first line. The fix is reach, not
/// ownership: `boxed_struct_payload_param_vars`.
///
/// Every cell is a direction the fix has to keep straight, and three of
/// them are the OVER-disarm directions — a field wrongly disarmed leaks
/// instead of double-freeing, so a test that only pinned the abort would
/// pass on a fix that traded one for the other:
///   - `Some(P { a, b, .. })` partial destructure — the reported abort;
///   - `Some(P { a, b, c, d })` all fields bound — abort too, which is what
///     ruled out `..` rest-field handling as the cause;
///   - `Some(P { a: _, b, .. })` — `a` is TESTED, not bound, so the box
///     must still free it;
///   - `Some(P { a, .. })` whose arm MOVES `a` out to the caller — the leaf
///     owns `a`, the box still owns `b`;
///   - `Some(t)` whole binding — no destructure, nothing to disarm, and it
///     was already clean; it must stay that way.
///
/// The one-heap-field and `Vec`-field cells pin that the axis is the
/// payload being a boxed user struct at all, not the field count: a `P`
/// with a single `String` aborted identically.
///
/// The NESTED cells are the second half of the fix and the reason it is not
/// a one-line gate change. `zero_struct_field_move_cap`'s tail arm recurses
/// into a user-struct field and zeroes every cap it finds, so disarming a
/// nested field WHOLE stopped the box freeing the nested fields the
/// sub-pattern never bound: `Some(P { i: Inner { s, .. }, .. })` traded the
/// abort for 11 B lost per call. The disarm now recurses with the same
/// bind-vs-test rule, and the three-level cell pins that it keeps
/// recursing rather than falling back to the blunt zero at depth two.
///
/// A `Drop`-bearing payload needs no cell — the typechecker rejects a
/// pattern that moves fields out of a struct with its own `impl Drop`
/// (design.md § Part 8), so that shape cannot reach codegen.
#[test]
fn asan_boxed_struct_param_payload_destructure_no_double_free() {
    assert_clean_asan_run(
        r#"
struct P { a: String, b: String, c: i64, d: i64 }
struct Q { a: String, c: i64, d: i64, e: i64 }
struct V { a: Vec[String], b: String, c: i64, d: i64 }
struct J { u: String, v: String }
struct Inner { j: J, t: String }
struct N { i: Inner, b: String, c: i64, d: i64 }

fn partial(x: Option[P]) {
    match x { Some(P { a, b, .. }) => { println(f"p:{a}:{b}"); } None => { println("pn"); } }
}

fn allbound(x: Option[P]) {
    match x { Some(P { a, b, c, d }) => { println(f"f:{a}:{b}:{c}{d}"); } None => { println("fn"); } }
}

fn tested(x: Option[P]) {
    match x { Some(P { a: _, b, .. }) => { println(f"t:{b}"); } None => { println("tn"); } }
}

fn moveout(x: Option[P]) -> String {
    match x { Some(P { a, .. }) => a, None => f"none" }
}

fn whole(x: Option[P]) {
    match x { Some(t) => { println(f"w:{t.a}"); } None => { println("wn"); } }
}

fn onefield(x: Option[Q]) {
    match x { Some(Q { a, .. }) => { println(f"o:{a}"); } None => { println("on"); } }
}

fn vecfield(x: Option[V]) {
    match x { Some(V { a, b, .. }) => { println(f"v:{a.len()}:{b}"); } None => { println("vn"); } }
}

fn nested(x: Option[N]) {
    match x { Some(N { i: Inner { t, .. }, b, .. }) => { println(f"n:{t}:{b}"); } None => { println("nn"); } }
}

fn deep(x: Option[N]) {
    match x { Some(N { i: Inner { j: J { u, .. }, .. }, b, .. }) => { println(f"d:{u}:{b}"); } None => { println("dn"); } }
}

fn main() {
    let mut n = 0;
    while n < 3 {
        partial(Some(P { a: f"pa-{n}-padpad", b: f"pb-{n}-padpad", c: 1, d: 2 }));
        allbound(Some(P { a: f"fa-{n}-padpad", b: f"fb-{n}-padpad", c: 3, d: 4 }));
        tested(Some(P { a: f"ta-{n}-padpad", b: f"tb-{n}-padpad", c: 5, d: 6 }));
        let m = moveout(Some(P { a: f"ma-{n}-padpad", b: f"mb-{n}-padpad", c: 7, d: 8 }));
        println(f"m:{m}");
        whole(Some(P { a: f"wa-{n}-padpad", b: f"wb-{n}-padpad", c: 9, d: 0 }));
        onefield(Some(Q { a: f"oa-{n}-padpad", c: 1, d: 2, e: 3 }));
        let mut vs: Vec[String] = Vec.new();
        vs.push(f"v0-{n}-padpad");
        vs.push(f"v1-{n}-padpad");
        vecfield(Some(V { a: vs, b: f"vb-{n}-padpad", c: 1, d: 2 }));
        // A NAMED-BINDING argument reaches the same callee through a different
        // caller-side owner (the let-site drop, not an arg temp) and aborted
        // identically -- the row recorded only the fresh-temp spelling.
        let named: Option[P] = Some(P { a: f"na-{n}-padpad", b: f"nb-{n}-padpad", c: 1, d: 2 });
        partial(named);
        nested(Some(N { i: Inner { j: J { u: f"nu-{n}-padpad", v: f"nv-{n}-padpad" }, t: f"nt-{n}-padpad" }, b: f"nb2-{n}-padpad", c: 1, d: 2 }));
        deep(Some(N { i: Inner { j: J { u: f"du-{n}-padpad", v: f"dv-{n}-padpad" }, t: f"dt-{n}-padpad" }, b: f"db-{n}-padpad", c: 1, d: 2 }));
        n = n + 1;
    }
}
"#,
        &[
            "p:pa-0-padpad:pb-0-padpad",
            "f:fa-0-padpad:fb-0-padpad:34",
            "t:tb-0-padpad",
            "m:ma-0-padpad",
            "w:wa-0-padpad",
            "o:oa-0-padpad",
            "v:2:vb-0-padpad",
            "p:na-0-padpad:nb-0-padpad",
            "n:nt-0-padpad:nb2-0-padpad",
            "d:du-0-padpad:db-0-padpad",
            "p:pa-1-padpad:pb-1-padpad",
            "f:fa-1-padpad:fb-1-padpad:34",
            "t:tb-1-padpad",
            "m:ma-1-padpad",
            "w:wa-1-padpad",
            "o:oa-1-padpad",
            "v:2:vb-1-padpad",
            "p:na-1-padpad:nb-1-padpad",
            "n:nt-1-padpad:nb2-1-padpad",
            "d:du-1-padpad:db-1-padpad",
            "p:pa-2-padpad:pb-2-padpad",
            "f:fa-2-padpad:fb-2-padpad:34",
            "t:tb-2-padpad",
            "m:ma-2-padpad",
            "w:wa-2-padpad",
            "o:oa-2-padpad",
            "v:2:vb-2-padpad",
            "p:na-2-padpad:nb-2-padpad",
            "n:nt-2-padpad:nb2-2-padpad",
            "d:du-2-padpad:db-2-padpad",
        ],
        "asan_boxed_struct_param_payload_destructure_no_double_free",
    );
}

/// B-2026-09-06-56 — the `Result` sibling of the test above, and NOT the
/// same defect despite the identical source shape. The `Option` spelling
/// had TWO owners and aborted; the `Result` spelling had NONE and leaked,
/// because both frames declined it citing the same per-variant asymmetry:
/// the caller-side registration read `Option` only, and the callee's
/// owned-param loop skipped every non-`Option` enum.
///
/// Measured before the fix, `KARAC_AUTO_PAR=0` at `-O0` under valgrind:
/// 192 B definitely lost in 3 blocks for the fresh-temp partial
/// destructure, 0 invalid frees. 64 B per call is the ENVELOPE alone —
/// both heap fields are bound by the arm, so the bindings own them.
///
/// The cells and what each pins:
///   - `Ok(P { a, b, .. })` fresh temp — the reported leak;
///   - `Ok(P { a, .. })` — `b` is neither bound nor owned by anyone, so the
///     interior leaked too (27 B indirect over three calls), which is what
///     makes this the WHOLE payload and not just its envelope;
///   - `Ok(P { a: _, b, .. })` — `a` is TESTED, not bound; the box keeps it;
///   - `Ok(p)` whole binding — no destructure, nothing to disarm;
///   - a NAMED-LOCAL `Result[P, i64]` argument, which is the row's
///     correction: it does not leak, it ABORTS (`free(): double free
///     detected in tcache 2`, 6 invalid frees over three calls). Its let
///     site already owns the box interior, so the callee's silent
///     destructure made the arm's leaf bindings a second owner — the
///     `Option` mechanism exactly, in the spelling the row filed as a leak;
///   - the `Err` SIDE boxed (`Result[i64, P]`), which leaked at the same
///     rate and was on the row's NOT-MEASURED list;
///   - BOTH sides boxed (`Result[P, Q]`), exercised down both paths: 384 B
///     over six calls before the fix, which is why the registration is
///     per-variant rather than one arm standing for the pair;
///   - a single-heap-field payload wide enough to box against `Result`'s
///     5-word area, pinning that the axis is boxedness and not field count;
///   - a NESTED destructure, pinning that the recursive disarm added for
///     B-2026-09-06-50 reaches through the `Result` gate too.
#[test]
fn asan_boxed_struct_result_param_payload_destructure_no_leak() {
    assert_clean_asan_run(
        r#"
struct P { a: String, b: String, c: i64, d: i64 }
struct Q { m: String, n: String, o: i64, p: i64 }
struct W { g: String, h: i64, i: i64, j: i64, k: i64, l: i64 }
struct Inner { s: String, t: String }
struct N { i: Inner, u: String, v: i64, w: i64, x: i64 }

fn partial(x: Result[P, i64]) {
    match x { Ok(P { a, b, .. }) => { println(f"p:{a}:{b}"); } Err(e) => { println(f"pe:{e}"); } }
}

fn unbound(x: Result[P, i64]) {
    match x { Ok(P { a, .. }) => { println(f"u:{a}"); } Err(e) => { println(f"ue:{e}"); } }
}

fn tested(x: Result[P, i64]) {
    match x { Ok(P { a: _, b, .. }) => { println(f"t:{b}"); } Err(e) => { println(f"te:{e}"); } }
}

fn whole(x: Result[P, i64]) {
    match x { Ok(r) => { println(f"w:{r.a}"); } Err(e) => { println(f"we:{e}"); } }
}

fn errside(x: Result[i64, P]) {
    match x { Ok(v) => { println(f"k:{v}"); } Err(P { a, b, .. }) => { println(f"r:{a}:{b}"); } }
}

fn bothsides(x: Result[P, Q]) {
    match x { Ok(P { a, b, .. }) => { println(f"b:{a}:{b}"); } Err(Q { m, n, .. }) => { println(f"c:{m}:{n}"); } }
}

fn onefield(x: Result[W, i64]) {
    match x { Ok(W { g, .. }) => { println(f"o:{g}"); } Err(e) => { println(f"oe:{e}"); } }
}

fn nested(x: Result[N, i64]) {
    match x { Ok(N { i: Inner { s, .. }, u, .. }) => { println(f"n:{s}:{u}"); } Err(e) => { println(f"ne:{e}"); } }
}

fn main() {
    let mut n = 0;
    while n < 3 {
        partial(Ok(P { a: f"pa-{n}-padpad", b: f"pb-{n}-padpad", c: 1, d: 2 }));
        unbound(Ok(P { a: f"ua-{n}-padpad", b: f"ub-{n}-padpad", c: 3, d: 4 }));
        tested(Ok(P { a: f"ta-{n}-padpad", b: f"tb-{n}-padpad", c: 5, d: 6 }));
        whole(Ok(P { a: f"wa-{n}-padpad", b: f"wb-{n}-padpad", c: 7, d: 8 }));
        // The NAMED-LOCAL spelling, which ABORTED rather than leaked: its let
        // site owns the box, so only the callee-side disarm keeps the arm's
        // bindings from becoming a second owner.
        let named: Result[P, i64] = Ok(P { a: f"na-{n}-padpad", b: f"nb-{n}-padpad", c: 9, d: 0 });
        partial(named);
        errside(Err(P { a: f"ra-{n}-padpad", b: f"rb-{n}-padpad", c: 1, d: 2 }));
        errside(Ok(7));
        bothsides(Ok(P { a: f"ba-{n}-padpad", b: f"bb-{n}-padpad", c: 1, d: 2 }));
        bothsides(Err(Q { m: f"cm-{n}-padpad", n: f"cn-{n}-padpad", o: 1, p: 2 }));
        onefield(Ok(W { g: f"og-{n}-padpad", h: 1, i: 2, j: 3, k: 4, l: 5 }));
        nested(Ok(N { i: Inner { s: f"ns-{n}-padpad", t: f"nt-{n}-padpad" }, u: f"nu-{n}-padpad", v: 1, w: 2, x: 3 }));
        n = n + 1;
    }
}
"#,
        &[
            "p:pa-0-padpad:pb-0-padpad",
            "u:ua-0-padpad",
            "t:tb-0-padpad",
            "w:wa-0-padpad",
            "p:na-0-padpad:nb-0-padpad",
            "r:ra-0-padpad:rb-0-padpad",
            "k:7",
            "b:ba-0-padpad:bb-0-padpad",
            "c:cm-0-padpad:cn-0-padpad",
            "o:og-0-padpad",
            "n:ns-0-padpad:nu-0-padpad",
            "p:pa-1-padpad:pb-1-padpad",
            "u:ua-1-padpad",
            "t:tb-1-padpad",
            "w:wa-1-padpad",
            "p:na-1-padpad:nb-1-padpad",
            "r:ra-1-padpad:rb-1-padpad",
            "k:7",
            "b:ba-1-padpad:bb-1-padpad",
            "c:cm-1-padpad:cn-1-padpad",
            "o:og-1-padpad",
            "n:ns-1-padpad:nu-1-padpad",
            "p:pa-2-padpad:pb-2-padpad",
            "u:ua-2-padpad",
            "t:tb-2-padpad",
            "w:wa-2-padpad",
            "p:na-2-padpad:nb-2-padpad",
            "r:ra-2-padpad:rb-2-padpad",
            "k:7",
            "b:ba-2-padpad:bb-2-padpad",
            "c:cm-2-padpad:cn-2-padpad",
            "o:og-2-padpad",
            "n:ns-2-padpad:nu-2-padpad",
        ],
        "asan_boxed_struct_result_param_payload_destructure_no_leak",
    );
}

/// B-2026-07-03-27 (FIXED 009fd479-follow-on): an `Option[E]` field where `E`
/// is a PLAIN (non-`shared`) user enum carrying a heap payload, destructured
/// into a local and dropped, leaked the enum payload's heap buffer — the
/// inline-Option drop path (cf. B-2026-06-10-6, which covered
/// `Option[String]`/`[Vec]`/`[Map]`) did not recurse into a user-enum (or
/// struct) payload's drop, and the destructure leaf got no cleanup at all
/// (`destructure_field_needs_cleanup` excludes `Option`; struct drop skips
/// `Option` fields — B-2026-07-03-28). The fix registers a tag-guarded
/// `karac_drop_Option_<payload>` (`emit_option_drop_fn` — the same fn the
/// `Vec[Option[..]]` element path uses, handling the heap-BOXED wide payload)
/// on the leaf when the destructure OWNS the source, paired with a Some-arm
/// tag-zeroing suppressor so a `Some(v)` move-out doesn't double-free.
/// Exercises: (1) `Some(_)` wildcard match then drop; (2) `Some(v)` move-out
/// (the bound payload frees it once, source drop suppressed); (3) a
/// fresh-temp source destructure. Payloads are 40 bytes (LSan reachability).
/// B-2026-07-03-27 (FIXED): an `Option[E]` field where `E` is a PLAIN
/// (non-`shared`) user enum/struct carrying a heap payload, destructured into
/// a local and dropped UNDESTRUCTURED (a `Some(_)` wildcard match, or plain
/// scope-drop), leaked the payload's heap buffer. The inline-Option drop
/// (B-2026-06-10-6) covered only `Option[String]`/`[Vec]`/`[Map]`, and the
/// destructure leaf got no cleanup (`destructure_field_needs_cleanup` excludes
/// `Option`; struct drop skips `Option` fields — B-2026-07-03-28). The fix
/// registers a tag-guarded `karac_drop_Option_<payload>` (`emit_option_drop_fn`
/// — the same fn the `Vec[Option[..]]` element path uses, handling the
/// heap-BOXED wide enum payload) on the leaf when the destructure OWNS the
/// source. LSan-confirmed: without the fix this leaks 360 B / 10 allocs.
/// Covers a Vec-sourced (moved-in owned param) source and a fresh-temp source.
#[test]
fn asan_b27_option_enum_undestructured_drop_no_leak() {
    assert_clean_asan_run(
        r#"
enum Val { Nothing, Ident(String) }
struct A { value: Option[Val] }
fn use_wild(a: A) -> i64 { let A { value } = a; match value { Some(_) => 1, None => 0 } }
fn mk() -> A { A { value: Some(Val.Ident("kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk".to_string())) } }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { value: Some(Val.Ident("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string())) }); i = i + 1; }
    v
}
fn main() {
    let mut t = 0;
    let xs = build();                            // moved-in owned param source
    for a in xs { t = t + use_wild(a); }
    let mut i = 0;                               // fresh-temp source
    while i < 6 { t = t + use_wild(mk()); i = i + 1; }
    println(t);
}
"#,
        &["12"],
        "b27_option_enum_undestructured_drop",
    );
}

/// Sibling of the enum case — an `Option[<user struct>]` field
/// (`Option[Inner]`, `Inner { s: String }`) destructured and dropped
/// undestructured. Same fix (`emit_option_drop_fn` recurses into the
/// struct's `__karac_drop_struct_Inner`). B-2026-07-03-27.
///
/// COVERAGE HOLE CLOSED 2026-08-05 (B-2026-08-05-22). This fixture used to
/// carry only the Vec-sourced (moved-in owned param) half, while its enum
/// sibling carries BOTH that and a fresh-temp source — and the fresh-temp
/// half is the one that leaks. A 2x2 over {enum, struct} payload x {Vec,
/// fresh-temp} source says the SOURCE is the axis and the payload kind is
/// not: Vec-sourced is clean for both, fresh-temp leaks for both (enum
/// 360 B/10, struct 200 B/5 at -O0). So this test passed for a reason that
/// had nothing to do with the struct payload — it simply never ran the
/// broken path. The `use_a(mk())` half below is that path.
///
/// -O0 ONLY, deliberately, and it is NOT floored with
/// `assert_clean_asan_run_min_allocs`. `Some(_)` never reads the payload,
/// so this is B-2026-08-04-17's DISCARD-shaped family: at -O2 LLVM deletes
/// the allocation whether or not it is freed, which is why the added half
/// reads clean there. A min-allocs floor would therefore fail at -O2 on a
/// correct compiler. Per that row's conclusion for this family, the -O0
/// sweep is the only lever and the fixture is not contorted to chase -O2.
#[test]
fn asan_b27_option_struct_undestructured_drop_no_leak() {
    assert_clean_asan_run(
        r#"
struct Inner { s: String }
struct A { value: Option[Inner] }
fn use_a(a: A) -> i64 { let A { value } = a; match value { Some(_) => 1, None => 0 } }
fn mk() -> A { A { value: Some(Inner { s: "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm".to_string() }) } }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { value: Some(Inner { s: "ssssssssssssssssssssssssssssssssssssssss".to_string() }) }); i = i + 1; }
    v
}
fn main() {
    let xs = build();
    let mut t = 0;
    for a in xs { t = t + use_a(a); }            // Vec-sourced: always was clean
    let mut i = 0;                               // fresh-temp source: the broken path
    while i < 6 { t = t + use_a(mk()); i = i + 1; }
    println(t);
}
"#,
        &["12"],
        "b27_option_struct_undestructured_drop",
    );
}

/// B-2026-08-05-7 (destructure leg): destructuring a struct field typed
/// `Option[<transparent single-heap-field wrapper>]` registered TWO
/// scope-exit frees for one payload, and both fired — double-free.
///
/// `Inner { s: String }` lays out bit-identically to a bare `String`, so
/// `inline_heap_payload_elem`'s transparent-wrapper arm makes
/// `destructure_field_needs_cleanup` true and the destructure chain gives the
/// leaf an inline-Option free; `option_field_agg_drop_ok` then independently
/// claims the same field as a struct/enum payload and armed a second free on
/// the same binding. Its comment asserted the branches could not overlap
/// ("no `Option` arm above"), which held for every payload EXCEPT the
/// transparent one.
///
/// The sibling fixture above is the same shape with a two-field `Inner`,
/// which is not layout-transparent and was therefore always clean — that
/// near-miss is why the pair is worth keeping side by side.
///
/// NOTE: this pins at -O0 only. At -O2 the second free is unreachable
/// because the optimizer deletes the allocation it would have hit, so no
/// -O2 assertion can catch a regression here; the -O0 sweep is the gate.
#[test]
fn asan_destructured_option_transparent_wrapper_single_free() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Inner { s: String }
struct A { value: Option[Inner] }
fn use_a(a: A) -> i64 { let A { value } = a; match value { Some(_) => 1, None => 0 } }
fn build(n: i64) -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < n + 49 { v.push(A { value: Some(Inner { s: f"payload-{i}-ssssssssssssssssssssssssssssss" }) }); i = i + 1; }
    v
}
fn main() {
    let base: i64 = env.args().len();
    let xs = build(base);
    let mut t = 0;
    for a in xs { t = t + use_a(a); }
    println(t);
}
"#,
        &["50"],
        "destructured_option_transparent_wrapper_single_free",
        // 50 runtime-derived payloads plus Vec growth.
        50,
    );
}

/// B-2026-08-29-31 — the memory leg of
/// `e2e_wildcard_let_discard_owns_what_its_arm_hands_out`.
///
/// The `let _ =` spelling of a discarded branch stranded whatever its arm
/// handed out: 9 B per evaluation for an enclosing local, 3 B for an arm's
/// own pattern binding, measured with valgrind at `KARAC_OPT_LEVEL=0`
/// against the pre-fix compiler, and 0 after. The BARE-STATEMENT spelling
/// of the identical program was already clean, which is what identified
/// the statement kind rather than the branch as the unit.
///
/// NO OPT-LEVEL SPLIT, unlike the B-2026-08-29-30 fixture below, and the
/// difference is worth stating because it makes this the stronger of the
/// two. There the discarded value was a pure temporary with no other use,
/// so at `-O2` LLVM deleted the whole allocation chain and the pre-fix
/// program did not leak at all. HERE the value starts life as a real
/// binding (`let r = mk(41)`) that is live until the discard, so the
/// allocation survives optimization: the 20-iteration row measures 760 B
/// definitely lost in 20 blocks at BOTH `KARAC_OPT_LEVEL=0` AND the
/// default `-O2`, and 0 at both after. So this fixture is a genuine leak
/// gate at the level CI actually runs.
///
/// It gates the opposite direction too, and that is the live hazard: this
/// row hands ownership between three sites — the arm, the statement frame,
/// and the binding's own scope exit — and two successive attempts at the
/// block leg produced a DOUBLE body before the guard was narrowed
/// correctly. The `guard:` rows are those cases.
///
/// In `go()` rather than `main` because LSan scans the live stack
/// conservatively.
#[test]
fn asan_wildcard_let_discard_owns_what_its_arm_hands_out() {
    const H: &str = "struct R { id: i64, s: String }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mk(i: i64) -> R { return R { id: i, s: payload() }; }\n\
             fn main() { println(go()); }\n";
    let rows: &[(&str, &str, &[&str])] = &[
        (
            "if-hands-out-a-local",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let r = mk(41);\n\
                 \x20  let _ = if n == 9 { r } else { mk(9) };\n\
                 \x20  1 }\n",
            &["1"],
        ),
        (
            "match-hands-out-a-local",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let r = mk(41);\n\
                 \x20  let _ = match n { 9 => r, _ => mk(9) };\n\
                 \x20  1 }\n",
            &["1"],
        ),
        (
            "block-hands-out-a-local",
            "fn go() -> i64 { let r = mk(41);\n\
                 \x20  let _ = { r };\n\
                 \x20  1 }\n",
            &["1"],
        ),
        (
            "payload-braced-arm",
            "fn go() -> i64 { let o: Option[R] = Some(mk(1));\n\
                 \x20  let _ = match o { Some(r) => { r } None => { mk(9) } };\n\
                 \x20  1 }\n",
            &["1"],
        ),
        // The leak signal: 20 discarded allocations, so a per-evaluation
        // leak compounds instead of hiding as one. 760 B / 20 blocks
        // before this row, at both opt levels; 0 after.
        (
            "twenty-iteration-loop-compounds-the-leak",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let mut i = 0;\n\
                 \x20  while i < 20 {\n\
                 \x20    let r = mk(i);\n\
                 \x20    let _ = if n == 9 { r } else { mk(9) };\n\
                 \x20    i = i + 1;\n\
                 \x20  }\n\
                 \x20  1 }\n",
            &["1"],
        ),
        // ── double-own guards: a second owner here is a DOUBLE FREE ──
        (
            "guard: block-wrapped match is owned by the statement site",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = { match n { 1 => { mk(7) } _ => { mk(3) } } };\n\
                 \x20  1 }\n",
            &["1"],
        ),
        (
            "guard: block-wrapped call is owned by the statement site",
            "fn go() -> i64 { let _ = { mk(7) };\n\
                 \x20  1 }\n",
            &["1"],
        ),
        (
            "guard: mixed branch owns the minted arm without doubling the local",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let r = mk(18);\n\
                 \x20  let _ = if n == 9 { r } else { mk(4) };\n\
                 \x20  1 }\n",
            &["1"],
        ),
        // ── controls, clean before and after ─────────────────────────
        (
            "control: bare-statement form",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let r = mk(41);\n\
                 \x20  if n == 9 { r } else { mk(9) };\n\
                 \x20  1 }\n",
            &["1"],
        ),
        (
            "control: all-mint if stays statement-owned",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = if n == 1 { mk(2) } else { mk(3) };\n\
                 \x20  1 }\n",
            &["1"],
        ),
        (
            "control: all-mint match stays statement-owned",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = match n { 1 => mk(2), _ => mk(3) };\n\
                 \x20  1 }\n",
            &["1"],
        ),
    ];
    for (label, body, want) in rows {
        assert_clean_asan_run(&format!("{H}{body}"), want, label);
    }
}

/// B-2026-08-29-30 (remaining half) — the LEAK leg of
/// `e2e_no_else_if_arm_owns_the_value_it_mints`.
///
/// A no-`else` `if` whose taken arm MINTS an owned value stranded one
/// allocation per evaluation: `compile_if`'s merge yields a const-0
/// placeholder with no `else`, so the statement discard site never sees
/// the value, and B-2026-08-29-5's arm-level owner declined it because
/// `materialize_owned_temp` owns a Vec/String buffer, a Map/Set handle and
/// an RC box — never a plain user struct.
///
/// WHAT THIS FIXTURE GATES AT EACH OPT LEVEL, stated plainly because the
/// two answers differ and the difference is not a weakness in the test:
///
///   * At `KARAC_OPT_LEVEL=0` it is a LEAK gate. Measured on a
///     20-iteration loop of the `let _ =` spelling against the pre-fix
///     compiler: 770 B definitely lost in 20 blocks, and 0 after.
///   * At the DEFAULT `-O2` — the level this harness builds at, and the
///     one CI runs — the pre-fix program does not leak AT ALL, and that is
///     a true fact about it rather than a hidden failure: the discarded
///     allocation is dead, so LLVM deletes the whole chain. The same
///     20-iteration loop measures 0 bytes in 0 blocks BEFORE and after. So
///     at `-O2` these rows gate the OTHER direction — that the owner this
///     fix adds does not DOUBLE FREE or use-after-free — which is what the
///     control rows are for, and is exactly the hazard a new owner brings.
///
/// The observable half of the defect at `-O2` is the missing `Drop` BODY,
/// and that is gated at CI's level by
/// `codegen::e2e_no_else_if_arm_owns_the_value_it_mints`. Do not try to
/// rescue the leak claim here with an allocation floor: the audit reads
/// 22–23 allocations either way, because the surviving `println` and
/// f-string machinery dwarfs the one allocation at issue.
///
/// In `go()` rather than `main` for the reason the sibling above records:
/// LSan scans the live stack conservatively, so a stale `{ptr,len,cap}`
/// still sitting in `main`'s frame reads as reachable and is not reported.
///
/// The CONTROLS are the half that matters most: each was CLEAN before the
/// fix and is a shape where the new owner, if it over-reached, would be a
/// DOUBLE FREE rather than a leak — a tail naming a live local, an `if`
/// WITH an `else` (owned by the statement site), and a struct literal
/// whose field names a live local.
#[test]
fn asan_no_else_if_arm_owns_the_value_it_mints() {
    const H: &str = "struct D { s: String }\n\
             struct R { id: i64, s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mk(i: i64) -> R { return R { id: i, s: payload() }; }\n\
             fn slen(s: String) -> i64 { if s.contains(\"payload\") { s.len() } else { 0 } }\n\
             fn main() { println(go()); }\n";
    let rows: &[(&str, &str, &[&str])] = &[
        (
            "no-drop-impl-still-frees-its-heap",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = if n == 1 { D { s: payload() } };\n\
                 \x20  1 }\n",
            &["1"],
        ),
        (
            "wildcard-let-struct-literal",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = if n == 1 { R { id: 7, s: payload() } };\n\
                 \x20  1 }\n",
            &["dR7", "1"],
        ),
        (
            "wildcard-let-call",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = if n == 1 { mk(7) };\n\
                 \x20  1 }\n",
            &["dR7", "1"],
        ),
        (
            "bare-statement-struct-literal",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  if n == 1 { R { id: 7, s: payload() } };\n\
                 \x20  1 }\n",
            &["dR7", "1"],
        ),
        (
            "bare-statement-call",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  if n == 1 { mk(7) };\n\
                 \x20  1 }\n",
            &["dR7", "1"],
        ),
        // A heap-bearing struct with NO `impl Drop`: nothing observable
        // fires, so this row is a memory assertion and nothing else — the
        // half a body-count fixture structurally cannot see. At `-O0` it
        // is this fixture's cleanest leak signal; at `-O2` it holds that
        // the widened admission gate did not start double-freeing a
        // struct that has no user drop to make the mistake visible.
        (
            "block-wrapped-rhs",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = { if n == 1 { mk(19) } };\n\
                 \x20  1 }\n",
            &["dR19", "1"],
        ),
        (
            "nested-branch-tail",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = if n == 1 { if n == 1 { mk(6) } else { mk(7) } };\n\
                 \x20  1 }\n",
            &["dR6", "1"],
        ),
        (
            "tuple-literal-tail",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = if n == 1 { (mk(12), 20) };\n\
                 \x20  1 }\n",
            &["dR12", "1"],
        ),
        // Once per iteration, so a per-evaluation leak compounds rather
        // than costing one allocation.
        (
            "loop-body-per-iteration",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  for i in 0..3 { if n == 1 { mk(i) }; }\n\
                 \x20  1 }\n",
            &["dR0", "dR1", "dR2", "1"],
        ),
        // The `-O0` LEAK SIGNAL of this fixture, and the row whose numbers
        // the doc quotes: 20 discarded allocations, so a per-evaluation
        // leak compounds to 770 B / 20 blocks instead of hiding as one.
        // `D` rather than `R` so stdout stays a single line and the row is
        // purely a memory claim.
        (
            "twenty-iteration-loop-compounds-the-leak",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let mut i = 0;\n\
                 \x20  while i < 20 {\n\
                 \x20    let _ = if n == 1 { D { s: payload() } };\n\
                 \x20    i = i + 1;\n\
                 \x20  }\n\
                 \x20  1 }\n",
            &["1"],
        ),
        // ── controls: CLEAN before the fix; a double free if over-eager ──
        (
            "control: place-tail-bare",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let r = mk(1);\n\
                 \x20  if n == 1 { r };\n\
                 \x20  1 }\n",
            &["dR1", "1"],
        ),
        (
            "control: else-in-both-arms",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = if n == 1 { mk(4) } else { mk(5) };\n\
                 \x20  1 }\n",
            &["dR4", "1"],
        ),
        (
            "control: field-is-a-place",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let s = payload();\n\
                 \x20  let _ = if n == 1 { D { s: s } };\n\
                 \x20  1 }\n",
            &["1"],
        ),
        (
            "control: branch-not-taken",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = if n == 99 { mk(9) };\n\
                 \x20  1 }\n",
            &["1"],
        ),
        (
            "control: bound-value-still-owns-itself",
            "fn go() -> i64 { let d = D { s: payload() };\n\
                 \x20  slen(d.s) }\n",
            &["38"],
        ),
    ];
    for (label, body, want) in rows {
        assert_clean_asan_run(&format!("{H}{body}"), want, label);
    }
}

/// B-2026-08-29-5 — a branch construct whose VALUE IS DISCARDED strands
/// whatever its taken arm hands out.
///
/// TWO mechanisms, opposite in shape, which is why one fix would not have
/// covered the corpus:
///
///   * the arm tail names something that ALREADY HAS AN OWNER — a pattern
///     binding, or a local in scope. Handing it out suppresses that owner's
///     cleanup so the consumer can take over; with no consumer the buffer
///     is orphaned. The fix is to STOP SUPPRESSING, at the three arm-tail
///     sites (`compile_match`, `compile_if_let`, and — through
///     `branch_arm_value_discarded` — `compile_block_with_frame`).
///
///   * the arm tail MINTS its value (a call). There is no owner to leave
///     armed; nothing ever held it. The fix is to REGISTER one, in the
///     arm's own frame, gated on `expr_yields_fresh_owned_temp` so an
///     aliasing place-expr tail is never freed out from under its binding.
///
/// The `match` STATEMENT site already covered the second mechanism for
/// `match` alone (`discarded_match_value_tail`); it cannot cover `if` /
/// `if let`, because a no-`else` branch hands its arm's value to the
/// statement not at all — the merge yields a placeholder, so the buffer is
/// unreachable by the time that site runs. Hence an owner inside the arm.
///
/// The control rows are the real gate: every one of them was CLEAN before
/// the fix, and each is a shape where an over-eager free is a double free
/// rather than a leak.
#[test]
fn asan_discarded_branch_frees_the_value_its_arm_hands_out() {
    // EVERY CASE RUNS IN `go()`, NOT IN `main`, and that is load-bearing
    // rather than tidiness. LeakSanitizer scans the live stack
    // conservatively, so a leaked buffer whose `{ptr,len,cap}` is still
    // sitting in `main`'s frame reads as REACHABLE and is not reported —
    // six of these rows passed against the unfixed compiler when written
    // directly in `main`, while valgrind's `definitely lost` flagged every
    // one of them. Calling through a helper puts the stale slot below the
    // stack pointer by the time LSan looks, so the leak is a leak.
    const H: &str = "struct D { s: String }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn other() -> String { f\"other-{seed()}-bbbbbbbbbbbbbbbbbbbbbbbbbbbb\" }\n\
             fn slen(s: String) -> i64 { if s.contains(\"payload\") { s.len() } else { 0 } }\n\
             fn pass(s: String) -> String { s }\n\
             fn main() { println(go()); }\n";
    for (label, body, want) in [
        // ── mechanism 1: the tail hands out something already owned ──
        (
            "iflet-hands-payload-out",
            "fn go() -> i64 { let vv: Option[String] = Some(payload());\n\
                 \x20  if let Some(s) = vv { s };\n\
                 \x20  1 }\n",
            "1",
        ),
        (
            "match-hands-payload-out",
            "fn go() -> i64 { let vv: Option[String] = Some(payload());\n\
                 \x20  match vv { Some(s) => s, None => other() };\n\
                 \x20  1 }\n",
            "1",
        ),
        // A STRUCT payload: the binding owns the struct's interior heap
        // rather than a bare buffer, so it travels a different
        // registration.
        (
            "iflet-hands-struct-payload-out",
            "fn go() -> i64 { let vv: Option[D] = Some(D { s: payload() });\n\
                 \x20  if let Some(d) = vv { d };\n\
                 \x20  1 }\n",
            "1",
        ),
        (
            "iflet-with-else-hands-payload-out",
            "fn go() -> i64 { let vv: Option[String] = Some(payload());\n\
                 \x20  if let Some(s) = vv { s } else { other() };\n\
                 \x20  1 }\n",
            "1",
        ),
        // A plain `if` reaches the suppressor through
        // `compile_block_with_frame`, which holds no condition — hence the
        // carried flag rather than a `branch_value_is_owned` call.
        (
            "if-hands-local-out",
            "fn go() -> i64 { let a = payload(); let b = other();\n\
                 \x20  if seed() > 0 { a } else { b };\n\
                 \x20  1 }\n",
            "1",
        ),
        (
            "match-hands-local-out",
            "fn go() -> i64 { let a = payload(); let b = other();\n\
                 \x20  match seed() { 1 => a, _ => b };\n\
                 \x20  1 }\n",
            "1",
        ),
        // The if-let ELSE arm is its own site: it routes through
        // `compile_block_with_frame` while the THEN arm hand-rolls a frame.
        (
            "iflet-else-hands-local-out",
            "fn go() -> i64 { let vv: Option[String] = None; let b = other();\n\
                 \x20  if let Some(s) = vv { s } else { b };\n\
                 \x20  1 }\n",
            "1",
        ),
        // The sharp row: leaving the source ARMED is the fix, so the
        // source must still be readable afterwards AND freed exactly once.
        // An orphaned buffer reads fine and leaks; a disarmed-and-freed one
        // reads freed memory.
        (
            "source-survives-the-discard",
            "fn go() -> i64 { let a = payload();\n\
                 \x20  if seed() > 0 { a } else { other() };\n\
                 \x20  slen(a) }\n",
            "38",
        ),
        // ── mechanism 2: the tail mints its value ──
        (
            "if-fresh-in-both-arms",
            "fn go() -> i64 { if seed() > 0 { payload() } else { other() };\n\
                 \x20  1 }\n",
            "1",
        ),
        (
            "iflet-fresh-call-tail",
            "fn go() -> i64 { let vv: Option[String] = Some(payload());\n\
                 \x20  if let Some(s) = vv { pass(s) };\n\
                 \x20  1 }\n",
            "1",
        ),
        // A loop body's trailing branch is one of the three discarding
        // positions `compute_discarded_branch_spans` records; it leaks once
        // per iteration, so it also proves the owner is per-arm.
        (
            "if-fresh-in-loop-body",
            "fn go() -> i64 { for i in 0..3 { if seed() > 0 { payload() } else { other() }; }\n\
                 \x20  1 }\n",
            "1",
        ),
        (
            "block-wrapped-discard",
            "fn go() -> i64 { { if seed() > 0 { payload() } else { other() } };\n\
                 \x20  1 }\n",
            "1",
        ),
        // ── controls: an owner EXISTS, so a second free is a double free ──
        (
            "bound-and-used-not-handed-out",
            "fn go() -> i64 { let vv: Option[String] = Some(payload());\n\
                 \x20  if let Some(s) = vv { slen(s) } else { 0 } }\n",
            "38",
        ),
        (
            "iflet-value-bound",
            "fn go() -> i64 { let vv: Option[String] = Some(payload());\n\
                 \x20  let k = if let Some(s) = vv { s } else { other() };\n\
                 \x20  slen(k) }\n",
            "38",
        ),
        (
            "if-fresh-value-bound",
            "fn go() -> i64 { let k = if seed() > 0 { payload() } else { other() };\n\
                 \x20  slen(k) }\n",
            "38",
        ),
        // The aliasing place-expr branch WITH a consumer — the exact shape
        // the pre-fix deferral in `test_ir_discarded_branching_tail_temp_*`
        // was protecting. Here suppression is correct and must still run.
        (
            "if-local-value-bound",
            "fn go() -> i64 { let a = payload(); let b = other();\n\
                 \x20  let k = if seed() > 0 { a } else { b };\n\
                 \x20  slen(k) }\n",
            "38",
        ),
        (
            "arm-rebinds-the-payload",
            "fn go() -> i64 { let vv: Option[String] = Some(payload());\n\
                 \x20  if let Some(s) = vv { let t = s; }\n\
                 \x20  1 }\n",
            "1",
        ),
        // A discarded `match` whose arms are all fresh calls was already
        // covered by `discarded_match_value_tail`; it must not now be owned
        // twice.
        (
            "match-fresh-arms-already-covered",
            "fn go() -> i64 { match seed() { 1 => payload(), _ => other() };\n\
                 \x20  1 }\n",
            "1",
        ),
    ] {
        assert_clean_asan_run(&format!("{H}{body}"), &[want], label);
    }
}

// ── nested / enum tuple-destructure leaves from a LOCAL (B-2026-08-28-8,
//    B-2026-08-28-9) ─────────────────────────────────────────────────
//
// The place-source destructure walker now reaches two leaf shapes it used
// to skip, and registering a leaf's cleanup there is only sound because it
// ALSO cap-zeroes the source at that (possibly nested) index. Both halves
// are double-free risks in exactly the way this suite exists to catch: the
// source's own tuple walk does reach a nested leaf when the tuple is not
// destructured, so an over-registered leaf frees twice, and an
// over-zeroed source frees nothing.
//
// Every leaf here carries a heap `String` so the pairing is observable to
// ASAN at all (an all-scalar leaf would pass vacuously), and the loop makes
// a double-free or UAF trip rather than merely being possible.
#[test]
fn asan_nested_and_enum_tuple_destructure_leaves_from_a_local() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
enum E { A(R), B }
fn make_nested(n: i64) -> ((R, i64), i64) { ((R { id: n, name: f"n{n}" }, 2), 1) }
fn main() {
    let mut i = 0;
    while i < 200 {
        // nested pattern, LOCAL source — the leaf takes the body and the memory
        let p = ((R { id: i, name: f"a{i}" }, 2), 1);
        let ((r, m), n) = p;
        if i == 0 { println(f"{r.name} {r.id + m + n}"); }
        // the same nested shape from a CALL — the fresh path, already correct
        let ((r2, m2), n2) = make_nested(i);
        if i == 0 { println(f"{r2.name} {r2.id + m2 + n2}"); }
        // enum leaf, LOCAL source — reached only once the element type resolves
        let q = (E.A(R { id: i, name: f"e{i}" }), 1);
        let (e, k) = q;
        if i == 0 { println(f"{k}"); }
        // CONTROL — the same nested local NEVER destructured: the source's own
        // walk owns the leaf, so nothing here may have been zeroed away.
        let u = ((R { id: i, name: f"u{i}" }, 2), 1);
        if i == 0 { println(f"{u.1}"); }
        i = i + 1;
    }
    println("end");
}
"#,
        &["a0 3", "n0 3", "1", "1", "end"],
        "nested_and_enum_tuple_destructure_leaves_from_a_local",
    );
}

#[test]
fn asan_ref_param_field_chain_iflet_whilelet_letelse_no_leak_no_double_free() {
    // B-2026-07-21-8 memory leg: the if-let / while-let / let-else routes
    // of the ref-chain clone. Double-free half: consumed payloads freed
    // exactly once by their bindings, never again by the caller. Leak
    // half: the clone's unbound remainder must drain (freshtemp EnumDrop /
    // clone StructDrop), and while-let's final NON-matching header
    // evaluation must free its clone on the miss edge (the forced
    // wholesale drop) — a miss leak accumulates per call under LSan.
    // The no-match Holder exercises the miss edges each iteration.
    assert_clean_asan_run(
        r#"
enum Tok { Plus, Ident(String) }
struct Pt { s: String, x: i64 }
struct Holder { tok: Tok, inner: Pt, n: i64 }
fn iflet_enum(h: ref Holder) -> i64 {
    if let Ident(name) = h.tok {
        return ("i:".to_string() + name).len();
    }
    return 0;
}
fn whilelet_enum(h: ref Holder) -> i64 {
    while let Ident(name) = h.tok {
        return ("w:".to_string() + name).len();
    }
    return 0;
}
fn letelse_struct(h: ref Holder) -> i64 {
    let Pt { s, x } = h.inner else {
        return 0;
    }
    return ("l:".to_string() + s).len() + x;
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a = Holder { tok: Tok.Ident("token".to_string()), inner: Pt { s: "pt".to_string(), x: 3 }, n: 1 };
        acc = acc + iflet_enum(a) + whilelet_enum(a) + letelse_struct(a);
        let p = Holder { tok: Tok.Plus, inner: Pt { s: "q".to_string(), x: 1 }, n: 2 };
        acc = acc + iflet_enum(p) + whilelet_enum(p);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["840"],
        "ref_param_field_chain_iflet_whilelet_letelse",
    );
}

/// B-2026-08-06-31 — the STRUCT-payload sibling of B-2026-08-06-9 leg A,
/// both halves.
///
/// `H` is 4 LLVM words against `Option`'s seeded 3-word area, so
/// `coerce_to_payload_words` boxes it. Leg A gave the CALLEE the box for a
/// boxed NON-struct payload and excluded struct ones; what that left behind
/// was two distinct holes:
///
///   * A NAMED binding (`let a: Option[H] = …; readk(a);`) had its slot
///     zeroed as a move by the arg site, which disarmed the let-site
///     `BoxedEnumDrop`, and nothing took over — 320 B of box per 10 calls
///     at `-O0`, plus the interior.
///   * A FRESH TEMP got a caller-side drop, but a BOX-ONLY one, so the
///     payload's `String` leaked at BOTH opt levels.
///
/// WHICH FRAME OWNS a boxed struct payload turns on the ARGUMENT FORM, not
/// on the callee, and all three forms are exercised because a fix has to
/// land on exactly one of them each time: a `FieldAccess` argument leaves
/// the owning struct's field drop in charge
/// (`place_optres_field_whole_move_info` refuses a boxed payload for that
/// reason, and `f(nd.hp)` was already clean), a named binding keeps its
/// let-site drop, and a fresh temp has neither.
///
/// THE MOVE-OUT ARMS ARE THE POINT, and there are THREE of them, not two.
/// Leaving the caller armed is only safe because a callee arm that moves
/// the payload out neutralizes through the box's own words —
/// B-2026-08-06-10's mirror, gated on this owned-param shape. That mirror
/// had consumers for a FIELD move-out (`x.label`) and a WHOLE-value move
/// (`return x`), and NOT for a LET-DESTRUCTURE (`let H { label, k } = x;`),
/// which neutralizes by retracting a cleanup action in the callee's own
/// queue — invisible to a caller whose drop fn reads the box's data.
///
/// That third shape is why the first attempt at the named half was
/// reverted: it passed a matrix carrying only the first two and SIGSEGV'd
/// `selfhost_parser_matches_rust_parser_items`, whose `render_generics`
/// does exactly `let GenericParamsNode { params, span } = gp;` inside a
/// `Some(gp)` arm over a by-value param. Every one of the 17 corpus entries
/// that failed was a generic item. `take_destructure` below is that shape
/// reduced, and it is the arm that would catch a regression of it.
///
/// Plus the leak-direction controls a suppression change needs: `None`
/// sources on two callee shapes, and a binding never passed at all, whose
/// let-site drop must still fire.
///
/// COVERAGE. Against the pre-fix compiler this is RED at BOTH opt levels —
/// the named half's box folds away at `-O2` but the fresh-temp interior
/// does not — so unlike most of this family it bites on the default leg as
/// well as the `-O0` one (`scripts/asan-o0-leg.sh`, B-2026-08-04-17).
///
/// The expected value is COMPUTED, not read off a run: every payload is
/// seeded from the opaque `env.args().len()`, each scalar arm subtracts the
/// seed back out to leave `i`, and each byte-read arm contributes 1 —
/// `8i + 4` per iteration, so `8 * (0+…+39) + 40 * 4 = 6400`.
/// B-2026-08-28-73 shape 1 — a consuming arm that hands its bound BOXED
/// ENUM payload out as the match's value double-freed that payload.
///
/// REGRESSION from B-2026-08-28-63 (f862656). That commit widened the
/// boxed-payload BODIES branch in `pattern_binding.rs` to admit a user enum
/// — `enum G { A(String), B }` is 4 words against `Option`'s 3, so it boxes
/// — but left the VIEW-recording gate a few lines above it keyed on
/// `struct_types` alone. `boxed_optres_payload_view_vars` is what
/// `suppress_boxed_payload_view_move` reads to clear the box's inner walk
/// when the binding moves on, so a boxed ENUM payload got the registration
/// without the bookkeeping that neutralizes it: the source walk stayed
/// armed beside the destination's own drop and the payload's `String` was
/// freed twice.
///
/// NOT the braces hole B-2026-08-28-66 turned out to be — checked, because
/// the shapes look alike. Both `Some(g) => { g }` and `Some(g) => g` abort
/// identically here, so the arm-tail neutralizers are not involved.
///
/// SHIPPED-BINARY SEVERITY, not a sanitizer finding: plain `karac run`
/// aborts with `free(): double free detected in tcache 2` on the original
/// spelling, with no sanitizer in the picture.
///
/// COVERAGE — the `Drop` body READS the payload (`match self { G.A(s) => …`)
/// deliberately. With a body that prints a literal, nothing observes the
/// `String`, LLVM deletes the allocation at `-O2`, and the AOT binary passes
/// against the broken compiler; the row's own note calls that pass an
/// artifact. Reading the payload makes every compiled path abort at every
/// optimization level, so this fixture is red on the default leg as well as
/// under `KARAC_OPT_LEVEL=0`.
/// B-2026-08-28-73 shape 1, the BOUNDARY matrix beside
/// [`Self::asan_consuming_arm_boxed_enum_payload_handed_out_is_owned_once`].
///
/// That fixture carries the headline shape under a loop with an allocation
/// floor, which is what proves the payload is really being allocated. This
/// one carries the corners the fix sits between, because they fail in
/// OPPOSITE directions and a single shape cannot hold both still:
///
///   * `read-only-arm` — the arm only READS its binding, so the source keeps
///     the box and its interior. Neutralizing here is a use-after-free.
///   * `discarded` — the match result is thrown away, so nothing downstream
///     owns the value and the box must stay armed. Neutralizing here trades
///     the double free for a LEAK, which is exactly what
///     `branch_value_is_owned` exists to prevent; without that guard this row
///     is the one that goes red.
///   * `struct-control` — the axis. The same program one type over was
///     correct before the fix, through the neutralizer the enum spelling now
///     also reaches.
///
/// The rest are the other MOVE positions the same neutralizer serves — a
/// bare tail, a `Result` scrutinee, a by-value call argument from both
/// `match` and `if let`, and a rebind — so a future narrowing of the gate
/// fails on the position it narrowed rather than passing on the one shape
/// someone sampled.
///
/// Every `Drop` body READS its payload for the reason the sibling states: a
/// literal-printing body lets LLVM delete the allocation at `-O2`, and the
/// fixture then passes against a compiler that double-frees everywhere else.
#[test]
fn asan_consuming_arm_boxed_enum_payload_move_positions_and_boundaries() {
    const H: &str = "enum G { A(String), B }\n\
             impl Drop for G { fn drop(mut ref self) {\n\
             \x20   match self { G.A(s) => { println(f\"dG{s}\") } G.B => { println(\"dGB\") } } } }\n\
             fn sink(g: G) -> i64 { println(\"sink\"); return 1; }\n";
    // The BARE tail reaches the neutralizer by a different path from the
    // braced one (`block_tail_expr` collapses to the identifier itself).
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let o: Option[G] = Some(G.A(f\"z{{9}}\"));\n\
             \x20            let k = match o {{ Some(g) => g, None => G.B }};\n\
             \x20            println(\"kept\"); }}\n"
        ),
        &["dGz9", "kept"],
        "bare-tail",
    );
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let r: Result[G, i64] = Ok(G.A(f\"z{{9}}\"));\n\
             \x20            let k = match r {{ Ok(g) => {{ g }} Err(n) => {{ G.B }} }};\n\
             \x20            println(\"kept\"); }}\n"
        ),
        &["dGz9", "kept"],
        "result-twin",
    );
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let o: Option[G] = Some(G.A(f\"z{{9}}\"));\n\
             \x20            match o {{ Some(g) => {{ let n = sink(g); println(f\"n{{n}}\") }}\n\
             \x20                       None => {{ println(\"none\") }} }} }}\n"
        ),
        &["sink", "n1", "dGz9"],
        "into-call",
    );
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let o: Option[G] = Some(G.A(f\"z{{9}}\"));\n\
             \x20            if let Some(g) = o {{ let n = sink(g); println(f\"n{{n}}\") }} }}\n"
        ),
        &["sink", "n1", "dGz9"],
        "if-let-into-call",
    );
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let o: Option[G] = Some(G.A(f\"z{{9}}\"));\n\
             \x20            match o {{ Some(g) => {{ let q = g; println(\"bound\") }}\n\
             \x20                       None => {{ println(\"none\") }} }} }}\n"
        ),
        &["dGz9", "bound"],
        "rebind",
    );
    // BOUNDARY — the source keeps everything.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let o: Option[G] = Some(G.A(f\"z{{9}}\"));\n\
             \x20            match o {{ Some(g) => {{ println(\"saw\") }}\n\
             \x20                       None => {{ println(\"none\") }} }} }}\n"
        ),
        &["saw", "dGz9"],
        "read-only-arm",
    );
    // BOUNDARY — no destination, so the box stays armed. NO body runs here
    // on any backend; that agreed silence predates this row and is filed
    // separately. The claim asserted is the MEMORY.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let o: Option[G] = Some(G.A(f\"z{{9}}\"));\n\
             \x20            match o {{ Some(g) => {{ g }} None => {{ G.B }} }};\n\
             \x20            println(\"kept\"); }}\n"
        ),
        &["kept"],
        "discarded",
    );
    // THE AXIS — the struct payload, correct before the fix and after it.
    assert_clean_asan_run(
        "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.name}\") } }\n\
             fn main() { let o: Option[R] = Some(R { id: 1, name: f\"z{9}\" });\n\
             \x20            let k = match o { Some(r) => { r }\n\
             \x20                              None => { R { id: 0, name: f\"e{0}\" } } };\n\
             \x20            println(\"kept\"); }\n",
        &["dRz9", "kept"],
        "struct-control",
    );
}

#[test]
fn asan_consuming_arm_boxed_enum_payload_handed_out_is_owned_once() {
    let expected: Vec<&str> = vec!["dGtrue"; 8];
    assert_clean_asan_run_min_allocs(
        r#"
enum G { A(String), B }
impl Drop for G {
    fn drop(mut ref self) {
        match self {
            G.A(s) => { println(f"dG{s.contains("row")}") }
            G.B => { println("dGB") }
        }
    }
}

fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    while i < 8 {
        let o: Option[G] = Option.Some(G.A("row-" + (n + i).to_string()));
        let k = match o { Option.Some(g) => { g } Option.None => { G.B } };
        i = i + 1;
    }
}
"#,
        &expected,
        "consuming-arm-boxed-enum-payload-handed-out",
        8,
    );
}

/// B-2026-08-28-66 — a `match` arm written with BRACES that yields its
/// bound boxed payload double-frees that payload's interior.
///
/// Four arm-tail neutralizers in `compile_match` key on `arm.body` being an
/// `ExprKind::Identifier`. An arm written `Some(r) => { r }` is a Block, so
/// `suppress_boxed_payload_view_move` returned early and the box's interior
/// walk stayed armed beside the destination binding's own drop — one buffer,
/// two owners.
///
/// THE BRACES WERE THE WHOLE DIFFERENCE, which is what localized it: the
/// identical program written `Some(r) => r` was clean at 11 allocs / 11
/// frees, while `Some(r) => { r }` aborted `karac run` with a glibc double
/// free and reported `Invalid free()` under valgrind at 11 / 12. It is the
/// same hole the f-string neutralizer beside it already documents closing
/// for its own shape ("its `ExprKind::Identifier`-only guard returns early").
///
/// Reaches only a BOXED payload — `R { id: i64, name: String }` is 4 words
/// against `Option`'s 3-word area — which is why the same program with a
/// scalar-only payload is correct today and why this cannot be found by
/// widening the body-count fixtures.
///
/// The `branch_value_is_owned` guard is REQUIRED, not defensive:
/// neutralizing assumes a destination that registers its own drop, and a
/// DISCARDED result has none. Arm C pins that — without the guard it went
/// from 11/11 to 11/10 with 2 bytes lost, trading the double free for a
/// leak. The guard applies to the BARE tail too, which fixes a second
/// pre-existing shape rather than merely preserving it:
/// `match o { Some(r) => r, .. };` in statement position leaked 2 bytes
/// (10 allocs / 9 frees) and is now 10/10.
///
/// COVERAGE. Pre-fix this program ABORTS on both compiled backends
/// (`free(): double free detected in tcache 2`) while the interpreter
/// prints all 16 lines, so it is red by CRASH rather than by count — and
/// unlike most of this family that holds at the default `-O2` as well as
/// under `KARAC_OPT_LEVEL=0`. The reduced single-binding shape behind it
/// is `-O0`-only, which is why the row records both. Bodies read their
/// buffer (`contains`) and every payload is seeded from the opaque
/// `env.args().len()`, so no arm can be folded away.
#[test]
fn asan_braced_arm_yielding_boxed_payload_is_owned_once() {
    // A and B each keep a payload alive to a binding, one body apiece.
    // Arm C contributes NONE: a discarded match result inside a loop runs
    // no body on ANY backend — measured identical before and after this
    // fix and identical across interp / LLJIT / AOT, so it is a
    // pre-existing backend-AGREED gap, not this row. C earns its place by
    // pinning the `branch_value_is_owned` guard's MEMORY claim, which is
    // the half that regressed without it.
    let expected: Vec<&str> = vec!["dtrue"; 16];
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.name.contains("row")}") } }

fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    while i < 8 {
        // A — BRACED arm yielding the binding, result bound: the reported shape.
        let oa: Option[R] = Option.Some(R { id: n + i, name: "row-" + (n + i).to_string() });
        let ka = match oa { Option.Some(r) => { r } Option.None => { R { id: 0, name: "z".to_string() } } };
        // B — the BARE spelling of the same thing, which was already correct.
        let ob: Option[R] = Option.Some(R { id: n + i, name: "row-" + (n + i).to_string() });
        let kb = match ob { Option.Some(r) => r, Option.None => R { id: 0, name: "z".to_string() } };
        // C — braced arm, result DISCARDED: nothing downstream owns it, so the
        // neutralizer must NOT fire.
        let oc: Option[R] = Option.Some(R { id: n + i, name: "row-" + (n + i).to_string() });
        match oc { Option.Some(r) => { r } Option.None => { R { id: 0, name: "z".to_string() } } };
        i = i + 1;
    }
}
"#,
        &expected,
        "braced-arm-yielding-boxed-payload",
        8,
    );
}

#[test]
fn asan_ref_param_struct_field_struct_pattern_no_leak_no_double_free() {
    // B-2026-07-21-7 memory leg: an ESCAPING struct-pattern match over
    // `<refparam>.field` now deep-clones the scrutinee and rides a
    // StructDrop on the clone slot, with each arm's per-field suppression
    // firing against the CLONE. The double-free half: a consumed String
    // field must be freed exactly once by its binding (not again by the
    // caller's struct drop). The leak half: an UNBOUND String field
    // (`b: _`) must be freed exactly once by the clone's StructDrop
    // (a lost registration would leak one buffer per call — LSan).
    // Loop so any per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
struct Pair { a: String, b: String, x: i64 }
struct Holder { pair: Pair, n: i64 }
fn take_a(h: ref Holder) -> String {
    match h.pair {
        Pair { a, b: _, x: _ } => { return a; }
    }
    return "?".to_string();
}
fn tag(h: ref Holder) -> String {
    match h.pair {
        Pair { a, b, x } => { return a + "/" + b + ":" + x.to_string(); }
    }
    return "?".to_string();
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let h = Holder { pair: Pair { a: "alpha".to_string(), b: "bravo".to_string(), x: 3 }, n: 1 };
        acc = acc + take_a(h).len();
        acc = acc + take_a(h).len();
        acc = acc + tag(h).len();
        i = i + 1;
    }
    println(acc);
}
"#,
        &["920"],
        "ref_param_struct_field_struct_pattern",
    );
}

#[test]
fn asan_result_consumed_match_no_double_free() {
    // `match r { Ok(v) => ... }` binds the payload out; the source
    // `Result`'s scope-exit free must be suppressed (cap zeroed on the
    // taken arm) or this double-frees the same buffer on macOS.
    assert_clean_asan_run(
        r#"
fn mk() -> Result[String, i64] { Ok(f"consumed-ok-runtime-heap") }
fn main() {
    let r = mk();
    match r {
        Ok(v) => { println(v); }
        Err(_e) => { println("err"); }
    };
}
"#,
        &["consumed-ok-runtime-heap"],
        "result_consumed_match_no_double_free",
    );
}

#[test]
fn asan_vecdeque_payload_in_match_freed_once() {
    // B-2026-06-10-3: a VecDeque bound out of an Option via `match` is
    // reconstructed as a 3-word `{ptr,len,cap}` value (the gates now handle
    // `VecDeque`, not just `Vec`/`String`) and freed exactly once at the
    // arm's scope exit. A 1-word-default reconstruction freed a garbage
    // pointer (SIGTRAP); a mis-registered cleanup could double-free. Heap
    // buffer (cap > 0 via pushes) so the free actually fires.
    assert_clean_asan_run(
        r#"
fn mk() -> VecDeque[i64] {
    let mut q: VecDeque[i64] = VecDeque.new();
    q.push_back(5);
    q.push_back(6);
    q
}
fn main() {
    let o: Option[VecDeque[i64]] = Some(mk());
    match o {
        Some(v) => { println(v.len()); println(v[0]); }
        None => { println("n"); }
    }
}
"#,
        &["2", "5"],
        "vecdeque_payload_in_match_freed_once",
    );
}

// ── Slice A: auto-par return slots, move-only no-double-drop ──
//
// Phase-7 Slice A (Par codegen: return values, 2026-05-09) lifts
// the slice-2 `group_defines_binding_used_outside` gate by
// materializing a parent-allocated return struct and per-branch
// slot writes. Decision (iii) of the slice locks in move-only
// slot semantics — the branch's `scope_cleanup_actions` are
// discarded on `emit_par_branch_fn` exit so destructor-bearing
// values bit-copied through the slot don't double-drop, and the
// parent's `track_vec_var` is the unique cleanup owner. This
// test exercises that contract under ASAN with destructor-bearing
// `Vec[i64]` slot values: four branches each construct a fresh
// `Vec[i64]`, the parent reads each back from its slot via the
// synthesized `__karac_ParGroup_*_Returns` struct, sums their
// lengths into the printed result, and the parent's scope-exit
// cleanup releases the four heap buffers exactly once.

/// B-2026-08-25-12 — the boxed-payload gate must not trade a segfault for
/// a LEAK. Suppressing the bogus inline drop is only correct because the
/// `BoxedEnumDrop` registered for the same value owns the box and its
/// contents; if that were not so, every `String` in here would leak.
///
/// LSan (Linux CI) is the half that actually proves it — a local macOS asan
/// run catches the use-after-free side only. Payload is deliberately WIDER
/// than `Result`'s 5-word inline area (three `Vec` fields) so it is boxed,
/// and carries real heap in two of the three so a missed free is visible.
#[test]
fn test_boxed_wide_result_payload_match_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
struct Err1 { message: String }
struct Out { v1: Vec[String], v2: Vec[String], v3: Vec[String], tag: i64 }

fn go(n: i64) -> Result[Out, Err1] {
    if n < 0i64 {
        return Err(Err1 { message: "negative" });
    }
    let mut a: Vec[String] = Vec.new();
    a.push("alpha");
    a.push("gamma");
    let mut b: Vec[String] = Vec.new();
    b.push("beta");
    return Ok(Out { v1: a, v2: b, v3: [], tag: n });
}

fn main() {
    let mut i = 0i64;
    while i < 3i64 {
        match go(i) {
            Ok(a) => { println(a.v1.len()); }
            Err(e) => { println(e.message); }
        }
        i = i + 1;
    }
    // The Err half too — its payload is narrow and stays INLINE, so the gate
    // must remain per-half rather than disarming the whole action.
    match go(-1i64) {
        Ok(a) => { println(a.tag); }
        Err(e) => { println(e.message); }
    }
}
"#,
        &["2", "2", "2", "negative"],
        "boxed_wide_result_payload_match",
    );
}

#[test]
fn asan_enum_field_struct_transfer_destructure_no_double_free() {
    // #19 (phase-12 self-hosting): a by-value TRANSFER of an enum-field struct
    // (`let b = wrap(a)`, `wrap(s: Span) -> Span { s }`) followed by a
    // destructure that USES the bound payload double-freed on the old
    // caller-retains path — the transferred result aliased the source's enum
    // buffer, and both struct drops freed it. Entry-copy for enum-field structs
    // (`field_copy_supported` user-enum arm) gives the callee an independent
    // copy, so source and result own distinct buffers. Exercised in a loop with
    // a BORROW arm (`println(s)` keeps the binding's cleanup) and a CONSUME arm
    // (`sink(s)` moves it), one-level and nested two-level (`b.sp.tok`). The
    // `Int` arms are guarded behind an impossible `> 99999` so no `to_string`
    // temp materializes (Linux `detect_leaks=1` stays green vs the separate
    // baseline temp leak).
    assert_clean_asan_run(
        r#"
enum Tok { Id(String), Int(i64) }
struct Span { tok: Tok, off: i64 }
struct Wrap { sp: Span, hi: i64 }
fn wrap(s: Span) -> Span { s }
fn fwd(w: Wrap) -> Wrap { w }
fn sink(s: String) -> i64 { s.len() }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 4 {
        // One-level transfer + BORROW arm.
        let a = Span { tok: Tok.Id(f"a-{i}"), off: i };
        let b = wrap(a);
        match b.tok { Id(s) => println(s), Int(n) => { if n > 99999 { println("never"); } } }
        // One-level transfer + CONSUME arm.
        let c = Span { tok: Tok.Id(f"c-{i}"), off: i };
        let d = wrap(c);
        match d.tok { Id(s) => { acc = acc + sink(s); } Int(n) => { if n > 99999 { println("never"); } } }
        // Nested two-level transfer + BORROW arm.
        let e = Wrap { sp: Span { tok: Tok.Id(f"e-{i}"), off: i }, hi: i };
        let g = fwd(e);
        match g.sp.tok { Id(s) => println(s), Int(n) => { if n > 99999 { println("never"); } } }
        i = i + 1;
    }
    if acc > 99999 { println("never"); }
    println("done");
}
"#,
        &[
            "a-0", "e-0", "a-1", "e-1", "a-2", "e-2", "a-3", "e-3", "done",
        ],
        "enum_field_struct_transfer_destructure_no_double_free",
    );
}

#[test]
fn asan_struct_pattern_destructure_no_double_free() {
    // #16 (phase-12 self-hosting): a plain struct-pattern match destructure of
    // an OWNED local struct (`match v { S { a, b: _ } => … }`) moves each
    // CONSUMED field's heap payload into the new binding; without
    // `suppress_destructured_struct_pattern_cleanup` the source struct's
    // `__karac_drop_<S>` re-frees the same buffer at scope exit → double-free
    // (exit 134 under guardmalloc). Exercises: flat String fields fully bound,
    // a partial bind (`b: _` — the unconsumed field must STILL be freed by the
    // source drop, so a too-eager suppression would leak it — Linux LSan
    // guards that), a nested-struct field moved whole (transitive cap-zero via
    // `zero_struct_move_caps`), and an enum field moved whole (`zero_enum_
    // payload_caps`). Looped so a per-iteration leak accumulates for LSan.
    assert_clean_asan_run(
        r#"
struct Inner { s: String }
enum Tok { Id(String), Eof }
struct S { a: String, b: String, inner: Inner, tok: Tok, n: i64 }
fn mk(i: i64) -> S {
    let mut x: String = String.new();
    x.push_str("a");
    x.push_str(i.to_string());
    let mut y: String = String.new();
    y.push_str("b");
    y.push_str(i.to_string());
    S { a: x, b: y, inner: Inner { s: "deep".to_string() }, tok: Tok.Id("id".to_string()), n: i }
}
fn main() {
    let mut i: i64 = 0;
    while i < 4 {
        let v = mk(i);
        match v {
            S { a, b: _, inner, tok, n } => {
                let Inner { s } = inner;
                let mut line: String = String.new();
                line.push_str(a);
                line.push_str("|");
                line.push_str(s);
                line.push_str("|");
                match tok { Id(t) => line.push_str(t), Eof => line.push_str("eof") }
                line.push_str("|");
                line.push_str(n.to_string());
                println(line);
            }
        }
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "a0|deep|id|0",
            "a1|deep|id|1",
            "a2|deep|id|2",
            "a3|deep|id|3",
            "done",
        ],
        "struct_pattern_destructure_no_double_free",
    );
}

#[test]
fn asan_for_loop_owned_agg_elem_direct_match_no_double_free() {
    // Direct `for it in items { match it { V(payload) => … } }` over a
    // heap-bearing user-enum element (registered `for_loop_owned_agg_vars`,
    // NOT `for_loop_borrow_vars`). The struct payload bound out of the
    // borrowed element aliases the container slot's heap (a String buffer +
    // a Vec buffer here); before the fix its scope-exit drop double-freed
    // against the container's per-element drop when `v` unwound. Guards
    // no-double-free (ASAN) / no-leak (LSan): every buffer reclaimed exactly
    // once. ≥36-byte String so the fault is loud.
    assert_clean_asan_run(
        r#"
struct Named { name: String, nums: Vec[i64] }
enum Item { A(Named), B }
fn total(items: ref Vec[Item]) -> i64 {
    let mut n = 0;
    for it in items {
        match it {
            A(x) => { n = n + x.name.len(); for t in x.nums { n = n + t; } }
            B => {}
        }
    }
    n
}
fn main() {
    let mut v: Vec[Item] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut tg: Vec[i64] = Vec.new();
        tg.push(1); tg.push(2);
        v.push(Item.A(Named { name: "owned-agg-elem-direct-match-payload-xx".to_string(), nums: tg }));
        v.push(Item.B);
        i = i + 1;
    }
    println(total(v));
}
"#,
        &["246"],
        "for_loop_owned_agg_elem_direct_match",
    );
}

#[test]
fn asan_let_wildcard_block_tail_temp_freed() {
    // `let _ = { make_map() };` — wildcard-let discard of a block-tail
    // Map handle. Routes through the early Wildcard arm; the chokepoint
    // recognizes the `Map[K,V]` TypeExpr (hint table) and queues a
    // `karac_map_free` against the peeled tail. Looped to turn any
    // double-free of the map handle into a macOS fault.
    assert_clean_asan_run(
        r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 2_i64);
    return m;
}

fn main() {
    let mut i = 0;
    while i < 8 {
        let _ = { make_map() };
        i = i + 1;
    }
    println(i);
}
"#,
        &["8"],
        "let_wildcard_block_tail_temp_freed",
    );
}

#[test]
fn asan_iflet_freshtemp_enum_bound_field_no_double_free() {
    // `if let Full(v, n) = make()` — the Vec is moved into `v`. The
    // materialized temp's EnumDrop must SKIP that field (cap zeroed by
    // suppression); `v`'s own cleanup frees it once. Without suppression
    // this double-frees under macOS ASAN.
    let src = format!(
            "{B_ASAN_PRELUDE}\nfn main() {{\n    let mut i = 0;\n    while i < 8 {{\n        if let Holder.Full(v, n) = make() {{ println(v.len() + n); }}\n        i = i + 1;\n    }}\n    println(i);\n}}\n"
        );
    assert_clean_asan_run(
        &src,
        &["44", "44", "44", "44", "44", "44", "44", "44", "8"],
        "iflet_freshtemp_enum_bound_field_no_double_free",
    );
}

#[test]
fn asan_iflet_freshtemp_enum_unbound_field_clean() {
    // `if let Full(_, n) = make()` — the Vec is UNBOUND. The enum drop walk
    // frees it (Linux leak oracle). macOS verifies the added drop doesn't
    // fault (e.g. freeing a garbage/aliased pointer).
    let src = format!(
            "{B_ASAN_PRELUDE}\nfn main() {{\n    let mut i = 0;\n    while i < 8 {{\n        if let Holder.Full(_, n) = make() {{ println(n); }}\n        i = i + 1;\n    }}\n    println(i);\n}}\n"
        );
    assert_clean_asan_run(
        &src,
        &["42", "42", "42", "42", "42", "42", "42", "42", "8"],
        "iflet_freshtemp_enum_unbound_field_clean",
    );
}

#[test]
fn asan_match_freshtemp_enum_unbound_field_clean() {
    // `match make() { Full(_, n) => …, Empty => … }` — match surface of the
    // unbound-field drop. The matched `Full(_, _)` arm's unbound Vec is
    // freed by the materialized temp's EnumDrop.
    let src = format!(
            "{B_ASAN_PRELUDE}\nfn main() {{\n    let mut i = 0;\n    while i < 8 {{\n        match make() {{ Holder.Full(_, n) => println(n), Holder.Empty => println(0) }}\n        i = i + 1;\n    }}\n    println(i);\n}}\n"
        );
    assert_clean_asan_run(
        &src,
        &["42", "42", "42", "42", "42", "42", "42", "42", "8"],
        "match_freshtemp_enum_unbound_field_clean",
    );
}

#[test]
fn asan_iflet_freshtemp_enum_miss_wholesale_clean() {
    // Miss edge: `make()` returns `Full(Vec, _)` but the pattern is
    // `Empty`, so the arm misses and the whole heap-bearing temp must drop
    // wholesale before/at the else. No suppression runs on the miss edge,
    // so the enum drop walk frees the entire payload. Looped leak/UAF gate.
    let src = format!(
            "{B_ASAN_PRELUDE}\nfn main() {{\n    let mut i = 0;\n    while i < 8 {{\n        if let Holder.Empty = make() {{ println(1); }} else {{ println(2); }}\n        i = i + 1;\n    }}\n    println(i);\n}}\n"
        );
    assert_clean_asan_run(
        &src,
        &["2", "2", "2", "2", "2", "2", "2", "2", "8"],
        "iflet_freshtemp_enum_miss_wholesale_clean",
    );
}

#[test]
fn asan_whilelet_freshtemp_enum_unbound_field_clean() {
    // `while let Full(_, n) = next(i)` — the Vec is unbound each iteration;
    // the per-iteration EnumDrop frees it before the next scrutinee eval.
    let src = format!(
            "{B_WHILELET_PRELUDE}\nfn main() {{\n    let mut i = 0;\n    while let Holder.Full(_, n) = next(i) {{\n        println(n);\n        i = i + 1;\n    }}\n    println(99);\n}}\n"
        );
    assert_clean_asan_run(
        &src,
        &["0", "1", "2", "3", "4", "5", "99"],
        "whilelet_freshtemp_enum_unbound_field_clean",
    );
}

#[test]
fn asan_whilelet_freshtemp_enum_bound_field_no_double_free() {
    // `while let Full(v, n) = next(i)` — the Vec is moved into `v` each
    // iteration. Suppression zeroes the moved field's cap in the
    // (reused) alloca so the per-iteration EnumDrop skips it; `v`'s
    // per-iteration binding cleanup frees it once. A double-free would
    // fault on macOS.
    let src = format!(
            "{B_WHILELET_PRELUDE}\nfn main() {{\n    let mut i = 0;\n    while let Holder.Full(v, n) = next(i) {{\n        println(v.len() + n);\n        i = i + 1;\n    }}\n    println(99);\n}}\n"
        );
    assert_clean_asan_run(
        &src,
        &["2", "3", "4", "5", "6", "7", "99"],
        "whilelet_freshtemp_enum_bound_field_no_double_free",
    );
}

#[test]
fn asan_whilelet_miss_variant_no_double_free() {
    // B follow-up #2: the loop terminates on a *heap-bearing* non-matching
    // variant (`Stop(Vec)` vs the matched `Go`). The final scrutinee is
    // freed wholesale on the new `whilelet.miss` edge. This guards the fix
    // against a double-free (macOS ASAN has no LeakSanitizer, so the leak
    // closure itself is pinned by the IR test; here we verify the
    // wholesale miss-drop doesn't double-free against the per-iteration
    // bound-field cleanup of the matched iterations). Several matches then
    // one miss.
    assert_clean_asan_run(
        r#"
enum Item { Go(Vec[i64]), Stop(Vec[i64]) }
fn mk(x: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(x);
    return v;
}
fn step(c: i64) -> Item {
    if c < 3 {
        return Item.Go(mk(c));
    }
    return Item.Stop(mk(99));
}
fn main() {
    let mut c: i64 = 0;
    while let Go(xs) = step(c) {
        println(xs.len() + c);
        c = c + 1;
    }
    println(c);
}
"#,
        &["1", "2", "3", "3"],
        "whilelet_miss_variant_no_double_free",
    );
}

#[test]
fn asan_nested_struct_pattern_no_double_free() {
    // Nested struct pattern (`let Outer { inner: Inner { data }, n } = mk()`)
    // — the dispatch fix made `data.len()` compile; this confirms the
    // nested field's heap is freed exactly once. The enclosing `inner`
    // field is discard-freed as a unit (running Inner's drop → frees the
    // Vec), and `data` carries no separate cleanup, so looping faults under
    // ASAN if the Vec is freed twice (or aliased + freed).
    assert_clean_asan_run(
        r#"
struct Inner { data: Vec[i64] }
struct Outer { inner: Inner, n: i64 }
fn mk(x: i64) -> Outer {
    let mut v: Vec[i64] = Vec.new();
    v.push(x);
    v.push(x);
    return Outer { inner: Inner { data: v }, n: x };
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let Outer { inner: Inner { data }, n } = mk(i);
        println(data.len() + n);
        i = i + 1;
    }
    println(99);
}
"#,
        &["2", "3", "4", "5", "6", "99"],
        "nested_struct_pattern_no_double_free",
    );
}

/// Oversized-enum-payload §1/§2 (fresh-temp scrutinee box-free, move-OUT):
/// `match make(i) { Some(h) => … }` over a fresh-temp boxed `Option[H]`
/// where `H` owns a `Vec` (5 words → boxed). The bound `h` now owns the
/// inner Vec and frees it via its own scope cleanup; the fresh-temp
/// `BoxedEnumDrop` must free ONLY the box (no inner struct drop) or the
/// Vec buffer is freed twice. The loop turns any imbalance into a
/// deterministic ASAN double-free. Complements
/// `asan_boxed_option_inner_heap_no_double_free` (which isolates the
/// move-IN suppression with no `match`).
#[test]
fn asan_freshtemp_boxed_option_match_move_out_no_double_free() {
    assert_clean_asan_run(
        r#"
struct H { v: Vec[i64], a: i64, b: i64 }
fn make(i: i64) -> Option[H] {
    let mut vv: Vec[i64] = Vec.new();
    vv.push(i);
    return Some(H { v: vv, a: 1, b: 2 });
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        match make(i) {
            Some(h) => println(h.v[0] + h.a),
            None => println(-1),
        }
        i = i + 1;
    }
    println(99);
}
"#,
        &["1", "2", "3", "4", "5", "99"],
        "freshtemp_boxed_option_match_move_out_no_double_free",
    );
}

// ── while let / let else drop paths (phase-6 line 489) ───────

#[test]
fn asan_while_let_per_iteration_heap_local_freed() {
    // `compile_while_let` pushes a per-iteration scope-cleanup frame.
    // A heap String created inside the loop body must be freed at each
    // iteration's exit — not leaked across iterations, not double-freed
    // when the next iteration reuses the binding's slot.
    assert_clean_asan_run(
        r#"
fn pop(v: mut ref Vec[i64]) -> Option[i64] {
    if v.len() == 0 {
        return Option.None;
    }
    let last = v.len() - 1;
    let x = v[last];
    v.remove(last);
    return Option.Some(x);
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    v.push(3_i64);
    while let Some(x) = pop(mut v) {
        let prefix = "n=";
        let line = prefix + "x";
        println(f"{line} {x}");
    }
    println("done");
}
"#,
        &["n=x 3", "n=x 2", "n=x 1", "done"],
        "while_let_per_iteration_heap_local_freed",
    );
}

/// B-2026-08-27-43 — the BALANCE half of the arm-local branch-leaf fix.
///
/// Like its sibling above, the reported fault is invisible to this harness
/// on the DEFAULT leg: it is a read through a box the callee's param dec
/// already freed, and the link-step `-fsanitize=address` buys only the
/// allocator interposition, so
/// `option_shared_from_an_arm_local_leaf_survives_repeated_consumption` in
/// `tests/codegen.rs` is what gates the use-after-free. The
/// `KARAC_SANITIZE_ADDRESS=1` leg sees the read itself (B-2026-09-07-40). What LSan gates is
/// the pair of wrong fixes on either side of it.
///
/// The fix teaches case (g) to register the consuming binding when the
/// arm-local leaf actually took a retain, which arms a scope-exit dec and a
/// per-use retain. Register too eagerly — on a leaf that took no retain —
/// and the scope-exit dec frees a box nobody handed over, which is a
/// double-free ASAN reports. Retain too eagerly — at the phi, or on an arm
/// whose producer already carries its own `+1` — and the count never
/// reaches zero, which is a leak LSan reports. Every leg alternates its arms
/// across five iterations so an over-count on either side accumulates rather
/// than cancelling.
///
/// The third leg is the one that separates the two directions: its arm-local
/// ALIASES an enclosing binding (`let z = d; z`), so the escaping `+1` must
/// be the arm-local's own and `d`'s must survive to its own scope exit.
///
/// Measured RED against the unfixed compiler — but by the OVERFLOW PANIC the
/// garbage read out of the freed box produces when it is folded into `s`,
/// not by an LSan report, so record it as a balance check rather than as
/// evidence the sanitizer sees this bug. It does not: the fault is a read,
/// and the process dies on the arithmetic before any leak check runs.
#[test]
fn asan_option_shared_arm_local_branch_leaf_is_balanced() {
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64 }

fn make(n: i64) -> Option[Node] { return Some(Node { val: n }); }

fn show(t: Option[Node]) -> i64 {
    match t { None => { return 0; } Some(n) => { return n.val; } }
}

fn main() {
    let mut s = 0;
    let mut i = 0;
    while i < 5 {
        // Both arms hand out an arm-LOCAL, alternating which is taken.
        let t = if i % 2 == 0 { let u = make(i); u } else { let v = make(i + 100); v };
        s = s + show(t) + show(t) + show(t);
        // MIXED: an arm-local against an enclosing binding.
        let c = make(i + 1000);
        let u2 = if i % 2 == 0 { let w = make(i + 7); w } else { c };
        s = s + show(u2) + show(u2) + show(u2);
        // The arm-local ALIASES an enclosing binding, which must outlive it.
        let d = make(i + 10000);
        let w2 = if i % 2 == 0 { let z = d; z } else { make(3) };
        s = s + show(w2) + show(w2) + show(w2);
        // `if let`, arm-local in the THEN arm.
        let o = if i % 2 == 0 { Some(1) } else { None };
        let e = make(i + 20000);
        let x = if let Some(q) = o { let y = make(q + i); y } else { e };
        s = s + show(x) + show(x) + show(x);
        i = i + 1;
    }
    println(s);
}
"#,
        &["216798"],
        "option_shared_arm_local_branch_leaf",
    );
}

// ── kata-#24: pattern-binding alias acquire ───────────────────

#[test]
fn asan_if_let_shared_binding_field_displacement() {
    // The kata-#24 minimal UAF: `if let Some(second) = first.next`
    // bound a NON-retained alias; `first.next = second.next`
    // released the field's only ref to that node, freeing it under
    // the live binding — the `second.val` read below is a
    // heap-use-after-free pre-fix. `bind_pattern_values`' alias
    // acquire (+1 at bind, scope-exit RcDec) keeps it alive.
    assert_clean_asan_run(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn from3() -> Option[ListNode] {
    let head = ListNode { val: 1, next: None };
    let n2 = ListNode { val: 2, next: None };
    let n3 = ListNode { val: 3, next: None };
    n2.next = Some(n3);
    head.next = Some(n2);
    Some(head)
}
fn poke(head: Option[ListNode]) {
    if let Some(first) = head {
        if let Some(second) = first.next {
            first.next = second.next;
            println(second.val);
        }
    }
}
fn main() {
    poke(from3());
}
"#,
        &["2"],
        "if_let_shared_binding_field_displacement",
    );
}

#[test]
fn asan_match_byvalue_shared_enum_bind_without_consume_no_leak() {
    // B-2026-06-14-29 — a `match` on a BY-VALUE shared enum whose taken arm
    // BINDS the struct payload (`Add(b) =>`) but does NOT consume its shared
    // children, returning a FRESH tree instead. The original scrutinee box +
    // its children must be freed exactly once at the function's RC cleanup.
    //
    // Root cause / closure: this was a DUPLICATE of B-2026-06-14-28 bug #3
    // (the shared-enum box-drop walker `emit_shared_enum_rc_drop_fn` /
    // `emit_nested_struct_shared_rc_decs` not recursing into a STRUCT
    // payload's shared fields), NOT a distinct `compile_match` suppression
    // bug. A malloc/free-balance bisect over `0890627c` (the B-28 fix)
    // showed the struct-wrapped `Add(BinOp)` shape leaked unconditionally
    // pre-fix (independent of whether the arm consumed `b` — the "leaky"
    // bind-ignore variant and the fully-consuming variant leaked IDENTICALLY,
    // disproving the consumption-gated hypothesis and the
    // `control_flow_match.rs` locus) and is balanced post-fix; the
    // direct-payload `Add(Expr,Expr)` shape never leaked. So the match path
    // needed no change — this test pins the no-leak so any reintroduction in
    // the box-drop walker is caught by the Linux-CI LSan gate. Looped to
    // surface a per-iteration leak. (struct-wrapped shape.)
    assert_clean_asan_run(
        r#"
shared enum Expr { Num(i64), Add(BinOp) }
struct BinOp { left: Expr, right: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
    }
}
fn fold(e: Expr) -> Expr {
    match e {
        Num(n) => Num(n),
        Add(b) => Add(BinOp { left: Num(99), right: Num(99) }),
    }
}
fn build(n: i64) -> Expr {
    if n <= 0 { Num(n) }
    else { Add(BinOp { left: Num(n), right: build(n - 1) }) }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 60 {
        let t: Expr = build(3);
        let t2: Expr = fold(t);
        total = total + eval(t2);
        i = i + 1;
    }
    println(total);
}
"#,
        &["11880"],
        "match_byvalue_bind_without_consume_struct",
    );
}

#[test]
fn asan_match_byvalue_shared_enum_bind_without_consume_direct_payload_no_leak() {
    // B-2026-06-14-29 (direct-payload axis) — the same bind-without-consume
    // shape on a DIRECT-payload shared enum `Add(Expr, Expr)` (no struct
    // wrapper). The ledger flagged this shape as also reproducing; the
    // bisect showed it was in fact already leak-free at the B-28 parent
    // commit (the box-drop walker recursed correctly for direct payloads).
    // Pinned here so the two axes stay covered together.
    assert_clean_asan_run(
        r#"
shared enum Expr { Num(i64), Add(Expr, Expr) }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(l, r) => eval(l) + eval(r),
    }
}
fn fold(e: Expr) -> Expr {
    match e {
        Num(n) => Num(n),
        Add(l, r) => Add(Num(99), Num(99)),
    }
}
fn build(n: i64) -> Expr {
    if n <= 0 { Num(n) }
    else { Add(Num(n), build(n - 1)) }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 60 {
        let t: Expr = build(3);
        let t2: Expr = fold(t);
        total = total + eval(t2);
        i = i + 1;
    }
    println(total);
}
"#,
        &["11880"],
        "match_byvalue_bind_without_consume_direct",
    );
}

#[test]
fn asan_match_byvalue_shared_enum_fully_consumed_arm_no_double_free() {
    // B-2026-06-14-29 no-regression direction: the already-OK FULLY-CONSUMING
    // arm (`Add(b) => eval(b.left) + eval(b.right)` consumes both shared
    // children). The scrutinee box + its children must be freed exactly once
    // — no double-free of a child that the arm consumed, no leak. Bisect
    // confirmed this leaked equally with the bind-ignore variant pre-B-28 and
    // is balanced post-fix, so it locks in that the box-drop fix did not
    // introduce a double-free in the consuming path. (struct-wrapped shape.)
    assert_clean_asan_run(
        r#"
shared enum Expr { Num(i64), Add(BinOp) }
struct BinOp { left: Expr, right: Expr }
fn fold(e: Expr) -> Expr {
    match e {
        Num(n) => Num(n),
        Add(b) => Add(BinOp { left: fold(b.left), right: fold(b.right) }),
    }
}
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
    }
}
fn build(n: i64) -> Expr {
    if n <= 0 { Num(n) }
    else { Add(BinOp { left: Num(n), right: build(n - 1) }) }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 60 {
        let t: Expr = build(3);
        let t2: Expr = fold(t);
        total = total + eval(t2);
        i = i + 1;
    }
    println(total);
}
"#,
        &["360"],
        "match_byvalue_fully_consumed_arm",
    );
}

#[test]
fn asan_match_byvalue_shared_enum_reconstruct_from_fresh_locals_no_leak() {
    // B-2026-06-14-29 no-regression direction: the already-OK
    // RECONSTRUCT-FROM-FRESH-LOCALS arm — `Add(b)` binds `b`, ignores it, and
    // rebuilds from fresh `let`-bound locals (`let l = Num(1); let r = Num(2);
    // Add(BinOp { left: l, right: r })`). The fresh locals are moved into the
    // new tree (no double-free) and the original box + children freed once
    // (no leak). The `Num(n)` arm (no shared child) is exercised by `build`.
    assert_clean_asan_run(
        r#"
shared enum Expr { Num(i64), Add(BinOp) }
struct BinOp { left: Expr, right: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
    }
}
fn fold(e: Expr) -> Expr {
    match e {
        Num(n) => Num(n),
        Add(b) => {
            let l: Expr = Num(1);
            let r: Expr = Num(2);
            Add(BinOp { left: l, right: r })
        }
    }
}
fn build(n: i64) -> Expr {
    if n <= 0 { Num(n) }
    else { Add(BinOp { left: Num(n), right: build(n - 1) }) }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 60 {
        let t: Expr = build(3);
        let t2: Expr = fold(t);
        total = total + eval(t2);
        i = i + 1;
    }
    println(total);
}
"#,
        &["180"],
        "match_byvalue_reconstruct_from_fresh_locals",
    );
}

#[test]
fn asan_shared_enum_view_destructure_bare_shared_child_no_double_free() {
    // B-2026-07-09-12 (clone-on-extract half): a shared-enum whose struct
    // payload carries BARE-SHARED children (`BinNode { left: Expr, right: Expr
    // }`, the AST binary-node shape). The arm binds the payload as a VIEW
    // (`Bin(b)`, not deep-cloned because it is shared-bearing) then
    // DESTRUCTURES it (`let BinNode { left, right, op } = b`) and CONSUMES the
    // moved-out shared children (`eval(left)`, `eval(right)`). Pre-fix the
    // extracted `left`/`right` aliased the RC box's inline handles, so the
    // recursive consume rc-dec AND the box's rc-drop both freed each child box
    // — a double-free. The clone-on-extract fix rc-INCs each bare-shared child
    // at the destructure (`clone_on_extract_view_field`) so the leaf co-owns
    // the box; the leaf's consume balances the inc, the box's rc-drop balances
    // its original ref. Also covers the String LEAF (`n.name`) via the Lit arm.
    // Looped x20 so a per-iteration double-free / leak hits the LSan gate.
    assert_clean_asan_run(
        r#"
struct LitNode { name: String, val: i64 }
shared enum Expr { Lit(LitNode), Bin(BinNode) }
struct BinNode { left: Expr, right: Expr, op: i64 }
fn eval(e: Expr) -> i64 {
    match e {
        Lit(n) => n.val,
        Bin(b) => {
            let BinNode { left, right, op } = b;
            eval(left) + eval(right) + op
        }
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        let a = Expr.Lit(LitNode { name: "a_long_name_for_heap_visibility_xxxxxxxx".to_string(), val: 3 });
        let bn = Expr.Lit(LitNode { name: "b_long_name_for_heap_visibility_xxxxxxxx".to_string(), val: 4 });
        let inner = Expr.Bin(BinNode { left: a, right: bn, op: 10 });
        let c = Expr.Lit(LitNode { name: "c_long_name_for_heap_visibility_xxxxxxxx".to_string(), val: 5 });
        let tree = Expr.Bin(BinNode { left: inner, right: c, op: 100 });
        total = total + eval(tree);
        i = i + 1;
    }
    println(total);
}
"#,
        &["2440"],
        "shared_enum_view_destructure_bare_shared_child",
    );
}

/// B-2026-08-05-7 (boxed-payload destructure leg): destructuring a boxed
/// `Option` payload inside a match arm leaked the payload's buffer.
///
/// `suppress_boxed_payload_view_move` clears the box's `inner_drop_fn` for
/// any source in `boxed_optres_payload_view_vars`, on the stated assumption
/// that the move destination "registers its own drop". True for a
/// whole-value move (`let x = w;`) and false for a DESTRUCTURE: the leaves
/// register nothing, because `callee_owned_src` requires a `StructDrop` on
/// the source slot and a payload view binding has none — its drop lives on
/// the OPTION's slot. So the box stopped freeing the interior and nobody
/// started.
///
/// Both spellings are covered because both leaked: a BOUND field
/// (`let Wide { tag, payload } = w`) and an UNBOUND one
/// (`payload: _`, which has no leaf to own it at all). The
/// destructure-in-the-arm-pattern form (`Some(Wide { .. })`) was always
/// clean and is kept here as the control that localizes the bug to the
/// nested `let`.
///
/// The payload must be WIDER than the Option's 3-word area or it is never
/// boxed and none of this applies — hence the `tag: i64` beside the String.
///
/// NOTE: pins at -O0 only; -O2 deletes the allocation outright.
#[test]
fn asan_destructured_boxed_option_payload_owned_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Wide { tag: i64, payload: String }
fn main() {
    let base: i64 = env.args().len();
    let mut acc = 0i64;
    let mut i = 0;
    while i < base + 49 {
        let a: Option[Wide] = Some(Wide { tag: 7, payload: f"pay-{i}-long-enough-aaaa" });
        acc = acc + match a {
            Some(w) => { let Wide { tag, payload } = w; if payload.starts_with("pay") { tag } else { 0i64 } }
            None => 0i64,
        };
        let b: Option[Wide] = Some(Wide { tag: 3, payload: f"unb-{i}-long-enough-aaaa" });
        acc = acc + match b {
            Some(w) => { let Wide { tag, payload: _ } = w; tag }
            None => 0i64,
        };
        let c: Option[Wide] = Some(Wide { tag: 2, payload: f"arm-{i}-long-enough-aaaa" });
        acc = acc + match c {
            Some(Wide { tag, payload }) => { if payload.starts_with("arm") { tag } else { 0i64 } }
            None => 0i64,
        };
        i = i + 1;
    }
    println(acc);
}
"#,
        &["600"],
        "destructured_boxed_option_payload_owned_once",
        // Three boxed payloads per iteration over 50 iterations.
        100,
    );
}

/// B-2026-08-05-7 (leak B): an `Option` field whose payload is HEAP-BOXED,
/// destructured out of its owning struct, leaked both the box envelope and
/// the payload's own buffer.
///
/// The leaf registration used `track_inline_option_agg_payload_var`, which
/// reads the payload WORDS as the value — but for a boxed payload word 0 is
/// a box POINTER, so it freed nothing. The source struct's drop, which does
/// handle the boxed case, had just been disarmed by the move-out tag-zero,
/// so nothing was left owning either allocation.
///
/// NEVER destructuring the holder was always clean, which is what localized
/// this to the destructure rather than to the struct-literal move that the
/// sibling fixture's name suggests.
///
/// The swap to a box drop is restricted to a payload whose own drop can be
/// resolved (a known user struct). Unfiltered it replaces a working inline
/// registration with a box-ONLY free for payloads that cannot be named — an
/// enum payload, a tuple — and the interior leaks instead of the envelope;
/// that cost two other fixtures before the filter went in.
///
/// NOTE: pins at -O0 only; -O2 deletes the allocations outright.
#[test]
fn asan_destructured_struct_field_boxed_option_owned_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Wide { tag: i64, payload: String }
struct Holder { name: String, body: Option[Wide] }
fn build(i: i64) -> Holder {
    let body: Option[Wide] = Some(Wide { tag: 7, payload: f"pay-{i}-long-enough-aaaa" });
    Holder { name: f"holder-{i}-long-enough", body: body }
}
fn read(h: Holder) -> i64 {
    let Holder { name, body } = h;
    let mut n = name.len() as i64;
    if name.starts_with("holder") { n = n + 1i64; }
    match body {
        Some(w) => { let Wide { tag, payload } = w; tag + (payload.len() as i64) + n }
        None => n,
    }
}
fn main() {
    let base: i64 = env.args().len();
    let mut acc = 0i64;
    let mut i = 0;
    while i < base + 49 { acc = acc + read(build(i)); i = i + 1; }
    println(acc);
}
"#,
        &["2580"],
        "destructured_struct_field_boxed_option_owned_once",
        100,
    );
}

#[test]
fn asan_getmove_match_struct_payload_moveout_no_double_free() {
    // Slice 3s: the struct-payload sibling — `Holder` rides the boxed
    // wide-payload path through the scrutinee, and the bound copy's
    // `name` aliases the bucket's until the arm-move clone.
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, Holder] = Map.new();
        let h = Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i };
        m.insert(i, h);
        let out = match m.get(i) {
            Some(x) => x,
            None => Holder { name: f"none-{i}", id: 0 },
        };
        println(out.name.len() + out.id);
        i = i + 1;
    };
}
"#,
        &["47", "48", "49"],
        "getmove_match_struct_payload_moveout_no_double_free",
    );
}

#[test]
fn asan_getmove_iflet_readonly_no_double_free() {
    // Slice 3s: READ-ONLY `if let Some(x) = m.get(k)` crashed pre-fix —
    // if-let (unlike match) never consulted `scrutinee_is_borrow_call`,
    // so the aliased payload got an owned track and the arm-end drain
    // freed the bucket's buffer (exit 133 on plain Map[i64, String]).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(i, f"map string payload padded beyond thirty-six bytes {i}");
        if let Some(x) = m.get(i) {
            println(x.len());
        }
        i = i + 1;
    };
}
"#,
        &["51", "51", "51"],
        "getmove_iflet_readonly_no_double_free",
    );
}

#[test]
fn asan_getmove_iflet_owned_tail_move_no_double_free() {
    // Slice 3s: `if let Some(x) = v.pop() { x }` with an OWNED scrutinee
    // crashed pre-fix — if-let had NO then-tail move suppression (match
    // has had it for arms all along), so the drain freed the escaping
    // buffer and the caller's binding double-freed. Distinct from the
    // borrow-clone leg: this is the owned-binding tail-move hole.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut v: Vec[String] = Vec.new();
        v.push(f"vec string payload padded beyond thirty-six bytes {i}");
        let s = if let Some(x) = v.pop() { x } else { f"none-{i}" };
        println(s.len());
        i = i + 1;
    };
}
"#,
        &["51", "51", "51"],
        "getmove_iflet_owned_tail_move_no_double_free",
    );
}

#[test]
fn asan_getmove_whilelet_get_readonly_no_double_free() {
    // Slice 3s: `while let Some(x) = m.get(i)` — the while-let bind site
    // gets the same borrow-call classification as match/if-let.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[i64, String] = Map.new();
    m.insert(0, f"map string payload padded beyond thirty-six bytes {0}");
    let mut i = 0;
    while let Some(x) = m.get(i) {
        println(x.len());
        i = i + 1;
    }
    println(i);
}
"#,
        &["51", "1"],
        "getmove_whilelet_get_readonly_no_double_free",
    );
}

#[test]
fn asan_structpat_option_destructure_no_double_free() {
    // Slice 3t: `Some(Holder { name, id })` was UNIMPLEMENTED in codegen
    // (the payload-width helpers defaulted a Struct pattern to one word,
    // the reconstruction bound the raw word, and every field stayed
    // unbound — "Undefined variable"). Once fields bind, the named
    // binding's BoxedEnumDrop inner walk also freed the consumed fields
    // (double-free, DCE-masked unless the payload is observed) —
    // `suppress_boxed_payload_struct_destructure` zeroes consumed field
    // caps inside the box.
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let o: Option[Holder] = Some(Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        match o {
            Some(Holder { name, id }) => { println(name.len() + id); },
            None => { println("missing"); },
        }
        i = i + 1;
    };
}
"#,
        &["47", "48", "49"],
        "structpat_option_destructure_no_double_free",
    );
}

#[test]
fn asan_structpat_partial_destructure_unbound_field_no_leak() {
    // Slice 3t: `Some(Pair2 { first, .. })` — the UNBOUND `second` stays
    // owned by the box; the per-field cap-zero must not disarm it (the
    // box's inner walk is its only free).
    assert_clean_asan_run(
        r#"
struct Pair2 { first: String, second: String }
fn main() {
    let mut i = 0;
    while i < 3 {
        let o: Option[Pair2] = Some(Pair2 { first: f"first payload padded beyond thirty-six bytes {i}", second: f"second payload padded beyond thirty-six bytes {i}" });
        match o {
            Some(Pair2 { first, .. }) => { println(first.len()); },
            None => { println("missing"); },
        }
        i = i + 1;
    };
}
"#,
        &["46", "46", "46"],
        "structpat_partial_destructure_unbound_field_no_leak",
    );
}

#[test]
fn asan_structpat_result_ok_destructure_no_double_free() {
    // Slice 3t: the Result sibling — `Ok(Holder { name, id })` over a
    // named `Result[Holder, i64]` binding (boxed Ok payload).
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let r: Result[Holder, i64] = Ok(Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        match r {
            Ok(Holder { name, id }) => { println(name.len() + id); },
            Err(e) => { println(e); },
        }
        i = i + 1;
    };
}
"#,
        &["47", "48", "49"],
        "structpat_result_ok_destructure_no_double_free",
    );
}

#[test]
fn asan_structpat_mapget_destructure_readonly_no_double_free() {
    // Slice 3t: destructuring a `Map.get` payload READ-ONLY — the field
    // bindings alias the bucket (borrow mode), the scrutinee's box is
    // box-only-freed, the map keeps sole ownership.
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, Holder] = Map.new();
        m.insert(i, Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        match m.get(i) {
            Some(Holder { name, id }) => { println(name.len() + id); },
            None => { println("missing"); },
        }
        i = i + 1;
    };
}
"#,
        &["47", "48", "49"],
        "structpat_mapget_destructure_readonly_no_double_free",
    );
}

#[test]
fn asan_structpat_iflet_destructure_no_double_free() {
    // Slice 3t: the if-let form of the boxed-payload struct destructure.
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let o: Option[Holder] = Some(Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        if let Some(Holder { name, id }) = o {
            println(name.len() + id);
        }
        i = i + 1;
    };
}
"#,
        &["47", "48", "49"],
        "structpat_iflet_destructure_no_double_free",
    );
}

#[test]
fn asan_structpat_whilelet_pop_destructure_no_double_free() {
    // Slice 3t: `while let Some(Holder { name, id }) = v.pop()` — the
    // fresh-temp boxed scrutinee path (box-only free; fields owned by
    // their bindings), looped.
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut v: Vec[Holder] = Vec.new();
    let mut i = 0;
    while i < 3 {
        v.push(Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        i = i + 1;
    };
    let mut total = 0;
    while let Some(Holder { name, id }) = v.pop() {
        total = total + (name.len() as i64) + id;
    }
    println(total);
}
"#,
        &["144"],
        "structpat_whilelet_pop_destructure_no_double_free",
    );
}

/// B-2026-07-03-28 Facet A — by-value param destructured, the `Option` leaf
/// matched+consumed. The param is entry-copied (independent `Option` payload),
/// the destructure zeros the source tag, and the `match` frees the leaf —
/// no double-free (the pre-fix prototype double-freed here), no leak.
#[test]
fn asan_option_field_destructure_match_consume_clean() {
    assert_clean_asan_run(
        r#"
struct A { path: Vec[String], sv: Option[String] }
fn f(a: A) -> i64 {
    let A { path, sv } = a;
    let mut t = 0;
    for s in path { if s.len() >= 0 { t = t + 1; } }
    match sv { Some(x) => { if x.len() >= 0 { t = t + 1; } } None => {} }
    t
}
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut p: Vec[String] = Vec.new();
        p.push("facet_a_destructure_vec_payload_alpha_aaaa".to_string());
        v.push(A { path: p, sv: Some("facet_a_destructure_option_payload_beta_bb".to_string()) });
        i = i + 1;
    }
    v
}
fn main() {
    let xs = build();
    let mut t = 0;
    for a in xs { t = t + f(a); }
    println(t);
}
"#,
        &["12"],
        "option_field_destructure_match_consume",
    );
}

/// B-2026-07-03-28 Facet A — destructured, the `Option` leaf never consumed:
/// its tracked inline-Option cleanup frees the payload at scope exit while the
/// source struct drop skips it (tag zeroed). No leak, no double-free.
#[test]
fn asan_option_field_destructure_unused_freed() {
    assert_clean_asan_run(
        r#"
struct A { path: Vec[String], sv: Option[String] }
fn f(a: A) -> i64 {
    let A { path, sv } = a;
    0
}
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut p: Vec[String] = Vec.new();
        p.push("facet_a_unused_vec_payload_gamma_cccccccccc".to_string());
        v.push(A { path: p, sv: Some("facet_a_unused_option_payload_delta_dddddddd".to_string()) });
        i = i + 1;
    }
    v
}
fn main() {
    let xs = build();
    let mut t = 0;
    for a in xs { t = t + f(a); }
    println(t);
}
"#,
        &["0"],
        "option_field_destructure_unused_freed",
    );
}

#[test]
fn asan_rc_elide_if_let_consumed_payload_no_leak() {
    // Residual shape via if-let: `if let Some(n) = p { r = sink(n); }`. `p`
    // is scrutinee-only (condition 2 holds) but its payload is moved out, so
    // condition 4 declines to elide `probe3`; runs balanced. Prints 1400.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn sink(x: Node) -> i64 { x.val }
fn probe3(p: Option[Node]) -> i64 { let mut r = 0i64; if let Some(n) = p { r = sink(n); } r }
fn main() {
    let mut pool: Vec[Option[Node]] = Vec.new();
    pool.push(Some(Node { val: 5i64, left: None, right: None }));
    pool.push(Some(Node { val: 9i64, left: None, right: None }));
    let mut t: i64 = 0i64;
    let mut rep: i64 = 0i64;
    while rep < 200i64 { let idx = rep % 2i64; t = t + probe3(pool[idx].clone()); rep = rep + 1i64; }
    println(f"{t}")
}
"#,
        &["1400"],
        "rc_elide_if_let_consumed_payload_no_leak",
    );
}

/// B-2026-08-07-11 residual — the envelope chain a MATCH ARM parks when it
/// disarms a struct field's drop.
///
/// `own_boxed_option_field_envelope_at` (B-2026-08-07-7) exists because
/// zeroing an `Option` field's tag is a single guard away from two different
/// frees: `karac_drop_Option_<P>` tests `tag == Some` once and behind it does
/// BOTH the deep drop of the interior and the `free` of the box. The arm
/// needs the first suppressed and the second kept, so it parks the box
/// pointer in a private slot and registers a box-only `BoxedEnumDrop`
/// against it. That registration owned exactly ONE envelope, and a field
/// whose payload boxes AGAIN left the rest owned by nobody.
///
/// A FIRST OWNER, NOT A SECOND, which is what made it safe to add and is
/// measured rather than argued: pre-fix the shape LEAKED. The field's own
/// drop is disarmed by the tag zero, and the arm's binding owns the
/// INTERIOR only — had anything else claimed these envelopes the result
/// would have been a double free. `224c945f` deferred them here on the
/// opposite assumption while B-2026-08-07-6/-11 were in flight; all three of
/// those legs have since landed and none reaches this path.
///
/// HOW THE SURVIVOR WAS IDENTIFIED, since the row's earlier passes guessed
/// wrong twice. Varying only the nesting depth gives depth 2 clean, depth 3
/// losing one box, depth 4 losing two — exactly one envelope freed at every
/// depth, which is a missing WALK rather than a missing owner. The emitted
/// module then showed each `karac_drop_Option_...` level freeing its own box
/// and delegating correctly, so the drop functions were never the problem;
/// `main` was reaching them through a single-box parked action instead. That
/// is the "count frees against mallocs in the emitted module" step this row
/// prescribed, and it is what separated the two candidates.
///
/// ARMS: `a2` is the empty-chain control that must stay at exactly one
/// free; `a3` the reported shape; `a4` two envelopes below the parked one;
/// `g` a NON-binding arm over the same field, which takes the other path
/// through the guard; `s3` the scalar-interior sibling that leg (b) fixed,
/// re-asserted so the two owners cannot start fighting; `u` a struct built
/// whose field is never matched at all.
///
/// The interior is a heap `String` in every String arm, which is the
/// envelope/interior boundary as an assertion: the walk frees envelopes and
/// must not reach the `String` the arm binds out. Pre-fix this program lost
/// 2,560 B definitely plus 1,280 B indirectly at `KARAC_OPT_LEVEL=0` and was
/// CLEAN at `-O2`, so the memory half rides on the `-O0` leg; the floor is
/// real regardless (606 allocations at `-O2`) because the `String`s survive
/// folding even though the envelopes do not.
///
/// The expected value is COMPUTED: three String arms contribute 1 each and
/// the scalar arm subtracts the opaque `env.args().len()` seed back out to
/// leave `i` — `i + 3` per iteration, so `(0+…+39) + 3 * 40 = 900`.
#[test]
fn asan_match_arm_parked_envelope_frees_every_level() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Hs2 { b: Option[Option[String]] }
struct Hs3 { b: Option[Option[Option[String]]] }
struct Hs4 { b: Option[Option[Option[Option[String]]]] }
struct Hi3 { b: Option[Option[Option[i64]]] }
fn mkstr(n: i64) -> String {
    let mut s: String = String.new();
    s.push_str("envelope-");
    s.push_str(n.to_string());
    s.push_str("-padding-to-force-heap");
    s
}
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a2: Hs2 = Hs2 { b: Option.Some(Option.Some(mkstr(n + i))) };
        match a2.b { Option.Some(Option.Some(t)) => { if t.contains("envelope-") { acc = acc + 1; } } _ => { acc = acc - 1; } }

        let a3: Hs3 = Hs3 { b: Option.Some(Option.Some(Option.Some(mkstr(n + i)))) };
        match a3.b { Option.Some(Option.Some(Option.Some(t))) => { if t.contains("envelope-") { acc = acc + 1; } } _ => { acc = acc - 1; } }

        let a4: Hs4 = Hs4 { b: Option.Some(Option.Some(Option.Some(Option.Some(mkstr(n + i))))) };
        match a4.b { Option.Some(Option.Some(Option.Some(Option.Some(t)))) => { if t.contains("envelope-") { acc = acc + 1; } } _ => { acc = acc - 1; } }

        let g: Hs3 = Hs3 { b: Option.Some(Option.Some(Option.Some(mkstr(n + i)))) };
        match g.b { Option.Some(Option.Some(Option.None)) => { acc = acc - 1; } _ => { acc = acc + 0; } }

        let s3: Hi3 = Hi3 { b: Option.Some(Option.Some(Option.Some(n + i))) };
        match s3.b { Option.Some(Option.Some(Option.Some(x))) => { acc = acc + x - n; } _ => { acc = acc - 1; } }

        let u: Hs3 = Hs3 { b: Option.Some(Option.Some(Option.Some(mkstr(n + i)))) };
        acc = acc + 0;

        i = i + 1;
    }
    println(acc);
}
"#,
        &["900"],
        "match_arm_parked_envelope_frees_every_level",
        200,
    );
}

/// B-2026-08-07-7 — a match arm binding the INTERIOR out of a struct
/// field whose `Option` payload is heap-BOXED. Double free at BOTH opt
/// levels before this: corruption, where every sibling shape merely leaks.
///
/// `struct W { o: Option[Option[String]] }` matched as
/// `Option.Some(Option.Some(s))`. `__karac_drop_struct_W` routes the field
/// to the DEEP `karac_drop_Option_Option_String`, which frees the `String`
/// inside the box, and the arm's `s` owns that same buffer. The arm cannot
/// disarm it through the normal channel — `EnumDropKind::is_heap_bearing()`
/// is false for `BoxedOptRes`, so the match-out suppressor skips a boxed
/// payload BY DESIGN — so the fix writes to the STRUCT instead, zeroing the
/// field's tag, which the drop guards on before it touches the box.
///
/// THE GATE IS THE FIX, and both of its edges were paid for in failures:
///
///   * BOX-ONLY UNCONDITIONALLY (the previous attempt) fixed this shape and
///     turned FIVE passing memory_sanitizer tests into LeakSanitizer
///     failures — every one an undestructured or borrow-only shape whose
///     interior the deep drop legitimately owns. The `unbound` arm here is
///     that population in miniature and must stay clean.
///   * FIRING ON ANY CONSUMING PATTERN over-corrected the other way:
///     `match a.value { Some(v) => … }` binds the WHOLE payload, `v` owns
///     the payload and the struct drop still owns the box — one allocation
///     each, correctly divided. Zeroing there orphans both (measured: 504 B
///     over 14 allocations in `asan_b04_7_option_heap_enum_struct_field_
///     drop`, caught by the -O0 leg). So the gate requires a NESTED variant
///     sub-pattern, not merely a consuming one.
///
/// The `scalar` arm is the no-heap control (`Option[Option[i64]]` — nothing
/// to double-free, and its box must still be freed), and the
/// `Some(None)` / `None` arms are the tag guards.
///
/// QUARANTINED ON THE -O0 LEG, and the entry is this fix's admitted price:
/// zeroing the tag also skips the box free, so the 32-byte ENVELOPE leaks
/// in the consuming case. That is a leak replacing corruption, tracked by
/// the row rather than hidden — see `tests/asan-o0-known-failures.txt`. At
/// -O2 the envelope folds away and this runs fully clean, which is where
/// the double-free assertion bites: the pre-fix abort reproduced at BOTH
/// levels, so the default leg catches any regression on its own.
///
/// The `whole` arm is B-2026-08-07-9, the two-step spelling of the same
/// reach: `Some(inner) => match inner { Some(s) => … }` gets to the
/// interior via a binding instead of a nested pattern, and double-freed
/// identically. It cannot be admitted by widening the pattern test — that
/// is precisely the over-fire the second bullet above forbids, since at the
/// arm it and `asan_b04_7`'s correct `Some(v) => ident_len(v)` are both
/// `Some(<binding>)`. What separates them is what happens to the binding
/// NEXT, so the gate asks the arm BODY: a plain binding qualifies only when
/// the body goes on to destructure it. `asan_b04_7` consumes `v` whole and
/// stays untouched, which the -O0 leg checks on every run.
///
/// Expected value is COMPUTED: per iteration the three `String` arms give
/// 1 each, the scalar arm gives `i`, and the two payload-absent arms -1
/// each — `i + 1` per iteration, so `(0+…+39) + 40 = 820`.
#[test]
fn asan_struct_field_boxed_payload_interior_match_out_no_double_free() {
    assert_clean_asan_run_min_allocs(
        r#"
struct W { o: Option[Option[String]] }
struct V { o: Option[Option[i64]] }
fn bound(w: W) -> i64 {
    match w.o { Option.Some(Option.Some(s)) => if s.len() > 0 { 1 } else { 0 }, _ => -1 }
}
fn unbound(w: W) -> i64 {
    match w.o { Option.Some(Option.Some(_)) => 1, _ => -1 }
}
fn whole(w: W) -> i64 {
    match w.o { Option.Some(inner) => match inner { Option.Some(s) => if s.len() > 0 { 1 } else { 0 }, Option.None => -1 }, Option.None => -1 }
}
fn scalar(v: V) -> i64 {
    match v.o { Option.Some(Option.Some(x)) => x, _ => -1 }
}
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        acc = acc + bound(W { o: Option.Some(Option.Some(f"p{n + i}")) });
        acc = acc + unbound(W { o: Option.Some(Option.Some(f"p{n + i}")) });
        acc = acc + whole(W { o: Option.Some(Option.Some(f"p{n + i}")) });
        acc = acc + scalar(V { o: Option.Some(Option.Some(n + i)) }) - n;
        let w2: W = W { o: Option.Some(Option.None) };
        acc = acc + bound(w2);
        let w3: W = W { o: Option.None };
        acc = acc + bound(w3);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["820"],
        "struct_field_boxed_payload_interior_match_out",
        30,
    );
}

/// B-2026-08-07-7 residue — the ENVELOPE the arm's tag zero orphans must
/// have an owner of its own.
///
/// The sibling fixture above stops the double free by zeroing the struct
/// field's tag, and `karac_drop_Option_<P>` tests that tag before it does
/// EITHER of its two jobs: the deep drop of the interior (the corruption)
/// and the `free` of the 32-byte box (which nothing else claims). So the
/// only channel the arm has necessarily paid for the fix with a leak. The
/// box pointer is now parked in a private slot at the arm and freed
/// BOX-ONLY when the arm's frame drains; the interior's owner is unchanged.
///
/// SEPARATE FROM THE SIBLING FIXTURE ON PURPOSE, and the separation is the
/// measurement. That one passes its `W` BY VALUE to a callee, which orphans
/// the CALLER's envelope for an unrelated reason (the callee's entry copy
/// duplicates the box and shares the interior, B-2026-08-07-12) — so it
/// stays quarantined on the -O0 leg and could never witness this fix. Every
/// `W` here is a local place matched in the same frame, which isolates the
/// envelope to exactly one owner question.
///
/// CARRIED BY THE -O0 LEG. At -O2 the box folds away with the allocation, so
/// the default run is a shape check rather than a leak check; the pre-fix
/// measurement was 320 B over 10 blocks at `KARAC_OPT_LEVEL=0` and nothing
/// at -O2. The three controls do gate at both levels: `b`'s non-consuming
/// arm must keep the deep drop (box-only unconditionally is what broke five
/// tests), `c` is a box with an absent interior, and `d` has no box at all.
///
/// Expected value is COMPUTED: 1 + 2 + 4 + 8 = 15 per iteration, ×40 = 600.
#[test]
fn asan_struct_field_boxed_payload_match_out_envelope_owned() {
    assert_clean_asan_run_min_allocs(
        r#"
struct W { o: Option[Option[String]] }
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a: W = W { o: Option.Some(Option.Some(f"envelope-{n + i}")) };
        acc = acc + match a.o {
            Option.Some(Option.Some(s)) => if s.len() > 0 { 1 } else { 0 },
            Option.Some(Option.None) => 100,
            Option.None => 200,
        };
        let b: W = W { o: Option.Some(Option.Some(f"envelope-{n + i}")) };
        acc = acc + match b.o {
            Option.Some(Option.Some(_)) => 2,
            Option.Some(Option.None) => 100,
            Option.None => 200,
        };
        let c: W = W { o: Option.Some(Option.None) };
        acc = acc + match c.o {
            Option.Some(Option.Some(s)) => s.len() as i64,
            Option.Some(Option.None) => 4,
            Option.None => 200,
        };
        let d: W = W { o: Option.None };
        acc = acc + match d.o {
            Option.Some(Option.Some(s)) => s.len() as i64,
            Option.Some(Option.None) => 100,
            Option.None => 8,
        };
        i = i + 1;
    }
    println(acc);
}
"#,
        &["600"],
        "struct_field_boxed_payload_match_out_envelope_owned",
        30,
    );
}

#[test]
fn asan_nested_option_pattern_boxed_payload_lifecycle_clean() {
    // B-2026-07-15-5: an inner `Option[T]` payload is heap-BOXED (4 words
    // > Option's 3-word area / a user enum's 1-word enum-payload
    // carve-out). Nested variant patterns (`Option.Some(Option.Some(x))`,
    // `Wrap.W(Option.Some(x))`) must debox on both the condition and bind
    // paths, the box must be freed at scope exit (32-byte leak pre-fix),
    // and matching a nested pattern against a NON-matching variant
    // (`Option.None` value) must not NULL-deref the zero payload word —
    // the debox load is gated behind the outer tag comparison.
    assert_clean_asan_run(
        r#"
enum Wrap {
    W(Option[String]),
    Empty,
}
fn classify(v: Option[Option[i64]]) -> String {
    match v {
        Option.Some(Option.Some(x)) => f"inner {x}",
        Option.Some(Option.None) => "inner none",
        Option.None => "outer none",
    }
}
fn main() {
    println(classify(Option.Some(Option.Some(42))));
    println(classify(Option.Some(Option.None)));
    println(classify(Option.None));
    let w: Wrap = Wrap.W(Option.Some("boxed heap payload well beyond inline width"));
    match w {
        Wrap.W(Option.Some(s)) => println(s.len()),
        Wrap.W(Option.None) => println(-1),
        Wrap.Empty => println(-2),
    }
    let dropped: Option[Option[String]] = Option.Some(Option.Some("never matched out"));
    match dropped {
        Option.None => println("none"),
        _ => println("kept"),
    }
}
"#,
        &["inner 42", "inner none", "outer none", "43", "kept"],
        "nested_option_pattern_boxed_payload_lifecycle_clean",
    );
}

#[test]
fn asan_match_on_match_result_scrutinee_clean() {
    // B-2026-08-18-8 — the SCRUTINEE-position twin of the fixture above.
    // Identical program but for one thing: the inner match's result is fed
    // straight into the outer match instead of being bound to a `let`
    // first. That single difference used to leak the boxed `Option`
    // payload — 32 bytes per evaluation, 3200 bytes in 100 objects over
    // these 200 iterations — because a match RESULT consumed as a
    // temporary registered no box cleanup, where a `let`-bound one did.
    //
    // Written with an explicit nested `match` and NO `?.` anywhere: the
    // defect was found through `?.` (which lowers to exactly this shape)
    // but is not `?.`'s, and spelling it out keeps this fixture honest if
    // the `?.` lowering ever changes again.
    assert_clean_asan_run(
        r#"
struct City { name: String, zip: i64 }
struct Address { city: Option[City], tag: String }
struct User { address: Option[Address] }

fn mk(i: i64) -> User {
    if i % 2 == 0 { return User { address: Some(Address { city: Some(City { name: "Paris", zip: 7 }), tag: "t" }) }; }
    return User { address: None };
}

fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 200 {
        let u = mk(i);
        match (match u.address { Some(a) => { a.city } None => { None } }) {
            Some(v) => { total = total + (v.name.len() as i64); }
            None => { total = total + 1; }
        }
        i = i + 1;
    }
    println(total);
}
"#,
        &["600"],
        "match_on_match_result_scrutinee_clean",
    );
}

#[test]
fn asan_optional_chain_as_match_scrutinee_clean() {
    // B-2026-08-18-8, second spelling. `?.` is synthesized into the very
    // match the fixture above spells out (B-2026-08-17-28), so a chain in
    // scrutinee position inherited the same orphaned box — and it is the
    // spelling anyone would actually write.
    //
    // It reaches the fix through a DIFFERENT arm than the explicit match:
    // `compile_optional_chain` builds its match from the `OptionalChain`
    // node during compilation, so the node the scrutinee tracker sees is
    // still the chain, not a `Match`. TWO levels deep on purpose — the
    // outer chain's source is the inner chain, which is what exercises the
    // recursive owned-source resolution.
    //
    // The payloads carry RUNTIME-built strings (not literals, whose
    // `cap == 0` makes an interior leak invisible), so this asserts the
    // box's interior as well as its 32-byte envelope.
    assert_clean_asan_run(
        r#"
struct City { name: String, zip: i64 }
struct Address { city: Option[City], tag: String }
struct User { address: Option[Address] }

fn mk(i: i64) -> User {
    if i % 2 == 0 {
        return User { address: Some(Address { city: Some(City { name: f"city-{i}", zip: i }), tag: f"t{i}" }) };
    }
    return User { address: None };
}

fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 200 {
        let u = mk(i);
        match u.address?.city?.name {
            Some(v) => { total = total + (v.len() as i64); }
            None => { total = total + 1; }
        }
        i = i + 1;
    }
    println(total);
}
"#,
        &["845"],
        "optional_chain_as_match_scrutinee_clean",
    );
}

#[test]
fn asan_match_scrutinee_identifier_arm_hands_over_its_box() {
    // B-2026-08-18-10 — the arm shape B-2026-08-18-8 declined. An arm that
    // hands out a LOCAL owning a boxed payload is a move: the arm-tail
    // disarm zeroes the source's payload word in that arm's own block, so
    // after B-2026-08-18-8 the source was disarmed and the scrutinee still
    // owned nothing — 1088 B in 34 boxes plus their 117 B of string
    // interiors, at 200 iterations.
    //
    // THE ARM SPLIT IS THE POINT, and the `% 2` / `% 3` interleave is what
    // exercises it: `o` is `Some` on even `i`, and the handing-out arm runs
    // on `i % 3 == 0`, so all four combinations occur. The moving arm must
    // free the box exactly once, and the arm that keeps `o` must leave its
    // own drop armed — a fix that disarmed unconditionally would leak the
    // second case, and one that never disarmed would double-free the first.
    assert_clean_asan_run(
        r#"
struct City { name: String, zip: i64 }

fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 200 {
        let o: Option[City] = if i % 2 == 0 { Some(City { name: f"c{i}", zip: i }) } else { None };
        match (match i % 3 { 0 => o, _ => None }) {
            Some(v) => { total = total + (v.name.len() as i64); }
            None => { total = total + 1; }
        }
        i = i + 1;
    }
    println(total);
}
"#,
        &["283"],
        "match_scrutinee_identifier_arm_hands_over_its_box",
    );
}

#[test]
fn asan_match_scrutinee_field_arm_from_an_unrelated_local_clean() {
    // B-2026-08-18-10, second half. B-2026-08-18-8 accepted a field
    // projection only when the chain was rooted at that arm's OWN payload
    // binding, and called the restriction load-bearing. Measurement says
    // otherwise: this program — the projection rooted at an unrelated live
    // local, which is then READ AGAIN after the match — leaked 3545 B in
    // 200 objects under that rule, while its `let`-bound twin was clean.
    // Same defect, narrower fix. The trailing `other.tag` read is the part
    // that would expose an over-eager free as corruption rather than a
    // quiet leak.
    assert_clean_asan_run(
        r#"
struct City { name: String, zip: i64 }
struct Address { city: Option[City], tag: String }

fn mkaddr(i: i64) -> Address {
    return Address { city: Some(City { name: f"c{i}", zip: i }), tag: f"t{i}" };
}

fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 200 {
        let other = mkaddr(i);
        match (match i % 2 { 0 => { other.city } _ => { None } }) {
            Some(v) => { total = total + (v.name.len() as i64); }
            None => { total = total + 1; }
        }
        total = total + (other.tag.len() as i64);
        i = i + 1;
    }
    println(total);
}
"#,
        &["1135"],
        "match_scrutinee_field_arm_from_an_unrelated_local_clean",
    );
}

#[test]
fn asan_rewrap_match_bound_shared_enum_struct_payload_no_double_free() {
    // B-2026-07-18-29: re-wrapping a match-bound shared-enum STRUCT payload
    // that carries a DIRECT shared/RC child (`MethodCallExpr.object: Expr`)
    // into a new variant node (`match v { MethodCall(mc) => emit(MethodCall(mc)) }`)
    // double-freed under AOT (single non-String child → UAF), interp correct.
    // The payload failed `struct_clone_fully_duplicates` (its clone would
    // shallow-copy the shared handle), so it was bound as a non-owning VIEW
    // aliasing the RC box; moving the whole view into the re-wrapped node made
    // a SECOND owner of the aliased buffers. Fixed in three coordinated pieces:
    // (1) upgrade the view to an OWNED deep copy with shared-child RETAIN at
    // bind (deep_copy + copy_support_for_loop_shared_mode); (2) `track_struct_var`
    // registers the COMBINED value-drop + shared-field rc-DEC (already picked
    // for a `struct_owns_shared_field` struct); (3) at the shared-enum variant
    // constructor, retract the moved binding's combined StructDrop wholesale
    // (`suppress_struct_cleanup_for_tail_identifier`) so exactly the new box
    // owns it. Loops 300× building a fresh node each time (fresh inner shared
    // node + String + Vec payload) and round-trips it through the re-wrap, so a
    // per-iteration double-free aborts and any leak accumulates for LSan.
    assert_clean_asan_run(
        r#"
struct MCall { object: Expr, method: String, args: Vec[i64] }
shared enum Expr {
    Lit(String),
    MethodCall(MCall),
}
fn emit(e: Expr) -> i64 {
    match e {
        Lit(s) => s.len(),
        MethodCall(mc) => mc.method.len() + mc.args.len(),
    }
}
fn process(value: Expr) -> i64 {
    match value {
        Lit(s) => s.len(),
        MethodCall(mc) => emit(MethodCall(mc)),
    }
}
fn main() {
    let mut total = 0i64;
    let mut i = 0i64;
    while i < 300 {
        let inner: Expr = Lit("x".to_string());
        let mut a: Vec[i64] = Vec.new();
        a.push(1);
        a.push(2);
        let mc = MCall { object: inner, method: "substring".to_string(), args: a };
        let v: Expr = MethodCall(mc);
        total = total + process(v);
        i = i + 1;
    }
    println(total);
}
"#,
        &["3300"],
        "rewrap_match_bound_shared_enum_struct_payload_no_double_free",
    );
}

/// B-2026-08-02-25 (match-arm leg) — re-homing a BOXED payload's Drop body
/// onto the arm binding must run it against the BOX, not the binding's copy.
///
/// This is the guard for the sharp edge of that leg. A boxed payload's
/// memory stays owned by the box drop; the arm binding holds a reconstructed
/// COPY of `{ptr,len,cap}`. Run `Full.drop` — whose body calls
/// `self.buf.clear()` — against that copy and the buffer is freed while the
/// BOX keeps the stale pointer, so the scope-exit box drop frees it again.
/// The first attempt at this leg did exactly that and double-freed all four
/// consuming shapes below. Re-registering the SOURCE's own
/// `__karac_dropelems_opt_*` action under the binding's NAME (same walker,
/// same slot, new fire point) is what makes the mutation land where the
/// later free reads it.
///
/// The read-only bodies the E2E pins use cannot catch this — they touch no
/// heap — so the payload here deliberately mutates. `d` is the control: a
/// non-consuming arm keeps the source's walk and must stay clean too.
#[test]
fn asan_boxed_optres_payload_arm_body_runs_against_the_box() {
    assert_clean_asan_run(
        r#"
struct Full { name: String, buf: Vec[i64] }
impl Drop for Full {
    fn drop(mut ref self) {
        if let Some(v) = self.buf.first() { if v < 0i64 { println(v); } }
        self.buf.clear();
    }
}

fn mk(i: i64) -> Full {
    let mut b: Vec[i64] = Vec.new();
    b.push(i);
    return Full { name: "payload-string-data", buf: b };
}

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        // NAMED source, consuming match arm — the body re-homes onto `r`.
        let a: Option[Full] = Option.Some(mk(i));
        match a {
            Option.Some(r) => { n = n + r.buf.len(); }
            Option.None => { n = n + 100i64; }
        }
        // if-let sibling.
        let b: Option[Full] = Option.Some(mk(i));
        if let Option.Some(r) = b { n = n + r.buf.len(); }
        // let-else sibling: the binding lands in the ENCLOSING frame, so its
        // action drains there rather than in an arm frame.
        let c: Option[Full] = Option.Some(mk(i));
        let Option.Some(r3) = c else { return; }
        n = n + r3.buf.len();
        // CONTROL: non-consuming arm, source keeps its own walk.
        let d: Option[Full] = Option.Some(mk(i));
        match d {
            Option.Some(_) => { n = n + 1i64; }
            Option.None => { n = n + 100i64; }
        }
        i = i + 1;
    }
    println(n);
}
"#,
        // 1 + 1 + 1 element read + 1 = 4 per iteration x 200 = 800.
        &["800"],
        "boxed_optres_payload_arm_body_runs_against_the_box",
    );
}

/// B-2026-08-04-1 — the FRESH-TEMP sibling of the guard above.
///
/// Same defect, same subject rule, different route into it: a temp has no
/// named source whose action can be re-homed, so the bind site used to
/// register a bodies-only walker over the binding's reconstructed COPY.
/// `self.buf.clear()` then freed the buffer and zeroed the copy's cap while
/// the box kept the stale `{ptr,len,cap}`, and the box drop freed it again.
/// The fix threads the staged `__freshtemp_boxed_scrut` slot through so the
/// walk runs against the box.
///
/// `d` is the control on the other side: a non-consuming `Some(_)` arm
/// binds nothing, so there is no registration to make and nothing here may
/// change for it. (That shape has its own PRE-EXISTING 32-byte leak — a
/// wildcard payload leaves `inner_struct_name` empty, so the box drop is
/// box-only — filed as B-2026-08-04-3 and deliberately kept OUT of this
/// program, since LSan would attribute it here.)
#[test]
fn asan_freshtemp_boxed_optres_payload_arm_body_runs_against_the_box() {
    assert_clean_asan_run(
        r#"
struct Full { name: String, buf: Vec[i64] }
impl Drop for Full {
    fn drop(mut ref self) {
        if let Some(v) = self.buf.first() { if v < 0i64 { println(v); } }
        self.buf.clear();
    }
}

fn mk(i: i64) -> Full {
    let mut b: Vec[i64] = Vec.new();
    b.push(i);
    return Full { name: "payload-string-data", buf: b };
}

fn opt(i: i64) -> Option[Full] { return Option.Some(mk(i)); }

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        // FRESH-TEMP match arm — no named source to re-home from.
        match opt(i) {
            Option.Some(r) => { n = n + r.buf.len(); }
            Option.None => { n = n + 100i64; }
        }
        // if-let sibling.
        if let Option.Some(r) = opt(i) { n = n + r.buf.len(); }
        // let-else sibling: the binding escapes into the enclosing frame.
        let Option.Some(r3) = opt(i) else { return; }
        n = n + r3.buf.len();
        // while-let over `pop()` — a fresh temp per iteration, and the one
        // shape here whose scrutinee type comes from the pop-family
        // derivation rather than the fn-return table.
        let mut v: Vec[Full] = Vec.new();
        v.push(mk(i));
        while let Option.Some(r) = v.pop() {
            n = n + r.buf.len();
        }
        i = i + 1;
    }
    println(n);
}
"#,
        // 4 element reads per iteration x 200 = 800.
        &["800"],
        "freshtemp_boxed_optres_payload_arm_body_runs_against_the_box",
    );
}

/// B-2026-08-04-3 — a WILDCARD payload arm still needs the box's inner
/// walk: it binds nothing, so the box is the only owner the interior will
/// ever have.
///
/// `track_freshtemp_boxed_enum_scrutinee` derived the payload struct name
/// from a `Binding` sub-pattern only, so a wildcard fell into the same
/// `_ => None` arm as a struct DESTRUCTURE and the free went box-ONLY,
/// stranding the payload's heap fields. That arm is correct for the
/// destructure — its leaf bindings each own and free their field — so
/// binds-nothing and binds-the-parts needed splitting apart.
///
/// A wildcard carries no type, which also meant the width gate above the
/// derivation rejected the arm first (`pattern_payload_word_count` falls to
/// its 1-word default), so both the size and the name now come from the
/// scrutinee's instantiation. Both `Option` and `Result`'s Err side are
/// here because the variant picks which generic arg to read.
#[test]
fn asan_wildcard_boxed_optres_payload_frees_the_struct_interior() {
    assert_clean_asan_run(
        r#"
struct Full { name: String, buf: Vec[i64] }
fn mk(i: i64) -> Full {
    let mut b: Vec[i64] = Vec.new();
    b.push(i);
    return Full { name: "payload-string-data", buf: b };
}
fn opt(i: i64) -> Option[Full] { return Option.Some(mk(i)); }
fn res(i: i64) -> Result[i64, Full] { return Result.Err(mk(i)); }

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        // Wildcard payload, Option — binds nothing, so the box must free the
        // interior.
        match opt(i) {
            Option.Some(_) => { n = n + 1i64; }
            Option.None => { n = n + 100i64; }
        }
        // Wildcard payload on Result's Err side — the variant decides which
        // generic arg the payload type comes from.
        match res(i) {
            Result.Ok(v) => { n = n + v; }
            Result.Err(_) => { n = n + 1i64; }
        }
        // CONTROL: a whole binding, where the box already owned the interior.
        match opt(i) {
            Option.Some(r) => { n = n + r.buf.len(); }
            Option.None => { n = n + 100i64; }
        }
        i = i + 1;
    }
    println(n);
}
"#,
        // 3 per iteration x 200 = 600.
        &["600"],
        "wildcard_boxed_optres_payload_frees_the_struct_interior",
    );
}

/// B-2026-08-04-5 — a STRUCT sub-pattern over a heap-BOXED payload
/// deboxes, and the leaf bindings end up owning the interior exactly once.
///
/// The fix that made this shape compile at all was a name-resolution one
/// (a bare `Full { .. }` path resolved to the prelude's
/// `ChannelError.Full`), but it lands squarely on the ownership machinery:
/// with the debox now firing, `suppress_boxed_payload_struct_destructure`
/// became reachable for the first time. This pin is what says the leaf
/// bindings free the fields and the box drop does not free them again.
///
/// Reading `.name` and `.buf.len()` in every case is load-bearing — with
/// the buffers dead the allocations are elided and the whole test passes
/// vacuously.
///
/// NOT covered, deliberately: a FRESH-TEMP scrutinee whose struct
/// destructure leaves the `Vec` field UNBOUND (`match opt(i) { Some(Full {
/// name, buf: _ }) => .. }`). That shape leaks the unbound field's buffer
/// — the box drop stays box-only for any struct destructure, which is
/// right when every field is bound and wrong when some are not. It is
/// filed as its own row; keeping it out of this pin stops LeakSanitizer
/// from attributing that leak here. The NAMED-scrutinee twin is clean and
/// IS covered below.
#[test]
fn asan_boxed_optres_payload_struct_destructure_owns_the_interior_once() {
    assert_clean_asan_run(
        r#"
struct Full { name: String, buf: Vec[i64] }
struct Narrow { name: String }
fn mk(i: i64) -> Full {
    let mut b: Vec[i64] = Vec.new();
    b.push(i);
    return Full { name: "payload-string-data", buf: b };
}
fn opt(i: i64) -> Option[Full] { return Option.Some(mk(i)); }
fn res(i: i64) -> Result[i64, Full] { return Result.Err(mk(i)); }

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        // Named scrutinee, full destructure.
        let o: Option[Full] = Option.Some(mk(i));
        match o {
            Option.Some(Full { name, buf }) => { n = n + name.len() + buf.len(); }
            Option.None => { n = n + 100i64; }
        }
        // Fresh temp, full destructure.
        match opt(i) {
            Option.Some(Full { name, buf }) => { n = n + name.len() + buf.len(); }
            Option.None => { n = n + 100i64; }
        }
        // Result's Err half — the variant picks which generic arg is boxed.
        match res(i) {
            Result.Ok(v) => { n = n + v; }
            Result.Err(Full { name, buf }) => { n = n + name.len() + buf.len(); }
        }
        // Named scrutinee, PARTIAL destructure: the box keeps the unbound
        // field. (The fresh-temp twin of this leaks — see the doc note.)
        let o4: Option[Full] = Option.Some(mk(i));
        match o4 {
            Option.Some(Full { name, buf: _ }) => { n = n + name.len(); }
            Option.None => { n = n + 100i64; }
        }
        // A bound field that ESCAPES the arm still has exactly one owner.
        let o5: Option[Full] = Option.Some(mk(i));
        let mut keep: Vec[String] = Vec.new();
        match o5 {
            Option.Some(Full { name, buf }) => { keep.push(name); n = n + buf.len(); }
            Option.None => { n = n + 100i64; }
        }
        n = n + keep[0].len();
        // CONTROL: the INLINE (3-word, never boxed) payload the fix must
        // leave byte-identical.
        let o6: Option[Narrow] = Option.Some(Narrow { name: "payload-string-data" });
        match o6 {
            Option.Some(Narrow { name }) => { n = n + name.len(); }
            Option.None => { n = n + 100i64; }
        }
        i = i + 1;
    }
    println(n);
}
"#,
        // Per iteration: 19+1, 19+1, 19+1, 19, 1+19, 19 = 118. x200 = 23600.
        &["23600"],
        "boxed_optres_payload_struct_destructure_owns_the_interior_once",
    );
}

/// B-2026-08-04-11 leg (b) — a fresh-temp `Result` arm that BINDS a struct
/// payload without consuming it owns that payload exactly once.
///
/// The arm's wrapper skip in `compile_match` comes from B-2026-07-12-2 gap
/// 2 and rests on "a struct-wrapper binding registers no cleanup of its
/// own", so a borrow-only read needs the source's inline-payload drop left
/// armed or the buffer leaks. B-2026-07-10-3 later falsified that premise
/// by tracking an inline `Option`/`Result` struct payload so its inner
/// `String`/`Vec` fields DO get freed — two owners, one buffer, `free():
/// double free detected`.
///
/// The last three shapes are the immunities, and the CONSUME one is the
/// blast-radius guard: it is gap 2's own recover-CONSUME case, where the
/// source must still suppress. Under LSan they also catch the opposite
/// failure — suppressing the source with no second owner leaks instead.
///
/// Two details fight the OPTIMIZER rather than the compiler, and dropping
/// either makes this vacuous at `-O2`. The seed is `env.args().len()`, not
/// a literal: a constant-seeded loop const-folds to its final total, and
/// the whole 1200-payload body then allocates NOTHING (measured: 1 alloc).
/// And each arm reads its buffer's BYTES via `contains`, because a payload
/// touched only through `.len()` is a provably dead allocation that LLVM
/// deletes outright — the `.len()`-only draft of this fixture still ran
/// clean against the unfixed compiler. With both, it allocates ~3.2k
/// buffers and aborts without the fix. The `min_allocs` floor below is
/// what keeps that true: it fails loudly if either property is ever lost.
#[test]
fn asan_freshtemp_result_arm_binding_a_struct_payload_owns_it_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct One { msg: String }
struct Two { code: i64, msg: String }
fn s_of(tag: String, i: i64) -> String {
    let mut s: String = String.new();
    s.push_str(tag);
    s.push_str(f"-payload-{i}");
    return s;
}
fn digits(i: i64) -> String {
    let mut d: String = String.new();
    d.push_str(f"{i}");
    return d;
}
fn g1(i: i64) -> Result[i64, One] { return Result.Err(One { msg: s_of("one", i) }); }
fn g2(i: i64) -> Result[i64, Two] { return Result.Err(Two { code: i, msg: s_of("two", i) }); }
fn main() {
    let base: i64 = env.args().len();
    let mut n = 0i64;
    let mut i = base;
    while i < base + 200i64 {
        // Bound and never read.
        match g1(i) {
            Result.Ok(v) => { n = n + v; }
            Result.Err(e) => { n = n + 1i64; }
        }
        // Bound and READ — borrow-only, which is what the skip keyed on.
        match g1(i) {
            Result.Ok(v) => { n = n + v; }
            Result.Err(e) => { if e.msg.contains(digits(i)) { n = n + e.msg.len(); } }
        }
        // Two-field payload, still inside Result's 5-word inline area.
        match g2(i) {
            Result.Ok(v) => { n = n + v; }
            Result.Err(e) => { if e.msg.contains(digits(i)) { n = n + e.msg.len() + e.code; } }
        }
        // IMMUNITY: a wildcard binds nothing, so nothing ever competed.
        match g1(i) {
            Result.Ok(v) => { n = n + v; }
            Result.Err(_) => { n = n + 2i64; }
        }
        // IMMUNITY: a named scrutinee takes the ordinary binding path.
        let r: Result[i64, One] = g1(i);
        match r {
            Result.Ok(v) => { n = n + v; }
            Result.Err(e) => { if e.msg.contains(digits(i)) { n = n + e.msg.len(); } }
        }
        // IMMUNITY / blast radius: gap 2's recover-CONSUME — the arm moves the
        // field out, and the source must still suppress.
        match g1(i) {
            Result.Ok(v) => { n = n + v; }
            Result.Err(e) => { let m: String = e.msg; if m.contains(digits(i)) { n = n + m.len(); } }
        }
        i = i + 1;
    }
    println(n);
}
"#,
        // base is 1 (argv is the binary alone), so i runs 1..=200. Per
        // iteration: 1 + 2 for the two scalar arms, four buffer reads of
        // 12+digits(i) each, plus `code` = i from the two-field arm — that
        // is 51 + 4*digits(i) + i. Summed over 1..=200: 10200 + 1968 +
        // 20100 = 32268.
        &["32268"],
        "freshtemp_result_arm_binding_a_struct_payload_owns_it_once",
        // ~3.2k buffers are intended (6 payloads x 200 iterations, plus the
        // needles). The floor sits far below that so ordinary allocator
        // variation never trips it, and far above the 1-to-6 a folded-away
        // or dead-stripped version reaches.
        1000,
    );
}

/// B-2026-08-04-6 — a FRESH-TEMP boxed payload destructured by a PARTIAL
/// struct pattern: the fields the pattern binds are freed by their
/// bindings, the fields it leaves out are freed by the box.
///
/// The tracker had only two answers for a fresh temp — walk the whole
/// interior, or free the box alone — and a struct destructure took the
/// second, so `Some(Full { name, buf: _ })` left `buf`'s buffer with no
/// owner at all. The NAMED scrutinee had always decided this per FIELD;
/// its suppressor just bailed on its first line for a temp, which wanted
/// an `Identifier`. The tracker now walks the interior for a struct
/// destructure and the suppressor disarms the bound fields inside the box
/// first, so the two halves meet in the middle.
///
/// The all-bound case is the one that keeps this honest in the other
/// direction: every cap is zeroed, the walk becomes a no-op, and the
/// bindings are still the sole owners — walking without the suppressor
/// would double-free instead.
///
/// Every case reads the fields it binds, and the two `String`s are built
/// by MUTATION rather than from a literal or f-string. Both matter: an
/// unread field's malloc is elided outright (measured — 5 allocs vs 3),
/// and an elided allocation cannot leak, so a lazier fixture passes
/// against a completely broken fix.
#[test]
fn asan_freshtemp_boxed_payload_partial_destructure_owns_every_field() {
    assert_clean_asan_run(
        r#"
struct Full { name: String, buf: Vec[i64] }
fn mk(i: i64) -> Full {
    let mut b: Vec[i64] = Vec.new();
    b.push(i);
    let mut s: String = String.new();
    s.push_str("payload-string-data");
    return Full { name: s, buf: b };
}
fn opt(i: i64) -> Option[Full] { return Option.Some(mk(i)); }
fn res(i: i64) -> Result[i64, Full] { return Result.Err(mk(i)); }

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        // `field: _` — the box keeps `buf`.
        match opt(i) {
            Option.Some(Full { name, buf: _ }) => { n = n + name.len(); }
            Option.None => { n = n + 100i64; }
        }
        // `..` — same shape, other spelling.
        match opt(i) {
            Option.Some(Full { name, .. }) => { n = n + name.len(); }
            Option.None => { n = n + 100i64; }
        }
        // The box keeps the STRING half instead. Symmetric with the two
        // above; it only looked exempt while its malloc was being elided.
        match opt(i) {
            Option.Some(Full { name: _, buf }) => { n = n + buf.len(); }
            Option.None => { n = n + 100i64; }
        }
        // Result's Err half — the variant picks which generic arg is boxed.
        match res(i) {
            Result.Ok(v) => { n = n + v; }
            Result.Err(Full { name, buf: _ }) => { n = n + name.len(); }
        }
        // CONTROL, the double-free direction: all fields bound, so every cap
        // is disarmed and the box's walk must find nothing left to free.
        match opt(i) {
            Option.Some(Full { name, buf }) => { n = n + name.len() + buf.len(); }
            Option.None => { n = n + 100i64; }
        }
        i = i + 1;
    }
    println(n);
}
"#,
        // Per iteration: 19, 19, 1, 19, 19+1 = 78. x200 = 15600.
        &["15600"],
        "freshtemp_boxed_payload_partial_destructure_owns_every_field",
    );
}

/// B-2026-07-30-11 (discarded-temp leg) — `let _ = <owned temp>;` now
/// registers the discarded temp's cleanup (Drop bodies + heap frees) at
/// the `;` for struct-literal / user-fn-call / tuple / Option-ctor
/// shapes, and the displaced `Some(old)` of a discarded `m.insert`.
/// This test is the DOUBLE-FREE-direction gate over heap `String`
/// fields (body walks read the fields the frees then release, LIFO);
/// the leak direction is pinned by the E2E body-output tests — a
/// regression that drops the registration makes the allocations dead
/// again, and LLVM DCE'd dead allocation chains are invisible to LSan.
#[test]
fn asan_wildcard_let_discard_temps_freed_once() {
    assert_clean_asan_run(
        r#"
struct G { id: i64, s: String }
impl Drop for G {
    fn drop(mut ref self) {
        if self.id < 0i64 { println(self.id); }
    }
}
fn mk(n: i64) -> G {
    G { id: n, s: f"call-{n}" }
}
fn main() {
    let mut it = 0i64;
    while it < 200i64 {
        let _ = G { id: it, s: f"lit-{it}" };
        let _ = mk(it);
        let _ = (G { id: it, s: f"tup-{it}" }, 40i64);
        let _ = Option.Some(G { id: it, s: f"opt-{it}" });
        it = it + 1i64;
    }
    let mut m: Map[i64, G] = Map.new();
    let _ = m.insert(1i64, G { id: 7i64, s: f"seven-{7i64}" });
    let _ = m.insert(1i64, G { id: 8i64, s: f"eight-{8i64}" });
    println("done");
}
"#,
        &["done"],
        "wildcard_let_discard_temps_freed_once",
    );
}

/// B-2026-08-08-25 leg 1, THE CONSUMING HALF — a CONSUMING arm over a live
/// `Option` local now gives the arm its own buffer, so both the arm and the
/// still-live source free exactly one each.
///
/// Every failure direction here is a memory bug, which is why this is
/// pinned under ASAN rather than only as an output test. Skip the clone and
/// the arm frees the source's buffer, so the later read is a use-after-free
/// and the source's scope-exit free is a double free — the pre-fix program
/// was 7 valgrind errors and printed garbage. Clone but let the source stay
/// disarmed and the original leaks every iteration. Clone but let the
/// consuming arm leave the clone's tag intact and the clone is freed twice.
///
/// Case 2 is the element-DEPTH case the row named as the double-free trap:
/// a shallow copy of the `Vec[String]` payload would alias both element
/// buffers, so each would be freed by the arm's clone AND by the source.
///
/// Case 3 is the load-bearing control in the other direction — with the
/// source DEAD the clone must NOT fire, and if it fired without the source
/// being disarmed the original would leak once per iteration. LSan is the
/// gate that would catch a liveness check that quietly said "always live".
///
/// Every payload is read BYTE-WISE (`println` of the string itself, never
/// `.len()`), because a buffer whose bytes are never read is a provably
/// dead allocation LLVM deletes outright — the fixture would then assert
/// nothing at all. The loop's `i` keeps each f-string opaque to the
/// optimizer for the same reason.
#[test]
fn asan_consuming_match_over_a_live_option_local_frees_each_buffer_once() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 4i64 {
        // 1. The consuming mapper, with the source re-read afterwards.
        let o: Option[String] = Some(f"hi{i}");
        match o.map(|s| s) { Some(v) => { println(v); } None => { println("-"); } }
        match o { Some(v) => { println(v); } None => { println("-"); } }
        // 2. Element depth: a shallow clone would alias both element buffers.
        let w: Option[Vec[String]] = Some([f"aa{i}", f"bb{i}"]);
        match w.map(|s| s) { Some(v) => { println(v[0]); } None => { println("-"); } }
        match w { Some(v) => { println(v[1]); } None => { println("-"); } }
        // 3. CONTROL — source DEAD, so no clone may be made. A clone here
        // without disarming the source would leak the original every pass.
        let d: Option[String] = Some(f"dead{i}");
        match d.map(|s| s) { Some(v) => { println(v); } None => { println("-"); } }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "hi0", "hi0", "aa0", "bb0", "dead0", "hi1", "hi1", "aa1", "bb1", "dead1", "hi2", "hi2",
            "aa2", "bb2", "dead2", "hi3", "hi3", "aa3", "bb3", "dead3", "end",
        ],
        "consuming_match_live_option_local",
    );
}

/// B-2026-08-30-8 under ASAN — a returned `Option` local that a READ-ONLY
/// `match` arm borrowed from must be freed exactly once, and the veto
/// override that achieves it must not turn a chain into a leak.
///
/// All three failure directions are memory bugs invisible to an output test,
/// which is why this is pinned here as well. Leave the source armed and the
/// callee frees the buffer on the way out while the caller frees it again —
/// pre-fix every `acc`/`plain` line below was a use-after-free read followed
/// by `free(): double free detected in tcache 2`. Disarm too eagerly and the
/// `chain` case leaks its buffer once per pass, because
/// `<src>.map(f).unwrap_or(d)` consumes a value the read-only arm left the
/// SOURCE owning (B-2026-08-08-25 leg 1) — the source is never handed out,
/// so zeroing its slot orphans the allocation. LSan on Linux CI is the only
/// gate that sees that half; a local macOS asan run does not run LSan at all.
///
/// `let _ = plain(..)` is the third direction: the caller DISCARDS the
/// returned option, so exactly one free must still happen. A fix that simply
/// dropped the callee's free without the caller taking ownership would leak
/// here and nowhere else.
///
/// `acc` is `Parser.collect_leading_doc_comments` condensed — accumulate
/// into an `Option[String]` across a loop, then return it — which is the
/// shape `selfhost_parser_matches_rust_parser_items` was dying on. Every
/// payload is read BYTE-WISE and the loop counter keeps each f-string opaque
/// to the optimizer, so no buffer here is a provably dead allocation LLVM
/// can delete out from under the assertion.
#[test]
fn asan_returned_option_local_borrowed_by_a_readonly_arm_is_freed_once() {
    assert_clean_asan_run(
        r#"
fn acc(n: i64) -> Option[String] {
    let mut buf: Option[String] = None;
    let mut i: i64 = 0i64;
    while i < n {
        let line = f"L{i}";
        match buf {
            Some(prev) => { let mut j = prev; j.push_str("-"); j.push_str(line); buf = Some(j); }
            None => { buf = Some(line); }
        }
        i = i + 1;
    }
    buf
}
fn plain(k: i64) -> Option[String] {
    let mut buf: Option[String] = Some(f"a{k}");
    match buf { Some(prev) => { println(prev); } None => {} }
    buf
}
fn chain(k: i64) -> i64 {
    let o: Option[String] = Some(f"hi{k}");
    match o { Some(s) => { println(s); } None => {} }
    o.map(|x| x.len()).unwrap_or(0i64)
}
fn main() {
    let mut i: i64 = 0i64;
    while i < 4i64 {
        match acc(3i64) { Some(s) => { println(s); } None => { println("-"); } }
        match plain(i) { Some(s) => { println(s); } None => { println("-"); } }
        let _ = plain(100i64 + i);
        println(f"{chain(i)}");
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "L0-L1-L2", "a0", "a0", "a100", "hi0", "3", "L0-L1-L2", "a1", "a1", "a101", "hi1", "3",
            "L0-L1-L2", "a2", "a2", "a102", "hi2", "3", "L0-L1-L2", "a3", "a3", "a103", "hi3", "3",
            "end",
        ],
        "returned_option_local_readonly_arm",
    );
}

/// B-2026-08-08-25 LEGS 2 AND 3 under ASAN — the `Result` and user-ENUM
/// channels, whose live-local clones must free exactly one buffer each.
///
/// Same three failure directions as leg 1's fixture, which is why these are
/// pinned here rather than only as output tests: no clone and the arm frees
/// the source's buffer (use-after-free on the later read, double free at
/// scope exit); a clone whose source stays disarmed leaks the original once
/// per iteration; a clone whose consuming arm leaves the payload area
/// intact is freed twice.
///
/// Cases 1 and 2 are the two halves of leg 2 (`Result`) — consuming and
/// read-only. Case 2 carries the SCALAR `Err` co-arm that was the actual
/// defect: it disqualified the caller-retains classifier for the whole
/// match, sending the `Ok` arm back to the transfer path.
///
/// Cases 3 and 4 are the two halves of leg 3 (user enum), and case 5 is the
/// liveness control in the other direction — with the source DEAD the clone
/// must NOT fire; if it fired without disarming the source, the original
/// would leak every pass, which is exactly what LSan catches and a local
/// macOS ASAN run would not.
///
/// Every payload is read BYTE-WISE (`println` of the string itself, never
/// `.len()`), because a buffer whose bytes are never read is a provably
/// dead allocation LLVM deletes outright — the fixture would then assert
/// nothing at all. The loop's `i` keeps each f-string opaque to the
/// optimizer for the same reason.
#[test]
fn asan_live_local_match_over_result_and_user_enum_frees_each_buffer_once() {
    assert_clean_asan_run(
        r#"
enum E { A(String), B }
fn main() {
    let mut i: i64 = 0i64;
    while i < 4i64 {
        // 1. LEG 2, consuming — `Result.map`, source re-read afterwards.
        let r: Result[String, i64] = Ok(f"hi{i}");
        match r.map(|s| s) { Ok(v) => { println(v); } Err(_) => { println("-"); } }
        match r { Ok(v) => { println(v); } Err(_) => { println("-"); } }
        // 2. LEG 2, read-only — the SCALAR `Err` co-arm that broke the gate.
        let q: Result[String, i64] = Ok(f"ro{i}");
        match q { Ok(v) => { println(v); } Err(e) => { println("-"); } }
        match q { Ok(v) => { println(v); } Err(e) => { println("-"); } }
        // 3. LEG 3, read-only — the user-enum channel.
        let e: E = E.A(f"en{i}");
        match e { E.A(v) => { println(v); } E.B => { println("-"); } }
        match e { E.A(v) => { println(v); } E.B => { println("-"); } }
        // 4. LEG 3, consuming — the arm MOVES the payload out.
        let m: E = E.A(f"mv{i}");
        match m { E.A(v) => { let k: String = v; println(k); } E.B => { println("-"); } }
        match m { E.A(v) => { println(v); } E.B => { println("-"); } }
        // 5. CONTROL — source DEAD, so no clone may be made. A clone here
        // without disarming the source would leak the original every pass.
        let d: E = E.A(f"dead{i}");
        match d { E.A(v) => { let k: String = v; println(k); } E.B => { println("-"); } }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "hi0", "hi0", "ro0", "ro0", "en0", "en0", "mv0", "mv0", "dead0", "hi1", "hi1", "ro1",
            "ro1", "en1", "en1", "mv1", "mv1", "dead1", "hi2", "hi2", "ro2", "ro2", "en2", "en2",
            "mv2", "mv2", "dead2", "hi3", "hi3", "ro3", "ro3", "en3", "en3", "mv3", "mv3", "dead3",
            "end",
        ],
        "live_local_match_result_and_user_enum",
    );
}

/// B-2026-08-09-14 — the DEAD-source twin of the `while let` case above,
/// which runs on the transfer path rather than the clone leg.
///
/// A consuming arm moved the payload into the binding without zeroing the
/// SOURCE's cap, so the source's `__karac_drop_<E>` re-freed a buffer the
/// binding had already freed. ASAN reports it as a double free; the
/// opposite mistake — suppressing on a path that does not own the payload
/// — is an LSan-only leak, which is why the read-only and miss-edge
/// controls below are here rather than only in the output-level pin.
///
/// Case 3 is the one a single-iteration test cannot reach: the source is
/// re-populated with a fresh payload each pass, so the fixture asserts the
/// suppression re-arms per iteration instead of permanently disarming the
/// slot. Case 4 never matches at all, so the miss edge must free the
/// source whole — the shape that breaks if the cap-zeroing is hoisted out
/// of the matched path.
///
/// Payloads are read BYTE-WISE for the reason the sibling fixtures
/// document: an unread buffer is a dead allocation LLVM deletes.
#[test]
fn asan_consuming_while_let_over_dead_enum_local_frees_each_buffer_once() {
    assert_clean_asan_run(
        r#"
enum E { A(String), B }
fn main() {
    let mut i: i64 = 0i64;
    while i < 4i64 {
        // 1. The row's own shape — consuming arm, source dead, reassigned.
        let mut a: E = E.A(f"re{i}");
        while let E.A(v) = a { let k: String = v; println(k); a = E.B; }
        // 2. Same, terminating via `break` with no reassignment at all.
        let mut b: E = E.A(f"br{i}");
        let mut n: i64 = 0i64;
        while let E.A(v) = b { let k: String = v; println(k); n = n + 1i64; if n > 0i64 { break } }
        // 3. MULTI-ITERATION — a fresh payload each pass, so the suppression
        //    must re-arm rather than disarm the slot once.
        let mut c: E = E.A(f"m{i}-1");
        let mut j: i64 = 0i64;
        while let E.A(v) = c {
            let k: String = v; println(k); j = j + 1i64;
            if j < 3i64 { c = E.A(f"m{i}-{j + 1i64}"); } else { c = E.B; }
        }
        // 4. CONTROL — never matches, so the miss edge frees the source whole.
        let d: E = E.A(f"miss{i}");
        while let E.B = d { println("unreachable"); }
        // 5. CONTROL — read-only arm over a dead source: the source still owns
        //    the payload, and suppressing here would leak it.
        let mut e: E = E.A(f"ro{i}");
        while let E.A(v) = e { println(v); e = E.B; }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "re0", "br0", "m0-1", "m0-2", "m0-3", "ro0", "re1", "br1", "m1-1", "m1-2", "m1-3",
            "ro1", "re2", "br2", "m2-1", "m2-2", "m2-3", "ro2", "re3", "br3", "m3-1", "m3-2",
            "m3-3", "ro3", "end",
        ],
        "consuming_while_let_dead_enum_local",
    );
}

/// B-2026-08-29-20 — the MEMORY side of widening the wildcard-let discard
/// gate to admit a `match`.
///
/// Before the fix `let _ = match .. ;` registered no cleanup at all, so a
/// HEAP-carrying arm value was simply leaked; now the gate routes it
/// through the same discard battery the bare-statement spelling uses. That
/// battery frees as well as running bodies, so the widening is the kind of
/// change that turns a leak into a double free if the value is not really
/// the discard site's to own — which is why this pins both directions on a
/// String-carrying payload rather than trusting the body count alone.
///
/// Case 2 is the boundary that keeps the widening honest: the arm hands
/// out an ENCLOSING LOCAL, so exactly one owner may free it — the discard
/// site, since the value moved there. This cell pinned `end` alone while
/// the discarded-`let` gate declined an identifier tail (a missing body,
/// not a leak: LSan was already clean). B-2026-08-29-31 closed that, so
/// the body now runs here, and the four spellings of the same discard —
/// bare `if`, `let _ = if`, bare `match`, `let _ = match` — all print
/// `dR41 forty-one` on all four surfaces (measured). ASAN is what proves
/// the newly-running body is a single free rather than a second one: the
/// `discarded_if_enclosing_local` case below is its bare-statement twin.
///
/// `name` is read byte-wise in the body for this file's usual reason.
#[test]
fn asan_discarded_let_wildcard_match_frees_once() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id} {self.name}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"heap-{i}" }; }
fn main() {
    let n = 1;
    let _ = match n { 1 => { R { id: 7, name: f"lit-seven" } } _ => { mk(0) } };
    let _ = match n { 1 => mk(3), _ => mk(0) };
    println("dropped");
}
"#,
        &["dR7 lit-seven", "dR3 heap-3", "dropped"],
        "discarded_let_wildcard_match",
    );
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id} {self.name}") } }
fn main() {
    let r = R { id: 41, name: f"forty-one" };
    let n = 0;
    let _ = match n { 0 => r, _ => R { id: 9, name: f"nine" } };
    println("end");
}
"#,
        &["dR41 forty-one", "end"],
        "discarded_let_wildcard_match_enclosing_local",
    );
}

/// THE SHAPE THAT KEEPS B-2026-08-29-32'S GUARD, and the regression its
/// narrowing (B-2026-08-31-44) is most able to cause. A FIELD projected
/// off a local declared OUTSIDE a loop is moved once per iteration, but
/// the move retraction is static and fires once — so admitting it frees
/// the same pointer five times. Measured `double-free` the moment the
/// guard is dropped wholesale, against `clean` here. No statement-count
/// matrix reaches this: the stacked form the guard was originally tuned on
/// is clean either way.
///
/// QUARANTINED AT `-O0`, and its own test rather than a case inside
/// `asan_discarded_branch_literal_over_a_binding_frees_once`, because the
/// two assert opposite things about the same program (B-2026-09-01-10).
/// The declined projection STRANDS 38 B by design — B-2026-09-01-5 owns
/// that leak and states why it cannot be closed at the discard site — so
/// this program can never be ASAN-clean at `-O0`, where the allocation
/// actually happens. At the default `-O2` it passes: the stranded buffer
/// is dead, so LLVM deletes the whole chain and there is nothing to
/// report. What it gates at BOTH levels is that the decline is a DECLINE
/// and not a corruption — no double free, no use-after-free — which is
/// the direction that matters while the guard stands. Splitting it out
/// keeps the other two cells of that fixture live on the `-O0` leg
/// instead of quarantining them along with this one.
/// B-2026-09-07-43 — a discarded `if`/`else` whose ARMS include an RC-boxed
/// field projection registers an owner, on every path.
///
/// THE DECLINE WAS PER-CONSTRUCT, NOT PER-PATH, and that is what these
/// cells pin. `try_track_discarded_user_drop_temp` is the only registrar a
/// discarded `if` reaches, and `discard_branch_tail_aliases_a_temp` gates
/// it by OR-ing `field_init_aliases_a_temp` across EVERY arm — so one
/// projecting arm declined the whole construct and whichever arm actually
/// RAN lost its heap. Cell (a) is the proof: its projecting arm is never
/// taken, and what leaked was the OTHER arm's freshly minted buffer.
///
/// WHY (a) IS THE LOAD-BEARING CELL. The two fixtures this row was filed
/// against both take the PROJECTING arm, where the stranded buffer is the
/// copy of `t.a` — a dead allocation at `-O2`, which LLVM elides. That is
/// the only reason this family read as `-O0`-only and sat on the
/// quarantine lists. Take the MINTING arm instead and the same defect
/// strands a live `payload2()` buffer that no optimizer can remove:
/// measured on the parent at 355 B in 5 blocks at BOTH opt levels, so this
/// cell is red under the plain `--features llvm` leg CI already runs, not
/// only under `scripts/asan-o0-leg.sh`.
///
/// The two arm strings are deliberately DIFFERENT LENGTHS (38 B vs 71 B).
/// That is what attributed the leak: 355 = 5 x 71 names the mint, and
/// 190 = 5 x 38 names the copy. With equal-length payloads both readings
/// are 190 B and the fixture cannot say which buffer it lost — which is
/// how the row came to describe this as a projecting arm poisoning its
/// SIBLING, when in fact the construct is unowned as a whole.
///
/// Cell (c) pins that arm ORDER is irrelevant (the projecting arm as
/// `else` measures identically), and cell (d) that BOTH arms projecting is
/// the same defect rather than a separate one.
#[test]
fn asan_discarded_branch_projecting_arm_still_owns_every_path() {
    // (a) The projecting arm is NOT taken — the minting sibling's buffer is
    // what strands, at both opt levels. 355 B / 5 pre-fix.
    assert_clean_asan_run_min_allocs(
        r#"
struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn payload2() -> String { f"payload2-{seed()}-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
fn go() -> i64 {
    let t = mkp(9);
    let mut i = 0i64;
    while i < 5i64 {
        if seed() > 99i64 { P { a: t.a, b: 1 } } else { P { a: payload2(), b: 2 } };
        i = i + 1;
    }
    return t.b;
}
fn main() { println(go()); }
"#,
        &["9"],
        "b0907-43-projecting-arm-not-taken",
        10,
    );
    // (c) Arm ORDER is irrelevant: the projecting arm as `else`, the
    // minting arm taken. Measures identically pre-fix (355 B / 5, both
    // levels), which is what rules out a first-arm-decides resolution.
    assert_clean_asan_run_min_allocs(
        r#"
struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn payload2() -> String { f"payload2-{seed()}-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
fn go() -> i64 {
    let t = mkp(9);
    let mut i = 0i64;
    while i < 5i64 {
        if seed() > 0i64 { P { a: payload2(), b: 2 } } else { P { a: t.a, b: 1 } };
        i = i + 1;
    }
    return t.b;
}
fn main() { println(go()); }
"#,
        &["9"],
        "b0907-43-projecting-arm-as-else",
        10,
    );
    // (d) BOTH arms project — the same defect, not a separate one. 190 B / 5
    // at `-O0` (the copy is the only heap here, so `-O2` elides it).
    assert_clean_asan_run_min_allocs(
        r#"
struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
fn go() -> i64 {
    let t = mkp(9);
    let mut i = 0i64;
    while i < 5i64 {
        if seed() > 0i64 { P { a: t.a, b: 1 } } else { P { a: t.a, b: 2 } };
        i = i + 1;
    }
    return t.b;
}
fn main() { println(go()); }
"#,
        &["9"],
        "b0907-43-both-arms-project",
        10,
    );
}

/// B-2026-09-01-11 — the MEMORY half of the discarded branch whose FIRST
/// arm is an enum construction.
///
/// `try_track_discarded_user_drop_temp` names a branch's type off its FIRST
/// arm, and its resolver understood a struct literal and a fn call but not
/// a variant construction — so `let _ = if c { E.A(mk(8)) } else { mke(9) };`
/// registered NOTHING, while the mirror-image `if c { mke(8) } else { E.A(mk(9)) }`
/// named its type off the call in first position and was owned all along.
///
/// The `guard:` cells are the point of putting this here at all: the
/// widening must not reach a value that already has an owner. The all-ctor
/// branch belongs to the representative-tail redirect, the mirror order to
/// the resolver's existing call arm, and a bound `let` to its binding —
/// registering a second owner over any of those is a double free, not a
/// leak.
#[test]
fn asan_a_discarded_branch_with_a_ctor_first_arm_is_owned_once() {
    // The `Drop` bodies are SILENT on purpose: the transcript (which body
    // ran, how many times) is pinned by the codegen E2E twin, and this
    // fixture asks only the memory question. Both impls must exist, since
    // an own-`Drop` type is what arms the registration under test.
    const H: &str = "struct R { id: i64, s: String }\n\
             impl Drop for R { fn drop(mut ref self) { } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { } }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { return f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\"; }\n\
             fn mk(i: i64) -> R { return R { id: i, s: payload() }; }\n\
             fn mke(i: i64) -> E { return E.A(mk(i)); }\n\
             fn main() { println(go()); }\n";
    let rows: &[(&str, &str)] = &[
        (
            "ctor FIRST, call second — the row's shape",
            "fn go() -> i64 { let c = seed() > 0;\n\
                 \x20  let _ = if c { E.A(mk(8)) } else { mke(9) };\n\
                 \x20  1 }\n",
        ),
        (
            "ctor FIRST, the OTHER arm taken",
            "fn go() -> i64 { let c = seed() > 99;\n\
                 \x20  let _ = if c { E.A(mk(8)) } else { mke(9) };\n\
                 \x20  1 }\n",
        ),
        (
            "ctor FIRST, bare-statement spelling",
            "fn go() -> i64 { let c = seed() > 0;\n\
                 \x20  if c { E.A(mk(8)) } else { mke(9) };\n\
                 \x20  1 }\n",
        ),
        (
            "ctor FIRST, `match` spelling",
            "fn go() -> i64 { let c = seed() > 0;\n\
                 \x20  let _ = match c { true => E.A(mk(8)), _ => mke(9) };\n\
                 \x20  1 }\n",
        ),
        (
            "a UNIT variant first, call second",
            "fn go() -> i64 { let c = seed() > 0;\n\
                 \x20  let _ = if c { E.B } else { mke(9) };\n\
                 \x20  1 }\n",
        ),
        // ── guards: values that already had exactly one owner ──
        (
            "guard: the MIRROR order, owned by the resolver's call arm",
            "fn go() -> i64 { let c = seed() > 0;\n\
                 \x20  let _ = if c { mke(8) } else { E.A(mk(9)) };\n\
                 \x20  1 }\n",
        ),
        (
            "guard: the mirror order, `match` spelling",
            "fn go() -> i64 { let c = seed() > 99;\n\
                 \x20  let _ = match c { true => mke(8), _ => E.A(mk(9)) };\n\
                 \x20  1 }\n",
        ),
        (
            "guard: ALL arms are ctors — owned by the redirect",
            "fn go() -> i64 { let c = seed() > 0;\n\
                 \x20  let _ = if c { E.A(mk(8)) } else { E.A(mk(9)) };\n\
                 \x20  1 }\n",
        ),
        (
            "guard: ALL arms are calls",
            "fn go() -> i64 { let c = seed() > 0;\n\
                 \x20  let _ = if c { mke(8) } else { mke(9) };\n\
                 \x20  1 }\n",
        ),
        (
            "guard: the direct call spelling",
            "fn go() -> i64 { let _ = mke(8);\n\
                 \x20  1 }\n",
        ),
        (
            "guard: the BOUND `let`, whose binding owns it",
            "fn go() -> i64 { let c = seed() > 0;\n\
                 \x20  let w = if c { E.A(mk(8)) } else { mke(9) };\n\
                 \x20  match w { E.A(r) => r.id - 7, _ => 1 } }\n",
        ),
    ];
    for (label, body) in rows {
        assert_clean_asan_run(&format!("{H}{body}"), &["1"], label);
    }
}

/// B-2026-09-01-10 (leg 1) — a BLOCK-BODIED arm of a DISCARDED `match`
/// stranded whatever its tail named.
///
/// `compile_if` has told each arm whether the construct's value has a
/// consumer since B-2026-08-29-5; `compile_match` never did, so a braced
/// arm compiled through `compile_block_with_frame` as though one existed
/// and ran `suppress_block_tail_cleanup` — a HANDOVER — with nothing on
/// the receiving end. The BARE spelling of the same arm was already
/// correct, because `compile_match`'s own arm-tail hook gates that
/// suppression on `owns_result`; only the braces lost the gate, which is
/// what identifies the STATEMENT KIND of the arm body as the unit rather
/// than the match.
///
/// Measured at `KARAC_OPT_LEVEL=0`, 38 B per evaluation for each leaking
/// cell and 0 after. Also a `-O0`-only gate in the leak direction, for the
/// reason its `if` sibling records: at `-O2` the discarded allocation is
/// dead and LLVM deletes the chain. What it gates at BOTH levels is the
/// opposite direction — the arm-level owner this fix arms must not free
/// something the statement site or the source binding still owns — which
/// is what the `guard:` cells are for.
#[test]
fn asan_discarded_match_braced_arm_owns_what_its_tail_names() {
    const H: &str = "struct R { id: i64, s: String }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mk(i: i64) -> R { return R { id: i, s: payload() }; }\n\
             fn main() { println(go()); }\n";
    let rows: &[(&str, &str)] = &[
        // The payload binding handed out of a braced arm — the cell that
        // failed the `-O0` leg on `main`, in both statement spellings.
        (
            "braced-arm-hands-out-a-payload-binding",
            "fn go() -> i64 { let o: Option[R] = Some(mk(1));\n\
                 \x20  let _ = match o { Some(r) => { r } None => { mk(9) } };\n\
                 \x20  1 }\n",
        ),
        (
            "braced-arm-hands-out-a-payload-binding-bare-statement",
            "fn go() -> i64 { let o: Option[R] = Some(mk(1));\n\
                 \x20  match o { Some(r) => { r } None => { mk(9) } };\n\
                 \x20  1 }\n",
        ),
        // An ENCLOSING LOCAL rather than a payload binding: same handover,
        // different source. The unbraced spelling of this one is a control
        // below and was clean throughout.
        (
            "braced-arm-hands-out-an-enclosing-local",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let r = mk(41);\n\
                 \x20  let _ = match n { 9 => { r } _ => { mk(9) } };\n\
                 \x20  1 }\n",
        ),
        // A block with a statement before the tail — the tail is still the
        // value, so the same handover applies.
        (
            "braced-arm-with-a-leading-statement",
            "fn go() -> i64 { let o: Option[R] = Some(mk(1));\n\
                 \x20  let _ = match o { Some(r) => { let z = 1; r } None => { mk(9) } };\n\
                 \x20  1 }\n",
        ),
        // The MINTED arm of a declined discarded match, taken. The braced
        // spelling was already owned; the BARE one had no owner anywhere,
        // which is the second half this row fixes.
        (
            "bare-arm-mints-on-the-taken-path",
            "fn go() -> i64 { let o: Option[R] = None;\n\
                 \x20  let _ = match o { Some(r) => r, None => mk(9) };\n\
                 \x20  1 }\n",
        ),
        (
            "braced-arm-mints-on-the-taken-path",
            "fn go() -> i64 { let o: Option[R] = None;\n\
                 \x20  let _ = match o { Some(r) => { r } None => { mk(9) } };\n\
                 \x20  1 }\n",
        ),
        // ── double-own guards: a second owner here is a DOUBLE FREE ──
        //
        // Every arm QUALIFIES in these, so the discard statement frame
        // owns the phi and no arm may. They are what keeps the new signal
        // from being "always discarded".
        (
            "guard: all-mint braced match stays statement-owned",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = match n { 1 => { mk(7) } _ => { mk(3) } };\n\
                 \x20  1 }\n",
        ),
        (
            "guard: all-mint bare match stays statement-owned",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = match n { 1 => mk(7), _ => mk(3) };\n\
                 \x20  1 }\n",
        ),
        (
            "guard: block-wrapped all-mint match stays statement-owned",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let _ = { match n { 1 => { mk(7) } _ => { mk(3) } } };\n\
                 \x20  1 }\n",
        ),
        // A CONSUMED match is not discarded at all — the binding owns the
        // value and no arm may register a second owner.
        (
            "guard: bound braced match is owned by its binding",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let r = mk(41);\n\
                 \x20  let q = match n { 9 => { r } _ => { mk(9) } };\n\
                 \x20  q.id - q.id + 1 }\n",
        ),
        // ── controls, clean before and after ─────────────────────────
        (
            "control: bare arm hands out a payload binding",
            "fn go() -> i64 { let o: Option[R] = Some(mk(1));\n\
                 \x20  let _ = match o { Some(r) => r, None => mk(9) };\n\
                 \x20  1 }\n",
        ),
        (
            "control: bare arm hands out an enclosing local",
            "fn go() -> i64 { let n = seed();\n\
                 \x20  let r = mk(41);\n\
                 \x20  let _ = match n { 9 => r, _ => mk(9) };\n\
                 \x20  1 }\n",
        ),
    ];
    for (label, body) in rows {
        assert_clean_asan_run(&format!("{H}{body}"), &["1"], label);
    }
}

/// B-2026-09-01-10 (leg 2) — a BOXED struct payload destructured by
/// `if let` / `while let` / `let … else` with a body that only READS the
/// bound fields lost them entirely.
///
/// The three `let`-family constructs ran
/// `suppress_boxed_payload_struct_destructure` unconditionally, on a
/// comment claiming it was "self-gated on `boxed_enum_payload_vars`
/// membership — only a binding OWNED here is registered, so borrow
/// scrutinees no-op". That table is a property of the SCRUTINEE VARIABLE,
/// not of the binding mode. When the body only reads, the scrutinee is
/// classified `pattern_binding_is_borrow` (via
/// `scrutinee_is_readonly_owned_enum_local`), the bindings ALIAS the box
/// and `bind_pattern_values` registers no cleanup for them — so disarming
/// the box left every bound field with no owner at all.
///
/// The `match` spelling has gated this on `!pattern_binding_is_borrow`
/// since it was written, which is what makes the asymmetry the axis:
/// measured 141 B / 3 blocks for the `if let` over three iterations,
/// against a clean `match` on the identical value, and clean again the
/// moment the body MOVES a field instead of reading it. That last control
/// is in here as the DOUBLE-FREE direction: with the bind owned, the
/// disarm must still fire or the binding and the box both free the buffer.
#[test]
fn asan_iflet_boxed_struct_destructure_readonly_body_frees_once() {
    // `Holder` is 4 words (String + i64) against `Option`'s 3-word inline
    // area, so its payload is BOXED — which is what routes it through the
    // suppressor at issue. A narrower payload stays inline and never
    // reaches it.
    const H: &str = r#"struct Holder { name: String, id: i64 }
fn mkh(i: i64) -> Holder { return Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i }; }
fn slen(s: String) -> i64 { if s.contains("holder") { s.len() } else { 0 } }
"#;
    for (label, body) in [
        (
            "iflet_readonly_body",
            r#"fn main() {
    let mut i = 0;
    while i < 3 {
        let o: Option[Holder] = Some(mkh(i));
        if let Some(Holder { name, id }) = o { println(name.len() + id); }
        i = i + 1;
    };
}"#,
        ),
        (
            "iflet_readonly_body_with_else",
            r#"fn main() {
    let mut i = 0;
    while i < 3 {
        let o: Option[Holder] = Some(mkh(i));
        if let Some(Holder { name, id }) = o { println(name.len() + id); } else { println("none"); }
        i = i + 1;
    };
}"#,
        ),
        (
            "iflet_readonly_body_rest_pattern",
            r#"fn main() {
    let mut i = 0;
    while i < 3 {
        let o: Option[Holder] = Some(mkh(i));
        if let Some(Holder { name, .. }) = o { println(name.len() + i); }
        i = i + 1;
    };
}"#,
        ),
        (
            "letelse_readonly_body",
            r#"fn main() {
    let mut i = 0;
    while i < 3 {
        let o: Option[Holder] = Some(mkh(i));
        let Some(Holder { name, id }) = o else { return; };
        println(name.len() + id);
        i = i + 1;
    };
}"#,
        ),
        // THE DOUBLE-FREE DIRECTION. With the body MOVING a field the bind
        // is owned, the binding registers its own cleanup, and the disarm
        // must still fire — clean before this row and after.
        (
            "control_iflet_moving_body_still_disarms",
            r#"fn main() {
    let mut i = 0;
    while i < 3 {
        let o: Option[Holder] = Some(mkh(i));
        if let Some(Holder { name, id }) = o { println(slen(name) + id); }
        i = i + 1;
    };
}"#,
        ),
        // The `match` spelling, which was correct throughout — in the same
        // fixture so the two can never answer differently again.
        (
            "control_match_readonly_body",
            r#"fn main() {
    let mut i = 0;
    while i < 3 {
        let o: Option[Holder] = Some(mkh(i));
        match o {
            Some(Holder { name, id }) => { println(name.len() + id); },
            None => { println("missing"); },
        }
        i = i + 1;
    };
}"#,
        ),
    ] {
        assert_clean_asan_run(&format!("{H}{body}"), &["47", "48", "49"], label);
    }
}

/// B-2026-08-11-30. A by-value `Result` parameter that the callee never
/// DESTRUCTURES is dropped by nobody, so the whole `Ok` payload leaks. The
/// caller retracts its own cleanup on the pass-as-arg move
/// (`suppress_inline_result_payload_cleanup_for_moved_arg`,
/// `control_flow_match.rs`) while `make_aggregate_param_callee_owned_inst`
/// bails on `Option`/`Result` (`param_own.rs`), so neither frame owns it.
///
/// The ARGUMENT FORM is irrelevant — the `let`-bound argument here leaks
/// exactly as `take(mk())` does — and so is the payload field type. The row
/// originally read this as Vec-specific, with `String` fields "freed
/// correctly"; that was an `-O2` artifact (LLVM deletes the dead String
/// allocation, so "no leak" meant "nothing was allocated"). At
/// `KARAC_OPT_LEVEL=0` a `String` payload leaks identically.
///
/// Uses a `Vec` payload because that one survives `-O2`, which is what this
/// harness builds.
///
/// FIXED: the callee now OWNS such a param by transfer (functions.rs), in
/// lockstep with the caller's zeroing, so it frees the payload the caller
/// already gave up.
#[test]
fn asan_result_param_never_destructured_leaks_payload() {
    assert_clean_asan_run(
        r#"
enum E { Missing(String) }
fn mkv() -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push("x"); return v; }
fn mk() -> Result[Vec[String], E] { return Ok(mkv()); }
fn take(r: Result[Vec[String], E]) { println("got"); }
fn main() { let r = mk(); take(r); println("end"); }
"#,
        &["got", "end"],
        "result_param_never_destructured",
    );
}

/// B-2026-08-11-30, `Option` twin. Same bail, same leak — the two built-in
/// enums share the `param_own.rs` exclusion. A user enum in this exact
/// shape is CLEAN (it takes the callee-owned entry-copy path), which is
/// what localises the defect to `Option`/`Result` rather than to enums.
#[test]
fn asan_option_param_never_destructured_leaks_payload() {
    assert_clean_asan_run(
        r#"
fn mkv() -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push("x"); return v; }
fn take(o: Option[Vec[String]]) { println("got"); }
fn main() { take(Some(mkv())); println("end"); }
"#,
        &["got", "end"],
        "option_param_never_destructured",
    );
}

#[test]
fn asan_if_arm_owned_producer_alternative_still_freed() {
    // B-2026-08-14-32's other direction, and the reason the clone belongs
    // in the ARM rather than at the if-expression's merge. By the merge the
    // value is a phi: cloning THAT would also clone the arms that produced
    // a fresh owned temporary, leaking every original. Only the arm knows
    // what it yielded.
    //
    // So this drives the arms that must NOT be cloned — a call whose return
    // is owned, a collection literal, a bare local move — through the same
    // `if` / `match` positions, taking BOTH sides of each branch across the
    // loop so neither arm goes unexercised.
    //
    // Stated plainly, because a test that cannot fail is worse than no
    // test: this one does NOT fail under either ablation of the fix it ships
    // with. Removing the arm clone leaves it green, and removing the
    // discard gate leaves it green — the two siblings cover those. It is a
    // FORWARD guard on the merge-versus-arm decision: `clone_owned_vec_index_element`
    // filters by expression kind, so the day someone widens that filter, or
    // moves the normalization to the phi, these arms start getting cloned
    // with nobody to free the originals and this goes red at 20 buffers per
    // site. Both siblings were verified live by ablation; this one is
    // deliberately kept without that property.
    assert_clean_asan_run(
        r#"
fn mk() -> String { "alphabetalphabet" }
fn mkv() -> Vec[i64] { [5, 6, 7] }
fn main() {
    let mut k = 0;
    while k < 20 {
        let even = k % 2 == 0;
        let a = if even { mk() } else { "gammagammagamma" };
        let b = match even { true => mkv(), false => [8, 9, 10] };
        let local = "deltadeltadeltad";
        let c = if even { local } else { mk() };
        println(f"{a.len()} {b[0]} {c.len()}");
        k = k + 1;
    }
    println("done");
}
"#,
        ["16 5 16", "15 8 16"]
            .iter()
            .cycle()
            .take(20)
            .copied()
            .chain(std::iter::once("done"))
            .collect::<Vec<_>>()
            .as_slice(),
        "asan_if_arm_owned_producer_alternative_still_freed",
    );
}

#[test]
fn asan_discarded_if_arm_element_is_not_cloned() {
    // B-2026-08-14-32's leak edge, and the reason the arm clone is gated on
    // whether anything will own the result.
    //
    // `if c { v[0] } else { v[1] };` computes an element and throws it away.
    // Cloning the arm there — exactly what makes the BOUND form correct —
    // hands the clone to nobody.
    //
    // All FOUR discarding positions the language admits are here, and each
    // one was measured leaking before the gate covering it existed:
    //
    //   statement expression            330 bytes / 20 objects
    //   `for` body trailing expression   \
    //   `while` body trailing expression  > 990 bytes / 60 objects together
    //   block-wrapped (`{ if .. };`)     /
    //
    // The first three are the reason the gate is a SPAN SET and not the
    // one-shot flag it started as: a flag is taken by whichever branch
    // compiles first, which is the discarded one only when it is also the
    // next thing compiled. In a loop body after other statements, or inside
    // a block whose own statements compile first, the wrong branch took it.
    //
    // The `else if` chain and the `match` arm that is itself an `if` are
    // here because discard is INHERITED and each link compiles through its
    // OWN `compile_if`, looking up its OWN span. Recording only the
    // outermost branch leaves every inner link cloning into a leak — found
    // by reading the pass back rather than by a failing test, which is why
    // it is pinned here.
    //
    // The nested `let` inside a discarded arm is the mirror hazard: the
    // suppression must NOT reach it, or that binding stops cloning and the
    // double free comes back. A span set cannot make that mistake; the flag
    // could, and did.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("alphabetalphabet");
    v.push("gammagammagamma");
    v.push("deltadeltadeltad");
    for j in 0..20 { if j % 2 == 0 { v[0] } else { v[1] } }
    let mut i = 0;
    while i < 20 { i = i + 1; { if i % 2 == 0 { v[0] } else { v[1] } }; }
    let mut e = 0;
    while e < 20 {
        if e % 3 == 0 { v[0] } else if e % 3 == 1 { v[1] } else { v[2] };
        match e % 2 == 0 { true => if e > 5 { v[0] } else { v[1] }, false => v[2] };
        e = e + 1;
    }
    let mut k = 0;
    while k < 20 {
        if k % 2 == 0 { v[0] } else { v[1] };
        match k % 2 == 0 { true => v[1], false => v[2] };
        if v.len() > 0 {
            let w = if k % 2 == 0 { v[1] } else { "zetazetazetazeta" };
            println(f"{w}");
            v[0]
        } else { v[2] };
        k = k + 1;
        if k > 0 { v[2] } else { v[0] }
    }
    println(f"{v[0]} {v[1]} {v[2]}");
}
"#,
        ["gammagammagamma", "zetazetazetazeta"]
            .iter()
            .cycle()
            .take(20)
            .copied()
            .chain(std::iter::once(
                "alphabetalphabet gammagammagamma deltadeltadeltad",
            ))
            .collect::<Vec<_>>()
            .as_slice(),
        "asan_discarded_if_arm_element_is_not_cloned",
    );
}

/// B-2026-08-15-9 — a fresh-owned `Vec` temporary passed to a BARE-`T`
/// generic param that shares its type parameter with a returned sibling.
///
/// The row was filed believing the RETURNED param leaked. It does not:
/// `echo[T](x: T) -> T { x }` and the forwarding `nest[T](x: T) -> T {
/// id(id(x)) }` were both single-free already, because the buffer moves out
/// and the caller's result binding frees it exactly once. What leaked was the
/// SIBLING — `b` in `pick[T](a: T, b: T) -> T { id(a) }` — rejected for `a`'s
/// reason, since the gate tests the declared RETURN TYPE against a type-param
/// name both params share. Asymmetric argument sizes are what showed it: a
/// 16-element `b` against an 8-element `a` leaks 128 bytes per call, not 64.
///
/// `takeout` is the double-free direction and belongs here permanently. Its
/// `b` is used only as a `match` scrutinee, which the neighbouring
/// `nonescaping_param_names` calls non-escaping — and swapping that predicate
/// in at the call site turns this exact line into an ASAN double-free abort,
/// because the arm binds the scrutinee and returns it. It passes here by
/// being EXCLUDED from materialization, so if the strict rule is ever
/// loosened this fixture is what catches it.
/// B-2026-09-01-30 — a whole-value MATCH-ARM binding of an owned by-value
/// generic param returns the caller's own buffer.
///
/// `owned_vecstr_params` is what makes a retaining consume site deep-copy,
/// and it holds PARAM names. `match b { v => { return v; } }` returns `v`,
/// so the return-position copy asked about a name not in the set, moved the
/// caller's buffer out, and the caller freed it again. Measured before the
/// fix at `Vec[String]`: `free(): double free detected in tcache 2`, three
/// invalid frees under valgrind — both element buffers and the outer one.
///
/// THE ELEMENT TYPE IS WHY THIS HID. `Vec[i64]` and a plain `String` are
/// both single-free through the same path, so the two shapes a probe
/// reaches for first say nothing; only a `Vec` whose ELEMENTS are heap
/// shows it. All three are here so a future narrowing cannot pass by
/// covering the easy two.
///
/// EVERY PATTERN SPELLING IS HERE, not just the `match` the row named.
/// `if let` and `let ... else` bind a whole value under a new name exactly
/// as a `match` arm does, and each was three invalid frees on its own
/// before the fix. Covering only the spelling that happened to get reported
/// would leave one convention with two answers, which IS the defect.
///
/// `echo` is the control and the whole point: the bare `return x;`
/// spelling of the identical function was always correct, which is what
/// made this a non-uniformity in one convention rather than a missing
/// copy. It must stay single-free too — over-copying it is the dual
/// failure, and the loop is here so that leak accumulates for LSan.
#[test]
fn asan_whole_arm_binding_of_owned_generic_param_no_double_free() {
    assert_clean_asan_run(
        r#"
fn echo[T](x: T) -> T { return x; }
fn takeout[T](b: T) -> T { match b { v => { return v; } } }
fn iflet[T](b: T) -> T { if let w = b { return w; } return b; }
fn letelse[T](b: T) -> T { let w = b else { return b; }; return w; }
fn main() {
    let mut k: i64 = 0;
    let mut t: i64 = 0;
    while k < 3 {
        let vs: Vec[String] = [f"row-{k}-aaaaaaaa", f"row-{k}-bbbbbbbb"];
        let r1: Vec[String] = takeout(vs);
        println(r1[0]);
        let vi: Vec[i64] = [k, k + 1, k + 2];
        let r2: Vec[i64] = takeout(vi);
        t = t + r2[2];
        let s: String = f"str-{k}-cccccccc";
        let r3: String = takeout(s);
        println(r3);
        let es: Vec[String] = [f"ec-{k}-dddddddd", f"ec-{k}-eeeeeeee"];
        let r4: Vec[String] = echo(es);
        println(r4[1]);
        let fs: Vec[String] = [f"il-{k}-ffffffff", f"il-{k}-gggggggg"];
        let r5: Vec[String] = iflet(fs);
        println(r5[0]);
        let gs: Vec[String] = [f"le-{k}-hhhhhhhh", f"le-{k}-iiiiiiii"];
        let r6: Vec[String] = letelse(gs);
        println(r6[1]);
        k = k + 1;
    }
    println(f"{t}");
}
"#,
        &[
            "row-0-aaaaaaaa",
            "str-0-cccccccc",
            "ec-0-eeeeeeee",
            "il-0-ffffffff",
            "le-0-iiiiiiii",
            "row-1-aaaaaaaa",
            "str-1-cccccccc",
            "ec-1-eeeeeeee",
            "il-1-ffffffff",
            "le-1-iiiiiiii",
            "row-2-aaaaaaaa",
            "str-2-cccccccc",
            "ec-2-eeeeeeee",
            "il-2-ffffffff",
            "le-2-iiiiiiii",
            "9",
        ],
        "asan_whole_arm_binding_of_owned_generic_param_no_double_free",
    );
}

/// The `match`-arm sibling on a `shared enum` — a different heap layout
/// reached through a different node of the same recursion.
#[test]
fn asan_loop_break_match_arm_rvalue_single_owner() {
    assert_clean_asan_run_min_allocs(
        "shared enum Tree { Leaf(i64), Node(i64, i64) }\n\
             fn pick() -> Tree {\n\
             \x20   let mut i: i64 = env.args().len() - 1;\n\
             \x20   loop {\n\
             \x20       i = i + 1;\n\
             \x20       let scratch = Tree.Leaf(i);\n\
             \x20       let stop = match scratch { Leaf(a) => a == 2, Node(a, b) => a == b };\n\
             \x20       if stop { break match i { 2 => Tree.Node(i, i * 2), _ => Tree.Leaf(i) } }\n\
             \x20   }\n\
             }\n\
             fn main() { match pick() { Leaf(a) => println(a), Node(a, b) => println(a + b) } }\n",
        &["6"],
        "loop-break-match-arm-rvalue",
        4,
    );
}

/// B-2026-08-28-66 — a consuming `match` arm that hands a BOXED struct
/// payload out double-freed that payload's heap fields.
///
/// `Option[R]` with `R = { i64, String }` is 4 words, wider than the
/// `Option` inline payload area (3), so the payload is heap-BOXED. The
/// box's `BoxedEnumDrop` runs `__karac_drop_struct_R` over the interior,
/// freeing `name`; the arm hands the whole payload to the match's
/// destination, whose own drop frees `name` too. Visible in the IR as
/// `__karac_drop_struct_R(%o_box_ptr)` standing beside `karac_drop_R(%k)`.
///
/// The axis is inline-vs-BOXED, not scalar-vs-heap: an `i64`-only payload
/// fits inline and never takes this path. The heap field is only what makes
/// the doubled free observable — and at `-O2` the optimizer erases the
/// doubled malloc/free pair outright, so the DEFAULT build reports nothing.
/// Measured before the fix: `Invalid free()` with 11 allocs / 12 frees
/// under valgrind at `-O0` and an abort under the JIT (`free(): double free
/// detected in tcache 2`), against 9 allocs / 9 frees clean at `-O2`. That
/// masking is why a body-count matrix missed it — the body count is RIGHT
/// in this shape; only the memory is wrong. Which is also why this test is
/// the row's real gate: `assert_clean_asan_run` is what sees it.
///
/// The second case is the control that fixing this got wrong first. An arm
/// that BINDS the payload and yields something else must leave the box
/// owning the fields; disarming on "the arm does not merely borrow it"
/// leaked them (13 allocs / 12 frees where 13 / 13 is right). A DISCARDED
/// match is the other one — disarming without `branch_value_is_owned`
/// traded the double free for a leak (11 / 10 where 11 / 11 is right); it
/// is exercised by the codegen twin rather than here, because its `Drop`
/// ordering differs between backends for an unrelated reason
/// (B-2026-08-28-69).
///
/// Together those are why the gate is the POSITIVE signal "the arm's value
/// IS this binding, and someone downstream takes it" rather than any
/// consumption test.
///
/// STILL OPEN, deliberately excluded: a FRESH-TEMP scrutinee
/// (`match Some(R { .. }) { Some(r) => { r } .. }`) takes the same double
/// free through a separate staging path — B-2026-08-28-72, the same
/// named/fresh-temp split the destructure sibling already has (slice 3t vs
/// B-2026-08-04-6).
#[test]
fn asan_consuming_arm_boxed_payload_handed_on_is_freed_once() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}"); } }
fn main() {
    let base: i64 = env.args().len();
    // The row's repro: the arm's value IS the bound payload, and `k` takes it.
    let o: Option[R] = Some(R { id: base, name: f"a{base}" });
    let k = match o { Some(r) => { r } None => { R { id: 9, name: f"n9" } } };
    println(f"kept {k.id}");
    // CONTROL — binds the payload but yields something else, so the box must
    // keep owning its fields.
    let q: Option[R] = Some(R { id: 5, name: f"c{base}" });
    let n = match q { Some(r) => { R { id: 7, name: f"n7" } } None => { R { id: 8, name: f"n8" } } };
    println(f"other {n.id}");
}
"#,
        &["kept 1", "dR1", "other 7", "dR7"],
        "consuming-arm-boxed-payload-handed-on",
    );
}

/// B-2026-08-29-6 — a PASSTHROUGH CALL used DIRECTLY AS A MATCH SCRUTINEE,
/// with the result never bound, leaves the payload owned by the named
/// source alone.
///
/// Pre-fix the arm binding registered its own free on top of the source's
/// and both released the same buffer: `free(): double free detected in
/// tcache 2` on all three compiled backends, for a FREE FUNCTION and a
/// method alike, while the interpreter was correct. Not the method-path gap
/// B-2026-08-29-4 closed — that one records an ownership alias at the `let`
/// site, and a scrutinee temporary never passes through it.
///
/// TWO independent halves, which is why both spellings and both payload
/// types are here. The `Option` cases are fixed by the source-retains
/// classification at the match's scrutinee site alone; the `Result` case
/// ALSO needs the fresh-temp inline-`Result` registrar to decline, because
/// that registrar claims a `Result` temp where no registrar claims an
/// `Option` one. Dropping either half puts a `Result` case back to
/// aborting — established by bisecting the shapes individually after a
/// combined fixture still aborted with the first half in place.
///
/// The failure is a SIGNAL, not an output mismatch, so a revert kills the
/// process rather than printing wrong bytes.
#[test]
fn asan_passthrough_match_scrutinee_leaves_the_source_sole_owner() {
    assert_clean_asan_run(
        r#"
struct Bx { n: i64 }
impl Bx {
    fn take(ref self, o: Option[String]) -> Option[String] { o }
    fn take_mut(mut ref self, o: Option[String]) -> Option[String] { self.n = self.n + 1; o }
    fn take_res(ref self, o: Result[String, i64]) -> Result[String, i64] { o }
    fn second(ref self, a: Option[String], b: Option[String]) -> Option[String] { b }
    fn fresh(ref self, o: Option[String]) -> Option[String] { Option.Some(f"fresh-{self.n}") }
}
fn takef(o: Option[String]) -> Option[String] { o }
fn takef_res(o: Result[String, i64]) -> Result[String, i64] { o }
fn main() {
    let base: i64 = env.args().len();
    let b = Bx { n: base };
    // FREE FUNCTION, the spelling that shows this is not a method-path gap.
    let s0 = Option.Some(f"z{base}");
    match takef(s0) { Option.Some(v) => { println(f"got {v}"); } Option.None => { println("none"); } }
    let s0r: Result[String, i64] = Result.Ok(f"y{base}");
    match takef_res(s0r) { Result.Ok(v) => { println(f"got {v}"); } Result.Err(e) => { println(f"err {e}"); } }
    // METHOD spellings.
    let s1 = Option.Some(f"a{base}");
    match b.take(s1) { Option.Some(v) => { println(f"got {v}"); } Option.None => { println("none"); } }
    let mut c = Bx { n: base };
    let s2 = Option.Some(f"b{base}");
    match c.take_mut(s2) { Option.Some(v) => { println(f"got {v}"); } Option.None => { println("none"); } }
    // The `Result` case, which needs the second half of the fix.
    let s3: Result[String, i64] = Result.Ok(f"c{base}");
    match b.take_res(s3) { Result.Ok(v) => { println(f"got {v}"); } Result.Err(e) => { println(f"err {e}"); } }
    let p = Option.Some(f"d{base}");
    let q = Option.Some(f"e{base}");
    match b.second(p, q) { Option.Some(v) => { println(f"got {v}"); } Option.None => { println("none"); } }
    // CONTROL — no passthrough, so the scrutinee temp genuinely owns a freshly
    // produced payload and MUST keep its registration. A fix that declined
    // unconditionally would leak this one instead.
    let s4 = Option.Some(f"h{base}");
    match b.fresh(s4) { Option.Some(v) => { println(f"got {v}"); } Option.None => { println("none"); } }
    // CONTROL — the `let`-bound spelling, correct since B-2026-08-29-4. It must
    // stay correct: both routes now classify the same call, and if they
    // disagreed the payload would end up with two owners again or none.
    let s5 = Option.Some(f"k{base}");
    let o5 = b.take(s5);
    match o5 { Option.Some(v) => { println(f"got {v}"); } Option.None => { println("none"); } }
    println("end");
}
"#,
        &[
            "got z1",
            "got y1",
            "got a1",
            "got b1",
            "got c1",
            "got e1",
            "got fresh-1",
            "got k1",
            "end",
        ],
        "passthrough-match-scrutinee",
    );
}

/// B-2026-08-30-19 — the ASAN half of the read-only-arm borrow fix, and the
/// half that catches the ACTUAL defect. The E2E twin asserts output, which
/// a use-after-free only fails when the freed block happens to have been
/// reused; ASAN fails on the read itself, deterministically.
///
/// Pre-fix this is a genuine invalid read: valgrind reported "Invalid read
/// of size 2 ... 0 bytes inside a block of size 5 free'd" — the `String`
/// buffer of the struct payload, freed by the first arm and read by the
/// second. The program still printed and exited 0, which is why it needed a
/// sanitizer to be seen at all.
///
/// Also covers the other direction: the fix makes these arms BORROWS, so
/// the source keeps its payload and something else must free it. A borrow
/// that dropped the owner would leak instead, and LeakSanitizer on the
/// Linux CI leg fails on that.
///
/// E2E twin: `tests/codegen.rs::e2e_optres_struct_payload_read_only_arm_does_not_take_the_payload`.
#[test]
fn asan_optres_struct_payload_read_only_arm_is_a_borrow() {
    assert_clean_asan_run(
        r#"
struct Sstr { s: String }
struct Svec { v: Vec[i64] }
struct Sint { n: i64 }
struct Inner { s: String }
struct Outer { i: Inner }
shared struct Shr { w: String }
fn main() {
    let a: Option[Sstr] = Some(Sstr { s: f"alpha" });
    match a { Some(p) => { println(p.s); } None => {} }
    match a { Some(q) => println(q.s), None => println("no-a") }

    let b: Option[Svec] = Some(Svec { v: [11, 22, 33] });
    match b { Some(p) => { println(p.v[0]); } None => {} }
    match b { Some(q) => println(q.v[2]), None => println("no-b") }

    let c: Option[Outer] = Some(Outer { i: Inner { s: f"nested" } });
    match c { Some(p) => { println(p.i.s); } None => {} }
    match c { Some(q) => println(q.i.s), None => println("no-c") }

    let d: Result[Sstr, i64] = Ok(Sstr { s: f"okstr" });
    match d { Ok(p) => { println(p.s); } Err(_) => {} }
    match d { Ok(q) => println(q.s), Err(_) => println("no-d") }

    let e: Option[Sint] = Some(Sint { n: 42 });
    match e { Some(p) => { println(p.n); } None => {} }
    match e { Some(q) => println(q.n), None => println("no-e") }

    let g: Option[Shr] = Some(Shr { w: f"shared" });
    match g { Some(p) => { println(p.w); } None => {} }
    match g { Some(q) => println(q.w), None => println("no-g") }
}
"#,
        &[
            "alpha", "alpha", "11", "33", "nested", "nested", "okstr", "okstr", "42", "42",
            "shared", "shared",
        ],
        "optres struct payload read-only arm is a borrow",
    );
}

/// B-2026-08-30-52 under ASAN/LSan — the memory half of the
/// destructuring-arm borrow fix, and the half that actually proves it.
///
/// The output assertion in `e2e_optres_destructuring_read_only_arm_does_
/// not_take_the_payload` only fails a use-after-free when the freed block
/// happens to have been reused between the two reads; ASAN fails on the
/// read itself. Two of these shapes aborted outright before the fix
/// (`double free detected in tcache 2`) and two read back garbage.
///
/// LSan matters here too, in the opposite direction: classifying an arm as
/// a borrow REMOVES an owner, so the way this fix can be wrong is a LEAK
/// rather than a double free, and only the Linux LSan leg sees that.
#[test]
fn asan_optres_destructuring_read_only_arm_is_a_borrow() {
    assert_clean_asan_run(
        r#"
struct S1 { s: String }
struct S2 { s: String, t: String }
struct Inner { s: String }
struct Outer { i: Inner }
struct Sint { n: i64 }
fn main() {
    let a: Option[S1] = Some(S1 { s: f"alpha" });
    match a { Some(S1 { s }) => { println(s); } None => {} }
    match a { Some(S1 { s }) => println(s), None => println("no-a") }

    let b: Option[S2] = Some(S2 { s: f"bee", t: f"tee" });
    match b { Some(S2 { s, t }) => { println(s); } None => {} }
    match b { Some(S2 { s, t }) => println(t), None => println("no-b") }

    let c: Option[(String, i64)] = Some((f"tup", 1));
    match c { Some((s, n)) => { println(s); } None => {} }
    match c { Some((s, n)) => println(s), None => println("no-c") }

    let d: Option[Outer] = Some(Outer { i: Inner { s: f"deep" } });
    match d { Some(Outer { i }) => { println(i.s); } None => {} }
    match d { Some(Outer { i }) => println(i.s), None => println("no-d") }

    let e: Result[S1, i64] = Ok(S1 { s: f"okr" });
    match e { Ok(S1 { s }) => { println(s); } Err(_) => {} }
    match e { Ok(S1 { s }) => println(s), Err(_) => println("no-e") }

    let g: Option[S1] = Some(S1 { s: f"iflet" });
    if let Some(S1 { s }) = g { println(s); }
    if let Some(S1 { s }) = g { println(s); } else { println("no-g"); }

    let h: Option[Sint] = Some(Sint { n: 7 });
    match h { Some(Sint { n }) => { println(n); } None => {} }
    match h { Some(Sint { n }) => println(n), None => println("no-h") }
}
"#,
        &[
            "alpha", "alpha", "bee", "tee", "tup", "tup", "deep", "deep", "okr", "okr", "iflet",
            "iflet", "7", "7",
        ],
        "optres destructuring read-only arm is a borrow",
    );
}

/// B-2026-08-30-52 (b) under ASAN/LSan — a nested `match` over a payload
/// the outer arm bound WHOLE frees the buffer exactly once.
///
/// The E2E twin in `tests/codegen.rs` pins the TEXT, which catches the
/// double free (`Option[E]`, boxed) and the empty second read
/// (`Result[E, i64]`, inline) but says nothing about the other direction:
/// standing the arm down without the source keeping the payload is a LEAK,
/// and that is silent in output.
///
/// TWO THINGS THIS FIXTURE HAD TO GET RIGHT, both found by writing it
/// wrong first and watching it pass against a compiler that still had the
/// bug:
///
///   * THE SCRUTINEE MUST BE A FUNCTION-SCOPE LOCAL. Declaring `a` inside
///     the `while` puts it on a different path — measured pre-fix, the
///     loop-body spelling double frees under the JIT and is CLEAN under
///     AOT, and this harness builds AOT. Hence a helper called in a loop.
///   * THE PAYLOAD'S BYTES MUST BE READ, not just its length. `.len()`-only
///     use makes the buffer a provably dead allocation LLVM deletes
///     outright — the exact trap `assert_clean_asan_run`'s own comment
///     documents, and it made an earlier draft of this fixture green
///     against the reverted fix. `contains` reads bytes; the allocation
///     floor below is what keeps it that way.
#[test]
fn asan_nested_match_over_a_borrowed_payload_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"
enum E { A { s: String }, B }
fn s_of(i: i64) -> String {
    let mut s: String = String.new();
    s.push_str(f"payload-padded-out-well-past-thirty-six-bytes-{i}");
    return s;
}
fn digits(i: i64) -> String { let mut d: String = String.new(); d.push_str(f"{i}"); return d; }
// BOXED channel: `E` is 4 words, past Option's 3-word inline area.
fn boxed(i: i64) -> i64 {
    let mut n = 0;
    let a: Option[E] = Some(E.A { s: s_of(i) });
    match a { Some(p) => { match p { E.A { s } => { if s.contains(digits(i)) { n = n + s.len(); } } E.B => {} } } None => {} }
    match a { Some(p) => { match p { E.A { s } => { if s.contains(digits(i)) { n = n + s.len(); } } E.B => {} } } None => {} }
    return n;
}
// INLINE channel: Result's payload area is 5 words, so the same type fits.
fn inline_if_let(i: i64) -> i64 {
    let mut n = 0;
    let b: Result[E, i64] = Ok(E.A { s: s_of(i) });
    if let Ok(p) = b { match p { E.A { s } => { if s.contains(digits(i)) { n = n + s.len(); } } E.B => {} } }
    if let Ok(p) = b { match p { E.A { s } => { if s.contains(digits(i)) { n = n + s.len(); } } E.B => {} } }
    return n;
}
// CONTROL: the inner arm MOVES the leaf, so the transfer path is correct and
// must stay — standing this one down would leak the buffer instead.
//
// Deliberately UNCONDITIONAL. Wrapping the read in an `if` whose condition
// ALLOCATES (`owned.contains(digits(i))`) double frees on every compiled
// surface, and that is a separate, pre-existing defect on the TRANSFER path
// (B-2026-08-31-23) — not something this fixture's subject can fix, so
// including it here would pin a permanent red.
fn moved(i: i64) -> i64 {
    let a: Option[E] = Some(E.A { s: s_of(i) });
    match a {
        Some(p) => {
            match p {
                E.A { s } => {
                    let owned: String = s;
                    let probe: String = digits(i);
                    return owned.len() + probe.len();
                }
                E.B => {}
            }
        }
        None => {}
    }
    return 0;
}
fn main() {
    let base: i64 = env.args().len();
    let mut n = 0i64;
    let mut i = base;
    while i < base + 50i64 {
        n = n + boxed(i) + inline_if_let(i) + moved(i);
        i = i + 1;
    }
    if n > 0i64 { println("done"); }
}
"#,
        &["done"],
        "nested match over a borrowed payload frees once",
        200,
    );
}

/// B-2026-08-31-23 under ASAN/LSan — a consuming arm over a BOXED
/// user-ENUM payload frees the buffer exactly once, and the fields it does
/// NOT take are still freed by the box.
///
/// The E2E twin pins the text, which catches the double free. It cannot
/// see the other direction: disarming a field the arm never took is a
/// LEAK, and that is the failure both sibling disarmers were fixed for
/// (B-2026-08-04-6, B-2026-08-28-66). `two_fields_one_taken` is that
/// control — it binds `s` and leaves `t` to the box.
///
/// Same two fixture rules the B-2026-08-30-52 sibling records, for the
/// same reasons: the scrutinee is a FUNCTION-SCOPE local (a loop-body one
/// takes a different path where AOT is clean and only the JIT aborts), and
/// the payload's BYTES are read rather than its length (a `.len()`-only
/// buffer is a dead allocation LLVM deletes outright).
#[test]
fn asan_consuming_arm_over_a_boxed_enum_payload_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"
enum E { A { s: String }, B }
enum T { P { s: String, t: String }, Q }
fn s_of(i: i64) -> String {
    let mut s: String = String.new();
    s.push_str(f"payload-padded-out-well-past-thirty-six-bytes-{i}");
    return s;
}
fn digits(i: i64) -> String { let mut d: String = String.new(); d.push_str(f"{i}"); return d; }
// The row's NESTED spelling: outer arm binds the payload whole, inner match
// moves a heap field out of it.
fn nested(i: i64) -> i64 {
    let a: Option[E] = Some(E.A { s: s_of(i) });
    let mut r = 0;
    match a {
        Some(p) => { match p { E.A { s } => { let owned: String = s; if owned.contains(digits(i)) { r = owned.len(); } } E.B => {} } }
        None => {}
    }
    return r;
}
// The SINGLE-LEVEL spelling, which was broken for the same reason.
fn single(i: i64) -> i64 {
    let a: Option[E] = Some(E.A { s: s_of(i) });
    let mut r = 0;
    match a { Some(E.A { s }) => { let owned: String = s; if owned.contains(digits(i)) { r = owned.len(); } } Some(E.B) => {} None => {} }
    return r;
}
// CONTROL, the leak direction: TWO heap fields, the arm takes ONE. Disarming
// `t` as well would leak it — nobody else owns it.
fn two_fields_one_taken(i: i64) -> i64 {
    let a: Option[T] = Some(T.P { s: s_of(i), t: s_of(i) });
    let mut r = 0;
    match a { Some(T.P { s, .. }) => { let owned: String = s; if owned.contains(digits(i)) { r = owned.len(); } } Some(T.Q) => {} None => {} }
    return r;
}
// CONTROL: the arm only READS the payload (B-2026-08-30-52's borrow path).
fn read_only(i: i64) -> i64 {
    let a: Option[E] = Some(E.A { s: s_of(i) });
    let mut r = 0;
    match a { Some(p) => { match p { E.A { s } => { if s.contains(digits(i)) { r = s.len(); } } E.B => {} } } None => {} }
    match a { Some(p) => { match p { E.A { s } => { if s.contains(digits(i)) { r = r + s.len(); } } E.B => {} } } None => {} }
    return r;
}
fn main() {
    let base: i64 = env.args().len();
    let mut n = 0i64;
    let mut i = base;
    while i < base + 50i64 {
        n = n + nested(i) + single(i) + two_fields_one_taken(i) + read_only(i);
        i = i + 1;
    }
    if n > 0i64 { println("done"); }
}
"#,
        &["done"],
        "consuming arm over a boxed enum payload frees once",
        200,
    );
}

/// B-2026-08-30-16, the SAFETY half — the row's own objection under ASAN.
///
/// Releasing a fresh-temp `shared enum` scrutinee at the match's exit means
/// moving a DECREMENT, not just a body, and a dec that reaches zero frees
/// the box. The row argued that an arm binding aliasing into it would then
/// be a use-after-free rather than a reordering, which is why the value-enum
/// fix (B-2026-08-29-28) was not simply extended to this flavour.
///
/// So the fixture is built to break if that were true: every arm MOVES its
/// heap payload out and the value is read AFTER the match, once through a
/// binding and once straight out of the match expression. It is safe because
/// `suppress_shared_enum_payload_move_out` already zeroes the consumed
/// field's words in the box (B-2026-08-28-74), so the binding owns the
/// payload outright and the freed box no longer references it — but that is
/// an argument, and this is the measurement.
///
/// The loop keeps the boxes independent, so a dec that fired at the wrong
/// time shows up as a leak of the iterations that never got one rather than
/// a single ambiguous block.
///
/// ONE SHAPE IS DELIBERATELY NOT HERE. The same match in a NESTED EXPRESSION
/// position — a call argument, or interpolated into an f-string — leaks the
/// moved-out payload (15 B), and it does so BOTH before and after this fix,
/// on a VALUE enum as well as a shared one, so it is neither this row's
/// defect nor shared-specific. It was found while writing this fixture and
/// is filed as B-2026-09-01-26; including it here would have pinned an
/// unrelated leak to this row and made the fixture fail for a reason that
/// has nothing to do with what it tests. The let-bound spelling above is
/// clean, which is the contrast that isolated it.
#[test]
fn asan_freshtemp_shared_enum_scrutinee_release_at_match_exit() {
    assert_clean_asan_run(
        r#"
shared enum Sh { A(String), B }
impl Drop for Sh { fn drop(mut ref self) { println("dSh"); } }
fn mkSh(n: i64) -> Sh { return Sh.A(f"payloadpayload{n}"); }

#[allow(partial_move_of_drop_enum)]
fn main() {
    let mut i = 0;
    while i < 3 {
        let out = match mkSh(i) { Sh.A(s) => { s } Sh.B => { "none".to_string() } };
        println(f"w[{out}]");
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "dSh",
            "w[payloadpayload0]",
            "dSh",
            "w[payloadpayload1]",
            "dSh",
            "w[payloadpayload2]",
            "done",
        ],
        "b30-16-freshtemp-shared-enum-scrutinee-release",
    );
}

/// B-2026-09-02-8 — the memory half of the partial-destructure fix, which
/// the row asked for by name ("row E's memory side has NOT been measured
/// and should be, since a body that never runs on any backend may or may
/// not mean the buffer is also stranded").
///
/// It does not: `valgrind --leak-check=full` at `KARAC_OPT_LEVEL=0` on the
/// `let ... else` program reads 13 allocs / 13 frees PRE-fix and 14/14
/// post, 0 errors both times — the extra pair being the restored body's own
/// f-string. So the defect was bodies-only and the fix adds a body, not a
/// free.
///
/// Which is exactly why this test is worth having: restoring a body that
/// was not running is the shape that turns a silent omission into a
/// use-after-free if the payload it reads has already been reclaimed. The
/// bodies here READ `self.s.len()` through the payload's heap buffer, and
/// the loop reallocates on every pass so a stale pointer lands on
/// poisoned or reused memory rather than on quiet garbage.
#[test]
fn asan_optres_partial_destructure_body_reads_live_memory() {
    assert_clean_asan_run(
        r#"
fn pad(t: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-padded-out-well-past-thirty-six-bytes-");
    s.push_str(f"{t}");
    return s;
}
struct R { s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.s.len()}"); } }
struct H { r: R, n: i64 }
fn viaelse(t: i64) -> i64 {
    let e: Option[H] = Some(H { r: R { s: pad(t) }, n: 4 });
    let Some(H { n, .. }) = e else { return 0 };
    return n;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let a: Option[H] = Some(H { r: R { s: pad(i) }, n: 4 });
        if let Some(H { n, .. }) = a { println(f"A{n}") }
        let b: Option[H] = Some(H { r: R { s: pad(i) }, n: 5 });
        match b { Some(H { n, .. }) => println(f"B{n}"), None => {} }
        println(f"E{viaelse(i)}");
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "A4", "dR47", "B5", "dR47", "dR47", "E4", "A4", "dR47", "B5", "dR47", "dR47", "E4",
            "A4", "dR47", "B5", "dR47", "dR47", "E4", "done",
        ],
        "b0902-8-partial-destructure-body-live",
    );
}

#[test]
fn asan_bound_optres_local_missed_arm_body_reads_live_memory() {
    // B-2026-09-02-14 — a BOUND `Option`/`Result` local whose arm MISSED
    // lost its payload's `Drop` body, and the fix restores it.
    //
    // THE ROW'S OWN REPRO COULD NOT HAVE MEASURED THIS. It carries a
    // scalar payload, where the defect is a missing line of output and the
    // memory side says nothing — so a fix can look complete against it
    // while the restored body reads a husk. Restoring a silenced body is
    // exactly the change most able to introduce a double free or a
    // use-after-free: the retraction it un-does exists because a hit-edge
    // binding owns the same buffer.
    //
    // Each `W` owns a long `String` and the body READS THROUGH it
    // (`self.s.len()`), so a body running on a freed or moved-from object
    // is a sanitizer error rather than a quiet pass. Looped, so a per-path
    // flag that fails to reset between iterations shows up.
    //
    // Four cells: the miss edge (the row), the hit edge (whose binding owns
    // the payload and must be the sole runner), the later-use shape (one
    // body, at the later `match`), and the `match` arm set that binds only
    // `Ok` — the row's first unmeasured shape.
    assert_clean_asan_run(
        r#"
fn pad(t: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-padded-out-well-past-thirty-six-bytes-");
    s.push_str(f"{t}");
    return s;
}
struct W { s: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.s.len()}"); } }
fn mkerr() -> Result[W, W] { return Err(W { s: pad(7) }); }
fn mkok() -> Result[W, W] { return Ok(W { s: pad(1) }); }
fn main() {
    let mut i = 0;
    while i < 3 {
        let a: Result[W, W] = mkerr();
        if let Ok(w) = a { println(f"h{w.s.len()}"); }
        let b: Result[W, W] = mkok();
        if let Ok(w) = b { println(f"h{w.s.len()}"); }
        let c: Result[W, W] = mkerr();
        if let Ok(w) = c { println(f"h{w.s.len()}"); }
        println("mid");
        match c { Ok(x) => { println(f"x{x.s.len()}"); } Err(e) => { println(f"e{e.s.len()}"); } }
        let d: Result[W, W] = mkerr();
        match d { Ok(x) => { println(f"x{x.s.len()}"); } _ => { println("wild"); } }
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "dW47", "h47", "dW47", "mid", "e47", "dW47", "wild", "dW47", "dW47", "h47", "dW47",
            "mid", "e47", "dW47", "wild", "dW47", "dW47", "h47", "dW47", "mid", "e47", "dW47",
            "wild", "dW47", "done",
        ],
        "b0902-14-bound-optres-local-missed-arm",
    );
}

/// B-2026-09-15-21 — the same statement as the fixture above, with the
/// source one level in: an ARM-BOUND payload rather than the param itself.
///
/// `out = r` inside `match b { E.A(r) => … }`, where `b` is a by-value
/// param, freed the moved-in buffers NOWHERE. The assignment correctly
/// disarms the target's `cond_move_drop_flags` bit — for a by-value param
/// the `Drop` body is the caller's — and then asks
/// `register_param_view_mem_drop` to supply the free that withholding the
/// body withheld, exactly as the fixture above does. That registration
/// asks `source_carries_callee_owned_param_memory` about the SOURCE, and an
/// arm binding was in neither of the two sets that predicate reads: the
/// arm-binding site recorded the view in `param_view_locals` (who runs the
/// body) and never in `param_view_callee_owned` (whose heap it is). So the
/// registration declined and nothing owned the buffers.
///
/// Measured at `KARAC_OPT_LEVEL=0` under `valgrind --leak-check=full` on a
/// nine-shape sweep, before the fix: 406 B in 24 blocks across five
/// contexts, 146 allocations against 121 frees. After: one context and 144
/// frees, and the one remaining is a `shared struct` cell that is
/// byte-identical on both sides and is filed on its own row. The three
/// assignment spellings here — plain, `if let`, and via an intermediate
/// `let m = r` rebind — each lost 18 B per call, and the loop cell lost
/// 320 B in 20 blocks, which is what makes this unbounded rather than a
/// fixed cost.
///
/// `none` IS THE CELL AN OVER-EAGER FIX FAILS, and it is why the
/// registration is per-path rather than static: it passes `E.B`, so the
/// assignment never runs and `out` still holds the value its own `let`
/// gave it. That value must be freed by `out`'s ordinary drop, which the
/// flag leaves armed on this path. A fix that retracted unconditionally
/// instead would leak here, and the `dR0` in the expected output is what
/// pins the body half of the same path.
///
/// `read` and `borrow` are the controls that were always clean: an arm that
/// only reads its binding hands it to nobody, and a `ref` param's heap is
/// the caller's throughout. `borrow` is not decoration — it is one of the
/// three exclusions `source_carries_callee_owned_param_memory` carries, and
/// admitting it here would make this frame a SECOND owner of the caller's
/// buffer, which is a double free rather than a leak. The other two
/// exclusions (an RC-promoted param, a caller-retained aggregate) are not
/// reachable from this shape and are guarded by that predicate's own
/// fixtures.
///
/// FLOORED at 24 allocations, and the floor is doing real work here: the
/// row records that `KARAC_OPT_LEVEL=2` reports no leak at all, because
/// LLVM deletes an allocation nothing observes. So this cell is non-vacuous
/// only on the `-O0` leg — `scripts/asan-o0-leg.sh` — and a green default
/// `--features llvm` run is no evidence about it. Verified both ways
/// against the pre-fix tree.
#[test]
fn asan_arm_bound_payload_assigned_over_a_mut_local_has_an_owner() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }

fn mkr(i: i64) -> R { return R { id: i, tag: f"b1521-pay-{i}-aaaaaaaaaaaaaaaa" }; }

fn asg(b: E) -> i64 {
    let mut out: R = R { id: 0, tag: f"b1521-out-aaaaaaaaaaaaaaaa" };
    match b { E.A(r) => { out = r; } E.B => { } }
    return out.id;
}
fn iflet(b: E) -> i64 {
    let mut out: R = R { id: 0, tag: f"b1521-out-aaaaaaaaaaaaaaaa" };
    if let E.A(r) = b { out = r; }
    return out.id;
}
fn rebind(b: E) -> i64 {
    let mut out: R = R { id: 0, tag: f"b1521-out-aaaaaaaaaaaaaaaa" };
    match b { E.A(r) => { let m = r; out = m; } E.B => { } }
    return out.id;
}
fn readonly(b: E) -> i64 {
    match b { E.A(r) => { return r.id; } E.B => { return 0; } }
}
fn borrowed(b: ref E) -> i64 {
    match b { E.A(r) => { return r.id; } E.B => { return 0; } }
}

fn main() {
    let a1: i64 = asg(E.A(mkr(8)));
    println(f"asg:{a1}");
    let a2: i64 = asg(E.B);
    println(f"none:{a2}");
    let a3: i64 = iflet(E.A(mkr(7)));
    println(f"iflet:{a3}");
    let a4: i64 = rebind(E.A(mkr(6)));
    println(f"rebind:{a4}");
    let a5: i64 = readonly(E.A(mkr(5)));
    println(f"read:{a5}");
    let k: E = E.A(mkr(4));
    let a6: i64 = borrowed(k);
    println(f"borrow:{a6}");
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 3 {
        let one: i64 = asg(E.A(mkr(1)));
        acc = acc + one;
        i = i + 1;
    }
    println(f"loop:{acc}");
}
"#,
        &[
            "dR0", "dR8", "asg:8", "dR0", "none:0", "dR0", "dR7", "iflet:7", "dR0", "dR6",
            "rebind:6", "dR5", "read:5", "dR4", "borrow:4", "dR0", "dR1", "dR0", "dR1", "dR0",
            "dR1", "loop:3",
        ],
        "asan_arm_bound_payload_assigned_over_a_mut_local_has_an_owner",
        24,
    );
}

/// B-2026-09-05-9 — the memory half of
/// `e2e_agg_leaf_boxed_payload_moving_arm_frees_envelope`: every MOVING
/// arm over a heap-boxed agg destructure leaf payload, on both agg
/// families, frees the box envelope. Pre-fix each of the seven moving
/// cells lost 56 B (the box) at -O0 AND -O2 — the slot neutralization
/// hid the box pointer from the leaf's drop together with the moved
/// contents. LSan here runs the default level, so this pins the real
/// state; the borrow-only arms live in the -04-22 twin above.
#[test]
fn asan_agg_leaf_boxed_payload_moving_arm_frees_envelope() {
    let label = "agg_leaf_boxed_payload_moving_arm_frees_envelope";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { id: i64, x: String, y: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.x}{self.y}") } }
struct HoW { a: R, b: Result[W, String] }
fn mkw(k: i64) -> W { return W { id: k, x: f"x{k}", y: f"y{k}" } }
fn pickr(k: i64) -> W { let t: (R, Result[W, String]) = (R { id: k }, Result.Ok(mkw(k * 11))); let (a, b) = t; match b { Result.Ok(w) => w, Result.Err(e) => mkw(0) } }
fn picko(k: i64) -> W { let t: (R, Option[W]) = (R { id: k }, Option.Some(mkw(k * 11))); let (a, b) = t; match b { Option.Some(w) => w, Option.None => mkw(0) } }
fn pickboth(ok: bool) -> W { let t: (R, Result[W, W]) = (R { id: 13 }, if ok { Result.Ok(mkw(1313)) } else { Result.Err(mkw(1331)) }); let (a, b) = t; match b { Result.Ok(w) => w, Result.Err(w) => w } }
fn main() {
    { let t: (R, Result[W, String]) = (R { id: 1 }, Result.Ok(mkw(11))); let (a, b) = t; match b { Result.Ok(w) => { let g: W = w; println(f"g{g.id}") }, Result.Err(e) => println("err") } println("one") }
    { let w: W = pickr(2); println(f"got{w.id}"); println("two") }
    { let t: (R, Result[W, String]) = (R { id: 3 }, Result.Ok(mkw(33))); let (a, b) = t; if let Result.Ok(w) = b { let g: W = w; println(f"g{g.id}") } println("three") }
    { let t: (R, Option[W]) = (R { id: 4 }, Option.Some(mkw(44))); let (a, b) = t; match b { Option.Some(w) => { let g: W = w; println(f"g{g.id}") }, Option.None => println("none") } println("four") }
    { let w: W = picko(5); println(f"got{w.id}"); println("five") }
    { let t: (R, Option[W]) = (R { id: 6 }, Option.Some(mkw(66))); let (a, b) = t; if let Option.Some(w) = b { let g: W = w; println(f"g{g.id}") } println("six") }
    { let t: (R, Result[String, W]) = (R { id: 7 }, Result.Err(mkw(77))); let (a, b) = t; match b { Result.Ok(s) => println(f"ok{s}"), Result.Err(w) => { let g: W = w; println(f"g{g.id}") } } println("seven") }
    { let h: HoW = HoW { a: R { id: 8 }, b: Result.Ok(mkw(88)) }; let HoW { a, b } = h; match b { Result.Ok(w) => { let g: W = w; println(f"g{g.id}") }, Result.Err(e) => println("err") } println("eight") }
    { let t: (R, Result[W, String]) = (R { id: 9 }, Result.Err("e9")); let (a, b) = t; match b { Result.Ok(w) => { let g: W = w; println(f"g{g.id}") }, Result.Err(e) => println(f"err{e}") } println("nine") }
    { let t: (R, Option[W]) = (R { id: 10 }, Option.None); let (a, b) = t; match b { Option.Some(w) => { let g: W = w; println(f"g{g.id}") }, Option.None => println("none") } println("ten") }
    { let t: (R, Result[R, String]) = (R { id: 12 }, Result.Ok(R { id: 1212 })); let (a, b) = t; match b { Result.Ok(r) => { let g: R = r; println(f"g{g.id}") }, Result.Err(e) => println("err") } println("twelve") }
    { let w: W = pickboth(true); let v: W = pickboth(false); println(f"got{w.id}{v.id}"); println("thirteen") }
    println("end")
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported an error (exit {:?}); stdout:\n{stdout}",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "dR1",
            "g11",
            "dW11/x11y11",
            "one",
            "dR2",
            "got22",
            "dW22/x22y22",
            "two",
            "dR3",
            "g33",
            "dW33/x33y33",
            "three",
            "dR4",
            "g44",
            "dW44/x44y44",
            "four",
            "dR5",
            "got55",
            "dW55/x55y55",
            "five",
            "dR6",
            "g66",
            "dW66/x66y66",
            "six",
            "dR7",
            "g77",
            "dW77/x77y77",
            "seven",
            "dR8",
            "g88",
            "dW88/x88y88",
            "eight",
            "dR9",
            "erre9",
            "nine",
            "dR10",
            "none",
            "ten",
            "dR12",
            "g1212",
            "dR1212",
            "twelve",
            "dR13",
            "dR13",
            "got13131331",
            "dW1331/x1331y1331",
            "dW1313/x1313y1313",
            "thirteen",
            "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-03-34 — the memory half of
/// `e2e_match_arm_struct_payload_binding_runs_its_field_bodies`, over the
/// `Two` payload cells: the new field-bodies walk on the arm's binding runs
/// BEFORE the binding's memory drop and frees nothing itself, and the
/// destructure's transfer off it retracts per field, so this pins one free
/// per buffer (valgrind: every block freed at -O0). The `Ho2`
/// (`Option[R]`-field) cells are deliberately absent: they leak the boxed
/// payload on every path, a pre-existing defect filed separately.
#[test]
fn asan_match_arm_struct_payload_binding_field_bodies_clean() {
    let label = "match_arm_struct_payload_binding_field_bodies";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Two { a: R, b: R }
struct Nest { inner: Two, z: i64 }
enum Wrap { T(Two), Nn(Nest), N }
fn mk(k: i64) -> R { return R { id: k, tag: f"t{k}", xs: [k] } }
fn main() {
    { let w: Wrap = Wrap.T(Two { a: mk(57), b: mk(157) }); match w { Wrap.T(h) => { let Two { a, b } = h; println("in") }, _ => println("n") } println("four") }
    { let w: Wrap = Wrap.T(Two { a: mk(58), b: mk(158) }); match w { Wrap.T(h) => { let Two { a, b: _ } = h; println("in") }, _ => println("n") } println("five") }
    { let w: Wrap = Wrap.Nn(Nest { inner: Two { a: mk(59), b: mk(159) }, z: 1 }); match w { Wrap.Nn(h) => { let Nest { inner, z } = h; println("in") }, _ => println("n") } println("six") }
    { let w: Wrap = Wrap.T(Two { a: mk(60), b: mk(160) }); match w { Wrap.T(h) => { println(f"use{h.a.id}") }, _ => println("n") } println("seven") }
    { let w: Wrap = Wrap.T(Two { a: mk(61), b: mk(161) }); match w { Wrap.T(h) => { let g: Two = h; println("in") }, _ => println("n") } println("eight") }
    { let w: Wrap = Wrap.T(Two { a: mk(62), b: mk(162) }); match w { Wrap.T(h) => { println("in") }, _ => println("n") } println("nine") }
    println("end")
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported an error (exit {:?}); stdout:\n{stdout}",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "dR157", "dR57", "in", "four", "dR158", "dR58", "in", "five", "dR159", "dR59", "in",
            "six", "use60", "dR160", "dR60", "seven", "dR161", "dR61", "in", "eight", "in",
            "dR162", "dR62", "nine", "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-07-33 — a callee that hands a BOXED payload out of a `match`
/// arm must not write into the envelope it just freed.
///
/// The shape B-2026-09-07-16's fixture deliberately left out, because it
/// was still red when that row landed: `payout` frees the payload box in
/// the arm (B-2026-09-05-26's envelope free, without which the box leaks)
/// and the move-out mirror then stored three zero words THROUGH that freed
/// pointer — `Invalid write of size 8` at offsets 0, 40 and 48 of a
/// 56-byte block, on a program that printed the right answer at both opt
/// levels and under `--interp`.
///
/// WHAT THIS PIN DOES AND DOES NOT CATCH, stated plainly because the
/// distinction cost a measurement to find. On the DEFAULT leg it does NOT
/// catch the row's defect: this suite links `-fsanitize=address` but the
/// karac-emitted object is not INSTRUMENTED there
/// (`link_executable_with_sanitizer` passes the flag at the link step
/// only), so the ASAN runtime sees what it can intercept in the allocator —
/// leaks, double and invalid frees — and an invalid WRITE into a freed
/// block is invisible to it. Measured: this fixture is green on the PARENT
/// at `-O2` and reports no use-after-free on the parent at `-O0` either,
/// while valgrind reports three `Invalid write of size 8` on the same
/// program.
///
/// B-2026-09-07-40 CLOSED THAT GAP, and this fixture is the row's own
/// A/B. `KARAC_SANITIZE_ADDRESS=1` runs LLVM's `asan` module pass over the
/// emitted module, and on a compiler with this fix REVERTED the same
/// program at `-O0` now exits 23 with `AddressSanitizer:
/// heap-use-after-free`, `WRITE of size 8`, 0 bytes into the same 56-byte
/// region — valgrind's offset-0 write, seen by the suite that is named for
/// it. Uninstrumented, that identical binary exits 0 and prints the right
/// answer. So this fixture is a real regression gate on the
/// `scripts/asan-instrumented-leg.sh` leg and remains the trade-pin
/// described below on every other one.
///
/// What it DOES pin is the fix's own risk direction, which is the reason to
/// keep it. The fix works by DROPPING a registration, and an over-broad
/// drop turns the use-after-free into a leaked envelope or a double-freed
/// payload — both squarely allocator-visible, both caught here, and both
/// caught on the `-O0` leg (`scripts/asan-o0-leg.sh`) where these payloads
/// actually get allocated. So the E2E twin pins the value, valgrind pinned
/// the defect by hand, and this pins the trade.
///
/// The neighbouring shapes are carried for that same reason: an outliving
/// STORE, and the same hand-out over a COPY-SUPPORTED payload whose
/// envelope the callee owns outright. A whole-value REBIND belongs here too
/// and is deliberately ABSENT — it strands its payload at `-O0`
/// (B-2026-09-07-41, measured identical on both sides of this fix), so
/// including it would paint this fixture red on the `-O0` leg for a defect
/// that is not this row's.
#[test]
fn asan_boxed_payload_handed_out_of_a_match_arm_is_not_written_after_free() {
    assert_clean_asan_run_min_allocs(
        "struct X1 { a: Option[i64], s: String }\n\
struct Ctl { s: String, n: i64 }\n\
enum W { T(X1), U(i64) }\n\
enum C { T(Ctl), U(i64) }\n\
fn mkx(i: i64) -> X1 { return X1 { a: Option.Some(i), s: f\"s{i}\" }; }\n\
fn mkc(i: i64) -> Ctl { return Ctl { s: f\"c{i}\", n: i }; }\n\
fn payout(w: W) -> X1 { return match w { W.T(x) => x, W.U(n) => mkx(n) }; }\n\
fn payoutc(c: C) -> Ctl { return match c { C.T(x) => x, C.U(n) => mkc(n) }; }\n\
fn store(w: W, out: mut ref Vec[W]) { out.push(w); }\n\
fn main() {\n\
  // THE DEFECT: hand a boxed, copy-declined payload out of the arm.\n\
  let p = payout(W.T(mkx(23))); println(f\"a={p.a.unwrap_or(0)}\")\n\
  // the U arm of the same callee -- no payload box to free at all\n\
  let q = payout(W.U(24)); println(f\"b={q.a.unwrap_or(0)}\")\n\
  // COPY-SUPPORTED twin: entry-copied, the envelope is the callee's own\n\
  let r = payoutc(C.T(mkc(25))); println(f\"c={r.n}\")\n\
  // a neighbour that was already clean and must stay clean\n\
  let mut v: Vec[W] = Vec.new(); store(W.T(mkx(27)), mut v); println(f\"e={v.len()}\")\n\
  println(\"end\")\n\
}\n",
        &["a=23", "b=24", "c=25", "e=1", "end"],
        "boxed payload handed out of a match arm",
        5,
    );
}

/// B-2026-09-05-27 — the memory half of
/// `e2e_match_arm_handing_out_a_tuple_element_frees_it_once`: every buffer
/// freed exactly once across one and two calls, the explicit `return`,
/// the single-heap-field element, the local-bound and local-scrutinee
/// spellings (valgrind: 0 errors, every block freed at -O0).
#[test]
fn asan_match_arm_handing_out_a_tuple_element_clean() {
    let label = "match_arm_handing_out_a_tuple_element";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct S1 { id: i64, tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS{self.id}") } }
enum E { A(R), B }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn mk1(i: i64) -> S1 { return S1 { id: i, tag: f"t{i}" } }
fn p4(t: (R, i64)) -> R { match t { (r, k) => { r } } }
fn p4r(t: (R, i64)) -> R { match t { (r, k) => { return r } } }
fn p1(t: (S1, i64)) -> S1 { match t { (r, k) => { r } } }
fn pe(e: E) -> R { match e { E.A(r) => { r }, E.B => mk(0) } }
fn pd(t: (R, i64)) -> R { let x: R = match t { (r, k) => r }; return x }
fn pl() -> R { let t: (R, i64) = (mk(13), 0); match t { (r, k) => { r } } }
fn pc(t: (R, i64)) -> R { match t { (r, k) => { let g: R = r; g } } }
fn main() {
    { let a: R = p4((mk(3), 0)); println(f"got{a.id}"); println("one") }
    { let a: R = p4((mk(3), 0)); let b: R = p4((mk(6), 0)); println(f"got{a.id}{b.id}"); println("two") }
    { let a: R = p4r((mk(4), 0)); let b: R = p4r((mk(7), 0)); println(f"got{a.id}{b.id}"); println("three") }
    { let a: S1 = p1((mk1(5), 0)); let b: S1 = p1((mk1(8), 0)); println(f"got{a.id}{b.id}"); println("four") }
    { let a: R = pe(E.A(mk(9))); let b: R = pe(E.A(mk(10))); println(f"got{a.id}{b.id}"); println("five") }
    { let t: (R, i64) = (mk(11), 0); let a: R = p4(t); let u: (R, i64) = (mk(12), 0); let b: R = p4(u); println(f"got{a.id}{b.id}"); println("six") }
    { let a: R = pd((mk(14), 0)); println(f"got{a.id}"); println("seven") }
    { let a: R = pl(); println(f"got{a.id}"); println("eight") }
    { let a: R = pc((mk(15), 0)); println(f"got{a.id}"); println("nine") }
    println("end")
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported an error (exit {:?}); stdout:\n{stdout}",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "got3", "dR3", "one", "got36", "dR6", "dR3", "two", "got47", "dR7", "dR4", "three",
            "got58", "dS8", "dS5", "four", "got910", "dR10", "dR9", "five", "got1112", "dR12",
            "dR11", "six", "got14", "dR14", "seven", "got13", "dR13", "eight", "got15", "dR15",
            "nine", "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-05-28 / B-2026-09-05-30 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_match_arm_element_moved_into_a_callee_or_unread_runs_one_body`:
/// every element of an owned tuple argument whose match arm moves it into
/// a callee, leaves it unread, wildcards it, or hands its sibling out is
/// freed exactly once, with its `Drop` body run exactly once. Heap `R`
/// (`String` + `Vec`) so a lost body would be a leak LSan sees and a
/// doubled one a double free.
#[test]
fn asan_match_arm_element_moved_into_a_callee_or_unread_clean() {
    let label = "match_arm_element_moved_into_a_callee_or_unread";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn consume(x: R) -> i64 { return x.id }
struct H { n: i64 }
impl H {
    fn m_call(ref self, t: (R, i64)) -> i64 { match t { (r, k) => { consume(r) + self.n } } }
    fn m_unread(ref self, t: (R, i64)) -> i64 { match t { (r, k) => { k + self.n } } }
}
fn p_call(t: (R, i64)) -> i64 { match t { (r, k) => { consume(r) } } }
fn p_unread(t: (R, i64)) -> i64 { match t { (r, k) => { k } } }
fn p_ret(t: (R, i64)) -> R { match t { (r, k) => { r } } }
fn p_read(t: (R, i64)) -> i64 { match t { (r, k) => { r.id } } }
fn p_rebind_call(t: (R, i64)) -> i64 { match t { (r, k) => { let g: R = r; consume(g) } } }
fn p_wild(t: (R, i64)) -> i64 { match t { (_, k) => { k } } }
fn p_let_call(t: (R, i64)) -> i64 { let (r, k) = t; consume(r) }
fn t_two(t: (R, R)) -> R { match t { (a, b) => { a } } }
fn t_two_call(t: (R, R)) -> i64 { match t { (a, b) => { consume(a) } } }
fn t_nested(t: ((R, i64), i64)) -> i64 { match t { ((r, j), k) => { k } } }
fn t_cond(t: (R, i64), c: bool) -> i64 { match t { (r, k) => { if c { consume(r) } else { k } } } }
fn t_nested_match(t: (R, i64)) -> i64 { match t { (r, k) => { match k { 0 => { consume(r) }, _ => { k } } } } }
fn t_two_arms(t: (R, i64)) -> i64 { match t { (r, 0) => { consume(r) }, (r, k) => { k } } }
fn main() {
    let h: H = H { n: 100 };
    { let d: i64 = p_call((mk(1), 0)); println(f"r{d}"); println("one") }
    { let d: i64 = p_unread((mk(2), 0)); println(f"r{d}"); println("two") }
    { let a: R = p_ret((mk(3), 0)); println(f"r{a.id}"); println("three") }
    { let d: i64 = p_read((mk(4), 0)); println(f"r{d}"); println("four") }
    { let d: i64 = p_rebind_call((mk(6), 0)); println(f"r{d}"); println("six") }
    { let d: i64 = p_wild((mk(7), 0)); println(f"r{d}"); println("seven") }
    { let d: i64 = p_let_call((mk(9), 0)); println(f"r{d}"); println("nine") }
    { let t: (R, i64) = (mk(10), 0); let d: i64 = p_call(t); println(f"r{d}"); println("ten") }
    { let t: (R, i64) = (mk(11), 0); let d: i64 = p_unread(t); println(f"r{d}"); println("eleven") }
    { let a: R = t_two((mk(12), mk(13))); println(f"r{a.id}"); println("twelve") }
    { let d: i64 = t_two_call((mk(14), mk(15))); println(f"r{d}"); println("fourteen") }
    { let d: i64 = t_nested(((mk(16), 0), 0)); println(f"r{d}"); println("sixteen") }
    { let d: i64 = h.m_call((mk(17), 0)); println(f"r{d}"); println("seventeen") }
    { let d: i64 = h.m_unread((mk(18), 0)); println(f"r{d}"); println("eighteen") }
    { let d: i64 = t_cond((mk(19), 0), true); println(f"r{d}"); println("nineteen") }
    { let d: i64 = t_cond((mk(20), 0), false); println(f"r{d}"); println("twenty") }
    { let d: i64 = t_nested_match((mk(21), 0)); println(f"r{d}"); println("twentyone") }
    { let d: i64 = t_nested_match((mk(22), 5)); println(f"r{d}"); println("twentytwo") }
    { let d: i64 = t_two_arms((mk(23), 0)); println(f"r{d}"); println("twentythree") }
    { let d: i64 = t_two_arms((mk(24), 3)); println(f"r{d}"); println("twentyfour") }
    { let t: (R, R) = (mk(25), mk(26)); let d: i64 = t_two_call(t); println(f"r{d}"); println("twentysix") }
    println("end")
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported an error (exit {:?}); stdout:\n{stdout}",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "dR1",
            "r1",
            "one",
            "dR2",
            "r0",
            "two",
            "r3",
            "dR3",
            "three",
            "dR4",
            "r4",
            "four",
            "dR6",
            "r6",
            "six",
            "dR7",
            "r0",
            "seven",
            "dR9",
            "r9",
            "nine",
            "dR10",
            "r10",
            "ten",
            "dR11",
            "r0",
            "eleven",
            "dR13",
            "r12",
            "dR12",
            "twelve",
            "dR14",
            "dR15",
            "r14",
            "fourteen",
            "dR16",
            "r0",
            "sixteen",
            "dR17",
            "r117",
            "seventeen",
            "dR18",
            "r100",
            "eighteen",
            "dR19",
            "r19",
            "nineteen",
            "dR20",
            "r0",
            "twenty",
            "dR21",
            "r21",
            "twentyone",
            "dR22",
            "r5",
            "twentytwo",
            "dR23",
            "r23",
            "twentythree",
            "dR24",
            "r3",
            "twentyfour",
            "dR25",
            "dR26",
            "r25",
            "twentysix",
            "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-05-33 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_match_arm_element_escaping_by_call_store_or_assignment_has_one_owner`:
/// a tuple element forwarded, stashed, pushed, assigned out, or handed out
/// of a method arm is freed exactly once and its body runs exactly once.
/// `three` and `four` were double frees before the source-element zeroing;
/// heap `R` (`String` + `Vec`) so a lost free is a leak LSan sees.
#[test]
fn asan_match_arm_element_escaping_by_call_store_or_assignment_clean() {
    let label = "match_arm_element_escaping_by_call_store_or_assignment";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn consume(x: R) -> i64 { return x.id }
fn wrap(x: R) -> R { return x }
fn stash(x: R, v: mut ref Vec[R]) { v.push(x) }
struct H { n: i64 }
impl H {
    fn m_ret(ref self, t: (R, i64)) -> R { match t { (r, k) => { r } } }
    fn m_two(ref self, t: (R, R)) -> R { match t { (a, b) => { b } } }
}
fn t_fwd(t: (R, i64)) -> R { match t { (r, k) => { wrap(r) } } }
fn t_stash(t: (R, i64), v: mut ref Vec[R]) -> i64 { match t { (r, k) => { stash(r, v); k } } }
fn t_push(t: (R, i64), v: mut ref Vec[R]) -> i64 { match t { (r, k) => { v.push(r); k } } }
fn t_assign(t: (R, i64)) -> R { let mut out: R = mk(50); match t { (r, k) => { out = r; } } out }
fn t_two_push(t: (R, R), v: mut ref Vec[R]) -> i64 { match t { (a, b) => { v.push(a); consume(b) } } }
fn main() {
    let h: H = H { n: 1 };
    { let a: R = t_fwd((mk(1), 0)); println(f"r{a.id}"); println("one") }
    { let mut v: Vec[R] = []; let d: i64 = t_stash((mk(2), 0), mut v); println(f"r{d} n{v.len()}"); println("two") }
    { let mut v: Vec[R] = []; let d: i64 = t_push((mk(3), 0), mut v); println(f"r{d} n{v.len()}"); println("three") }
    { let a: R = t_assign((mk(4), 0)); println(f"r{a.id}"); println("four") }
    { let a: R = h.m_ret((mk(5), 0)); println(f"r{a.id}"); println("five") }
    { let a: R = h.m_two((mk(6), mk(7))); println(f"r{a.id}"); println("six") }
    { let mut v: Vec[R] = []; let d: i64 = t_two_push((mk(8), mk(9)), mut v); println(f"r{d} n{v.len()}"); println("eight") }
    { let t: (R, i64) = (mk(10), 0); let a: R = t_fwd(t); println(f"r{a.id}"); println("ten") }
    { let t: (R, i64) = (mk(11), 0); let mut v: Vec[R] = []; let d: i64 = t_push(t, mut v); println(f"r{d} n{v.len()}"); println("eleven") }
    { let t: (R, i64) = (mk(12), 0); let a: R = h.m_ret(t); println(f"r{a.id}"); println("twelve") }
    { let a: R = t_fwd((mk(13), 0)); let b: R = t_fwd((mk(14), 0)); println(f"r{a.id}{b.id}"); println("thirteen") }
    println("end")
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported an error (exit {:?}); stdout:\n{stdout}",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "r1", "dR1", "one", "r0 n1", "dR2", "two", "r0 n1", "dR3", "three", "dR50", "r4",
            "dR4", "four", "r5", "dR5", "five", "dR6", "r7", "dR7", "six", "r9 n1", "dR8", "eight",
            "r10", "dR10", "ten", "r0 n1", "dR11", "eleven", "r12", "dR12", "twelve", "r1314",
            "dR14", "dR13", "thirteen", "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-05-35 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_enum_payload_consumed_or_unread_in_the_arm_runs_one_body`: an enum
/// payload consumed, read, unread, handed out, stashed or pushed from a
/// match arm is freed exactly once and its body runs exactly once, with
/// the per-variant masked walker in play. Heap `R` (`String` + `Vec`) so
/// a lost free is a leak LSan sees.
#[test]
fn asan_enum_payload_consumed_or_unread_in_the_arm_clean() {
    let label = "enum_payload_consumed_or_unread_in_the_arm";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn consume(x: R) -> i64 { return x.id }
fn wrap(x: R) -> R { return x }
fn stash(x: R, v: mut ref Vec[R]) { v.push(x) }
enum E { A(R), B(i64) }
enum O { S(R), N }
struct H { n: i64 }
impl H {
    fn m_call(ref self, b: E) -> i64 { match b { E.A(r) => { consume(r) + self.n }, E.B(k) => { k } } }
    fn m_unread(ref self, b: E) -> i64 { match b { E.A(r) => { self.n }, E.B(k) => { k } } }
}
fn e_call(b: E) -> i64 { match b { E.A(r) => { consume(r) }, E.B(k) => { k } } }
fn e_unread(b: E) -> i64 { match b { E.A(r) => { 5 }, E.B(k) => { k } } }
fn e_read(b: E) -> i64 { match b { E.A(r) => { r.id }, E.B(k) => { k } } }
fn e_ret(b: E) -> R { match b { E.A(r) => { r }, E.B(k) => { mk(k) } } }
fn e_fwd(b: E) -> R { match b { E.A(r) => { wrap(r) }, E.B(k) => { mk(k) } } }
fn e_stash(b: E, v: mut ref Vec[R]) -> i64 { match b { E.A(r) => { stash(r, v); 1 }, E.B(k) => { k } } }
fn e_push(b: E, v: mut ref Vec[R]) -> i64 { match b { E.A(r) => { v.push(r); 1 }, E.B(k) => { k } } }
fn e_call_stmt(b: E) -> i64 { match b { E.A(r) => { let d: i64 = consume(r); d + 1 }, E.B(k) => { k } } }
fn o_call(b: O) -> i64 { match b { O.S(r) => { consume(r) }, O.N => { 0 } } }
fn o_unread(b: O) -> i64 { match b { O.S(r) => { 5 }, O.N => { 0 } } }
fn o_unread_single(b: O) -> i64 { if let O.S(r) = b { 5 } else { 0 } }
fn o_call_single(b: O) -> i64 { if let O.S(r) = b { consume(r) } else { 0 } }
fn o_wild(b: O) -> i64 { match b { O.S(_) => { 5 }, O.N => { 0 } } }
fn main() {
    let h: H = H { n: 100 };
    { let d: i64 = e_call(E.A(mk(1))); println(f"r{d}"); println("one") }
    { let d: i64 = e_unread(E.A(mk(2))); println(f"r{d}"); println("two") }
    { let d: i64 = e_read(E.A(mk(3))); println(f"r{d}"); println("three") }
    { let a: R = e_ret(E.A(mk(4))); println(f"r{a.id}"); println("four") }
    { let a: R = e_fwd(E.A(mk(5))); println(f"r{a.id}"); println("five") }
    { let mut v: Vec[R] = []; let d: i64 = e_stash(E.A(mk(6)), mut v); println(f"r{d} n{v.len()}"); println("six") }
    { let mut v: Vec[R] = []; let d: i64 = e_push(E.A(mk(7)), mut v); println(f"r{d} n{v.len()}"); println("seven") }
    { let d: i64 = e_call_stmt(E.A(mk(8))); println(f"r{d}"); println("eight") }
    { let d: i64 = o_call(O.S(mk(9))); println(f"r{d}"); println("nine") }
    { let d: i64 = o_unread(O.S(mk(10))); println(f"r{d}"); println("ten") }
    { let d: i64 = o_unread_single(O.S(mk(11))); println(f"r{d}"); println("eleven") }
    { let d: i64 = o_call_single(O.S(mk(12))); println(f"r{d}"); println("twelve") }
    { let d: i64 = o_wild(O.S(mk(13))); println(f"r{d}"); println("thirteen") }
    { let d: i64 = h.m_call(E.A(mk(14))); println(f"r{d}"); println("fourteen") }
    { let d: i64 = h.m_unread(E.A(mk(15))); println(f"r{d}"); println("fifteen") }
    { let e: E = E.A(mk(16)); let d: i64 = e_call(e); println(f"r{d}"); println("sixteen") }
    { let e: E = E.A(mk(17)); let d: i64 = e_unread(e); println(f"r{d}"); println("seventeen") }
    { let e: E = E.A(mk(18)); let a: R = e_ret(e); println(f"r{a.id}"); println("eighteen") }
    { let d: i64 = e_call(E.B(19)); println(f"r{d}"); println("nineteen") }
    println("end")
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported an error (exit {:?}); stdout:\n{stdout}",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "dR1",
            "r1",
            "one",
            "dR2",
            "r5",
            "two",
            "dR3",
            "r3",
            "three",
            "r4",
            "dR4",
            "four",
            "r5",
            "dR5",
            "five",
            "r1 n1",
            "dR6",
            "six",
            "r1 n1",
            "dR7",
            "seven",
            "dR8",
            "r9",
            "eight",
            "dR9",
            "r9",
            "nine",
            "dR10",
            "r5",
            "ten",
            "dR11",
            "r5",
            "eleven",
            "dR12",
            "r12",
            "twelve",
            "dR13",
            "r5",
            "thirteen",
            "dR14",
            "r114",
            "fourteen",
            "dR15",
            "r100",
            "fifteen",
            "dR16",
            "r16",
            "sixteen",
            "dR17",
            "r5",
            "seventeen",
            "r18",
            "dR18",
            "eighteen",
            "r19",
            "nineteen",
            "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-06-20 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_match_over_a_masked_wrap_slot_binds_a_view`: a payload bound out
/// of a masked slot goes memory-only, so its buffer is still freed
/// exactly once (heap `R`, `String` field) while its body is the
/// caller's.
#[test]
fn asan_match_over_a_masked_wrap_slot_clean() {
    let label = "match_over_a_masked_wrap_slot";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
enum W2 { Two(R, R), None2 }
struct S3 { a: R, b: R }
fn m_direct(r: R) -> i64 { let w: W2 = W2.Two(r, mk(2)); match w { W2.Two(a, b) => { return b.id; } W2.None2 => { return 0; } } }
fn m_rebind(r: R) -> i64 { let w: W2 = W2.Two(r, mk(4)); let w2: W2 = w; match w2 { W2.Two(a, b) => { return b.id; } W2.None2 => { return 0; } } }
fn m_direct_a(r: R) -> i64 { let w: W2 = W2.Two(r, mk(6)); match w { W2.Two(a, b) => { return a.id; } W2.None2 => { return 0; } } }
fn m_unread(r: R) -> i64 { let w: W2 = W2.Two(r, mk(8)); match w { W2.Two(a, b) => { return 1; } W2.None2 => { return 0; } } }
fn m_iflet(r: R) -> i64 { let w: W2 = W2.Two(r, mk(10)); if let W2.Two(a, b) = w { return b.id; } return 0; }
fn m_rebind_in_arm(r: R) -> i64 { let w: W2 = W2.Two(r, mk(12)); match w { W2.Two(a, b) => { let m: R = a; return m.id; } W2.None2 => { return 0; } } }
fn m_swap(r: R) -> i64 { let w: W2 = W2.Two(mk(14), r); match w { W2.Two(a, b) => { return a.id; } W2.None2 => { return 0; } } }
fn t_direct(r: R) -> i64 { let t: (R, R) = (r, mk(21)); match t { (a, b) => { return b.id; } } }
fn main() {
    { let v: i64 = m_direct(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = m_rebind(mk(3)); println(f"v={v}"); println("two") }
    { let v: i64 = m_direct_a(mk(5)); println(f"v={v}"); println("three") }
    { let v: i64 = m_unread(mk(7)); println(f"v={v}"); println("four") }
    { let v: i64 = m_iflet(mk(9)); println(f"v={v}"); println("five") }
    { let v: i64 = m_rebind_in_arm(mk(11)); println(f"v={v}"); println("six") }
    { let v: i64 = m_swap(mk(13)); println(f"v={v}"); println("seven") }
    { let v: i64 = t_direct(mk(20)); println(f"v={v}"); println("ten") }
    println("end")
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported an error (exit {:?}); stdout:\n{stdout}",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "dR2", "dR1", "v=2", "one", "dR4", "dR3", "v=4", "two", "dR6", "dR5", "v=5", "three",
            "dR8", "dR7", "v=1", "four", "dR10", "dR9", "v=10", "five", "dR12", "dR11", "v=11",
            "six", "dR14", "dR13", "v=14", "seven", "dR20", "v=21", "ten", "end"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-06-22 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_match_over_a_masked_struct_field_binds_a_view`: a field bound out
/// of a masked struct field goes memory-only, so its buffer is still
/// freed exactly once (heap `R`, `String` field) while its body is the
/// caller's.
#[test]
fn asan_match_over_a_masked_struct_field_clean() {
    let label = "match_over_a_masked_struct_field";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
struct S3 { a: R, b: R }
fn s_direct(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(2) }; match s { S3 { a, b } => { return b.id; } } }
fn s_direct_a(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(4) }; match s { S3 { a, b } => { return a.id; } } }
fn s_unread(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(6) }; match s { S3 { a, b } => { return 1; } } }
fn s_rebind(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(8) }; let s2: S3 = s; match s2 { S3 { a, b } => { return b.id; } } }
fn s_iflet(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(10) }; if let S3 { a, b } = s { return b.id; } return 0; }
fn s_rebind_in_arm(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(12) }; match s { S3 { a, b } => { let m: R = a; return m.id; } } }
fn s_swap(r: R) -> i64 { let s: S3 = S3 { a: mk(14), b: r }; match s { S3 { a, b } => { return a.id; } } }
fn s_fresh(r: R) -> i64 { let s: S3 = S3 { a: mk(16), b: mk(17) }; match s { S3 { a, b } => { return a.id; } } }
fn s_partial(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(21) }; match s { S3 { b, .. } => { return b.id; } } }
fn main() {
    { let v: i64 = s_direct(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = s_direct_a(mk(3)); println(f"v={v}"); println("two") }
    { let v: i64 = s_unread(mk(5)); println(f"v={v}"); println("three") }
    { let v: i64 = s_rebind(mk(7)); println(f"v={v}"); println("four") }
    { let v: i64 = s_iflet(mk(9)); println(f"v={v}"); println("five") }
    { let v: i64 = s_rebind_in_arm(mk(11)); println(f"v={v}"); println("six") }
    { let v: i64 = s_swap(mk(13)); println(f"v={v}"); println("seven") }
    { let v: i64 = s_fresh(mk(15)); println(f"v={v}"); println("eight") }
    { let v: i64 = s_partial(mk(20)); println(f"v={v}"); println("ten") }
    println("end")
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported an error (exit {:?}); stdout:\n{stdout}",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "dR2", "dR1", "v=2", "one", "dR4", "dR3", "v=3", "two", "dR6", "dR5", "v=1", "three",
            "dR8", "dR7", "v=8", "four", "dR10", "dR9", "v=10", "five", "dR12", "dR11", "v=11",
            "six", "dR14", "dR13", "v=14", "seven", "dR17", "dR16", "dR15", "v=16", "eight",
            "dR21", "dR20", "v=21", "ten", "end"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-06-40 UPDATED THE `two` CELL: the compiled backends now drain a
/// reordered `let` pattern in PATTERN order, so it reads `dR7 dR8 dR6`
/// where it read `dR8 dR7 dR6`. This fixture's own subject is unchanged —
/// ASAN is clean either way and each body still runs exactly once; only
/// the sequence of the two reordered leaves moved.
/// B-2026-09-06-33 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_partial_struct_let_pattern_with_drop_fields_binds_by_name`: a
/// partial / reordered / renamed struct `let` pattern over heap-carrying
/// fields frees each field exactly once now that the leaf is bound to
/// the field it names (the misread field was a double free before).
#[test]
fn asan_partial_struct_let_pattern_with_drop_fields_clean() {
    let label = "partial_struct_let_pattern_with_drop_fields";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
struct S3 { a: R, b: R }
fn p_fresh(r: R) -> i64 { let s: S3 = S3 { a: mk(2), b: mk(3) }; let S3 { b, .. } = s; return b.id; }
fn p_swapped(r: R) -> i64 { let s: S3 = S3 { a: mk(7), b: mk(8) }; let S3 { b, a } = s; return b.id * 100 + a.id; }
fn p_rename(r: R) -> i64 { let s: S3 = S3 { a: mk(12), b: mk(13) }; let S3 { b: q, .. } = s; return q.id; }
fn main() {
    { let v: i64 = p_fresh(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = p_swapped(mk(6)); println(f"v={v}"); println("two") }
    { let v: i64 = p_rename(mk(11)); println(f"v={v}"); println("three") }
    println("end")
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported an error (exit {:?}); stdout:\n{stdout}",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "dR2", "dR3", "dR1", "v=3", "one", "dR7", "dR8", "dR6", "v=807", "two", "dR12", "dR13",
            "dR11", "v=13", "three", "end"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-07-59, ARM-LOCAL OUTER spelling — the receiver's outer is a
/// match-arm binding (`Some(n) => n.left.clone()`), so its name has been
/// reverted with the arm frame by the time the consuming `let` classifies
/// its RHS. The env cannot answer what the clone retained; the emission
/// record can, which is the same asymmetry `option_shared_leaf_retains`
/// exists for. Without it this spelling kept the use-after-free after the
/// direct one was fixed, so it is gated separately.
#[test]
fn asan_niche_option_field_clone_from_match_arm_local_is_owned() {
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut t = 0i64;
    let mut i = 0i64;
    while i < 20i64 {
        let root: Option[Node] = Some(Node { val: 5, left: Some(Node { val: 6, left: None, right: None }), right: None });
        let s = match root { None => None, Some(n) => n.left.clone() };
        let l0 = clone_offset(s, 10);
        let l1 = clone_offset(s, 20);
        t = t + count_nodes(l0) + count_nodes(l1);
        i = i + 1i64;
    }
    println(f"{t}");
}
"#,
        &["40"],
        "asan-niche-option-field-clone-arm-local",
    );
}

/// B-2026-09-09-19 — a boxed enum payload's INTERIOR when the `Option`
/// lives in a struct FIELD and the arm destructures INTO it.
///
/// `suppress_struct_field_boxed_payload_match_out` (B-2026-08-07-7) zeroes
/// the field's `Option` tag so the owning struct's drop skips the payload,
/// and parks the box so the ENVELOPE still gets freed. Its stated premise
/// is that "the arm's binding owns the INTERIOR only". That holds for a
/// `String` / `Vec` leaf, which the inline-payload machinery gives an
/// owner. It does NOT hold for a leaf bound out of a NESTED user-enum
/// variant inside the payload: `bind_pattern_values`'
/// `is_copy_supported_user_struct` refuses every `Option`/`Result`
/// scrutinee, so the leaf got no `track_struct_var` and the payload's heap
/// was owned by nobody once the tag was zeroed.
///
/// Measured on this program: 243 B in 27 blocks at `-O0` (245 allocs / 218
/// frees) and 27 B in 3 blocks at `-O2`, clean at both after. `-O2` does
/// NOT hide it, unlike the sibling class B-2026-09-07-44 records.
///
/// Eleven cells. The three that LEAKED — 9 blocks each, `R2`'s three
/// `String`s over three calls:
///
///   - `field`: the by-value param spelling the row was filed on;
///   - `lo`: the same shape over a plain LOCAL, which is what proves the
///     defect is not about params at all — the row was filed as a param bug
///     and it is not one;
///   - `handsout`: the arm binds the leaf and passes it to a CONSUMING
///     call. Pinned because it looks like the double-free direction and is
///     not: it leaked before this fix too, and the arm's own move-out
///     retraction (`zero_struct_move_caps`) covers the new registration.
///
/// The eight that were ALREADY CLEAN, every one a direction this fix could
/// plausibly have turned into a double free:
///
///   - `untouched`: the body never matches the field, so no disarm fires
///     and the struct's own field drop does all of it;
///   - `wildcard` (`Some(_)`) and `variantwild` (`Some(K.A(_))`): the pair
///     that isolates the BINDING as the trigger — the same arm shapes
///     differing only in whether a name is bound. `variantwild` is also the
///     population an unconditional box-only drop broke (five LSan failures,
///     recorded on B-2026-08-07-7);
///   - `whole` (`Some(kk)` then an inner match): the division-of-ownership
///     shape `asan_b04_7_option_heap_enum_struct_field_drop_no_leak` guards
///     — the binding owns the payload and the struct's drop owns the box,
///     and the suppressor's own comment says zeroing the tag there orphans
///     both;
///   - `stringpay`: a `String` leaf. Clean before AND after, and it is the
///     cell that rules out the alternative fix: attaching the interior to
///     the parked `boxenv` action would have double-freed here, because a
///     `String` leaf already has an owner;
///   - `bl`: a BARE `Option[K]` local with the identical arm. Clean
///     throughout — its let-site keeps its own owner, which is why the
///     defect needs the field;
///   - `rebind` (`let m = r`): the rebind channel, retracted by name;
///   - `w`/`f` at `-O2` as well as `-O0`, via the shared runner.
///
/// DELIBERATELY NOT A CELL: the `Result` spelling
/// (`struct HolderR { k: Result[K, i64] }`) leaks 240 B in 3 blocks plus 36
/// B indirect, and is UNCHANGED by this fix — measured byte-identical on
/// the tree before and after. It is a different defect (the field gets no
/// drop registered at all, rather than one that is disarmed), the
/// suppressor above is `Option`-only by construction
/// (`option_payload_te` requires the head), and it is filed on its own row.
/// Pinning it here would fail this fixture for a defect it does not own.
#[test]
fn asan_struct_field_boxed_payload_arm_leaf_owns_its_interior() {
    assert_clean_asan_run(
        r#"
struct R2 { s: String, t: String, u: String }
enum K { A(R2), B }
enum Ks { A(String), B }
struct Holder { k: Option[K], n: i64 }
struct HolderS { k: Option[Ks], n: i64 }

fn mkr(i: i64) -> R2 { return R2 { s: f"ssssssss{i}", t: f"tttttttt{i}", u: f"uuuuuuuu{i}" }; }
fn eat(r: R2) -> i64 { return r.s.len(); }

fn field(h: Holder) {
    match h.k { Option.Some(K.A(r)) => { println(f"f:{r.s}"); } Option.Some(K.B) => {} Option.None => {} }
}
fn untouched(h: Holder) { println(f"u:{h.n}"); }
fn wildcard(h: Holder) {
    match h.k { Option.Some(_) => { println("wc"); } Option.None => {} }
}
fn variantwild(h: Holder) {
    match h.k { Option.Some(K.A(_)) => { println("vw"); } Option.Some(K.B) => {} Option.None => {} }
}
fn whole(h: Holder) {
    match h.k { Option.Some(kk) => { match kk { K.A(r) => { println(f"w:{r.s}"); } K.B => {} } } Option.None => {} }
}
fn handsout(h: Holder) {
    match h.k { Option.Some(K.A(r)) => { println(f"h:{eat(r)}"); } Option.Some(K.B) => {} Option.None => {} }
}
fn rebind(h: Holder) {
    match h.k { Option.Some(K.A(r)) => { let m = r; println(f"rb:{m.s}"); } Option.Some(K.B) => {} Option.None => {} }
}
fn stringpay(h: HolderS) {
    match h.k { Option.Some(Ks.A(s)) => { println(f"sp:{s}"); } Option.Some(Ks.B) => {} Option.None => {} }
}

fn main() {
    let mut i = 0;
    while i < 3 {
        field(Holder { k: Option.Some(K.A(mkr(i))), n: i });
        untouched(Holder { k: Option.Some(K.A(mkr(i))), n: i });
        wildcard(Holder { k: Option.Some(K.A(mkr(i))), n: i });
        variantwild(Holder { k: Option.Some(K.A(mkr(i))), n: i });
        whole(Holder { k: Option.Some(K.A(mkr(i))), n: i });
        handsout(Holder { k: Option.Some(K.A(mkr(i))), n: i });
        rebind(Holder { k: Option.Some(K.A(mkr(i))), n: i });
        stringpay(HolderS { k: Option.Some(Ks.A(f"pppppppp{i}")), n: i });
        let lh = Holder { k: Option.Some(K.A(mkr(i))), n: i };
        match lh.k { Option.Some(K.A(r)) => { println(f"lo:{r.s}"); } Option.Some(K.B) => {} Option.None => {} }
        let bl: Option[K] = Option.Some(K.A(mkr(i)));
        match bl { Option.Some(K.A(r)) => { println(f"bl:{r.s}"); } Option.Some(K.B) => {} Option.None => {} }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "f:ssssssss0",
            "u:0",
            "wc",
            "vw",
            "w:ssssssss0",
            "h:9",
            "rb:ssssssss0",
            "sp:pppppppp0",
            "lo:ssssssss0",
            "bl:ssssssss0",
            "f:ssssssss1",
            "u:1",
            "wc",
            "vw",
            "w:ssssssss1",
            "h:9",
            "rb:ssssssss1",
            "sp:pppppppp1",
            "lo:ssssssss1",
            "bl:ssssssss1",
            "f:ssssssss2",
            "u:2",
            "wc",
            "vw",
            "w:ssssssss2",
            "h:9",
            "rb:ssssssss2",
            "sp:pppppppp2",
            "lo:ssssssss2",
            "bl:ssssssss2",
            "end",
        ],
        "asan_struct_field_boxed_payload_arm_leaf_owns_its_interior",
    );
}

/// B-2026-09-20-44's memory twin — the heap half of the body-loss row, and
/// it is here as a HARM GUARD rather than as the row's own instrument.
///
/// The defect this fixture's E2E sibling pins is a PURE body loss: valgrind
/// read `0 errors, all heap blocks freed` on every losing cell before the
/// fix, so no memory gate and neither ASAN ratchet could see it. What they
/// CAN see is the failure mode a repair here risks — the bodies mask
/// standing down where it is not the sole channel, which doubles a body and
/// can hand the same buffer two owners. Nine of this program's twelve rows
/// are guards that read identically on both arms for exactly that reason.
/// The stdout assertion carries the body COUNT, so a doubled body fails
/// here as a line mismatch even when the heap stays balanced.
#[test]
fn asan_generic_enum_arm_primitive_field_read_runs_payload_drop_body() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Rn { id: i64 }
impl Drop for Rn { fn drop(mut ref self) { println(f"dRn{self.id}") } }
struct Rw { id: i64, a: String, b: String }
impl Drop for Rw { fn drop(mut ref self) { println(f"dRw{self.id}") } }
struct Wd { a: R, b: String, c: String }
enum G[T] { X(T), Y }
enum Ng { X(R), Y }

fn ret(g: G[R]) -> i64 { match g { G.X(v) => { return v.id }, G.Y => { return 0 } } }
fn letret(g: G[R]) -> i64 { match g { G.X(v) => { let x: i64 = v.id; return x }, G.Y => { return 0 } } }
fn prnt(g: G[R]) -> i64 { match g { G.X(v) => { println(f"use{v.id}"); return 0 }, G.Y => { return 0 } } }
fn none(g: G[R]) -> i64 { match g { G.X(v) => { return 0 }, G.Y => { return 0 } } }
fn whole(g: G[R]) -> R { match g { G.X(v) => { return v }, G.Y => { return R { id: 0, s: f"b2044-zzzzzzzzzzzzzzzzzzzzzzzz" } } } }
fn narrow(g: G[Rn]) -> i64 { match g { G.X(v) => { return v.id }, G.Y => { return 0 } } }
fn wide(g: G[Rw]) -> i64 { match g { G.X(v) => { return v.id }, G.Y => { return 0 } } }
fn ngret(g: Ng) -> i64 { match g { Ng.X(v) => { return v.id }, Ng.Y => { return 0 } } }
fn ngprnt(g: Ng) -> i64 { match g { Ng.X(v) => { println(f"nguse{v.id}"); return 0 }, Ng.Y => { return 0 } } }
fn nest(g: G[Wd]) -> i64 { match g { G.X(v) => { return v.a.id }, G.Y => { return 0 } } }

fn main() {
    { let a1: R = R { id: 1, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w1: G[R] = G.X(a1); println(f"ret={ret(w1)}") }
    { let a2: R = R { id: 2, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w2: G[R] = G.X(a2); println(f"letret={letret(w2)}") }
    { let a3: R = R { id: 3, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w3: G[R] = G.X(a3); println(f"prnt={prnt(w3)}") }
    { let a4: R = R { id: 4, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w4: G[R] = G.X(a4); println(f"none={none(w4)}") }
    { let a5: R = R { id: 5, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w5: G[R] = G.X(a5); let q: R = whole(w5); println(f"whole={q.id}") }
    { let a6: Rn = Rn { id: 6 }; let w6: G[Rn] = G.X(a6); println(f"narrow={narrow(w6)}") }
    { let a7: Rw = Rw { id: 7, a: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa", b: f"b2044-bbbbbbbbbbbbbbbbbbbbbbbb" }; let w7: G[Rw] = G.X(a7); println(f"wide={wide(w7)}") }
    { let a8: R = R { id: 8, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w8: Ng = Ng.X(a8); println(f"ngret={ngret(w8)}") }
    { let a9: R = R { id: 9, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w9: Ng = Ng.X(a9); println(f"ngprnt={ngprnt(w9)}") }
    { let aa: Wd = Wd { a: R { id: 10, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }, b: f"b2044-bbbbbbbbbbbbbbbbbbbbbbbb", c: f"b2044-cccccccccccccccccccccccc" }; let wa: G[Wd] = G.X(aa); println(f"nest={nest(wa)}") }
    { let ab: R = R { id: 11, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let wb: G[R] = G.X(ab); let n: i64 = match wb { G.X(v) => { v.id }, G.Y => { 0 } }; println(f"local={n}") }
    println("end");
}
"#,
        &[
            "dR1", "ret=1", "dR2", "letret=2", "use3", "dR3", "prnt=0", "dR4", "none=0", "whole=5",
            "dR5", "narrow=6", "dRn6", "dRw7", "wide=7", "ngret=8", "dR8", "nguse9", "ngprnt=0",
            "dR9", "dR10", "nest=10", "dR11", "local=11", "end",
        ],
        "asan_generic_enum_arm_primitive_field_read_runs_payload_drop_body",
    );
}

/// B-2026-09-13-9 — a nested destructure of a BOXED enum payload whose leaf
/// is WIDER than the envelope's payload area, where the arm MOVES the leaf.
///
/// B-2026-09-12-25 leg 2 gave `suppress_nested_boxed_payload_cleanup` a
/// width ceiling: a leaf at or under the area (3 words for `Option`, 5 for
/// `Result`) is materialised as an owning copy and the box must stand down
/// for it, while a WIDER leaf is a view into the box that owns nothing, so
/// the box must keep freeing it. That is correct for a READ-ONLY arm — it is
/// what the four fixtures leg 2 repaired all measure — and it is blind to
/// the other axis: a REBIND or a move-out mints a second owner for the same
/// buffers at ANY width, and nothing else stood the box down for it.
///
/// Every cell below aborted at `KARAC_OPT_LEVEL=0`, at `-O2`, and under
/// auto-par, while `--interp` printed the expected lines and exited 0. The
/// counts are valgrind's at `-O0`, 9-word leaf unless noted:
///
/// ```text
///   let m = r;                 3 errors   (2 at a 6-word leaf)
///   acc.push(r)  mut ref Vec   5 errors
///   return r     from the arm  3 errors
///   acc.held = r mut ref field 7 errors
/// ```
///
/// The fix exempts exactly the positions the arm body MOVES from the
/// ceiling, so the read-only shapes keep it. Cells 5-7 are the controls that
/// pin that: a read-only arm at the same width, `eat(r)` (by-value to a
/// callee, clean before the fix because the callee's entry copy is not a
/// second owner of the box's buffers), and a rebind at a leaf that FITS the
/// area — the shape leg 2 already fixed, which must stay fixed.
#[test]
fn asan_wide_boxed_enum_leaf_moved_out_of_an_arm_is_freed_once() {
    const PRE: &str = "struct R3 { s: String, t: String, u: String }\n\
             enum Ke { A(R3), B }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn mk(i: i64) -> R3 { return R3 { s: f\"ssssssss{i}{seed()}\", t: f\"tttttttt{i}\", u: f\"uuuuuuuu{i}\" }; }\n";

    // 1 — the row's own cell: an immutable rebind of the wide leaf.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn f(x: Option[Ke]) {{ match x {{ Option.Some(Ke.A(r)) => {{ let m = r; println(f\"rb:{{m.s}}\"); }} _ => {{}} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{ f(Option.Some(Ke.A(mk(i)))); i = i + 1; }} }}\n"
            ),
            &["rb:ssssssss01", "rb:ssssssss11"],
            "b9-wide-leaf-rebind",
        );

    // 2 — the leaf pushed into a `mut ref` container, which outlives the arm.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn f(x: Option[Ke], acc: mut ref Vec[R3]) {{ match x {{ Option.Some(Ke.A(r)) => {{ acc.push(r); }} _ => {{}} }} }}\n\
                 fn main() {{ let mut acc: Vec[R3] = Vec.new(); let mut i = 0;\n\
                 \x20  while i < 2 {{ f(Option.Some(Ke.A(mk(i))), mut acc); i = i + 1; }}\n\
                 \x20  println(f\"rb:{{acc[0].s}}\"); }}\n"
            ),
            &["rb:ssssssss01"],
            "b9-wide-leaf-push-mut-ref-vec",
        );

    // 3 — the leaf RETURNED out of the arm.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn f(x: Option[Ke]) -> R3 {{ match x {{ Option.Some(Ke.A(r)) => {{ return r; }} _ => {{ return mk(9); }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{ let g = f(Option.Some(Ke.A(mk(i)))); println(f\"rb:{{g.s}}\"); i = i + 1; }} }}\n"
            ),
            &["rb:ssssssss01", "rb:ssssssss11"],
            "b9-wide-leaf-arm-return",
        );

    // 4 — the leaf assigned into a `mut ref` struct field.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 struct Holder {{ held: R3 }}\n\
                 fn f(x: Option[Ke], acc: mut ref Holder) {{ match x {{ Option.Some(Ke.A(r)) => {{ acc.held = r; }} _ => {{}} }} }}\n\
                 fn main() {{ let mut h = Holder {{ held: mk(9) }}; let mut i = 0;\n\
                 \x20  while i < 2 {{ f(Option.Some(Ke.A(mk(i))), mut h); i = i + 1; }}\n\
                 \x20  println(f\"rb:{{h.held.s}}\"); }}\n"
            ),
            &["rb:ssssssss11"],
            "b9-wide-leaf-mut-ref-field-assign",
        );

    // 5 — CONTROL, read-only arm at the same width. Clean BEFORE the fix and
    //     the shape leg 2's ceiling exists to protect: the box is the single
    //     owner and exempting this position would LEAK.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn f(x: Option[Ke]) {{ match x {{ Option.Some(Ke.A(r)) => {{ println(f\"rb:{{r.s}}\"); }} _ => {{}} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{ f(Option.Some(Ke.A(mk(i)))); i = i + 1; }} }}\n"
            ),
            &["rb:ssssssss01", "rb:ssssssss11"],
            "b9-wide-leaf-readonly-control",
        );

    // 6 — CONTROL, the leaf handed BY VALUE to a callee. Clean before the
    //     fix: the callee's entry copy is not a second owner of the box's
    //     buffers, which is what puts the axis on a rebind or a transfer
    //     into storage rather than on "any consume".
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn eat(v: R3) {{ println(f\"rb:{{v.s}}\"); }}\n\
                 fn f(x: Option[Ke]) {{ match x {{ Option.Some(Ke.A(r)) => {{ eat(r); }} _ => {{}} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{ f(Option.Some(Ke.A(mk(i)))); i = i + 1; }} }}\n"
            ),
            &["rb:ssssssss01", "rb:ssssssss11"],
            "b9-wide-leaf-callee-by-value-control",
        );

    // 7 — CONTROL, a rebind at a leaf that FITS `Option`'s 3-word area. This
    //     is leg 2's own shape; it must stay fixed.
    assert_clean_asan_run(
            "struct R1 { s: String }\n\
             enum Kn { A(R1), B }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn mk1(i: i64) -> R1 { return R1 { s: f\"ssssssss{i}{seed()}\" }; }\n\
             fn f(x: Option[Kn]) { match x { Option.Some(Kn.A(r)) => { let m = r; println(f\"rb:{m.s}\"); } _ => {} } }\n\
             fn main() { let mut i = 0; while i < 2 { f(Option.Some(Kn.A(mk1(i)))); i = i + 1; } }\n",
            &["rb:ssssssss01", "rb:ssssssss11"],
            "b9-narrow-leaf-rebind-control",
        );
}

/// B-2026-09-14-26 — a match arm that CONSUMES a whole-TUPLE enum payload
/// leaked its elements: `500 B in 18 blocks`, `111 allocs / 93 frees` over
/// the ten cells below, exit 0 with the RIGHT output on every surface, so
/// nothing but a leak checker could see it.
///
/// WHY ONLY A TUPLE. The disarm at the arm is correct and shared by every
/// payload shape: `suppress_destructured_enum_payload_cleanup_at_limited`
/// zeroes the source's payload words for each position the pattern moves
/// out, on the stated understanding that "the bound binding's own cleanup
/// frees it once". For `String`/`Vec`, a user struct and an `Array` that
/// understanding holds — `bind_pattern_values` registers the binding on the
/// buffer, struct or array channel, each keyed on a TYPE NAME. A tuple has
/// no type name, so it reached none of them and the zeroing handed its
/// buffers to nobody. Measured directly: the `String`, `Vec[String]`,
/// `Array[String, 2]` and user-struct payloads are all clean on the same
/// free-fn-arg arm that loses 132 B for `(String, String)`.
///
/// WHY THE ROW'S OWN CANDIDATE WAS NOT IT, which its `NOT MEASURED` note
/// half-predicted. `clear_boxed_enum_inner_drop` /
/// `boxed_payload_interior_taken_by_arm` govern a BOXED payload, and a
/// 6-word `(String, String)` fits `T`'s inline payload area — the enum is a
/// flat `{i64 x 7}` and no box exists to retract. The backtrace from the
/// zeroing site named `compile_match` directly.
///
/// THE REPAIR registers the binding's own `synthesize_tuple_drop_fn_te`
/// drop at the one point the source gives the position up, so both halves
/// of the hand-off are written in the same place. It is balanced rather
/// than doubled because an arm that mints a SECOND owner reaches the
/// ordinary source-disarm helpers on that move and stands the new action
/// down with it — which is why `rebind`, `structlit`, `push` and the
/// per-element `destructure` cells are pinned here: every one of them was
/// already clean and has the identical alloc/free count after.
///
/// DELIBERATELY NOT INCLUDED: an owned-PARAM scrutinee
/// (`fn f(e: T) { match e { T.A(p) => take(p) } }`). This fix halves its
/// leak (220 B / 8 blocks → 110 B / 4) and does not close it, and the
/// residual is NOT tuple-specific — the same cell loses 58 B with a
/// `String` payload, 110 B with a user struct and 96 B with a
/// `Vec[String]`, all of them clean under a LOCAL scrutinee. That is a
/// different defect with its own row (B-2026-09-16-13) rather than a
/// half-measured widening of this one.
#[test]
fn asan_consuming_arm_over_a_whole_tuple_enum_payload_frees_its_elements() {
    assert_clean_asan_run(
        r#"
enum T { A((String, String)), B }
enum U { A((String, i64)), B }
struct H { p: (String, String) }

fn mk(n: i64) -> T {
    return T.A((f"b1426-left-aaaaaaaaaaaaaaaa-{n}", f"b1426-right-bbbbbbbbbbbb-{n}"));
}
fn mku(n: i64) -> U { return U.A((f"b1426-mixed-cccccccccccccccc-{n}", n)); }

fn take(p: (String, String)) -> i64 { return p.0.len() + p.1.len(); }
fn takeu(p: (String, i64)) -> i64 { return p.0.len() + p.1; }
fn takes(s: String) -> i64 { return s.len(); }

struct Sink { n: i64 }
impl Sink { fn eat(mut ref self, p: (String, String)) -> i64 { self.n = self.n + 1; return p.0.len(); } }

fn main() {
    let mut s = Sink { n: 0 };
    let mut i: i64 = 0;
    while i < 2 {
        let e1: T = mk(i);
        match e1 {
            T.A(p) => { println(f"freefn:{take(p)}"); }
            T.B => {}
        }

        match mk(i) {
            T.A(p) => { println(f"freshtemp:{take(p)}"); }
            T.B => {}
        }

        let e2: T = mk(i);
        if let T.A(p) = e2 { println(f"iflet:{take(p)}"); }

        let e3: T = mk(i);
        match e3 {
            T.A(p) => { println(f"methodarg:{s.eat(p)}"); }
            T.B => {}
        }

        let e4: U = mku(i);
        match e4 {
            U.A(p) => { println(f"mixed:{takeu(p)}"); }
            U.B => {}
        }

        let e5: T = mk(i);
        match e5 {
            T.A(p) => { println(f"readonly:{p.1.len()}"); }
            T.B => {}
        }

        let e6: T = mk(i);
        match e6 {
            T.A(p) => { let q = p; println(f"rebind:{take(q)}"); }
            T.B => {}
        }

        let e7: T = mk(i);
        match e7 {
            T.A(p) => { let h = H { p: p }; println(f"structlit:{h.p.0.len()}"); }
            T.B => {}
        }

        let e8: T = mk(i);
        let mut v: Vec[(String, String)] = Vec.new();
        match e8 {
            T.A(p) => { v.push(p); }
            T.B => {}
        }
        println(f"push:{v.len()}");

        let e9: T = mk(i);
        match e9 {
            T.A((a, b)) => { println(f"destructure:{takes(a) + takes(b)}"); }
            T.B => {}
        }

        i = i + 1;
    }
    println(f"sink:{s.n}");
}
"#,
        &[
            "freefn:55",
            "freshtemp:55",
            "iflet:55",
            "methodarg:29",
            "mixed:30",
            "readonly:26",
            "rebind:55",
            "structlit:29",
            "push:1",
            "destructure:55",
            "freefn:55",
            "freshtemp:55",
            "iflet:55",
            "methodarg:29",
            "mixed:31",
            "readonly:26",
            "rebind:55",
            "structlit:29",
            "push:1",
            "destructure:55",
            "sink:2",
        ],
        "asan_consuming_arm_over_a_whole_tuple_enum_payload_frees_its_elements",
    );
}

#[test]
fn asan_a_freshtemp_boxed_payload_binding_the_arm_moves_on_has_one_owner() {
    // B-2026-09-13-21. A whole-payload binding taken out of a HEAP-BOXED
    // `Option`/`Result` and then MOVED onward had two owners on every
    // compiled backend: the destination it was moved into, and the box's
    // own interior drop. `free(): double free detected in tcache 2`, no
    // output at all, while `--interp` printed the right answer.
    //
    // The registration (B-2026-07-18-3) was sound for a NAMED scrutinee
    // because `retract_boxed_tuple_inner_drop_for_arm` stands the interior
    // drop down when the arm takes ownership. It could not reach a
    // FRESH-TEMP scrutinee: `boxed_tuple_payload_arm_takes_ownership`
    // opened with `let ExprKind::Identifier(name) = &scrutinee.kind else {
    // return None; }`, and a fresh temp has no identifier. The fix resolves
    // the name the way the fresh-temp registration actually keys its box --
    // by ENUM NAME.
    //
    // MEASURED on 4c544f2 over `Map[i64, (String, String)]`, interpreter
    // vs the three compiled surfaces. The named/fresh-temp pair is what
    // localized it:
    //
    //     match m.remove(k) { Some(a) => { let b = a; .. } }  ABORT -> ok
    //     match m.remove(k) { Some(a) => { keep.push(a) } }   ABORT -> ok
    //     match m.remove(k) { Some(a) => { m.insert(2, a) } } ABORT -> ok
    //     match m.remove(k) { Some(a) => { Bx { a: a, .. } } } ABORT -> ok
    //     match mk(1)       { Some(a) => { keep.push(a) } }   ABORT -> ok
    //     let o = m.remove(k); match o { .. push }            ok    -> ok
    //     match m.remove(k) { Some(a) => { println(a.0) } }   ok    -> ok
    //     match m.remove(k) { Some(a) => { eat(a) } }         ok    -> ok
    //
    // `eat(a)` was clean for a reason that is NOT the retraction, and the
    // two must not be confused: a copy-supported tuple param is
    // entry-copied by the callee, so no second owner is ever minted. The
    // whole-STRUCT payload branch was unaffected throughout (read, rebind
    // and push all clean) because a struct interior reaches the box through
    // `boxed_struct_payload_vars` and its own move-out mirror.
    //
    // 1 -- THE MINIMAL CASE: a plain rebind. No container, no callee.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, (String, String)] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, (f\"aaaaaaaaaaaa1\", f\"bbbbbbbbbbbb1\"));\n\
             \x20\x20\x20\x20match m.remove(1) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { let b = a; println(f\"r:{b.0}\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"r:none\"); }\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"m:{m.len()}\");\n\
             }\n",
        &["r:aaaaaaaaaaaa1", "m:0"],
        "freshtemp-boxed-tuple-arm-rebind",
    );
    // 2 -- moved back into the SAME container the box came from, which is
    //      the spelling most likely to be misclassified.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, (String, String)] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, (f\"aaaaaaaaaaaa1\", f\"bbbbbbbbbbbb1\"));\n\
             \x20\x20\x20\x20match m.remove(1) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { m.insert(2, a); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"none\"); }\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20match m.get(2) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(v) => { println(f\"g:{v.0}\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"g:missing\"); }\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"m:{m.len()}\");\n\
             }\n",
        &["g:aaaaaaaaaaaa1", "m:1"],
        "freshtemp-boxed-tuple-arm-reinsert",
    );
    // 3 -- moved into a STRUCT LITERAL field.
    assert_clean_asan_run(
            "struct Bx21 { a: (String, String), n: i64 }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, (String, String)] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, (f\"aaaaaaaaaaaa1\", f\"bbbbbbbbbbbb1\"));\n\
             \x20\x20\x20\x20match m.remove(1) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { let b = Bx21 { a: a, n: 5 }; println(f\"s:{b.a.0}:{b.n}\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"none\"); }\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"m:{m.len()}\");\n\
             }\n",
            &["s:aaaaaaaaaaaa1:5", "m:0"],
            "freshtemp-boxed-tuple-arm-struct-literal",
        );
    // 4 -- a PLAIN CALL scrutinee, no `Map` in the program. The fix is not
    //      map-shaped; the fresh temp is what matters.
    assert_clean_asan_run(
        "fn mk21(n: i64) -> Option[(String, String)] {\n\
             \x20\x20\x20\x20if n < 0 { return None; }\n\
             \x20\x20\x20\x20return Some((f\"aaaaaaaaaaaa{n}\", f\"bbbbbbbbbbbb{n}\"));\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20match mk21(1) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { let b = a; println(f\"r:{b.0}\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"r:none\"); }\n\
             \x20\x20\x20\x20}\n\
             }\n",
        &["r:aaaaaaaaaaaa1"],
        "freshtemp-boxed-tuple-arm-plain-call",
    );
    // 5 -- THE NAMED SCRUTINEE, which was correct before and must stay so.
    //      It is the cell that fails if the fix ever widens the retraction
    //      past the fresh-temp case and stands a named binding's box down
    //      twice.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, (String, String)] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, (f\"aaaaaaaaaaaa1\", f\"bbbbbbbbbbbb1\"));\n\
             \x20\x20\x20\x20let o = m.remove(1);\n\
             \x20\x20\x20\x20match o {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { let b = a; println(f\"r:{b.0}\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"r:none\"); }\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"m:{m.len()}\");\n\
             }\n",
        &["r:aaaaaaaaaaaa1", "m:0"],
        "named-boxed-tuple-arm-rebind-control",
    );
    // 6 -- THE READ-ONLY CONTROL. The interior drop must still RUN here:
    //      nothing else owns it, so a fix that retracted unconditionally
    //      would turn this into a leak. LSan makes that a failure.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, (String, String)] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, (f\"aaaaaaaaaaaa1\", f\"bbbbbbbbbbbb1\"));\n\
             \x20\x20\x20\x20match m.remove(1) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { println(f\"r:{a.0}\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"r:none\"); }\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"m:{m.len()}\");\n\
             }\n",
        &["r:aaaaaaaaaaaa1", "m:0"],
        "freshtemp-boxed-tuple-arm-read-only-control",
    );
    // 7 -- the BY-VALUE CALL consumer. Clean before and after, and NOT
    //      because of the retraction: the callee entry-copies a
    //      copy-supported tuple param, so no second owner exists. Pinned so
    //      that reading it as evidence about the retraction is harder.
    assert_clean_asan_run(
        "fn eat21(t: (String, String)) -> i64 { return t.0.len(); }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, (String, String)] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, (f\"aaaaaaaaaaaa1\", f\"bbbbbbbbbbbb1\"));\n\
             \x20\x20\x20\x20match m.remove(1) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { println(f\"r:{eat21(a)}\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"r:none\"); }\n\
             \x20\x20\x20\x20}\n\
             }\n",
        &["r:13"],
        "freshtemp-boxed-tuple-arm-byvalue-call-control",
    );
    // 8 -- THE ARRAY PAYLOAD IS NOT COVERED, because its registration is
    //      not landing here. This row fixes the RETRACTION, and with it the
    //      three MOVE spellings are clean for an array as well -- measured.
    //      But `Some(a) => { eat(a) }` over
    //      `Map[i64, Array[String, 2]]` still aborts once the array
    //      registration is added, because a by-value array param is
    //      CALLEE-OWNED while a copy-supported tuple param is entry-copied,
    //      and `binding_only_borrowed` encodes the latter. So the array
    //      registration waits on a per-binding predicate choice; see
    //      `boxed_tuple_payload_arm_takes_ownership`'s note and
    //      B-2026-09-13-2 cell 1.
    //
    //      Nothing about an array is asserted here, and that is the point:
    //      this row's fix is the tuple retraction, whose absence was a
    //      DOUBLE FREE on unmodified main.
    // 9 -- THE `push` SPELLING IS NOT ASSERTED CLEAN, and the reason is
    //      not this row. `keep.push(a)` over `Vec[Array[String, 2]]` leaks
    //      132 B in 6 blocks here, and so does `keep.push(mk(i))` with NO
    //      `match` anywhere in the program -- 132 B in 6, the identical
    //      figure -- while the named-source spelling `let e = [..];
    //      keep.push(e)` is clean. That is B-2026-09-10-36, already open:
    //      a `Vec[Array[T, N]]` fed from a TEMPORARY loses every element
    //      buffer, and an arm-bound array is a temporary to `push`.
    //
    //      It is CORRECTNESS-clean on all four surfaces, which is what this
    //      row is about. Asserting it here would fail on a leak this row
    //      did not cause and cannot fix.
}

/// B-2026-09-15-10 — a `shared enum` (or `par enum`) whose variant payload
/// is a NAMELESS AGGREGATE heap-boxes that payload, and until this fix
/// nothing ever freed the box.
///
/// The declaration spells the payload `Array[T, N]`, i.e.
/// `Path(["Array"], [Type(T), Const(N)])`, so
/// `payload_word_count_for_type_expr`'s real-width arm — keyed on
/// `TypeKind::Array`, a kind only INFERENCE produces — never fires, the
/// field is sized at the conservative 1 word, and the pack side boxes it.
/// On a NON-shared enum the per-binding `BoxedEnumDrop` scope-exit
/// registration frees that box. On a shared one
/// `user_enum_boxed_payload_variants` bails out, correctly: the box is
/// per-CONSTRUCTION and shared by every handle, so a per-binding
/// registration would free it once per handle. That left it owned by
/// nobody.
///
/// The free now sits where the last handle is known —
/// `emit_rc_dec_guarded`'s `rc_free` block and its atomic twin
/// `emit_arc_dec`'s `arc_free` — as a tag switch over the box's own tag
/// word, driven by the `EnumDropKind::BoxedArray` entries the pack side
/// already wrote into `EnumLayout::field_drop_kinds`. Additive-safe
/// precisely because the box had no owner before.
///
/// ENVELOPE ONLY, DELIBERATELY. The constructor excludes shared enums from
/// `disarm_array_sources`, so a NAMED array local keeps its own element
/// drop and frees the interior itself; walking the interior here would
/// double-free that spelling. Every cell below therefore sources its
/// payload from a named local. The FRESH-TEMP spellings keep an indirect
/// residual that this fix converts from indirect to direct rather than
/// reclaiming (`Sh.S(mka("x"))`: 48 direct + 34 indirect → 34 direct), and
/// they are deliberately NOT here — asserting them clean would tie this
/// guard to a row that is still open.
///
/// Measured at `KARAC_OPT_LEVEL=0` under `valgrind --leak-check=full`,
/// pre-fix → post-fix, definitely-lost bytes, with a marker `grep -c`
/// printing zero on the unfixed tree first. No cell reports an invalid
/// read, invalid free or mismatched free on either side — checked per cell
/// rather than read off `ERROR SUMMARY`, which counts a leak record as an
/// error once `--leak-check=full` is on and so cannot tell the two apart:
///
///     Array[String, 2], named local        48 → 0
///     Array[i64, 2], no interior at all     16 → 0
///     two handles, `let s2 = s1`            48 → 0   (ONE free, not two)
///     two boxing variants A and B           72 → 0   (48 + 24, one per arm)
///     nested Array[Array[String, 2], 2]     96 → 0
///     `par enum`, named local, Arc path     48 → 0
///
/// THE RC CELLS ARE A GATE ON `scripts/asan-o0-leg.sh`, NOT ON THE DEFAULT
/// `--features llvm` LEG, and that is measured rather than assumed. On the
/// unfixed tree at the harness default opt level the first five cells PASS
/// — LLVM deletes allocations nothing observes — and the fixture fails at
/// the sixth, the `par` cell, whose atomics survive that. At
/// `KARAC_OPT_LEVEL=0` the same fixture fails at the FIRST cell, so every
/// cell is live there. A green default-leg run on this fixture alone is
/// therefore evidence about the Arc path only.
///
/// The two-handles cell is what pins the PLACEMENT: the box is one per
/// construction, so a per-binding registration would have double-freed it
/// where this leaks. The `par` cell is what forced the second call site —
/// a `par enum` is registered in `shared_types` and boxes identically
/// (`EnumLayout`'s `is_shared` is `e.is_shared || e.is_par`) but releases
/// through `emit_arc_dec`, so the `emit_rc_dec_guarded`-only patch left
/// every `par` spelling leaking. Delete either call and this reddens.
///
/// NO CELL CONSTRUCTS A UNIT VARIANT, and that omission is load-bearing
/// rather than incidental: `Sh.N` alone strands 24 B — `{rc, tag, w0}`,
/// the RC SHELL rather than any payload box — byte-identical before and
/// after this change. That is B-2026-09-17-22, and a cell carrying it would make
/// this fixture red for a reason that is not its own.
/// B-2026-09-10-23 — a whole-payload arm binding over a BY-VALUE
/// `Option[(W, i64)]` param leaked the tuple element's INTERIOR.
///
/// 2 B in 1 block at `-O0` for `struct W { id: i64, name: String }`, the
/// `String` being the only allocation of that size in the program. Output
/// was correct on every surface, so the leak was the only observable.
///
/// THE MECHANISM IS A RETRACTION FIRING ON A FALSE PREMISE, and the
/// `Some(_)` cell below is what localises it: the param's own cleanup is
/// perfectly capable of freeing the interior and does so when no arm binds
/// the payload. When an arm DOES bind it,
/// `retract_boxed_tuple_inner_drop_for_arm` stands the box's interior
/// walker down on the premise that the binding now owns the interior — and
/// the binding registers BODIES only, never a memory owner. The premise was
/// decided by the syntactic classifier, which calls every projection off the
/// binding a partial move, so `return t.1` on an `i64` read as a take. The
/// retraction now asks a leaf-aware predicate restricted to PRIMITIVE
/// leaves, where a wrong answer cannot cost a double free.
///
/// THE LOSS SCALES WITH THE INTERIOR, which is what says the whole interior
/// was stranded rather than one particular buffer — and is more than the row
/// recorded: `vecelem` lost 32 B for a `Vec[i64]` element and `twostr` lost
/// 12 B in 2 blocks for two `String` fields. Both are cells here.
///
/// THE CONTROLS ARE THE ROW'S OWN, plus one it listed as unmeasured. `wild`
/// (`Some(_)`), `nomatch` (no match at all) and `nolocal` (the same match
/// over a LOCAL) were clean before the fix and must stay clean — they are
/// what establish that BOTH the by-value param and the arm binding are
/// required. `refparam` answers the unmeasured item: a `ref` parameter mode
/// is clean, which confirms the by-value entry copy is the mechanism.
/// `structpay` is the already-fixed named-struct sibling (B-2026-09-07-44),
/// whose arm registers a `StructDrop` for exactly this purpose.
///
/// THREE SPELLINGS STILL LEAK AND ARE NOT THIS ROW, each through a
/// different path, all measured at 2 B:
///   * the GENERIC leg (`fn take[T](o: Option[(T, i64)])`) — no retraction
///     fires there at all; it also loses the element's `Drop` body, which
///     is the boxed-generic gap B-2026-09-17-23 records (the monomorph
///     prologue runs none of the by-value param arms).
///   * the `Result` leg — the body runs correctly on all four surfaces and
///     the interior still leaks, so it is a third owner path.
///     B-2026-09-17-26.
///   * the DESTRUCTURING arm (`Some((a, b))`) — the admission test retracts
///     UNCONDITIONALLY for a tuple pattern, on the stated premise that "each
///     heap element gets its own `track_vec_var` owner". True for a
///     `Vec`/`String` element, false for a user-STRUCT element.
///     B-2026-09-17-27.
///
/// Keeping them out of this fixture is deliberate: each would redden the
/// suite for a defect its own row owns.
#[test]
fn asan_by_value_optres_tuple_param_arm_keeps_the_interior() {
    const W: &str = "struct W { id: i64, name: String }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}/{self.name}\") } }\n";

    // The row's own repro: the whole-payload binding, read through a scalar
    // projection.
    assert_clean_asan_run(
            &format!(
                "{W}fn take(o: Option[(W, i64)]) -> i64 {{ match o {{ Some(t) => {{ return t.1; }} None => {{ return 0; }} }} }}\n\
                 fn main() {{ println(f\"g{{take(Some((W {{ id: 3, name: f\"n3\" }}, 9)))}}\") }}\n"
            ),
            &["dW3/n3", "g9"],
            "b1023-whole-binding",
        );

    // The interior is a `Vec` buffer rather than a `String` — 32 B before
    // the fix, which is what shows the loss tracks the interior's size.
    assert_clean_asan_run(
            "struct W { id: i64, xs: Vec[i64] }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}/{self.xs.len()}\") } }\n\
             fn take(o: Option[(W, i64)]) -> i64 { match o { Some(t) => { return t.1; } None => { return 0; } } }\n\
             fn main() { let mut v: Vec[i64] = Vec.new(); v.push(7); v.push(8);\n\
             \x20           println(f\"g{take(Some((W { id: 3, xs: v }, 9)))}\") }\n",
            &["dW3/2", "g9"],
            "b1023-vec-interior",
        );

    // TWO heap fields, both stranded before the fix (12 B in 2 blocks).
    assert_clean_asan_run(
            "struct W { id: i64, a: String, b: String }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}/{self.a}/{self.b}\") } }\n\
             fn take(o: Option[(W, i64)]) -> i64 { match o { Some(t) => { return t.1; } None => { return 0; } } }\n\
             fn main() { println(f\"g{take(Some((W { id: 3, a: f\"aaaa1\", b: f\"bbbbbb2\" }, 9)))}\") }\n",
            &["dW3/aaaa1/bbbbbb2", "g9"],
            "b1023-two-heap-fields",
        );

    // ── controls, all clean BEFORE the fix ──────────────────────────────
    // No arm binds the payload: the param's own cleanup frees the interior.
    assert_clean_asan_run(
            &format!(
                "{W}fn take(o: Option[(W, i64)]) -> i64 {{ match o {{ Some(_) => {{ return 9; }} None => {{ return 0; }} }} }}\n\
                 fn main() {{ println(f\"g{{take(Some((W {{ id: 3, name: f\"n3\" }}, 9)))}}\") }}\n"
            ),
            &["dW3/n3", "g9"],
            "b1023-wildcard-arm",
        );
    // A by-value param with no match at all.
    assert_clean_asan_run(
        &format!(
                "{W}fn take(o: Option[(W, i64)]) -> i64 {{ return 9; }}\n\
                 fn main() {{ println(f\"g{{take(Some((W {{ id: 3, name: f\"n3\" }}, 9)))}}\") }}\n"
            ),
        &["dW3/n3", "g9"],
        "b1023-param-no-match",
    );
    // The same match over a LOCAL — the local owns the whole payload.
    assert_clean_asan_run(
            &format!(
                "{W}fn main() {{ let o: Option[(W, i64)] = Some((W {{ id: 3, name: f\"n3\" }}, 9));\n\
                 \x20           match o {{ Some(t) => {{ println(f\"g{{t.1}}\") }} None => {{ println(f\"n\") }} }} }}\n"
            ),
            &["g9", "dW3/n3"],
            "b1023-local-scrutinee",
        );
    // A `ref` parameter mode — the caller keeps the value, so no entry copy
    // and nothing to strand. The row listed this as unmeasured.
    assert_clean_asan_run(
            &format!(
                "{W}fn take(o: ref Option[(W, i64)]) -> i64 {{ match o {{ Some(t) => {{ return t.1; }} None => {{ return 0; }} }} }}\n\
                 fn main() {{ let o: Option[(W, i64)] = Some((W {{ id: 3, name: f\"n3\" }}, 9)); println(f\"g{{take(o)}}\") }}\n"
            ),
            &["g9", "dW3/n3"],
            "b1023-ref-param",
        );
    // The named-STRUCT payload sibling (B-2026-09-07-44), whose arm
    // registers a `StructDrop` — the arrangement the tuple envelope lacks.
    assert_clean_asan_run(
            &format!(
                "{W}fn take(o: Option[W]) -> i64 {{ match o {{ Some(t) => {{ return t.id; }} None => {{ return 0; }} }} }}\n\
                 fn main() {{ println(f\"g{{take(Some(W {{ id: 3, name: f\"n3\" }}))}}\") }}\n"
            ),
            &["dW3/n3", "g3"],
            "b1023-struct-payload",
        );
}

/// B-2026-09-14-18 — the MEMORY half of the two paired output fixtures.
///
/// The row itself is body-only: it measured `9-10 allocs with equal frees,
/// 0 errors, 0 bytes lost` on both its cells, and the fix moves who runs a
/// `Drop` BODY, which frees nothing. So this fixture is not re-measuring
/// the defect — it is guarding the fix's own mechanism, which is a mask.
///
/// Both backends now narrow a payload walk to the parts the callee did NOT
/// hand out. Codegen masks tuple elements out of the emitted walker
/// (`PayloadBodiesMask::TupleElems`); the interpreter REMOVES them from the
/// value the walk sees. A mask that takes too much loses a body, which the
/// output twins catch — and a mask that takes too little runs one twice,
/// which on a payload carrying HEAP is a double free rather than a
/// duplicate line. Every part here carries a `String` for exactly that
/// reason; the output fixtures' `R` is scalar and could not tell the two
/// apart.
///
/// The cells are the boundaries of the narrowing: one part out, every part
/// out, no part out, a part that is READ but not taken, and three parts
/// with the middle one leaving.
///
/// The WILDCARD position (`Some((_, b)) => return b`) belongs to this set
/// and is deliberately absent: the compiled backends return the `None`
/// arm's value for it, which reproduces on a clean checkout with this fix
/// reverted and is a wrong VALUE rather than a misplaced body. Filed
/// separately so this fixture keeps measuring one thing.
#[test]
fn asan_destructured_payload_mask_leaves_one_owner_per_part() {
    const DECLS: &str = "struct H { id: i64, s: String }\n\
             impl Drop for H { fn drop(mut ref self) { println(f\"dH{self.id}\") } }\n\
             fn oneOut(o: Option[(H, i64)]) -> i64 { match o { Option.Some((a, b)) => { return b; } Option.None => { return 0; } } }\n\
             fn dropOut(o: Option[(H, H)]) -> H { match o { Option.Some((a, b)) => { return a; } Option.None => { return H { id: 0, s: \"z\" }; } } }\n\
             fn bothOut(o: Option[(H, H)]) -> (H, H) { match o { Option.Some((a, b)) => { return (a, b); } Option.None => { return (H { id: 0, s: \"z\" }, H { id: 0, s: \"z\" }); } } }\n\
             fn noneOut(o: Option[(H, H)]) -> i64 { match o { Option.Some((a, b)) => { return a.id + b.id; } Option.None => { return 0; } } }\n\
             fn readOut(o: Option[(H, i64)]) -> i64 { match o { Option.Some((a, b)) => { println(f\"in{a.id}\"); return b; } Option.None => { return 0; } } }\n\
             fn midOut(o: Option[(H, H, H)]) -> H { match o { Option.Some((a, b, c)) => { return b; } Option.None => { return H { id: 0, s: \"z\" }; } } }\n";

    // One part out, and the sibling's `String` must still reach exactly one
    // owner.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: i64 = oneOut(Option.Some((H {{ id: 1, s: \"aaaaaaaaaaaa\" }}, 9))); println(f\"n{{g}}\"); }}\n\
                 \x20   {{ let g = dropOut(Option.Some((H {{ id: 2, s: \"bbbbbbbbbbbb\" }}, H {{ id: 3, s: \"cccccccccccc\" }}))); println(f\"n{{g.id}}\"); }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["dH1", "n9", "dH3", "n2", "dH2", "end"],
            "b91418-one-part-out",
        );

    // EVERY part out, and NO part out — the two ends the narrowing must
    // not touch. The first has nothing left for the caller's walk; the
    // second is the walk running in full.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g = bothOut(Option.Some((H {{ id: 4, s: \"dddddddddddd\" }}, H {{ id: 5, s: \"eeeeeeeeeeee\" }}))); println(f\"n{{g.0.id}}\"); }}\n\
                 \x20   {{ let g: i64 = noneOut(Option.Some((H {{ id: 6, s: \"ffffffffffff\" }}, H {{ id: 7, s: \"gggggggggggg\" }}))); println(f\"n{{g}}\"); }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["n4", "dH4", "dH5", "dH6", "dH7", "n13", "end"],
            "b91418-both-ends",
        );

    // A part the arm READS but does not take — a projection is not a
    // hand-off, so its body still belongs to the callee — beside a
    // three-part payload whose MIDDLE part leaves, the case an index set
    // gets wrong if it is read as a count.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: i64 = readOut(Option.Some((H {{ id: 8, s: \"hhhhhhhhhhhh\" }}, 9))); println(f\"n{{g}}\"); }}\n\
                 \x20   {{ let g = midOut(Option.Some((H {{ id: 9, s: \"iiiiiiiiiiii\" }}, H {{ id: 10, s: \"jjjjjjjjjjjj\" }}, H {{ id: 11, s: \"kkkkkkkkkkkk\" }}))); println(f\"n{{g.id}}\"); }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["in8", "dH8", "n9", "dH9", "dH11", "n10", "dH10", "end"],
            "b91418-read-and-middle",
        );
}

/// B-2026-09-19-57 — THE LEAK HALF OF THE EXTRA `Drop` BODY, WHICH THE ROW
/// LEFT AS ITS LAST UNANSWERED QUESTION.
///
/// `sink(fp(Some((P { .. }, P { .. }))))` over a BOXED `Option[(P, P)]`
/// whose elements each carry a `String` past inline capacity and a `Vec`
/// ran `a`'s `Drop` body twice on every compiled surface, once before the
/// consuming call. The row measured that the early body was NOT a
/// use-after-free — `sink` still read `r.name` back intact through 3200 B
/// of intervening churn — and then asked whether the pairing leaked, since
/// a body that runs twice while the memory drops once should not. Nothing
/// answered it: the row had no fixture anywhere in the tree.
///
/// `52602ba` (B-2026-09-19-34's fix) removed the early body. This cell is
/// the leak side of that, in a loop so a per-iteration leak accumulates
/// rather than rounding to one block, with both the `String` length and the
/// `Vec` length read in every printed line so a freed buffer cannot pass as
/// a live one.
///
/// The body half is `e2e_heap_payload_part_handed_out_of_an_arm_runs_one_body`
/// (tests/codegen.rs), which also carries the cell that refutes the row's
/// stated trigger: the discriminator is payload WIDTH, not heap.
#[test]
fn asan_heap_payload_part_handed_out_of_an_arm_no_leak() {
    assert_clean_asan_run(
        r#"
struct P { k: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"d{self.k}n{self.name.len()}x{self.xs.len()}") } }

fn fp(o: Option[(P, P)]) -> P { match o { Some(t) => { return t.0; } None => { return P { k: 0, name: "z", xs: [0] }; } } }
fn sink(r: P) { println(f"s{r.k}n{r.name.len()}x{r.xs.len()}") }

fn main() {
    let mut n = 0;
    while n < 3 {
        sink(fp(Some((P { k: n * 2, name: "alpha-padded-out-well-past-any-inline-capacity", xs: [n, n, n] }, P { k: n * 2 + 1, name: "beta-padded-out-well-past-any-inline-capacity-too", xs: [n, n] }))));
        n = n + 1;
    }
    println("end");
}
"#,
        &[
            "d1n49x2", "s0n46x3", "d0n46x3", "d3n49x2", "s2n46x3", "d2n46x3", "d5n49x2", "s4n46x3",
            "d4n46x3", "end",
        ],
        "b57-boxed-tuple-payload-part-handed-out",
    );
}

/// B-2026-09-21-1 — the `if let` / `let ... else` / `while let` twin of
/// `asan_freshtemp_struct_scrutinee_unbound_fields_are_freed` below.
///
/// That fixture covers the `match` spelling, which B-2026-09-16-18 fixed.
/// The other three kept losing the husk's field bodies AND leaking the
/// buffers under them — valgrind at `-O0` reported one 3-byte `name`
/// buffer definitely lost per construct — because the compiled channel was
/// gated to `match` on purpose: arming it alone would have converted a gap
/// all four surfaces shared into a run-vs-build divergence. Both backends
/// moved together in this row, so the buffers are freed here too.
///
/// `le_one` is the cell that looks wrong and is not: `let ... else` prints
/// `dR49 dR48` (husk, then binding) where `if let` prints `dR44 dR45`
/// (binding, then husk). One rule gives both — design.md ties a destructor
/// to its binding's live-range end and scopes a statement-position
/// temporary to its `;`, and the `let ... else` binding escapes the
/// statement while the `if let` binding does not.
///
/// `wl_none` runs three times so a husk leaked ONCE PER ITERATION shows as
/// three blocks rather than one; the stash is `take()`n at each drain,
/// which is what keeps one iteration's husk from reaching the next.
#[test]
fn asan_iflet_letelse_whilelet_freshtemp_husk_fields_are_freed() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"name-{i}-padding" }; }
struct S3 { a: R, b: R }
fn mks() -> S3 { return S3 { a: mk(61), b: mk(62) }; }

fn il_one() -> i64 { if let S3 { a, .. } = S3 { a: mk(44), b: mk(45) } { return a.id; } return 0; }
fn il_call() -> i64 { if let S3 { a, .. } = mks() { return a.id; } return 0; }
fn il_none() -> i64 { if let S3 { .. } = S3 { a: mk(46), b: mk(47) } { return 1; } return 0; }
fn le_one() -> i64 { let S3 { a, .. } = S3 { a: mk(48), b: mk(49) } else { return 0; }; return a.id; }
fn wl_none() -> i64 { let mut s: i64 = 0; while let S3 { .. } = S3 { a: mk(50), b: mk(51) } { s = 1; break; } return s; }

fn main() {
    println(f"I:{il_one()}");
    println(f"C:{il_call()}");
    println(f"N:{il_none()}");
    println(f"L:{le_one()}");
    let mut i = 0;
    while i < 3 {
        println(f"W:{wl_none()}");
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "dR44", "dR45", "I:44", "dR61", "dR62", "C:61", "dR47", "dR46", "N:1", "dR49", "dR48",
            "L:48", "dR51", "dR50", "W:1", "dR51", "dR50", "W:1", "dR51", "dR50", "W:1", "done",
        ],
        "asan_iflet_letelse_whilelet_freshtemp_husk_fields_are_freed",
        12,
    );
}

/// B-2026-09-21-2 — the MEMORY half of a GUARDED MULTI-ARM match over a
/// fresh-temp struct, which the body-count fixtures cannot see.
///
/// The arms are mutually exclusive, so no single compile-time mask suits
/// them all and the materializer took the UNION of what they bind. That
/// was not merely an over-mask: when the union covered every body-bearing
/// field — which two arms naming DIFFERENT fields do between them — the
/// materializer concluded the husk owed nothing and declined outright, so
/// the temp got no bodies walker AND no memory walk. valgrind at
/// `KARAC_OPT_LEVEL=0` measured `g_two` at 12 allocs / 11 frees with
/// `definitely lost: 3 bytes in 1 blocks`, and the three-field shape at
/// 13 / 11 and 6 bytes; every surface alike, so no A/B could see it either.
///
/// Each arm now stores its own walker in a slot the single fire site loads.
/// This program is 34 allocs / 34 frees, `ERROR SUMMARY: 0`, and identical
/// on all four surfaces.
///
/// WHY IT IS AN ASAN FIXTURE AND NOT ONLY A BODY-COUNT ONE: the two faults
/// are on separate channels and only one is visible to each instrument. A
/// lost `Drop` BODY frees nothing, so no sanitizer leg can see it; a leaked
/// BUFFER prints nothing, so no output comparison can. This construct had
/// both, which is why it needs a cell in each file.
///
/// `g_all`'s second arm binds every body-bearing field, so it owes nothing
/// and gets the no-op walker. It is here because the first shape of the fix
/// DECLINED that construct instead, which would have restored the lost body
/// and the leak for `g_all`'s other arm — an under-mask is a double body
/// and for a `drop()` that closes a handle a double close, so the direction
/// had to be proved rather than argued.
#[test]
fn asan_guarded_multi_arm_freshtemp_husk_fields_are_freed() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"name-{i}-padding" }; }
struct S3 { a: R, b: R }

fn g_two() -> i64 { match S3 { a: mk(80), b: mk(81) } { S3 { a, .. } if a.id > 900 => { return a.id; } S3 { b, .. } => { return b.id; } } }
fn g_first() -> i64 { match S3 { a: mk(82), b: mk(83) } { S3 { a, .. } if a.id > 0 => { return a.id; } S3 { b, .. } => { return b.id; } } }
fn g_thru() -> i64 { let v = match S3 { a: mk(89), b: mk(90) } { S3 { a, .. } if a.id > 900 => { a.id } S3 { b, .. } => { b.id } }; return v; }
fn g_three() -> i64 { match S3 { a: mk(86), b: mk(87) } { S3 { a, .. } if a.id > 900 => { return a.id; } S3 { b, .. } if b.id > 900 => { return b.id; } S3 { .. } => { return 3; } } }
fn g_all() -> i64 { match S3 { a: mk(99), b: mk(100) } { S3 { a, .. } if a.id > 900 => { return a.id; } S3 { a, b } => { return a.id + b.id; } } }

fn main() {
    println(f"T:{g_two()}");
    println(f"F:{g_first()}");
    println(f"H:{g_thru()}");
    println(f"R:{g_three()}");
    println(f"A:{g_all()}");
    println("done");
}
"#,
        &[
            "dR81", "dR80", "T:81", "dR82", "dR83", "F:82", "dR90", "dR89", "H:90", "dR87", "dR86",
            "R:3", "dR100", "dR99", "A:199", "done",
        ],
        "asan_guarded_multi_arm_freshtemp_husk_fields_are_freed",
        20,
    );
}

/// B-2026-09-23-28 — a tuple pattern over a borrowed element binds its heap
/// fields as BIT-COPIES the container still owns. Both spellings the fix
/// wired (`for (name, tag) in ps` over a `ref Vec`, and `let (name, tag) = ref
/// ps[i]`) must push no cleanup for `name` / `tag`: one that did would free
/// each String again when `ps` drops.
#[test]
fn asan_tuple_pattern_through_a_borrow_frees_nothing() {
    assert_clean_asan_run(
        r#"
fn s_of(i: i64) -> String {
    let mut s: String = String.new();
    s.push_str(f"payload-padded-out-well-past-thirty-six-bytes-{i}");
    return s;
}
fn walk(ps: ref Vec[(String, String)]) -> i64 {
    let mut n = 0;
    for (name, tag) in ps { n += name.len() + tag.len(); }
    return n;
}
fn index(ps: ref Vec[(String, String)]) -> i64 {
    let mut n = 0;
    for i in 0..ps.len() {
        let (name, tag) = ref ps[i];
        n += name.len() - tag.len();
    }
    return n;
}
fn main() {
    let base: i64 = env.args().len();
    let mut ps: Vec[(String, String)] = Vec.new();
    let mut i = base;
    while i < base + 40 {
        ps.push((s_of(i), s_of(i * 7)));
        i = i + 1;
    }
    let a = walk(ps);
    let b = index(ps);
    if a > 0 and b <= 0 { println("done"); }
}
"#,
        &["done"],
        "tuple pattern through a borrow frees nothing",
    );
}

/// B-2026-09-23-29 — iterator terminals over a BORROWED source whose elements
/// carry heap fields. Two desugarings made a second owner of each String:
/// the per-field projection a destructuring closure param became (`let n =
/// __dp.0`, under `fold` and `collect`), and the element binding `count`'s
/// sink wrote although its body never names the element (`filter(|p| p.1 >
/// 1).count()`). Both freed every String again when the source dropped.
#[test]
fn asan_iterator_terminals_over_heap_tuple_elements_free_once() {
    assert_clean_asan_run(
        r#"
struct Q { n: String, k: i64 }
fn s_of(i: i64) -> String {
    let mut s: String = String.new();
    s.push_str(f"payload-padded-out-well-past-thirty-six-bytes-{i}");
    return s;
}
fn main() {
    let base: i64 = env.args().len();
    let mut names: Vec[(String, i64)] = Vec.new();
    let mut qs: Vec[Q] = Vec.new();
    let mut i = base;
    while i < base + 30 {
        names.push((s_of(i), i));
        qs.push(Q { n: s_of(i * 3), k: i });
        i = i + 1;
    }
    let a = names.iter().filter(|p| p.1 > 10).count();
    let b = names.iter().filter(|(n, _)| n.len() > 40).count();
    let c = names.iter().fold(0, |acc, (n, k)| acc + n.len() + k);
    let d: Vec[i64] = names.iter().map(|(n, k)| n.len() + k).collect();
    let e: Vec[String] = names.iter().map(|(n, _)| n.clone()).collect();
    let f = names.iter().map(|(n, k)| n.len() * k).sum();
    let g = qs.iter().filter(|Q { n, k }| n.len() > 40 + k * 0).count();
    let h: Vec[i64] = qs.iter().map(|Q { n, k }| n.len() + k).collect();
    let j = names.iter().any(|(n, _)| n.len() == 0);
    let mut l = 0;
    for y in names.iter().map(|(n, k)| n.len() + k) {
        l = l + y;
    }
    if a > 0 and b > 0 and c > 0 and d.len() == 30 and e.len() == 30 and f > 0 and g > 0 and h.len() == 30 and not j and l > 0 {
        println("done");
    }
}
"#,
        &["done"],
        "iterator terminals over heap tuple elements free once",
    );
}

#[test]
/// B-2026-09-23-39 — a destructuring pattern inside `Vec.get` / `first` /
/// `last`'s `Option[ref T]` payload (`Some(B.S(w))`, `Some(P { a, n })`,
/// `Some((w, n))`) bound its fields as untyped: codegen then printed the
/// String's data pointer as an integer and had no `len` for it, and a struct or
/// tuple pattern did not count toward exhaustiveness. The fields now bind as
/// borrows of the element, as a bare `Some(w)` does.
fn asan_borrowed_get_payload_destructure_reads_its_fields() {
    assert_clean_asan_run_min_allocs(
        r#"
enum B { S(String), N }
enum B2 { S(String, i64), N }
struct P { a: String, n: i64 }
fn mk(i: i64) -> String { f"payload-{i}-long-enough-to-heap" }
fn leg_match_get() {
    let v: Vec[B] = [B.S(mk(1)), B.N];
    match v.get(0) { None => {} Some(B.S(w)) => { println(w); println(f"mg {w.len()}"); } Some(B.N) => {} }
    match v.get(1) { None => {} Some(B.S(w)) => { println(w); } Some(B.N) => { println("mg n"); } }
}
fn leg_if_let_first() {
    let v: Vec[B2] = [B2.S(mk(2), 5)];
    if let Some(B2.S(w, n)) = v.first() { println(f"il {w} {w.len()} {n}"); }
    if let Some(B2.S(w, _)) = v.last() { let c = w.clone(); println(f"il {c}"); }
}
fn leg_struct_and_tuple() {
    let ps: Vec[P] = [P { a: mk(3), n: 9 }];
    match ps.get(0) { None => {} Some(P { a, n }) => { println(f"st {a} {a.len()} {n}"); } }
    let ts: Vec[(String, i64)] = [(mk(4), 6)];
    match ts.get(0) { None => {} Some((w, n)) => { println(f"tu {w} {w.len()} {n}"); } }
}
fn leg_loop() {
    let v: Vec[B] = [B.S(mk(5)), B.S(mk(6))];
    let mut total = 0;
    let mut i = 0;
    while i < 2 { match v.get(i) { None => {} Some(B.S(w)) => { total = total + w.len(); } Some(B.N) => {} } i = i + 1; }
    println(f"lp {total}");
}
fn main() {
    leg_match_get();
    leg_if_let_first();
    leg_struct_and_tuple();
    leg_loop();
    println("done");
}
"#,
        &[
            "payload-1-long-enough-to-heap",
            "mg 29",
            "mg n",
            "il payload-2-long-enough-to-heap 29 5",
            "il payload-2-long-enough-to-heap",
            "st payload-3-long-enough-to-heap 29 9",
            "tu payload-4-long-enough-to-heap 29 6",
            "lp 58",
            "done",
        ],
        "asan_borrowed_get_payload_destructure_reads_its_fields",
        8,
    );
}

/// B-2026-09-16-11 — the ENUM twin of
/// `asan_self_assign_identity_arm_frees_only_the_distinct_value`:
/// `e = if c { pass(e) } else { e }` over `enum E { A(String), B }`.
///
/// The enum overwrite cleanup consumed the STRICT roundtrip predicate, which
/// declines a bare identifier arm, so the roundtripping arm's orphaned old
/// payload leaked (36 B in 1 block at `-O0`; 72 B in 2 over a four-pass
/// loop). It now admits the identity arm and guards the drop switch on the
/// old and incoming values differing — a whole-value `memcmp`, sound because
/// it is only taken for a padding-free enum layout.
///
/// Every cell prints its payload AFTER the store, so the identity arm freeing
/// the buffer it writes back shows up as garbage or a sanitizer report, not
/// only as a changed leak count:
///   `c1` the roundtripping arm taken, `c2` the identity arm taken,
///   `c3` a loop alternating them, `c4` a three-way `match` with an
///   identity arm, `c5` a variant change to the payload-free `B` then a
///   roundtrip, `c6` a two-field variant beside a payload-free one.
/// Two rounds, so a stale slot or a reused freed block surfaces.
#[test]
fn asan_enum_self_assign_identity_arm_frees_only_the_distinct_value() {
    let round: &[&str] = &[
        "c1 payload-one-aaaaaaaaaaaaaaaaaaaaaaaa",
        "c2 payload-two-aaaaaaaaaaaaaaaaaaaaaaaa",
        "c3 payload-three-aaaaaaaaaaaaaaaaaaaaaa",
        "c4 payload-four-aaaaaaaaaaaaaaaaaaaaaaa",
        "c5 b",
        "c6 payload-six-aaaaaaaaaaaaaaaaaaaaaaaa6",
        "end",
    ];
    let expected: Vec<&str> = round.iter().chain(round.iter()).copied().collect();
    assert_clean_asan_run(
        r#"enum E { A(String), B }
enum E2 { A(String, i64), B(i64) }
fn pass(e: E) -> E { return e; }
fn pass2(e: E2) -> E2 { return e; }
fn flip(e: E) -> E { return E.B; }
fn tag(e: ref E) -> String { match e { E.A(s) => s.clone(), E.B => f"b" } }
fn tag2(e: ref E2) -> String { match e { E2.A(s, n) => f"{s}{n}", E2.B(n) => f"b{n}" } }
fn round() {
    let t: bool = true;
    let f: bool = false;
    let mut e1: E = E.A(f"payload-one-aaaaaaaaaaaaaaaaaaaaaaaa");
    e1 = if t { pass(e1) } else { e1 };
    println(f"c1 {tag(e1)}");
    let mut e2: E = E.A(f"payload-two-aaaaaaaaaaaaaaaaaaaaaaaa");
    e2 = if f { pass(e2) } else { e2 };
    println(f"c2 {tag(e2)}");
    let mut e3: E = E.A(f"payload-three-aaaaaaaaaaaaaaaaaaaaaa");
    let mut i: i64 = 0;
    while i < 6 { e3 = if i % 2 == 0 { pass(e3) } else { e3 }; i = i + 1; }
    println(f"c3 {tag(e3)}");
    let mut e4: E = E.A(f"payload-four-aaaaaaaaaaaaaaaaaaaaaaa");
    let mut k: i64 = 0;
    while k < 3 { e4 = match k { 0 => pass(e4), 1 => e4, _ => pass(e4) }; k = k + 1; }
    println(f"c4 {tag(e4)}");
    let mut e5: E = E.A(f"payload-five-aaaaaaaaaaaaaaaaaaaaaaa");
    e5 = if t { flip(e5) } else { e5 };
    e5 = if t { pass(e5) } else { e5 };
    println(f"c5 {tag(e5)}");
    let mut e6: E2 = E2.A(f"payload-six-aaaaaaaaaaaaaaaaaaaaaaaa", 6);
    let mut j: i64 = 0;
    while j < 4 { e6 = if j % 2 == 0 { pass2(e6) } else { e6 }; j = j + 1; }
    println(f"c6 {tag2(e6)}");
    println("end");
}
fn main() {
    round()
    round()
}
"#,
        &expected,
        "asan_enum_self_assign_identity_arm_frees_only_the_distinct_value",
    );
}
