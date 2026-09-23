//! Map, Set, SortedMap, SortedSet -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen map_set::
//!
//! New fixtures about Map, Set, SortedMap, SortedSet belong in this file.

use super::*;

/// `dbg()` of a `Set` / `SortedSet` / `SortedMap` must give the SAME text on
/// every backend (B-2026-08-30-46) — the `Debug`-mode twin of the `Display`
/// comparison above.
///
/// The compiled backends were already right here; the interpreter guarded
/// those three arms of `render_typed_mode` with `if !debug` and so rendered
/// a `u64` element at or above 2^63 through an untyped catch-all that reads
/// the signed carrier — `Set{-1}` under `dbg` against `Set{...615}` from
/// `f"{s}"` in the same program, and against `Set{...615}` from a compiled
/// binary in BOTH modes.
///
/// Asserted as a twin rather than a pin because the two sides reach this
/// text by different machinery — codegen's `emit_display_fn_for_type_expr`
/// dispatcher against the interpreter's typed walker — so only comparing
/// them catches one drifting.
///
/// `dbg` writes to STDERR, which is why this reads `stderr` rather than
/// going through `run_program`.
#[test]
fn e2e_dbg_of_set_and_sorted_containers_agrees_with_the_interpreter() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "Set[u64] above i64::MAX",
            "let mut s: Set[u64] = Set.new();\n\
                 s.insert(18446744073709551615u64);\n\
                 dbg(s);",
            "Set{18446744073709551615}",
        ),
        (
            "SortedSet[u64] above i64::MAX",
            "let mut s: SortedSet[u64] = SortedSet.new();\n\
                 s.insert(18446744073709551615u64);\n\
                 dbg(s);",
            "SortedSet{18446744073709551615}",
        ),
        (
            "SortedMap[u64, u64] above i64::MAX",
            "let mut s: SortedMap[u64, u64] = SortedMap.new();\n\
                 s.insert(18446744073709551615u64, 18446744073709551615u64);\n\
                 dbg(s);",
            "SortedMap{18446744073709551615: 18446744073709551615}",
        ),
        // The `Map` control: its arm never carried the `!debug` guard, so
        // it was correct on both backends throughout. A change that fixed
        // the three above by breaking the shared `Value::Int` arm would
        // fail here.
        (
            "Map[u64, u64] control",
            "let mut s: Map[u64, u64] = Map.new();\n\
                 s.insert(18446744073709551615u64, 18446744073709551615u64);\n\
                 dbg(s);",
            "{18446744073709551615: 18446744073709551615}",
        ),
        // Exactly 2^63 — the first value that misses the signed carrier.
        // The all-ones row alone would pass against a fix special-casing it.
        (
            "Set[u64] exactly at the signed boundary",
            "let mut s: Set[u64] = Set.new();\n\
                 s.insert(9223372036854775808u64);\n\
                 dbg(s);",
            "Set{9223372036854775808}",
        ),
        // A genuinely signed element must still read as signed.
        (
            "Set[i64] negative control",
            "let mut s: Set[i64] = Set.new();\n\
                 s.insert(-1);\n\
                 dbg(s);",
            "Set{-1}",
        ),
        // Leaf QUOTING is the property the `!debug` guard was deferring to
        // protect. It is identical on both paths and on both backends,
        // which is what made the deferral unnecessary.
        (
            "Set[String] leaf quoting",
            "let mut s: Set[String] = Set.new();\n\
                 s.insert(f\"a\");\n\
                 dbg(s);",
            "Set{\"a\"}",
        ),
    ];
    for (label, body, want) in cases {
        let src = format!("fn main() {{\n{body}\n}}\n");
        let (_out, interp_dbg) =
            karac::run_program_with_dbg(&src, karac::interpreter::DbgOutputMode::Terminal);
        let interp = interp_dbg.join("\n");
        assert!(
            interp.contains(want),
            "{label}: the interpreter must render the element through its \
                 declared type — got {interp:?}, wanted {want:?}",
        );
        if let Some(run) = run_program_capturing(&src) {
            assert!(
                run.stderr.contains(want),
                "{label}: the compiled backend disagrees with the \
                     interpreter — got {:?}, wanted {want:?}",
                run.stderr,
            );
        }
    }
}

/// B-2026-09-05-31 — the NAMED-LOCAL argument to a generic whole-param
/// callee, the cell B-2026-09-05-29 declined. That row read the body count
/// (one, on all four surfaces) as "already correct" and shipped admitting
/// only a fresh temporary. The body count was right; the MEMORY was not.
///
/// The callee ENTRY-COPIES a copy-supported heap struct param, so
/// `let g = mk(88); let _ = passG(g);` has TWO objects and one owner: the
/// binding freed its original and the returned copy was orphaned. Measured
/// pre-fix on this exact program — 84 allocs / 69 frees, 240 B definitely
/// lost in 5 blocks — and at the DEFAULT optimization level, not only at
/// `-O0`. The payload is `Vec[String]` deliberately: with the row's
/// original `String` + `Vec[i64]` the dead entry copy is DCE'd at default
/// opt and the leak is visible only under `KARAC_OPT_LEVEL=0`, which is
/// how it stayed filed as a `-O0` curiosity.
///
/// The BOUND sibling is the same defect read through the body count, and
/// was an unfiled A/B divergence until this row measured it: pre-fix,
/// `let o2 = passG(g2)` printed `dR88 k88 dR88` on all three compiled
/// surfaces against the interpreter's `k88 dR88`, while the concrete twin
/// `passN` was correct on all four. One omission explains both — the
/// monomorph path never performed `compile_call`'s caller-side
/// `suppress_user_drop_body_keeping_memory` — so the fix is one
/// stand-down plus the registration that receives what it gives up.
///
/// Cells: the discarded named local (`a`); the BOUND named local (`k88`),
/// the divergence above; a CONDITIONAL-return generic discarded (`c`) and
/// bound (`m90`), the second of which diverged the same way; the CONCRETE
/// twin (`e`), correct throughout; the SCALAR return (`f`), which must
/// stay declined; a FORWARDING callee (`g`) — `D` owns a `shared` field,
/// so copy support declines, the callee takes the binding's own object,
/// and admitting it would be the double free the gate exists to prevent;
/// a struct with NO user `Drop` (`h`), memory-only and silent; the fresh
/// TEMPORARY (`dR94`), B-2026-09-05-29's shape, which must not regress;
/// and the LOOP, where the miss was unbounded.
#[test]
fn test_e2e_generic_whole_param_named_local_frees_the_entry_copy() {
    let out = run_program(
        r#"
struct R { id: i64, names: Vec[String] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, names: [f"a{i}", f"b{i}"] }; }
shared struct Sh { v: i64 }
struct D { s: Sh, tag: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.tag}") } }
fn mkd(i: i64) -> D { return D { s: Sh { v: i }, tag: f"x{i}" }; }
fn passG[T](x: T) -> T { println("inP"); return x; }
fn passN(x: R) -> R { println("inN"); return x; }
fn maybeG[T](x: T, k: bool) -> T { println("inM"); if k { return x; } return x; }
fn scalarG[T](x: T) -> i64 { println("inS"); return 3; }
fn main() {
  let g1 = mk(82); let _ = passG(g1);          println("a");
  let g2 = mk(88); let o2 = passG(g2);         println(f"k{o2.id}");
  let g3 = mk(89); let _ = maybeG(g3, true);   println("c");
  let g4 = mk(90); let o4 = maybeG(g4, false); println(f"m{o4.id}");
  let g5 = mk(91); let _ = passN(g5);          println("e");
  let g6 = mk(92); let _ = scalarG(g6);        println("f");
  let d7 = mkd(93); let _ = passG(d7);         println("g");
  let _ = passG(mk(94));                       println("h");
  let mut i = 0; while i < 3 { let gl = mk(95 + i); let _ = passG(gl); i = i + 1; } println("j");
  println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out, "inP\ndR82\na\ninP\nk88\ndR88\ninM\ndR89\nc\ninM\nm90\ndR90\ninN\ndR91\ne\ninS\ndR92\nf\ninP\ndDx93\ng\ninP\ndR94\nh\ninP\ndR95\ninP\ndR96\ninP\ndR97\nj\nend\n",
                "a named local handed to a generic whole-param callee owes exactly \
                 one body per object, and the callee's entry copy owes a free; \
                 got {out:?}"
            );
    }
}

/// B-2026-09-06-2 — the generic METHOD sibling of
/// `..._generic_whole_param_named_local_frees_the_entry_copy`, which
/// B-2026-09-05-31 scoped itself out of. A method is keyed `Type.method`,
/// which the discard registrar's free-function resolver answers `None` for,
/// and the monomorph argument loop's caller-side stand-down was gated to
/// `self_param.is_none()` because that loop's index is receiver-inclusive.
/// Both halves therefore skipped every method, and THREE separate defects
/// followed — measured pre-fix on this exact program, 77 allocs / 62 frees
/// with 240 B definitely lost in 5 blocks:
///
///   * the LEAK the row was filed for (`a`): the callee's entry copy had no
///     owner. Unbounded — 24,000 B in 500 blocks over a 500-iteration loop,
///     at the DEFAULT optimization level.
///   * the BOUND spelling ran the `Drop` body TWICE (`k88`, `m90` printed
///     `dR88 k88 dR88` / `dR90 m90 dR90`), against the interpreter's once
///     and against both CONCRETE method twins, which were correct.
///   * a LOST body for the fresh TEMPORARY (`h`): `let _ = h.keep(mk(94));`
///     ran NO body on any compiled surface. That is B-2026-09-05-29's own
///     defect in method form, which nothing had filed — its fix resolved
///     free functions only, and this cell is what shows the method half was
///     still open.
///
/// Cells, beyond those three: a CONDITIONAL-return generic method
/// discarded (`c`) and bound (`m90`); the CONCRETE method twin (`e`),
/// correct throughout; the SCALAR-returning generic method (`f`), which
/// must stay declined; a FORWARDING callee (`g`) — `D` owns a `shared`
/// field so copy support declines, the callee takes the binding's own
/// object, and admitting it would be a double free rather than a leak; and
/// the LOOP, unbounded pre-fix.
///
/// THE INTERPRETER DISAGREED ON THE DISCARD CELLS WHEN THIS LANDED, and
/// no longer does. `karac run --interp` dropped the body for a discarded
/// generic call outright — `a`, `c`, `g`, `h` and the loop were all silent
/// there — so this was written to pin the COMPILED columns alone, which
/// agreed with each other and with the concrete twins. B-2026-09-06-1
/// (`3ef6220`) has since fixed the interpreter, and all five surfaces now
/// produce this string; the expectation is unchanged because the compiled
/// columns were the correct ones throughout.
#[test]
fn test_e2e_generic_method_whole_param_frees_the_entry_copy() {
    let out = run_program(
        r#"
struct R { id: i64, names: Vec[String] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, names: [f"a{i}", f"b{i}"] }; }
shared struct Sh { v: i64 }
struct D { s: Sh, tag: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.tag}") } }
fn mkd(i: i64) -> D { return D { s: Sh { v: i }, tag: f"x{i}" }; }
struct H { n: i64 }
impl H {
  fn keep[T](ref self, x: T) -> T { println("inK"); return x; }
  fn keepN(ref self, x: R) -> R { println("inN"); return x; }
  fn maybe[T](ref self, x: T, k: bool) -> T { println("inM"); if k { return x; } return x; }
  fn scalar[T](ref self, x: T) -> i64 { println("inS"); return 3; }
}
fn main() {
  let h = H { n: 1 };
  let g1 = mk(82); let _ = h.keep(g1);          println("a");
  let g2 = mk(88); let o2 = h.keep(g2);         println(f"k{o2.id}");
  let g3 = mk(89); let _ = h.maybe(g3, true);   println("c");
  let g4 = mk(90); let o4 = h.maybe(g4, false); println(f"m{o4.id}");
  let g5 = mk(91); let _ = h.keepN(g5);         println("e");
  let g6 = mk(92); let _ = h.scalar(g6);        println("f");
  let d7 = mkd(93); let _ = h.keep(d7);         println("g");
  let _ = h.keep(mk(94));                       println("h");
  let mut i = 0; while i < 3 { let gl = mk(95 + i); let _ = h.keep(gl); i = i + 1; } println("j");
  println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out, "inK\ndR82\na\ninK\nk88\ndR88\ninM\ndR89\nc\ninM\nm90\ndR90\ninN\ndR91\ne\ninS\ndR92\nf\ninK\ndDx93\ng\ninK\ndR94\nh\ninK\ndR95\ninK\ndR96\ninK\ndR97\nj\nend\n",
                "a generic METHOD that hands its by-value param back owes one body \
                 per object and a free for its entry copy, on every compiled \
                 column; got {out:?}"
            );
    }
}

#[test]
/// B-2026-09-05-37 — a whole rebind of a by-value `Drop` param NESTED in a
/// branch frees the callee's entry copy on the path that never rebound.
///
/// `suppress_struct_cleanup_for_tail_identifier` is a compile-time frame
/// removal, so reaching it from inside a branch disarmed the source's
/// memory action on EVERY path: the destination covered the rebinding path
/// and nothing covered the rest. Measured before the fix at
/// `KARAC_OPT_LEVEL=0` as 3 B in 1 block per not-taken call — `brF`, `faF`,
/// `rdF` and the `tw` pair each leaking one `String` — and at the DEFAULT
/// `-O2` as well for `opt`, whose `println` between the branch and the
/// return stops LLVM deleting the dead copy.
///
/// The BODIES were right throughout and stay so here; this asserts the
/// output, and `asan_branch_nested_param_rebind_frees_the_entry_copy`
/// asserts the freeing. `top` is the control the fix must not disturb: a
/// TOP-LEVEL rebind keeps the static removal B-2026-08-09-16 put there.
/// `rdT`/`rdF` are the second control — Kāra does not reject a read after a
/// move, so `n=h28` and `s=h27` pin that the guard leaves the source's
/// bytes intact where a cap-zero would not.
fn test_e2e_branch_nested_param_rebind_frees_the_entry_copy() {
    let out = run_program(
        r#"
struct R { id: i64, name: String, xs: Vec[String] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", xs: [f"a{i}"] }; }
fn br(r: R, keep: bool) -> i64 { if keep { let m = r; return m.id; } return 0; }
fn fall(r: R, keep: bool) -> i64 { if keep { let m = r; println(f"in{m.id}"); } return 7; }
fn els(r: R, keep: bool) -> i64 { if keep { return 4; } else { let m = r; return m.id; } }
fn rd(r: R, keep: bool) -> i64 { if keep { let m = r; println(f"n={m.name}"); return 5; } println(f"s={r.name}"); return 6; }
fn arm(r: R, k: i64) -> i64 { match k { 1 => { let m = r; return m.id; } _ => { return 0; } } }
fn two(r: R, a: bool, b: bool) -> i64 { if a { if b { let m = r; return m.id; } return 2; } return 3; }
fn top(r: R) -> i64 { let m = r; return m.id; }
fn opt(r: R, keep: bool) -> Option[R] { if keep { let m = r; return Option.Some(m); } println("after"); return Option.None; }
fn main() {
  println(f"brF={br(mk(21), false)}");
  println(f"brT={br(mk(22), true)}");
  println(f"faF={fall(mk(23), false)}");
  println(f"faT={fall(mk(24), true)}");
  println(f"elF={els(mk(25), false)}");
  println(f"elT={els(mk(26), true)}");
  println(f"rdF={rd(mk(27), false)}");
  println(f"rdT={rd(mk(28), true)}");
  println(f"arF={arm(mk(29), 0)}");
  println(f"arT={arm(mk(30), 1)}");
  println(f"tw00={two(mk(31), false, false)}");
  println(f"tw10={two(mk(32), true, false)}");
  println(f"tw11={two(mk(33), true, true)}");
  println(f"top={top(mk(34))}");
  let _ = opt(mk(35), false);
  let o = opt(mk(36), true);
  match o { Option.Some(v) => { println(f"got{v.id}"); } _ => { println("none"); } }
  println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out, "dR21\nbrF=0\ndR22\nbrT=22\ndR23\nfaF=7\nin24\ndR24\nfaT=7\ndR25\nelF=25\ndR26\nelT=4\ns=h27\ndR27\nrdF=6\nn=h28\ndR28\nrdT=5\ndR29\narF=0\ndR30\narT=30\ndR31\ntw00=3\ndR32\ntw10=2\ndR33\ntw11=33\ndR34\ntop=34\nafter\ndR35\ngot36\ndR36\nend\n",
                "a whole rebind of a by-value `Drop` param nested in a branch owes \
                 one body per object on every path and a free for the entry copy \
                 on the path that never rebound; got {out:?}"
            );
    }
}

/// B-2026-08-08-30 — mapping a BORROWED SCALAR payload.
///
/// `Vec[T].first()` / `.get(i)` are typed `Option[ref T]` (`Map.get` is owned
/// `Option[V]`, which is why it never showed this). Two independent defects sat
/// on that borrowed payload, and the first one masked the second:
///
/// 1. RETURN-TYPE INFERENCE. `infer_closure_return_type`'s param map held the
///    param's SLOT type, which for a borrow is a pointer — but every read in
///    the body goes through `load_variable`, which derefs, so the body's value
///    type is the pointee. The closure was declared `-> ptr` while its body
///    returned `i64`, and LLVM module verification rejected the function. It
///    only bit when the mapper's RESULT type is the payload type: `|x| x > 5`
///    yields `bool` and `|x| x + 1.0` hits the float arm, so both compiled and
///    the family read as working.
///
/// 2. A LEAKED BORROW MARK. The closure's `ref` param registration was never
///    restored, so the OUTER function's next binding of that same name was
///    still marked a borrow. The row filed this as a panic — the `Some(x)`
///    arm's `x` deref'ing an `i64` — but the panic is the lucky case. When the
///    later `x` is a Vec the bogus deref SUCCEEDS and reads the wrong word:
///    `println(x.len())` printed 2 under `karac build` against the
///    interpreter's 3. A silent run-vs-build divergence, which is why the
///    registries are restored wholesale rather than only where they panicked.
///
/// The `ref`-annotated spelling is covered because B-2026-08-08-24's diagnostic
/// tells authors to write exactly that over a borrowed payload.
#[test]
fn test_e2e_optres_map_borrowed_scalar_payload() {
    let out = run_program(
        r#"
fn main() {
    let v: Vec[i64] = vec![7, 9];
    // Defect 1: result type == payload type, the shapes that failed to verify.
    match v.first().map(|a| a + 1) { Some(b) => println(b), None => println("-") }
    match v.first().map(|a| a) { Some(b) => println(b), None => println("-") }
    match v.first().map(|a: ref i64| a + 1) { Some(b) => println(b), None => println("-") }
    match v.get(1i64).map(|a| a + 1) { Some(b) => println(b), None => println("-") }
    println(v.first().map_or(0i64, |a| a + 1));
    println(v.first().map(|a| a + 1).unwrap_or(0i64));
    // Controls: these compiled before the fix and must keep their answers.
    match v.first().map(|a| a > 5i64) { Some(b) => println(b), None => println("-") }
    let f: Vec[f64] = vec![1.5];
    match f.first().map(|a| a + 1.0) { Some(b) => println(b), None => println("-") }
    let empty: Vec[i64] = Vec.new();
    match empty.first().map(|a| a + 1) { Some(b) => println(b), None => println("-") }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim_end(), "8\n7\n8\n10\n8\n8\ntrue\n2.5\n-");
    }
}

/// B-2026-08-09-5 — the HEAP half of the borrowed-payload mapper. An
/// indirect call site lowered the closure's return from the SURFACE `Fn(..)`
/// type while the emitted body used its own; when the two disagreed, the
/// call read the wrong number of words and nothing caught it, because an
/// indirect call's callee type is not verified.
///
/// `|s| s` over a borrowed String is recorded `-> ref String`, so the call
/// site read one `ptr` word while the body deep-copied and returned the
/// 3-word `{ptr,len,cap}` aggregate. `len` and `cap` were dropped and the
/// program printed the EMPTY STRING against the interpreter's `ab`. Before
/// B-2026-08-08-30's fix this shape failed LLVM verification instead, so the
/// regression window is exactly that commit — fixing the definition's return
/// type without the call site's turned a loud build error into a silent
/// wrong answer.
///
/// The scalar controls are the reason this went unseen: `|x| x` has the
/// identical signature disagreement and is correct only by luck, since an
/// `i64` read back through a `ptr` return keeps its bits.
#[test]
fn test_e2e_map_over_borrowed_heap_payload_returns_the_whole_value() {
    let out = run_program(
        r#"
fn main() {
    let v: Vec[String] = vec![f"ab", f"cd"];
    match v.first().map(|s| s) { Some(w) => println(w), None => println("-") }
    match v.last().map(|s| s) { Some(w) => println(w), None => println("-") }
    // Controls: these already agreed at both ends and must keep their answers.
    match v.first().map(|s| s.to_uppercase()) { Some(w) => println(w), None => println("-") }
    match v.first().map(|s| s.len()) { Some(w) => println(w), None => println("-") }
    let n: Vec[i64] = vec![1, 2];
    match n.first().map(|x| x) { Some(w) => println(w), None => println("-") }
    let empty: Vec[String] = Vec.new();
    match empty.first().map(|s| s) { Some(w) => println(w), None => println("-") }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim_end(), "ab\ncd\nAB\n2\n1\n-");
    }
}

/// The `weak` value read through a struct FIELD, over a real 2-node cycle —
/// the shape B-2026-08-08-28 caught for `Vec` and design.md's graph example
/// writes with a container. Two reads of the same live entry must both give
/// `Some`: one read taking the balancing acquire and the next seeing a
/// half-released target is exactly how the `Vec` twin failed.
#[test]
fn test_e2e_map_of_weak_field_rooted_read_survives_two_reads() {
    let out = run_program(
        r#"
shared struct N { mut v: i64, mut ns: Map[i64, weak N] }
fn build2() -> N {
    let a: N = N { v: 1i64, ns: Map.new() };
    let b: N = N { v: 2i64, ns: Map.new() };
    a.ns.insert(1i64, b);
    b.ns.insert(1i64, a);
    match a.ns.get(1i64) { Some(x) => { println(x.v); } None => { println(0 - 1); } }
    match a.ns.get(1i64) { Some(y) => { println(y.v); } None => { println(0 - 2); } }
    return a;
}
fn main() { println(build2().v); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2\n2\n1");
    }
}

#[test]
fn e2e_unannotated_map_new_insert_infers_field_type() {
    // Motivating case (protobuf `map<K,V>` construction): an
    // *unannotated* `let mut m = Map.new()` whose K/V are pinned only by
    // the subsequent `.insert(k, v)` calls, then moved into a typed
    // `Map[String, i64]` struct field and read back. Before the
    // typechecker back-propagation fix this failed `karac check` (the
    // inferred `Map[?K, ?V]` clashed with the annotated field); this
    // guards that the whole pipeline — inference → codegen → runtime —
    // handles the inferred-receiver form identically to the annotated one.
    if let Some(out) = run_program(
        "struct S { m: Map[String, i64] }\n\
             fn main() {\n\
                 let mut m = Map.new();\n\
                 m.insert(\"a\", 1);\n\
                 m.insert(\"b\", 2);\n\
                 let s = S { m: m };\n\
                 println(s.m.len());\n\
                 match s.m.get(\"a\") { Some(v) => println(v), None => println(-1) }\n\
                 match s.m.get(\"b\") { Some(v) => println(v), None => println(-1) }\n\
             }",
    ) {
        assert_eq!(out, "2\n1\n2\n");
    }
}

#[test]
fn e2e_map_values_keys_collect_to_vec() {
    // B-2026-07-08-17: `<map>.values().collect()` / `.keys().collect()`
    // failed codegen with "no handler for method 'collect' on non-identifier
    // receiver" — the two `collect`-on-MethodCall intercepts only covered
    // `chars()` and `map`/`filter` chains, so the eager `values()`/`keys()`
    // Vec fell through to the dispatch-fail error. `values`/`keys`/`entries`
    // already materialize an owned Vec, so `collect()` on them is identity.
    // Surfaced by leetcode/group_anagrams (`groups.values().collect()`).
    //
    // B-2026-08-12-8: the receiver is now `.values().iter()`, not
    // `.values()` directly. `.collect()` applied straight to a Vec is a
    // deliberate REJECTION, not a gap — the diagnostic spells the rule out
    // ("iterator adaptors/terminals require an explicit `.iter()`") — so
    // the old program was one no user could compile, and it only passed
    // because the harness discarded typecheck errors (B-2026-08-11-34).
    // `.iter()` keeps the receiver a MethodCall CHAIN, which is what the
    // original codegen dispatch failure was about, so the regression value
    // survives. (`let vs: Vec[i64] = m.values();` is the simpler thing a
    // user should actually write — `collect` on an already-materialized
    // Vec is identity either way.)
    // Assert order-independently (Map iteration order is unspecified per
    // design.md — sum the collected elements rather than compare positions).
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut m: Map[i64, i64] = Map.new();\n\
                 let _ = m.insert(1, 10);\n\
                 let _ = m.insert(2, 20);\n\
                 let _ = m.insert(3, 30);\n\
                 let vs: Vec[i64] = m.values().iter().collect();\n\
                 let ks: Vec[i64] = m.keys().iter().collect();\n\
                 let mut vsum = 0;\n\
                 for v in vs.iter() { vsum = vsum + v; }\n\
                 let mut ksum = 0;\n\
                 for k in ks.iter() { ksum = ksum + k; }\n\
                 println(vsum);\n\
                 println(ksum);\n\
                 println(vs.len());\n\
             }",
    ) {
        assert_eq!(out, "60\n6\n3\n");
    }
}

#[test]
fn e2e_map_field_constructed_in_associated_fn() {
    // B-2026-07-08-12: a `Map`/`Set`-typed struct field initialized with
    // `Map.new()` inside an associated constructor (`Cache.new()`) emitted
    // invalid IR — `Map.new()` has no expr-level codegen handler (only the
    // `let`-stmt path special-cases it), so it fell through to the `i64 0`
    // default and built `insertvalue i64 0` into the pointer-typed field
    // slot, aborting at module verification. Unmasked by the B-2026-07-08-9
    // call-result Display fix (lru_cache). Derive the handle from the
    // field's declared type instead. Exercise the full round-trip: build the
    // struct via an associated fn, insert, read back through the field.
    if let Some(out) = run_program(
        "struct Cache { capacity: u64, index: Map[i64, u64] }\n\
             impl Cache {\n\
                 fn new(capacity: u64) -> Cache { Cache { capacity, index: Map.new() } }\n\
                 fn put(mut ref self, k: i64, v: u64) { let _ = self.index.insert(k, v); }\n\
                 fn count(ref self) -> u64 { self.index.len() as u64 }\n\
             }\n\
             fn main() {\n\
                 let mut c = Cache.new(2);\n\
                 c.put(5, 9);\n\
                 c.put(7, 3);\n\
                 println(c.count());\n\
                 match c.index.get(5) {\n\
                     Some(v) => { println(v); }\n\
                     None => { println(-1); }\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "2\n9\n");
    }
}

#[test]
fn e2e_shared_struct_map_field_constructed_in_associated_fn() {
    // B-2026-07-08-12 shared-heap sibling: on the `shared struct` path the
    // same `Map.new()`-in-constructor bug is SILENT — an opaque-pointer
    // store of the `i64 0` default into the ptr-typed field slot passes
    // verification (pointee types aren't checked) but leaves a null handle,
    // so the program builds and then SEGFAULTS on first map use. Same fix
    // (derive the handle from the field's declared type) applied to the
    // shared branch; assert the round-trip works instead of crashing.
    if let Some(out) = run_program(
        "shared struct Cache { capacity: u64, index: Map[i64, u64] }\n\
             impl Cache {\n\
                 fn new(capacity: u64) -> Cache { Cache { capacity, index: Map.new() } }\n\
                 fn put(mut ref self, k: i64, v: u64) { let _ = self.index.insert(k, v); }\n\
                 fn count(ref self) -> u64 { self.index.len() as u64 }\n\
             }\n\
             fn main() {\n\
                 let mut c = Cache.new(2);\n\
                 c.put(5, 9);\n\
                 c.put(7, 3);\n\
                 println(c.count());\n\
             }",
    ) {
        assert_eq!(out, "2\n");
    }
}

#[test]
fn e2e_shared_enum_map_payload_drops_without_freeing_scalar_keys() {
    // B-2026-07-08-12 shared-enum sibling: the RC-drop for a `shared enum`
    // with a `Map`/`Set` payload hardcoded `karac_map_free_with_drop_vec(_,
    // 1, 1)`, so on drop the runtime read offset-16 of each 8-byte scalar
    // key as a bogus `cap` and freed the key VALUE as a pointer — heap
    // corruption on any populated `Map[i64, u64]` payload. AOT masked it
    // (the OOB `cap` read happened to be <= 0 in the release heap layout);
    // the JIT parity leg surfaced the crash on the struct sibling. Fixed by
    // deriving the `(drop_key, drop_val)` flags from the payload type via
    // `map_drop_flags` (scalar map -> (0, 0)), matching the struct/tuple
    // arms. Construct + drop the enum (no move-out — that is a separate
    // shared-enum match-binding double-free, filed as B-2026-07-08-22).
    if let Some(out) = run_program(
        "shared enum Store { Empty, Full(Map[i64, u64]) }\n\
             fn build() -> Store {\n\
                 let mut m = Map.new();\n\
                 let _ = m.insert(5, 9);\n\
                 let _ = m.insert(7, 3);\n\
                 Store.Full(m)\n\
             }\n\
             fn main() {\n\
                 let _s = build();\n\
                 println(1);\n\
             }",
    ) {
        assert_eq!(out, "1\n");
    }
}

#[test]
fn ir_map_scalar_clear_uses_plain_variant() {
    // A scalar-keyed/valued map owns no heap per entry, so clear must stay
    // on the cheap plain `karac_map_clear` — the drop variant appears only
    // as its (uncalled) declaration, so its occurrence count is exactly 1.
    let ir = ir_for(
        "fn main() {\n\
                 let mut m: Map[i64, i64] = Map.new();\n\
                 m.insert(1, 2);\n\
                 m.clear();\n\
                 println(m.len());\n\
             }",
    );
    assert_eq!(
            ir.matches("@karac_map_clear_with_drop_vec").count(),
            1,
            "scalar-keyed clear should use plain karac_map_clear (drop variant declared but not called); IR:\n{ir}"
        );
}

/// B-2026-08-06-6 — a `Map`/`Set` field moved out INDIVIDUALLY must
/// neutralize the source handle.
///
/// `zero_struct_move_caps_mono` has nulled a Map handle on a WHOLE-struct
/// move since B-2026-07-15-23; the per-FIELD neutralizer
/// `zero_struct_field_move_cap` never got the same arm. So the owner's
/// `FieldDrop::MapOrSet` freed storage the destination still owned:
///
///     struct MapH { m: Map[i64, String] }
///     fn take(h: MapH) -> Map[i64, String] { return h.m; }
///
/// segfaulted on a use-after-free — valgrind `Invalid read of size 8`
/// inside `karac_map_free_with_drop_vec`, into a block the callee's struct
/// drop had already freed — while the interpreter printed the right
/// answer. A concrete, non-generic program; nothing about it is exotic.
///
/// Three move-out spellings, because they reach the neutralizer by
/// different routes: returned out of a by-value param, bound to a local,
/// and passed to a consuming callee. Two controls that were always correct
/// — a field only READ, and a whole-struct move (which retracts the
/// source's entire StructDrop rather than neutralizing one field) — so a
/// fix that over-nulls turns them red.
///
/// The `SortedMap` arm is carried for symmetry with the drop classifier
/// (B-2026-08-02-21 taught it to free SortedMap/SortedSet). Stated
/// honestly: that leg measures clean both before and after, so it is
/// defensive symmetry rather than a measured fix — it is here so the shape
/// is covered if the routes ever converge.
///
/// Expected total is DERIVED, not read off a run: `mk` builds a 2-entry
/// map and `mks` a 1-entry one, so a round is 2+2+2+1+1+2+2 = 12, and
/// 12 x 40 = 480.
#[test]
fn e2e_map_field_move_out_neutralizes_the_source() {
    let Some(out) = run_program(
        r#"struct MapH { m: Map[i64, String], tag: String }
struct SortH { s: SortedMap[i64, String] }

fn mk(i: i64, n: i64) -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(i, f"mapv-{i}-padded-out-to-force-a-real-heap-buffer-{n}");
    m.insert(i + 100i64, f"mapw-{i}-padded-out-to-force-a-real-heap-buffer-{n}");
    return m;
}

fn mks(i: i64, n: i64) -> SortedMap[i64, String] {
    let mut m: SortedMap[i64, String] = SortedMap.new();
    m.insert(i, f"sortv-{i}-padded-out-to-force-a-real-heap-buffer-{n}");
    return m;
}

fn take(h: MapH) -> Map[i64, String] { return h.m; }
fn eat(m: Map[i64, String]) -> i64 { return m.len(); }
fn eats(m: SortedMap[i64, String]) -> i64 { return m.len(); }

fn main() {
    let n: i64 = env.args().len();
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40i64 {
        // (a) field returned out of a by-value param — the SIGSEGV
        let h1: MapH = MapH { m: mk(i, n), tag: f"tag-{i}-payload" };
        let m1 = take(h1);
        acc = acc + m1.len();
        // (b) field moved into a local
        let h2: MapH = MapH { m: mk(i, n), tag: f"tag-{i}-payload" };
        let m2 = h2.m;
        acc = acc + m2.len();
        // (c) field passed to a consuming callee
        let h3: MapH = MapH { m: mk(i, n), tag: f"tag-{i}-payload" };
        acc = acc + eat(h3.m);
        // (d) SortedMap field moved out
        let h4: SortH = SortH { s: mks(i, n) };
        acc = acc + eats(h4.s);
        // CONTROLS, correct before and after: a field only READ, and a
        // WHOLE-struct move (which retracts the source's StructDrop rather
        // than neutralizing one field).
        let h5: MapH = MapH { m: mk(i, n), tag: f"tag-{i}-payload" };
        if h5.tag.contains("payload") { acc = acc + 1i64; }
        acc = acc + h5.m.len();
        let h6: MapH = MapH { m: mk(i, n), tag: f"tag-{i}-payload" };
        let h7 = h6;
        acc = acc + h7.m.len();
        i = i + 1i64;
    }
    println(acc);
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "480\n");
}

#[test]
fn e2e_bare_generic_param_map_field_is_freed_and_neutralized() {
    let Some(out) = run_program(
        r#"struct Box[T] { v: T }
struct Trip[T] { a: String, v: T, n: i64 }

fn mk(i: i64, n: i64) -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(i, f"mapv-{i}-padded-out-to-force-a-real-heap-buffer-{n}");
    m.insert(i + 100i64, f"mapw-{i}-padded-out-to-force-a-real-heap-buffer-{n}");
    return m;
}

fn mkset(i: i64, n: i64) -> Set[String] {
    let mut s: Set[String] = Set.new();
    s.insert(f"setv-{i}-padded-out-to-force-a-real-heap-buffer-{n}");
    return s;
}

fn sink(b: Box[Map[i64, String]]) -> i64 { return b.v.len(); }
fn take(b: Box[Map[i64, String]]) -> Map[i64, String] { return b.v; }
fn eat(m: Map[i64, String]) -> i64 { return m.len(); }
fn peek(b: ref Box[Map[i64, String]]) -> i64 { return b.v.len(); }
fn sinkset(b: Box[Set[String]]) -> i64 { return b.v.len(); }
fn sinkmid(t: Trip[Map[i64, String]]) -> i64 {
    let mut r: i64 = t.v.len() + t.n;
    if t.a.contains("padding") { r = r + 1i64; }
    return r;
}

fn main() {
    let n: i64 = env.args().len();
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40i64 {
        // (a) read through a by-value param — the reported leak
        let b1 = Box { v: mk(i, n) };
        acc = acc + sink(b1);
        // (b) read through a plain LOCAL — not param-specific
        let b2 = Box { v: mk(i, n) };
        acc = acc + b2.v.len();
        // (c) the MID field of a multi-field wrapper (offset-sensitive)
        let t1 = Trip { a: f"lead-{i}-padding-{n}", v: mk(i, n), n: 3i64 };
        acc = acc + sinkmid(t1);
        // (d) the field RETURNED out of a by-value param
        let b3 = Box { v: mk(i, n) };
        let m1 = take(b3);
        acc = acc + m1.len();
        // (e) the field moved into a local
        let b4 = Box { v: mk(i, n) };
        let m2 = b4.v;
        acc = acc + m2.len();
        // (f) the field passed to a consuming callee
        let b5 = Box { v: mk(i, n) };
        acc = acc + eat(b5.v);
        // (g) a WHOLE-struct move
        let b6 = Box { v: mk(i, n) };
        let b7 = b6;
        acc = acc + sink(b7);
        // (h) a `ref` param read — the source keeps ownership
        let b8 = Box { v: mk(i, n) };
        acc = acc + peek(b8);
        // (i) the Set instantiation
        let b9 = Box { v: mkset(i, n) };
        acc = acc + sinkset(b9);
        // (j) a struct-pattern destructure
        let ba = Box { v: mk(i, n) };
        let Box { v } = ba;
        acc = acc + eat(v);
        // CONTROL, correct before and after: a struct that is never moved and
        // whose fields are only READ, so it must still free everything itself.
        let t2 = Trip { a: f"lead-{i}-padding-{n}", v: mk(i, n), n: 3i64 };
        if t2.a.contains("padding") { acc = acc + 1i64; }
        acc = acc + t2.v.len();
        i = i + 1i64;
    }
    println(acc);
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "1040\n");
}

/// B-2026-07-30-11 (Map-values leg) — a let-bound `Map[K, V]` runs each
/// stored VALUE's user `impl Drop` body when the binding dies, via a
/// bucket walk (`__karac_dropelems_map_*`) emitted directly against the
/// pinned `KaracMap` ABI — bodies only, memory stays with the
/// `FreeMapHandle` family. The gate is the SHARED static chain
/// (annotation -> bare-fn-callee return -> source-var record), mirrored
/// verbatim by the interpreter's `record_map_val_bodies_te`.
///
/// Every map here holds at most ONE Drop-bearing value, deliberately:
/// map iteration order differs per backend (insertion vs bucket — the
/// same unordered semantics `for (k, v) in m` already has), so parity
/// tests must be order-insensitive. Shapes: insert-literal held to the
/// end, `remove` (the moved-out Option binding owns the body — 95 fires
/// via the Option/Result leg, the map walk skips the tombstone), a
/// whole-map rebind (source disarmed, destination re-registers via the
/// source-var record), insert of a BINDING (the source's own action is
/// fully disarmed — this printed the body twice before
/// `disarm_moved_value_arg_user_drops`, and the same hole existed for
/// `v.push(r)` since the Vec leg), a heap-carrying value (valgrind-clean
/// — the walk frees nothing), and a scalar-value map registering
/// nothing.
///
/// Twin of `tests/interpreter.rs`'s `test_map_values_run_user_drop_bodies`
/// on identical source and expected output.
#[test]
fn e2e_map_values_run_user_drop_bodies() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(90 + self.id); } }\n\
             struct HeapRes { name: String, id: i64 }\n\
             impl Drop for HeapRes { fn drop(mut ref self) { println(80 + self.id); } }\n\
             fn main() {\n\
             \x20   let mut m: Map[i64, Res] = Map.new();\n\
             \x20   m.insert(1, Res { id: 1 });\n\
             \x20   println(1);\n\
             \x20   let mut m2: Map[i64, Res] = Map.new();\n\
             \x20   m2.insert(5, Res { id: 5 });\n\
             \x20   let out = m2.remove(5);\n\
             \x20   println(2);\n\
             \x20   let mut m3: Map[i64, Res] = Map.new();\n\
             \x20   m3.insert(7, Res { id: 7 });\n\
             \x20   let m4 = m3;\n\
             \x20   println(3);\n\
             \x20   let mut m5: Map[i64, Res] = Map.new();\n\
             \x20   let r = Res { id: 4 };\n\
             \x20   m5.insert(4, r);\n\
             \x20   println(4);\n\
             \x20   let mut m6: Map[i64, HeapRes] = Map.new();\n\
             \x20   m6.insert(9, HeapRes { name: \"value-payload-string\", id: 9 });\n\
             \x20   println(5);\n\
             \x20   let mut m7: Map[i64, i64] = Map.new();\n\
             \x20   m7.insert(2, 2);\n\
             \x20   println(6);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "91\n1\n95\n2\n97\n3\n94\n4\n89\n5\n6\n");
}

/// B-2026-07-31-27 — a whole-map/set rebind (`let m2 = m;`) transfers
/// handle OWNERSHIP to the destination, not just dispatch.
///
/// The move-out suppressor's Map/Set arm nulls the SOURCE slot (a
/// branch-safe runtime sentinel), which defuses the source's queued
/// `FreeMapHandle` — and the let-path Map/Set track is gated on a
/// fresh-handle RHS, so the destination was never tracked and the ENTIRE
/// map (handle + kv arrays + stored heap) leaked on every rebind.
/// `transfer_map_handle_on_rebind` now copies the source's queued free
/// onto the destination's slot. This test pins the BEHAVIOR half:
/// rebind-then-return stays readable in the caller (a wrong fix that
/// dropped the source-null instead would let the source's drain free the
/// returned handle — UAF), chained rebinds mutate through the final
/// binding, and a Set rebind reads back. The MEMORY half (exactly one
/// free) is pinned by `asan_map_value_user_drop_bodies_fire_once`'s
/// rebind block in `tests/memory_sanitizer.rs`.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_map_whole_rebind_transfers_ownership` on identical source and
/// expected output.
#[test]
fn e2e_map_whole_rebind_transfers_ownership() {
    let Some(out) = run_program(
        "fn make() -> Map[i64, i64] {\n\
             \x20   let mut m: Map[i64, i64] = Map.new();\n\
             \x20   m.insert(1, 11);\n\
             \x20   let m2 = m;\n\
             \x20   m2\n\
             }\n\
             fn main() {\n\
             \x20   let got = make();\n\
             \x20   println(got.get(1).unwrap());\n\
             \x20   let mut a: Map[i64, i64] = Map.new();\n\
             \x20   a.insert(3, 30);\n\
             \x20   let b = a;\n\
             \x20   let mut c = b;\n\
             \x20   c.insert(4, 40);\n\
             \x20   println(c.len());\n\
             \x20   println(c.get(4).unwrap());\n\
             \x20   let mut s: Set[i64] = Set.new();\n\
             \x20   s.insert(9);\n\
             \x20   let s2 = s;\n\
             \x20   println(s2.contains(9));\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "11\n2\n40\ntrue\n");
}

/// B-2026-07-30-11 (Set-elements leg) — a `Set[DropStruct]` element's
/// body fires at the binding's death: Set lowers to the KEY half of the
/// KaracMap table, so the walk is the key-side sibling of the values
/// walk (`__karac_dropelems_map_*` with the element at blob offset 0).
/// Twin of `tests/interpreter.rs`'s `test_set_element_drop_bodies_fire`,
/// same source and expected string.
#[test]
fn e2e_set_element_drop_bodies_fire() {
    let Some(out) = run_program(
        "#[derive(Hash, Eq)]\n\
             struct Res { id: i64 }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let mut s: Set[Res] = Set.new();\n\
             \x20   s.insert(Res { id: 5 });\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 5\nend\n");
}

/// B-2026-09-12-28 — a `Map.get` whose value is an enum WIDER than the
/// seeded 3-word `Option` area is boxed by `coerce_to_payload_words`, and
/// for a fresh-temp scrutinee that box now lives on the stack.
///
/// The optimization's whole safety argument is that the box pointer never
/// escapes the construct: an arm binds the payload through
/// `reconstruct_payload_value`, which `inttoptr`s word 0 and LOADS `T`, so
/// the binding owns T's inner heap and the box is a pure container. This
/// test walks every shape that reaches the box and pins the VALUES, so a
/// future change that silences a fault by dropping the payload instead of
/// handing it over shows up here rather than only in the ASAN twin
/// (`asan_map_get_stack_boxed_payload_has_one_owner`).
///
/// Case 8 is the one that matters most: the SOURCE MAP must still read
/// correctly after every preceding case, which is what proves the box
/// never owned the bucket's heap.
#[test]
fn e2e_map_get_wide_enum_payload_survives_stack_boxing() {
    let Some(out) = run_program(
            "enum B {\n\
             \x20\x20\x20\x20S(String),\n\
             \x20\x20\x20\x20C,\n\
             }\n\
             struct Wide {\n\
             \x20\x20\x20\x20a: String,\n\
             \x20\x20\x20\x20b: String,\n\
             \x20\x20\x20\x20n: i64,\n\
             }\n\
             enum W {\n\
             \x20\x20\x20\x20V(Wide),\n\
             \x20\x20\x20\x20E,\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[String, B] = Map.new();\n\
             \x20\x20\x20\x20let _ = m.insert(\"k1\", B.S(\"v1\"));\n\
             \x20\x20\x20\x20let _ = m.insert(\"k2\", B.C);\n\
             \x20\x20\x20\x20let mut i = 0;\n\
             \x20\x20\x20\x20let mut hits = 0;\n\
             \x20\x20\x20\x20while i < 5 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20let u = match m.get(\"k1\") {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20None => false,\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Some(B.S(w)) => w == \"v1\",\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Some(B.C) => false,\n\
             \x20\x20\x20\x20\x20\x20\x20\x20};\n\
             \x20\x20\x20\x20\x20\x20\x20\x20if u {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20hits = hits + 1;\n\
             \x20\x20\x20\x20\x20\x20\x20\x20}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"1 hits {hits}\");\n\
             \x20\x20\x20\x20let mut moved = String.new();\n\
             \x20\x20\x20\x20match m.get(\"k1\") {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => {}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(B.S(w)) => {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20moved = w;\n\
             \x20\x20\x20\x20\x20\x20\x20\x20}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(B.C) => {}\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"2 moved {moved}\");\n\
             \x20\x20\x20\x20match m.get(\"zz\") {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => println(\"3 none\"),\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(B.S(w)) => println(f\"3 {w}\"),\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(B.C) => println(\"3 conf\"),\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20match m.get(\"k1\") {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => println(\"4 n\"),\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(_) => println(\"4 s\"),\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20if let Some(B.S(w)) = m.get(\"k1\") {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20println(f\"5 {w}\");\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20match m.get(\"k2\") {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => println(\"6 none\"),\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(B.S(w)) => println(f\"6 {w}\"),\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(B.C) => println(\"6 conf\"),\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20let mut wm: Map[String, W] = Map.new();\n\
             \x20\x20\x20\x20let _ = wm.insert(\"w\", W.V(Wide { a: \"aa\", b: \"bb\", n: 7 }));\n\
             \x20\x20\x20\x20let mut j = 0;\n\
             \x20\x20\x20\x20while j < 3 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20match wm.get(\"w\") {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20None => println(\"7 none\"),\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Some(W.V(x)) => println(f\"7 {x.a}{x.b}{x.n}\"),\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Some(W.E) => println(\"7 e\"),\n\
             \x20\x20\x20\x20\x20\x20\x20\x20}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20j = j + 1;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20match m.get(\"k1\") {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => println(\"8 none\"),\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(B.S(w)) => println(f\"8 {w}\"),\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(B.C) => println(\"8 conf\"),\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(\"end\");\n\
             }\n\
             ",
        ) else {
            return;
        };
    assert_eq!(
        out,
        "1 hits 5\n2 moved v1\n3 none\n4 s\n5 v1\n6 conf\n7 aabb7\n7 aabb7\n7 aabb7\n8 v1\nend\n"
    );
}

/// B-2026-08-31-39 — an `Option`/`Result` payload must not inherit the
/// payload type a previous SAME-NAMED binding registered.
///
/// FORMERLY `codegen_declined_option_payload_does_not_inherit_a_stale_
/// same_named_entry`, and renamed because the word "declined" stopped being
/// true: the rendering half of -39 landed and these shapes now PRINT. The
/// five programs are unchanged; only the assertion moved from "takes the
/// structured refusal" to "matches the interpreter", which is what the
/// fixture's own NOTE ON SCOPE below said to do. `var_option_payload_te` / `var_result_payload_te` are keyed
/// by BINDING NAME and outlive the function that registered them, so a
/// declined registration used to leave the earlier entry standing, and the
/// Display path then rendered THIS payload's words at the PREVIOUS
/// payload's type.
///
/// Two monomorphs of one generic fn share their parameter's name, which is
/// what made this reachable: `show(Some(7))` registers `x -> i64`, then the
/// `Array`/`Vec`/`Slice` instantiation is declined and inherits it. The
/// symptoms were ORDER-DEPENDENT and ranged from a wrong scalar to a
/// SEGFAULT — measured pre-fix, all with the interpreter printing the right
/// answer:
///
///     i64 then Vec       Some(94432365992720)   raw pointer word as i64
///     i64 then Array     Some(1)                first element
///     String then Vec    Some()                 control block as a String
///     String then Array  <no output, rc=139>    SEGFAULT
///     Result, i64 then Vec        Ok(94527773793040)
///     generic METHOD, i64 then Vec   1:Some(93889133488912)
///
/// Reversing the order (aggregate FIRST) refused cleanly, which is the tell
/// that the stale entry — not the declined shape — was doing the damage.
///
/// This is the same hole B-2026-08-31-49 closed for the CROSS-kind case (a
/// stale `Result` entry answering for an `Option` name); the SAME-kind half
/// stayed open and is the one that segfaults. Retracting unconditionally
/// restores the clean refusal that `codegen_declined_option_payload_names_
/// its_shape` pins.
///
/// NOTE ON SCOPE, as originally written: "this does NOT make the aggregate
/// instantiations render — that is -39's still-open half, and these rows
/// assert a REFUSAL, not an answer. If a later change teaches codegen to
/// render them, these `codegen_error` calls are the ones to convert to
/// `run_program` assertions against the interpreter's output quoted above."
/// That change has landed, and this is that conversion. The expected
/// strings below ARE the interpreter's output for the same five programs.
///
/// The stale-entry defect this fixture was written for is now unreachable
/// by construction rather than by retraction: it needed a DECLINED
/// registration to leave the earlier entry standing, and no registration in
/// these programs is declined any more. The retraction is still in the
/// compiler and still correct; these rows now also prove the thing it was
/// protecting — each instantiation renders at ITS OWN payload type — which
/// is a strictly stronger assertion than "it refused".
///
/// REVERSED ORDER is asserted too. Aggregate-first was the direction that
/// always worked, so pre-fix it proved only that the stale entry was the
/// damage; post-fix both directions must render, and a regression that
/// re-poisons the name map would show up in exactly one of them.
///
/// The last two cases are the controls, and they are the reason the fix is
/// a retraction rather than a blanket clear: two DIFFERENT scalar
/// instantiations in one program must still both render (the second
/// registration legitimately succeeds and must win), and B-2026-08-31-49's
/// own case must still pass.
#[test]
fn codegen_option_payload_does_not_inherit_a_stale_same_named_entry() {
    // Each row: a scalar instantiation FIRST, then an aggregate one that
    // must render at ITS OWN payload type rather than at the scalar's.
    let poisoned: &[(&str, &str, &str)] = &[
        (
            "i64 then Vec",
            r#"fn show[T: Display](x: Option[T]) { println(f"{x}"); }
fn main() {
    show(Some(7));
    let v: Vec[i64] = vec![1];
    show(Some(v));
}
"#,
            "Some(7)\nSome([1])\n",
        ),
        (
            "Vec then i64 (reversed)",
            r#"fn show[T: Display](x: Option[T]) { println(f"{x}"); }
fn main() {
    let v: Vec[i64] = vec![1];
    show(Some(v));
    show(Some(7));
}
"#,
            "Some([1])\nSome(7)\n",
        ),
        (
            "String then Array",
            r#"fn show[T: Display](x: Option[T]) { println(f"{x}"); }
fn main() {
    show(Some("hi"));
    let a: Array[i64, 2] = [1, 2];
    show(Some(a));
}
"#,
            "Some(hi)\nSome([1, 2])\n",
        ),
        (
            "i64 then Slice",
            r#"fn show[T: Display](x: Option[T]) { println(f"{x}"); }
fn main() {
    show(Some(7));
    let v: Vec[i64] = vec![1, 2];
    show(Some(v.as_slice()));
}
"#,
            "Some(7)\nSome([1, 2])\n",
        ),
        (
            "Result, i64 then Vec",
            r#"fn mk1() -> Result[i64, i64] { return Ok(7); }
fn mk2() -> Result[Vec[i64], i64] { return Ok(vec![1]); }
fn showr[T: Display, E: Display](x: Result[T, E]) { println(f"{x}"); }
fn main() {
    showr(mk1());
    showr(mk2());
}
"#,
            "Ok(7)\nOk([1])\n",
        ),
        (
            "generic method, i64 then Vec",
            r#"struct H { n: i64 }
impl H {
    fn show[T: Display](ref self, x: Option[T]) { println(f"{self.n}:{x}"); }
}
fn main() {
    let h = H { n: 1 };
    h.show(Some(7));
    let v: Vec[i64] = vec![1];
    h.show(Some(v));
}
"#,
            "1:Some(7)\n1:Some([1])\n",
        ),
    ];
    for (label, src, want) in poisoned {
        let Some(out) = run_program(src) else {
            return;
        };
        assert_eq!(
            out, *want,
            "{label}: each instantiation must render at its own payload type"
        );
    }

    // Control 1 — two DIFFERENT scalar instantiations still both render.
    // The retraction must not clear an entry the current binding is about
    // to register successfully.
    let Some(out) = run_program(
        r#"fn show[T: Display](x: Option[T]) { println(f"{x}"); }
fn main() {
    show(Some(7));
    show(Some("hi"));
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "Some(7)\nSome(hi)\n");

    // Control 2 — B-2026-08-31-49's own case, the cross-kind direction.
    let Some(out) = run_program(
        r#"fn mkerr() -> Result[i64, i64] { return Err(9); }
fn show[T: Display](x: Option[T]) { println(f"{x}"); }
fn showr[T: Display, E: Display](x: Result[T, E]) { println(f"{x}"); }
fn main() {
    show(Some(3));
    showr(mkerr());
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "Some(3)\nErr(9)\n");
}

/// B-2026-08-01-28 — the B-2026-08-01-24 for-loop element double-free
/// through the consume arms its fix did not cover: `m.insert(k, h)`
/// (Map value), `set.insert(p)` (Set element = Map key), and
/// `names.push(h.name)` (a consuming FIELD read at an arg position).
/// All three aborted with free(): double free under JIT/AOT pre-fix.
/// The map/set arms now run the copy-depth == drop-depth deep-copy on
/// the staged key/value slots; the field read routes through the
/// borrowed-receiver defensive-copy arm (a loop element does not own
/// its fields — the container drain does). Twin of
/// `tests/interpreter.rs`'s `test_for_loop_elem_map_set_field_consumes`.
#[test]
fn e2e_for_loop_elem_map_set_field_consumes() {
    let Some(out) = run_program(
        "#[derive(Hash, Eq, Ord)]\n\
             struct P { a: i64, s: String }\n\
             struct Header { name: String, value: String }\n\
             fn main() {\n\
             \x20   let mut hs: Vec[Header] = Vec.new();\n\
             \x20   hs.push(Header { name: f\"n{1}\", value: f\"v{1}\" });\n\
             \x20   hs.push(Header { name: f\"n{2}\", value: f\"v{2}\" });\n\
             \x20   let mut m: Map[i64, Header] = Map.new();\n\
             \x20   let mut i = 0;\n\
             \x20   for h in hs {\n\
             \x20       let _ = m.insert(i, h);\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   match m.get(1) {\n\
             \x20       Some(h) => println(h.value),\n\
             \x20       None => println(\"none\"),\n\
             \x20   }\n\
             \x20   let mut ps: Vec[P] = Vec.new();\n\
             \x20   ps.push(P { a: 1, s: f\"x{1}\" });\n\
             \x20   ps.push(P { a: 2, s: f\"y{2}\" });\n\
             \x20   let mut set: Set[P] = Set.new();\n\
             \x20   for p in ps {\n\
             \x20       set.insert(p);\n\
             \x20   }\n\
             \x20   println(set.len());\n\
             \x20   let mut src: Vec[Header] = Vec.new();\n\
             \x20   src.push(Header { name: f\"a{7}\", value: f\"b{7}\" });\n\
             \x20   let mut names: Vec[String] = Vec.new();\n\
             \x20   for h in src {\n\
             \x20       names.push(h.name);\n\
             \x20   }\n\
             \x20   for n in names { println(n); }\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "v2\n2\na7\n");
}

/// B-2026-08-10-21, the TYPE axis — `Map`, `Set` and user-struct moves.
///
/// The last uncovered families, and the two collection cases were the most
/// severe symptom this bug had: a `Map`/`Set` handle bit-copied to a second
/// owner meant both freed one `KaracMap`, so the reuse SEGFAULTED rather
/// than printing garbage.
///
/// Each family needed its own duplicator at the same hook — the synthesized
/// map clone for `Map`/`Set` (a `Set[T]` clones as `Map[T, ()]`), and field
/// recursion for a user struct, whose own words are already bit-copied so
/// what aliases is the heap its FIELDS point at.
///
/// `shared struct` is deliberately absent: it is RC-managed, so a move is
/// an aliasing acquire rather than a transfer and there is no second free
/// to prevent.
#[test]
fn test_e2e_use_after_move_defensive_copy_map_set_struct() {
    // 1. Map — SEGFAULTED before.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut m: Map[String, i64] = Map.new();\n\
                     m.insert(f\"k\", 7i64);\n\
                     { let keep: Map[String, i64] = m; }\n\
                     println(m.get(f\"k\").unwrap_or(0i64));\n\
                 }"
        )
        .as_deref(),
        Some("7\n")
    );
    // 2. Set — SEGFAULTED before.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut st: Set[i64] = Set.new();\n\
                     st.insert(9i64);\n\
                     { let keep: Set[i64] = st; }\n\
                     println(st.contains(9i64));\n\
                 }"
        )
        .as_deref(),
        Some("true\n")
    );
    // 3. A struct carrying a heap field — printed an empty String before.
    assert_eq!(
        run_program(
            "struct H { name: String }\n\
                 fn main() {\n\
                     let h = H { name: f\"alpha\" };\n\
                     { let keep: H = h; }\n\
                     println(h.name);\n\
                 }"
        )
        .as_deref(),
        Some("alpha\n")
    );
    // 4. A Map whose VALUES are heap — the clone must walk the entries,
    // not merely duplicate the handle.
    //
    // Read with `match` rather than `.unwrap_or(...)` on purpose: the
    // heap-valued `m.get(k).unwrap_or(d)` spelling double-frees on its own,
    // with NO move anywhere in the program (filed separately as
    // B-2026-08-11-11). Using it here would have made this case fail for a
    // reason that has nothing to do with the defensive copy.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut m: Map[String, String] = Map.new();\n\
                     m.insert(f\"k\", f\"alpha\");\n\
                     { let keep: Map[String, String] = m; }\n\
                     match m.get(f\"k\") { Some(v) => println(v), None => println(f\"-\") }\n\
                 }"
        )
        .as_deref(),
        Some("alpha\n")
    );
}

/// B-2026-08-01-17 — a SORTED container's element/value Drop bodies
/// drain in the container's public iteration order (ascending keys):
/// `SortedMap[i64, DropV]` values walk sorted keys + `karac_map_get`,
/// and `SortedSet[DropT]` struct elements sort via the new field-wise
/// struct comparator. Pre-fix both drained in bucket order — observably
/// divergent from the interpreter on any key set whose hash order
/// differs (6,5,7 here). Plain Map/Set stay bucket-order by design
/// (unspecified, like their iteration order). Twin of
/// `tests/interpreter.rs`'s
/// `test_sorted_container_drop_bodies_key_order`.
#[test]
fn e2e_sorted_container_drop_bodies_key_order() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             #[derive(Hash, Eq, Ord)]\n\
             struct Tag { id: i64 }\n\
             impl Drop for Tag {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"tag {self.id}\")\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let mut m: SortedMap[i64, Res] = SortedMap.new();\n\
             \x20   let _ = m.insert(6, Res { id: 6, name: f\"s{6}\" });\n\
             \x20   let _ = m.insert(5, Res { id: 5, name: f\"s{5}\" });\n\
             \x20   let _ = m.insert(7, Res { id: 7, name: f\"s{7}\" });\n\
             \x20   println(\"b\");\n\
             \x20   let mut s: SortedSet[Tag] = SortedSet.new();\n\
             \x20   s.insert(Tag { id: 6 });\n\
             \x20   s.insert(Tag { id: 5 });\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a\ndrop 5 s5\ndrop 6 s6\ndrop 7 s7\nb\ntag 5\ntag 6\nend\n"
    );
}

/// B-2026-08-01-17 (comparator leg) — `SortedSet` with a STRUCT element
/// type iterates under `karac build`: the field-wise comparator
/// (declaration-order lexicographic, derived-`Ord` semantics — i64
/// first, String tie-break here) replaces the former loud codegen
/// error \"only integer and String keys sort in codegen\". NOTE: this
/// test is compile-gated pre-fix (the error made `run_program` skip),
/// so the non-vacuity proof rides the drop-order twin above, which
/// compiled and mis-ordered pre-fix.
#[test]
fn e2e_sortedset_struct_key_iteration() {
    let Some(out) = run_program(
        "#[derive(Hash, Eq, Ord)]\n\
             struct P { a: i64, s: String }\n\
             fn main() {\n\
             \x20   let mut set: SortedSet[P] = SortedSet.new();\n\
             \x20   set.insert(P { a: 2, s: f\"zz{2}\" });\n\
             \x20   set.insert(P { a: 1, s: f\"bb{1}\" });\n\
             \x20   set.insert(P { a: 1, s: f\"aa{1}\" });\n\
             \x20   for x in set {\n\
             \x20       println(f\"{x.a} {x.s}\");\n\
             \x20   }\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "1 aa1\n1 bb1\n2 zz2\n");
}

/// B-2026-09-06-60 — a by-value param whose struct has a direct `Map` field
/// double-freed with NO rebind at all: `fn norebind(q: Q) -> i64 { return
/// q.id; }` over `struct Q { id: i64, name: String, tbl: Map[i64, i64] }`
/// aborted under the JIT and at -O0 and SEGFAULTED at -O2, while `--interp`
/// was correct.
///
/// A `Map` field declines copy support (an entry copy cannot duplicate a
/// side-table handle) but is neither `shared` nor self-referential, so the
/// callee takes the param BY TRANSFER (B-2026-08-05-33) and owns it. That
/// arm's safety argument is a caller-side retraction held in lockstep — and
/// the retraction, `move_declined_copy_struct_arg`, reads an `Identifier`,
/// so it covers a NAMED argument only. A fresh temp went to the
/// argument-temp registrar instead, which registered the caller's own
/// wrapper beside the callee's. Both of that registrar's struct arms now
/// stand down under the same `struct_param_owned_by_transfer` predicate its
/// struct-literal sibling already consulted — the `Drop`-bearing arm and the
/// no-`Drop` one, which is the row's third cell.
///
/// The rebind spelling needed the other half: under transfer the callee owns
/// the value, so `let m = q;` carries the BODY as well as the memory, where
/// the param-view path would have registered memory alone (measured as no
/// body at all once the double free was gone).
///
/// Cells: the bare temp argument, one that reads the `Map`, the rebind, the
/// same struct with no `impl Drop`, a copy-supported struct as the control
/// that must keep its caller-side temp drop, and the same value never passed
/// to a callee.
///
/// Twin of `tests/interpreter.rs`'s `test_map_field_param_by_transfer`, pinned to the same string.
#[test]
fn e2e_map_field_param_by_transfer() {
    let Some(out) = run_program(
        r#"struct Q { id: i64, name: String, tbl: Map[i64, i64] }
impl Drop for Q { fn drop(mut ref self) { println(f"  dQ{self.id}") } }
struct QNoDrop { id: i64, name: String, tbl: Map[i64, i64] }
struct P { id: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"  dP{self.id}") } }

fn mkq(i: i64) -> Q { let mut t = Map[i64, i64].new(); t.insert(i, i); return Q { id: i, name: f"h{i}", tbl: t }; }
fn mkqn(i: i64) -> QNoDrop { let mut t = Map[i64, i64].new(); t.insert(i, i); return QNoDrop { id: i, name: f"h{i}", tbl: t }; }
fn mkp(i: i64) -> P { return P { id: i, name: f"p{i}", xs: [i] }; }

fn norebind(q: Q) -> i64 { return q.id; }
fn readmap(q: Q) -> i64 { return q.tbl.len(); }
fn rebind(q: Q) -> i64 { let m = q; return m.id; }
fn nodrop(q: QNoDrop) -> i64 { return q.id; }
fn copyable(p: P) -> i64 { let m = p; return m.id; }

fn main() {
    println("temp_arg"); println(f"  v={norebind(mkq(1))}");
    println("temp_arg_reads_map"); println(f"  v={readmap(mkq(2))}");
    println("temp_arg_rebind"); println(f"  v={rebind(mkq(3))}");
    println("no_drop_struct"); println(f"  v={nodrop(mkqn(4))}");
    println("copy_supported"); println(f"  v={copyable(mkp(5))}");
    println("no_call"); let a = mkq(6); println(f"  v={a.id}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"temp_arg
  dQ1
  v=1
temp_arg_reads_map
  dQ2
  v=1
temp_arg_rebind
  dQ3
  v=3
no_drop_struct
  v=4
copy_supported
  dP5
  v=5
no_call
  v=6
  dQ6
end
"#
    );
}

/// B-2026-07-30-11 (SortedMap-values leg) — a `SortedMap[K, V]` whose V
/// runs a user Drop fires each stored value's body at the binding's NLL
/// death, exactly like `Map` (they share the KaracMap lowering; the
/// registration head-gates just never admitted "SortedMap"). Values
/// walk in key order here; composition with the displaced-insert
/// discard (`let _ = sm.insert(existing, …)`) pins the whole chain.
/// Twin of `tests/interpreter.rs`'s
/// `test_sortedmap_value_drop_bodies_fire`.
#[test]
fn e2e_sortedmap_value_drop_bodies_fire() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let mut sm: SortedMap[i64, Res] = SortedMap.new();\n\
             \x20   sm.insert(2, Res { id: 72 });\n\
             \x20   sm.insert(1, Res { id: 71 });\n\
             \x20   println(\"a\");\n\
             \x20   let _ = sm.insert(1, Res { id: 73 });\n\
             \x20   println(\"b\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 71\ndrop 73\ndrop 72\nb\nend\n");
}

/// The opted-out map still has to WORK — a different hash is only useful
/// if every key it stored is still findable through it, and a removal
/// still rebuilds the right buckets.
#[test]
fn an_fx_hashed_map_still_finds_and_removes_its_keys() {
    let out = run_program(
        "fn main() {\n\
                 let mut m: Map[String, i64, FxBuildHasher] = Map.new();\n\
                 m.insert(\"alpha\", 1);\n\
                 m.insert(\"bravo\", 2);\n\
                 m.insert(\"charlie\", 3);\n\
                 m.remove(\"bravo\");\n\
                 match m.get(\"alpha\") { Some(v) => { println(v); } None => { println(-1); } }\n\
                 match m.get(\"charlie\") { Some(v) => { println(v); } None => { println(-1); } }\n\
                 match m.get(\"bravo\") { Some(v) => { println(v); } None => { println(-1); } }\n\
                 println(m.len());\n\
             }",
    );
    assert_eq!(out, Some("1\n3\n-1\n2\n".to_string()));
}

/// A `Set[T, H]` takes the selector in its own trailing position, and an
/// Fx-hashed set is still a set.
#[test]
fn an_fx_hashed_set_still_dedupes_and_answers_contains() {
    let out = run_program(
        "fn main() {\n\
                 let mut s: Set[String, FxBuildHasher] = Set.new();\n\
                 s.insert(\"alpha\");\n\
                 s.insert(\"bravo\");\n\
                 s.insert(\"alpha\");\n\
                 println(s.len());\n\
                 if s.contains(\"bravo\") { println(1); } else { println(0); }\n\
                 if s.contains(\"charlie\") { println(1); } else { println(0); }\n\
             }",
    );
    assert_eq!(out, Some("2\n1\n0\n".to_string()));
}

/// The whole point: a map hashed by user code still finds, removes and
/// counts. If any of the three emitted calls were missing the digest would
/// collapse to a constant, which this would NOT catch — every lookup still
/// succeeds through `==` — so the ordering test below is the one that
/// proves the user's permutation is actually running.
#[test]
fn a_user_hashed_map_still_finds_and_removes_its_keys() {
    let out = run_program(&format!(
            "{USER_HASHERS}\
             fn main() {{\n\
                 let mut m: Map[String, i64, FnvBuild] = Map.new();\n\
                 m.insert(\"alpha\", 1);\n\
                 m.insert(\"bravo\", 2);\n\
                 m.insert(\"charlie\", 3);\n\
                 m.remove(\"bravo\");\n\
                 match m.get(\"alpha\") {{ Some(v) => {{ println(v); }} None => {{ println(-1); }} }}\n\
                 match m.get(\"charlie\") {{ Some(v) => {{ println(v); }} None => {{ println(-1); }} }}\n\
                 match m.get(\"bravo\") {{ Some(v) => {{ println(v); }} None => {{ println(-1); }} }}\n\
                 println(m.len());\n\
             }}"
        ));
    assert_eq!(out, Some("1\n3\n-1\n2\n".to_string()));
}

/// B-2026-07-26-2: the bucket control byte carries a 7-bit hash tag, and
/// `src/codegen/mono.rs` emits its own probe loops against that encoding —
/// so the runtime and the emitted code have to agree. They cannot be
/// checked against each other directly (different crates), and disagreement
/// is a lookup that MISSES A PRESENT KEY: a silent wrong answer, not a
/// crash or a link error. This exercises the emitted probes end-to-end on
/// the conditions that would expose it.
///
/// Three probe families, since each is emitted separately: the scalar Map
/// (`Map[i64, i64]`), the String-key Map, and `Set[String].contains`. Each
/// drives enough keys to force several resizes — every control byte is
/// re-derived on a resize, so a tag written one way and tested another
/// survives an unresized table and fails here — then removes a third of
/// them to leave tombstones mid-chain and re-inserts, and finally asserts
/// BOTH directions: every live key is found with its right value, and every
/// absent key still misses (the direction a too-permissive tag breaks).
#[test]
fn map_control_byte_tag_survives_resize_and_tombstones() {
    let src = r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    let mut i = 0;
    while i < 900 {
        let _ = m.insert(i, i * 7);
        i = i + 1;
    }
    let mut j = 0;
    while j < 900 {
        let _ = m.remove(j);
        j = j + 3;
    }
    let mut k = 0;
    while k < 900 {
        let _ = m.insert(k, k * 11);
        k = k + 3;
    }
    let mut live = 0;
    let mut wrong = 0;
    let mut n = 0;
    while n < 900 {
        match m.get(n) {
            Some(v) => {
                live = live + 1;
                let want = if n % 3 == 0 { n * 11 } else { n * 7 };
                if v != want { wrong = wrong + 1; }
            }
            None => {}
        }
        n = n + 1;
    }
    let mut ghost = 0;
    let mut g = 900;
    while g < 1000 {
        match m.get(g) {
            Some(v) => {
                ghost = ghost + 1;
            }
            None => {}
        }
        g = g + 1;
    }
    println(live);
    println(wrong);
    println(ghost);
    println(m.len() as i64);

    let mut sm: Map[String, i64] = Map.new();
    let mut a = 0;
    while a < 600 {
        let _ = sm.insert("key" + a.to_string(), a * 3);
        a = a + 1;
    }
    let mut b = 0;
    while b < 600 {
        let _ = sm.remove("key" + b.to_string());
        b = b + 4;
    }
    let mut sfound = 0;
    let mut swrong = 0;
    let mut c = 0;
    while c < 600 {
        match sm.get("key" + c.to_string()) {
            Some(v) => {
                sfound = sfound + 1;
                if v != c * 3 { swrong = swrong + 1; }
            }
            None => {}
        }
        c = c + 1;
    }
    let mut sghost = 0;
    let mut d = 600;
    while d < 700 {
        match sm.get("key" + d.to_string()) {
            Some(v) => {
                sghost = sghost + 1;
            }
            None => {}
        }
        d = d + 1;
    }
    println(sfound);
    println(swrong);
    println(sghost);

    let mut st: Set[String] = Set.new();
    let mut e = 0;
    while e < 600 {
        st.insert("w" + e.to_string());
        e = e + 1;
    }
    let mut hits = 0;
    let mut f = 0;
    while f < 600 {
        if st.contains("w" + f.to_string()) { hits = hits + 1; }
        f = f + 1;
    }
    let mut misses = 0;
    let mut h = 600;
    while h < 700 {
        if st.contains("w" + h.to_string()) { misses = misses + 1; }
        h = h + 1;
    }
    println(hits);
    println(misses);
}
"#;
    let out = run_program(src).expect("map control-byte program should build and run");
    let got: Vec<&str> = out.lines().collect();
    // 900 live keys (removed ones were all re-inserted), none with a wrong
    // value, no phantom hits, len back to 900.
    assert_eq!(
        got[0], "900",
        "scalar map lost keys across resize+tombstones"
    );
    assert_eq!(got[1], "0", "scalar map returned a wrong value");
    assert_eq!(got[2], "0", "scalar map found an absent key");
    assert_eq!(got[3], "900", "scalar map len drifted");
    // 450 String keys survive (every 4th of 600 removed = 150 gone).
    assert_eq!(got[4], "450", "String-key map lost keys");
    assert_eq!(got[5], "0", "String-key map returned a wrong value");
    assert_eq!(got[6], "0", "String-key map found an absent key");
    assert_eq!(got[7], "600", "Set[String].contains missed a present key");
    assert_eq!(got[8], "0", "Set[String].contains found an absent key");
}

/// B-2026-08-13-15, the shape that found it: a parity set over a string's
/// bytes, which is the natural spelling of kata #266's toggle solver.
///
/// `s.bytes()` yields `u8`, the set is `Set[i64]`, and `contains` never
/// reported true — so the `remove` branch was dead and `insert` was a
/// no-op on an existing key. The set silently became the DISTINCT-character
/// set instead of the odd-parity one, which is why it hid so well: "aa",
/// "zzzzzzzz" and "code" all give the right answer under the wrong set. The
/// cases here are chosen so that the two sets DISAGREE.
///
/// WHAT THIS TEST IS AND IS NOT, measured rather than assumed: at the
/// harness's DEFAULT opt level this shape was GREEN pre-fix, and it went
/// red only at `KARAC_OPT_LEVEL=0` (`aab -> false`, exactly as reported) —
/// because the defect was undefined slot bytes, and at -O2 the insert and
/// the lookup happened to fold to the same residue while at -O0 they did
/// not. So this is the real-world SHAPE on the interpreter oracle, not the
/// regression guard; the deterministic guard is the sibling test above,
/// which fails pre-fix on all 14 of its lines. Keeping both is the same
/// call the String-consume-site test makes a few thousand lines up: when
/// opt level changes what a defect looks like, say so next to the
/// assertion instead of trusting one level's verdict.
#[test]
fn test_e2e_byte_parity_set_matches_interpreter() {
    let src = r#"
fn can_permute_palindrome(s: String) -> bool {
    let mut odd: Set[i64] = Set.new();
    for b in s.bytes() {
        if odd.contains(b) {
            odd.remove(b);
        } else {
            odd.insert(b);
        }
    }
    odd.len() <= 1
}

fn main() {
    let cases = vec!["aab", "aabb", "carerac", "aaabbb", "abc"];
    for c in cases {
        println(f"{c} {can_permute_palindrome(c)}");
    }
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // The distinct-character set answers `false` for "aab" and "aabb";
    // the odd-parity set answers `true`. Pin that the oracle is the
    // latter, so agreement cannot be reached on the wrong set.
    assert!(
        expected.contains("aab true") && expected.contains("aabb true"),
        "oracle is not computing the odd-parity set: {expected:?}"
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "compiled byte-parity set must match the interpreter"
    );
}

#[test]
fn test_ir_map_keys_loop_over_heap_half_is_lazy_not_materialized() {
    // B-2026-07-25-4: `for k in m.keys()` (and `.values()`/`.entries()`)
    // over a map with a HEAP half kept paying the eager materializer — a
    // `malloc`, a `karac_map_len`, a full copy of the key set into that
    // buffer, a DEEP CLONE of every heap key, then two `karac_free_buf` —
    // on EVERY evaluation. B-2026-07-24-2 routed the scalar-halved case to
    // the lazy runtime iterator but deliberately failed closed on a heap
    // half. It now routes too: the `(k, v)` destination path registers the
    // binding's side tables (`register_for_loop_bindings`) and borrow-marks
    // the heap half, which is what the eager Vec used to provide.
    //
    // Assert on the ABSENCE of the materializer, not just on output — the
    // whole bug is that a correct-but-slow lowering was chosen.
    let ir = ir_for(
        "fn total(m: ref Map[String, i64]) -> i64 {\n\
             \x20   let mut s = 0i64;\n\
             \x20   for k in m.keys() { s = s + k.len(); }\n\
             \x20   return s;\n\
             }\n\
             fn main() {\n\
             \x20   let mut m: Map[String, i64] = Map.new();\n\
             \x20   m.insert(\"aa\".to_string(), 1i64);\n\
             \x20   println(total(m));\n\
             }",
    );
    let total = ir
        .split("define")
        .find(|f| f.contains("@total"))
        .expect("@total not found in IR");
    assert!(
        total.contains("@karac_map_iter_new"),
        "a heap-half `keys()` loop must use the LAZY runtime iterator:\n{total}"
    );
    for eager in ["@malloc", "@karac_map_len", "@karac_free_buf"] {
        assert!(
            !total.contains(eager),
            "a heap-half `keys()` loop must not eagerly materialize a Vec \
                 (found `{eager}`):\n{total}"
        );
    }
}

#[test]
fn test_e2e_map_keys_values_entries_over_heap_halves_are_correct() {
    // B-2026-07-25-4 behavioural companion to the IR guard: moving a heap
    // half from an OWNED deep clone to a BORROW of the map's slot is an
    // ownership change, so pin the observable results across every shape —
    // heap key, heap value, both halves heap through a struct FIELD, a
    // consuming sink that must still get an independent copy
    // (`owned.push(k)`), an early `return` out of the loop body (the
    // iterator cleanup path), and the source map staying intact afterwards.
    if let Some(out) = run_program(
        "struct H { m: Map[String, Vec[String]] }\n\
             fn firstk(m: ref Map[String, i64]) -> i64 {\n\
             \x20   for k in m.keys() { if k.len() > 0i64 { return 1i64; } }\n\
             \x20   return 0i64;\n\
             }\n\
             fn main() {\n\
             \x20   let mut a: Map[String, i64] = Map.new();\n\
             \x20   a.insert(\"aa\".to_string(), 1i64);\n\
             \x20   a.insert(\"bbb\".to_string(), 2i64);\n\
             \x20   let mut s = 0i64;\n\
             \x20   for k in a.keys() { s = s + k.len(); }\n\
             \x20   println(s);\n\
             \x20   let mut owned: Vec[String] = Vec.new();\n\
             \x20   for k in a.keys() { owned.push(k); }\n\
             \x20   println(owned.len());\n\
             \x20   let mut t = 0i64;\n\
             \x20   for v in a.values() { t = t + v; }\n\
             \x20   println(t);\n\
             \x20   let mut e = 0i64;\n\
             \x20   for (k, v) in a.entries() { e = e + k.len() + v; }\n\
             \x20   println(e);\n\
             \x20   let mut b: Map[i64, String] = Map.new();\n\
             \x20   b.insert(1i64, \"xx\".to_string());\n\
             \x20   b.insert(2i64, \"yyy\".to_string());\n\
             \x20   let mut u = 0i64;\n\
             \x20   for v in b.values() { u = u + v.len(); }\n\
             \x20   println(u);\n\
             \x20   let mut h = H { m: Map.new() };\n\
             \x20   let mut inner: Vec[String] = Vec.new();\n\
             \x20   inner.push(\"p\".to_string());\n\
             \x20   h.m.insert(\"kk\".to_string(), inner);\n\
             \x20   let mut c = 0i64;\n\
             \x20   for k in h.m.keys() { c = c + k.len(); }\n\
             \x20   println(c);\n\
             \x20   println(firstk(a));\n\
             \x20   println(a.len());\n\
             }",
    ) {
        // The seventh line was `firstk`'s `k.len()` — 2 or 3 depending on
        // WHICH key came out first, which is now the per-process hash order
        // (B-2026-08-21-6). It returns a constant instead: the borrow of
        // `k` still happens inside the loop (the `k.len() > 0` test), so
        // the early `return` out of a `keys()` walk — the iterator-cleanup
        // path this line exists to exercise — is unchanged. Every other
        // line is a SUM over the whole map and was never order-dependent.
        assert_eq!(out, "5\n2\n3\n8\n5\n2\n1\n2\n");
    }
}

#[test]
fn test_e2e_map_entries_single_binding_still_materializes_tuple() {
    // B-2026-07-25-4 guard on the shape the routing must NOT take: a SINGLE
    // binding over `.entries()` wants one tuple VALUE per step (`e.0` /
    // `e.1`), which only the eager materializer produces. The `(k, v)`
    // receiver path binds a PAIR, so rewriting this shape bound the value
    // half to `e` and left `e.0` unregistered — the "no handler for method
    // 'bytes' on non-identifier receiver" failure that
    // `tests/cli.rs::test_run_derive_message_repeated_string_and_map_
    // roundtrip` polices (it is what `#[derive(Message)]` emits for a Map
    // field). Only a real 2-tuple pattern is rewritten.
    if let Some(out) = run_program(
        "fn main() {\n\
             \x20   let mut m: Map[String, i64] = Map.new();\n\
             \x20   m.insert(\"aa\".to_string(), 5i64);\n\
             \x20   let mut n = 0i64;\n\
             \x20   let mut l = 0i64;\n\
             \x20   for e in m.entries() { l = l + e.0.len(); n = n + e.1; }\n\
             \x20   println(l);\n\
             \x20   println(n);\n\
             }",
    ) {
        assert_eq!(out, "2\n5\n");
    }
}

/// Raw-pointer instance methods (design.md § raw pointers; additive-
/// interop Slice 4 Path A, `B-2026-07-08-4`): `.offset` (element-scaled
/// arithmetic), `.write` (store), `.read` (load), including a chained
/// `p.offset(i).write(v)` / `p.offset(i).read()` receiver. Round-trips a
/// 3-element buffer allocated via FFI `malloc` and freed via `free`.
#[test]
fn raw_pointer_offset_read_write() {
    let src = "unsafe extern \"C\" { fn malloc(n: usize) -> *mut i64; fn free(p: *mut i64); }\n\
                   fn main() {\n\
                   \x20   let p: *mut i64 = unsafe { malloc(24) };\n\
                   \x20   unsafe { p.write(10); }\n\
                   \x20   let q: *mut i64 = unsafe { p.offset(1) };\n\
                   \x20   unsafe { q.write(20); }\n\
                   \x20   unsafe { p.offset(2).write(30); }\n\
                   \x20   let s = unsafe { p.read() + p.offset(1).read() + p.offset(2).read() };\n\
                   \x20   println(s);\n\
                   \x20   unsafe { free(p); }\n\
                   }\n";
    if let Some(out) = run_program(src) {
        assert_eq!(out.trim(), "60", "pointer offset/read/write round-trip");
    }
}

/// UN-annotated chained pointer method dispatch: `let p1 = p.offset(1)` with
/// NO type annotation. Before the typechecker gained proper return-type
/// inference for pointer methods, `p.offset(..)` fell through to `Type::Error`,
/// so `p1` lost its `*const u8` type and `p1.read()` failed codegen with "no
/// handler for method 'read' on variable 'p1'". The annotated form
/// (`raw_pointer_offset_read_write` above) worked because the annotation pinned
/// the type; this pins the inference path.
#[test]
fn raw_pointer_unannotated_offset_chain() {
    let src = "fn main() {\n\
                   \x20   let s = c\"ABC\";\n\
                   \x20   let p = s.as_ptr();\n\
                   \x20   unsafe {\n\
                   \x20       let p1 = p.offset(1i64);\n\
                   \x20       let p2 = p1.offset(1i64);\n\
                   \x20       println(p1.read());\n\
                   \x20       println(p2.read());\n\
                   \x20   }\n\
                   }\n";
    if let Some(out) = run_program(src) {
        assert_eq!(out, "66\n67\n", "un-annotated offset chain reads B, C");
    }
}

#[test]
fn test_e2e_flat_map_terminals() {
    // B-2026-07-14-8 (flat_map terminals): the fused terminals treat a
    // peel-rejected flat_map receiver as a zero-step base — the
    // synthesized `for <elem> in <recv>` routes through compile_for's
    // nested-loop flat_map desugar. Covers every terminal the shared
    // peel serves. Must match the interpreter.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let vv: Vec[Vec[i64]] = [[1, 2], [3], [4, 5]];\n\
                 println(vv.iter().flat_map(|row| row.iter()).sum());\n\
                 println(vv.iter().flat_map(|row| row.iter()).count());\n\
                 println(vv.iter().flat_map(|row| row.iter()).fold(1, |a, x| a * x));\n\
                 println(vv.iter().flat_map(|row| row.iter().map(|v| v * 2)).sum());\n\
                 println(vv.iter().flat_map(|row| row.iter()).any(|v| v == 4));\n\
                 println(vv.iter().flat_map(|row| row.iter()).all(|v| v > 0));\n\
                 let m = vv.iter().flat_map(|row| row.iter()).reduce(|p, q| if p > q { p } else { q });\n\
                 println(m.unwrap_or(0 - 1));\n\
                 let mut t: i64 = 0;\n\
                 vv.iter().flat_map(|row| row.iter()).for_each(|v| { t = t + v; });\n\
                 println(t);\n\
             }",
        ) {
            assert_eq!(out, "15\n5\n120\n30\ntrue\ntrue\n5\n15\n");
        }
}

#[test]
fn test_e2e_zip_map_collect() {
    // B-2026-07-15-10: `A.iter().zip(B.iter()).map(f).collect()` — a map
    // over the zipped tuples (result `Vec[R]`, R not a 2-tuple) lowers to a
    // for-loop over the zip pushing the mapped body. Covers a single-var
    // closure param (`|p| p.0 + p.1`), a destructuring param (`|(x, y)|
    // x * y`) over a shorter source, and a String-producing map.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let a: Vec[i64] = [10, 10, 10];\n\
                 let b: Vec[i64] = [1, 2, 3];\n\
                 let c: Vec[i64] = a.iter().zip(b.iter()).map(|p| p.0 + p.1).collect();\n\
                 let mut i = 0;\n\
                 while i < c.len() { println(c[i]); i = i + 1; }\n\
                 let d: Vec[i64] = [5, 5, 5, 5];\n\
                 let e: Vec[i64] = [2, 3];\n\
                 let g: Vec[i64] = d.iter().zip(e.iter()).map(|(x, y)| x * y).collect();\n\
                 println(g.len());\n\
                 let s: Vec[String] = a.iter().zip(b.iter()).map(|p| f\"{p.0}:{p.1}\").collect();\n\
                 println(s[0]);\n\
             }",
    ) {
        assert_eq!(out, "11\n12\n13\n2\n10:1\n");
    }
}

#[test]
fn test_e2e_filter_map_scalar_and_heap() {
    // B-2026-07-19-14: `filter_map(f: Fn(T) -> Option[U])` is a fused
    // map+filter step. The FOR-LOOP path lowers it through the shared fused
    // chain (`match f(x) { Some(v) => <sink>, None => {} }`); the COLLECT
    // path routes `<filter_map-chain>.collect()` through the working
    // for-loop lowering via a `Vec.push` accumulator (the general collect
    // engine's separate peel has no `filter_map`). Covers: scalar collect,
    // scalar for-loop accumulation, a HEAP `U` (`String`) collected over an
    // enum receiver (payload sizing driven by the synthetic `Some` binding's
    // registered surface type), a HEAP for-loop, and composition with a
    // trailing `map` and a leading `filter`. Must match the interpreter.
    if let Some(out) = run_program(
            "enum Tok { Word(String), Num(i64) }\n\
             fn main() {\n\
                 let a = [1, 2, 3, 4];\n\
                 let sc: Vec[i64] = a.iter().filter_map(|x| if x > 2 { Some(x * 2) } else { None }).collect();\n\
                 println(sc.len());\n\
                 let mut s = 0;\n\
                 for n in a.iter().filter_map(|x| if x > 2 { Some(x * 2) } else { None }) { s = s + n; }\n\
                 println(s);\n\
                 let t = [Tok.Word(\"hi\".to_string()), Tok.Num(3), Tok.Word(\"yo\".to_string())];\n\
                 let w: Vec[String] = t.iter().filter_map(|k| match k { Tok.Word(z) => Some(z.to_uppercase()), Tok.Num(_) => None }).collect();\n\
                 println(w.join(\",\"));\n\
                 let hv = [\"hello\".to_string(), \"\".to_string(), \"world\".to_string()];\n\
                 let mut out = \"\".to_string();\n\
                 for hs in hv.iter().filter_map(|m| if m.len() > 0 { Some(m.to_uppercase()) } else { None }) { out.push_str(f\"{hs}|\"); }\n\
                 println(out);\n\
                 let c: Vec[i64] = a.iter().filter_map(|x| if x > 2 { Some(x * 2) } else { None }).map(|y| y + 4).collect();\n\
                 let mut cs = 0;\n\
                 for cn in c { cs = cs + cn; }\n\
                 println(cs);\n\
                 let b = [1, 2, 3, 4, 5, 6];\n\
                 let d: Vec[i64] = b.iter().filter(|x| x % 2 == 0).filter_map(|x| if x > 2 { Some(x * 2) } else { None }).collect();\n\
                 let mut ds = 0;\n\
                 for dn in d { ds = ds + dn; }\n\
                 println(ds);\n\
             }",
        ) {
            assert_eq!(out, "2\n14\nHI,YO\nHELLO|WORLD|\n22\n20\n");
        }
}

#[test]
fn test_e2e_find_map_scalar() {
    // B-2026-07-19-14: `find_map(f: Fn(T) -> Option[U]) -> Option[U]` — the
    // short-circuit map+find terminal. Desugars like `find` but the sink is
    // a synthesized `match f(x) { Some(v) => { __fm = Some(v); break }, None
    // => {} }`, sizing the `Some(v)` payload via the same span-registration
    // trick `filter_map` uses. Trivially-copyable payload only (a heap `U`
    // defers loud to `--interp`, like `find`). Covers a scalar hit, a
    // narrow-int payload (`u8`, must not be sized as i64), a float payload,
    // a `None` (no match) result, and composition with a leading `filter`.
    // Must match the interpreter.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let v = [1, 2, 3, 4, 5];\n\
                 match v.iter().find_map(|x| if x > 2 { Some(x * 2) } else { None }) { Some(n) => println(n), None => println(-1) }\n\
                 let w: Vec[u8] = [10u8, 20u8, 30u8, 40u8];\n\
                 match w.iter().find_map(|x| if x > 25u8 { Some(x) } else { None }) { Some(n) => println(n), None => println(0u8) }\n\
                 let f: Vec[f64] = [1.0, 2.0, 3.0];\n\
                 match f.iter().find_map(|x| if x > 2.0 { Some(x + 2.0) } else { None }) { Some(n) => println(n), None => println(0.0) }\n\
                 match v.iter().find_map(|x| if x > 100 { Some(x) } else { None }) { Some(n) => println(n), None => println(-1) }\n\
                 let b = [1, 2, 3, 4, 5, 6];\n\
                 match b.iter().filter(|x| x % 2 == 0).find_map(|x| if x > 2 { Some(x * 2) } else { None }) { Some(n) => println(n), None => println(-1) }\n\
             }",
        ) {
            assert_eq!(out, "6\n30\n5\n-1\n8\n");
        }
}

#[test]
fn test_e2e_slice_and_map_elem_field_read_is_a_copy() {
    // B-2026-08-13-5's VALUE side. The clone was extended to a BORROWED
    // container (a slice) and a side-table one (a map), so the thing to
    // pin is that neither lender is disturbed: after the field is consumed
    // — returned out of `take`, bound out of the map — reading it again
    // through the container yields the same string.
    //
    // That is the whole reason the fix clones instead of cap-zeroing the
    // lender's field: a read off a container is a COPY on both backends,
    // and for a BORROWED container the move model would not merely be a
    // use-after-free in this frame, it would corrupt a value this frame
    // does not own.
    //
    // `m[..].n` last: the scalar sibling of the cloned field must be
    // untouched, which a clone emitted at the wrong offset would corrupt.
    assert_eq!(
        run_program(
            "struct Pair { word: String, n: i64 }\n\
                 fn take(xs: Slice[Pair]) -> String { let w = xs[0].word; w }\n\
                 fn main() {\n\
                     let k = 1;\n\
                     let mut ps: Vec[Pair] = Vec.new();\n\
                     ps.push(Pair { word: f\"a{k}\", n: 7 });\n\
                     println(take(ps[0..1]));\n\
                     println(ps[0].word);\n\
                     let mut m: Map[String, Pair] = Map.new();\n\
                     m.insert(f\"k{k}\", Pair { word: f\"c{k}\", n: 8 });\n\
                     let mapped = m[f\"k{k}\"].word;\n\
                     println(mapped);\n\
                     println(m[f\"k{k}\"].word);\n\
                     println(m[f\"k{k}\"].n);\n\
                 }"
        )
        .as_deref(),
        Some("a1\na1\nc1\nc1\n8\n"),
    );
}

#[test]
fn test_e2e_sorted_set_min_max_still_bypass_the_iterator_desugar() {
    // The other side of B-2026-08-11-19's gate. SortedSet/SortedMap keep
    // their OWN min/max surfaces and must NOT be rewritten into an
    // `.iter()` chain — the desugar is narrowed to Vec/VecDeque receivers,
    // and moving the narrowing decision into the typechecker (where it is
    // actually made) must not have widened it.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut s: SortedSet[i64] = SortedSet.new();\n\
                     s.insert(4);\n\
                     s.insert(9);\n\
                     println(\"mx=\" + s.max().unwrap().to_string());\n\
                     println(\"mn=\" + s.min().unwrap().to_string());\n\
                 }\n"
        )
        .as_deref(),
        Some("mx=9\nmn=4\n"),
    );
}

#[test]
fn test_e2e_struct_move_out_map_set_enum_field_no_crash() {
    // B-2026-07-15-23: moving a struct that carries a Map/Set handle field or
    // an enum-with-heap-payload field — either a whole-struct move (`let g = f`)
    // or a nested-field move-out (`let bound = o.inner`) — double-freed the
    // handle / enum buffer at scope exit (SIGSEGV for Map/Set, `free(): double
    // free` for enum). `zero_struct_move_caps` had no Map/Set arm (a Map/Set
    // field is a bare `ptr` the drop frees UNCONDITIONALLY via
    // `karac_map_free_with_drop_vec`, which is null-safe), and the nested-field
    // suppressor's LLVM-type-driven `zero_aggregate_field_caps` sees neither a
    // Map/Set ptr nor an enum's all-i64 words. Fixed by (1) a Map/Set null-the-
    // handle arm in `zero_struct_move_caps` and (2) routing named-struct fields
    // through `zero_struct_move_caps` in `suppress_struct_field_move_into_literal`.
    // Output-correctness guard (the null-store must not corrupt the `.len()`
    // reads); the crash itself is caught by the ASAN sibling
    // `asan_struct_move_out_map_set_enum_field_no_double_free`. Covers Map + Set
    // whole-struct move, Map nested-field move (non-generic + generic), Vec+Map
    // mixed, and an enum-with-String-payload nested-field move.
    if let Some(out) = run_program(
        "enum Tok { Word(String), Num(i64) }\n\
             struct MHolder { m: Map[i64, i64] }\n\
             struct SHolder { s: Set[i64] }\n\
             struct MInner { m: Map[i64, i64] }\n\
             struct MOuter { inner: MInner }\n\
             struct GMInner[K, V] { m: Map[K, V] }\n\
             struct GMOuter[K, V] { inner: GMInner[K, V] }\n\
             struct VecMap { v: Vec[i64], m: Map[i64, i64] }\n\
             struct VecMapOuter { inner: VecMap }\n\
             struct EInner { t: Tok }\n\
             struct EOuter { inner: EInner }\n\
             fn main() {\n\
                 let mut mp: Map[i64, i64] = Map.new();\n\
                 mp.insert(1, 10); mp.insert(2, 20);\n\
                 let f: MHolder = MHolder { m: mp };\n\
                 let g: MHolder = f;\n\
                 println(g.m.len());\n\
                 let mut mp2: Map[i64, i64] = Map.new();\n\
                 mp2.insert(1, 1); mp2.insert(2, 2); mp2.insert(3, 3);\n\
                 let o: MOuter = MOuter { inner: MInner { m: mp2 } };\n\
                 let bound: MInner = o.inner;\n\
                 println(bound.m.len());\n\
                 let mut st: Set[i64] = Set.new();\n\
                 st.insert(7); st.insert(8);\n\
                 let sf: SHolder = SHolder { s: st };\n\
                 let sg: SHolder = sf;\n\
                 println(sg.s.len());\n\
                 let mut gm: Map[i64, i64] = Map.new();\n\
                 gm.insert(5, 50); gm.insert(6, 60); gm.insert(7, 70); gm.insert(8, 80);\n\
                 let go: GMOuter[i64, i64] = GMOuter { inner: GMInner { m: gm } };\n\
                 let gb: GMInner[i64, i64] = go.inner;\n\
                 println(gb.m.len());\n\
                 let mut vv: Vec[i64] = Vec.new();\n\
                 vv.push(1); vv.push(2); vv.push(3);\n\
                 let mut vm: Map[i64, i64] = Map.new();\n\
                 vm.insert(9, 90);\n\
                 let vmo: VecMapOuter = VecMapOuter { inner: VecMap { v: vv, m: vm } };\n\
                 let vb: VecMap = vmo.inner;\n\
                 println(vb.v.len());\n\
                 println(vb.m.len());\n\
                 let mut b: String = String.new();\n\
                 b.push_str(\"padded well past inline width to force heap alloc here\");\n\
                 let eo: EOuter = EOuter { inner: EInner { t: Tok.Word(b) } };\n\
                 let eb: EInner = eo.inner;\n\
                 match eb.t {\n\
                     Tok.Word(w) => println(w.len()),\n\
                     Tok.Num(n) => println(n),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "2\n3\n2\n4\n3\n1\n54\n");
    }
}

#[test]
fn test_e2e_mono_bare_t_heap_field_before_map_no_crash() {
    // B-2026-07-15-24: a generic struct with a bare generic-param field bound
    // to a WIDER heap type (`Vec`/`String` = 3 words) placed BEFORE a Map/Vec
    // field. The base `struct_types` layout erases the bare-T field to one i64
    // word, so every following field's drop/move-suppression GEP was offset by
    // the mono widening — a SIGSEGV/double-free at scope-exit drop (the reads
    // already used the mono layout, so only the drop crashed). Fixed by GEPing
    // the drop-synthesis + move-suppression fields with the per-monomorph
    // layout (`mono_struct_type_from_subst`) and generalizing the bare-T
    // heap-field classifier from single-field to any position. Covers: a
    // String bare-T before Map (no move); a `Vec[String]` bare-T before Map
    // (drains inner elements); two bare-T `Vec` fields before Map with a
    // whole-struct move; and a nested field move-out (`let bound = o.inner`).
    // Output guard; the crash itself is the ASAN sibling
    // `asan_mono_bare_t_heap_field_before_map_no_double_free`.
    if let Some(out) = run_program(
        "struct SBefore[T] { a: T, m: Map[i64, i64] }\n\
             struct TwoHeap[T] { a: T, b: T, m: Map[i64, i64] }\n\
             struct GInner[T] { a: T, m: Map[i64, i64] }\n\
             struct GOuter[T] { inner: GInner[T] }\n\
             fn main() {\n\
                 let mut m1: Map[i64, i64] = Map.new();\n\
                 m1.insert(1, 11);\n\
                 let s: String = \"hello\".to_string();\n\
                 let x1 = SBefore { a: s, m: m1 };\n\
                 println(x1.a.len());\n\
                 println(x1.m.get(1).unwrap());\n\
                 let mut m1b: Map[i64, i64] = Map.new();\n\
                 m1b.insert(1, 111);\n\
                 let vsa: Vec[String] = [\"aa\", \"bb\", \"cc\"];\n\
                 let x1b = SBefore { a: vsa, m: m1b };\n\
                 println(x1b.a[1]);\n\
                 println(x1b.m.get(1).unwrap());\n\
                 let mut m2: Map[i64, i64] = Map.new();\n\
                 m2.insert(2, 22);\n\
                 let v1: Vec[i64] = [1, 2, 3];\n\
                 let v2: Vec[i64] = [4, 5, 6, 7];\n\
                 let th = TwoHeap { a: v1, b: v2, m: m2 };\n\
                 let th2 = th;\n\
                 println(th2.a[2]);\n\
                 println(th2.b[3]);\n\
                 println(th2.m.get(2).unwrap());\n\
                 let mut mp: Map[i64, i64] = Map.new();\n\
                 mp.insert(9, 99);\n\
                 let vv: Vec[i64] = [7, 8, 9];\n\
                 let gi = GInner { a: vv, m: mp };\n\
                 let o = GOuter { inner: gi };\n\
                 let bound = o.inner;\n\
                 println(bound.a[0]);\n\
                 println(bound.m.get(9).unwrap());\n\
             }",
    ) {
        assert_eq!(out, "5\n11\nbb\n111\n3\n7\n22\n7\n99\n");
    }
}

#[test]
fn test_e2e_option_result_map_heap_payload() {
    // B-2026-07-12-11 heap half: `Option/Result.map` over a HEAP payload
    // (String / Vec) routes through match-synthesis, which owns the
    // move-out / drop / receiver-suppression machinery; the typechecker
    // records the mapper's solved `Fn(T)->R` so codegen types the closure.
    // Covers: scalar-returning (Vec→len, String→len, Result Ok→len), the
    // CHAINED `map(f).unwrap_or(d)` idiom (works via the Slice-1 span-
    // collision fix), identity MOVE (`|x| x`), a heap-returning mapper with
    // an annotated param (`|s: String| s.to_uppercase()`), and a named-fn
    // heap-returning mapper. interp == JIT == AOT.
    if let Some(out) = run_program(
            "fn shout(s: String) -> String { s.to_uppercase() }\n\
             fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(10); v.push(20); v.push(30);\n\
                 let opt: Option[Vec[i64]] = Some(v);\n\
                 println(f\"{opt.map(|xs| xs.len()).unwrap_or(-1)}\");\n\
                 let s: Option[String] = Some(f\"hello\");\n\
                 println(f\"{s.map(|x| x.len()).unwrap_or(-1)}\");\n\
                 let mv: Option[String] = Some(f\"kept\");\n\
                 let same = mv.map(|x| x);\n\
                 match same { Some(x) => { println(f\"{x.len()}\"); } None => { println(\"none\"); } }\n\
                 let ok: Result[String, i64] = Ok(f\"data\");\n\
                 println(f\"{ok.map(|d| d.len()).unwrap_or(-1)}\");\n\
                 let up: Option[String] = Some(f\"hi\");\n\
                 match up.map(|x: String| x.to_uppercase()) { Some(x) => { println(x); } None => { println(\"none\"); } }\n\
                 let nf: Option[String] = Some(f\"yo\");\n\
                 match nf.map(shout) { Some(x) => { println(x); } None => { println(\"none\"); } }\n\
                 let none: Option[String] = None;\n\
                 println(f\"{none.map(|x| x.len()).unwrap_or(-1)}\");\n\
             }",
        ) {
            assert_eq!(out, "3\n5\n4\n4\nHI\nYO\n-1\n");
        }
}

#[test]
fn test_e2e_result_map_heap_err_passthrough_disarms_source() {
    // B-2026-08-07-3 — the SCALAR-inner `Result[i64, String].map(f)` lowering
    // compiles its ABSENT (`Err`) branch as a SHALLOW copy of the receiver
    // (`recv_struct.into()`), so an `Err`'s heap String payload is aliased by
    // the map result. When that result is CONSUMED or DISCARDED the source
    // binding stayed armed too, and the one buffer was freed twice —
    // `free(): double free detected in tcache 2` at -O0 and under the JIT,
    // masked at the DEFAULT -O2 by DSE (so `karac build` looked clean while
    // `karac run` / the JIT-parity CI leg aborted with EMPTY output). Covers
    // all three consumption shapes the fix wires the `map_passthrough`
    // detector into: (b) `er.map(f).unwrap_or(d)` consume, (a) `let r =
    // er.map(f); r.unwrap_or(d)` let-bound consume, (c) `er.map(f);` discard
    // in statement position — plus an `Ok` receiver confirming the mapper
    // still fires. interp == JIT == AOT.
    if let Some(out) = run_program(
        "fn dbl(n: i64) -> i64 { n * 2 }\n\
             fn main() {\n\
                 let ok: Result[i64, String] = Ok(21);\n\
                 println(f\"{ok.map(dbl).unwrap_or(-1)}\");\n\
                 let eb: Result[i64, String] = Err(f\"boom-b\");\n\
                 println(f\"{eb.map(dbl).unwrap_or(-99)}\");\n\
                 let ea: Result[i64, String] = Err(f\"boom-a\");\n\
                 let r: Result[i64, String] = ea.map(dbl);\n\
                 println(f\"{r.unwrap_or(-98)}\");\n\
                 let ec: Result[i64, String] = Err(f\"boom-c\");\n\
                 ec.map(dbl);\n\
                 println(\"discarded-ok\");\n\
             }",
    ) {
        assert_eq!(out, "42\n-99\n-98\ndiscarded-ok\n");
    }
}

#[test]
fn test_e2e_result_map_preserves_a_heap_err_payload() {
    // B-2026-08-09-6 — the HEAP-inner sibling of the test above, and the
    // opposite failure. A heap `T` routes `.map` to
    // `compile_map_via_match_synthesis`, which synthesizes
    // `Err(e) => Err(e)` and seeds `e`'s binding type from
    // `method_unwrap_err_types`. The typechecker's `map` arm never wrote
    // that entry — its only producers are `unwrap_or` and the
    // absent-closure combinators — so `err_te` had been `None` since the
    // synthesis landed (4b941dc), and a heap `Err` payload was lowered as a
    // bare scalar word: the program printed the EMPTY STRING where
    // `--interp` printed the payload, and leaked the buffer.
    //
    // Silent wrong output, so an E2E pin rather than only a sanitizer one.
    // Covers the `let`-bound and inline receivers, an identity and a
    // heap-RETURNING mapper (the mapper never runs on the Err branch, but
    // it decides which lowering is picked), a `Vec` Err payload, and the
    // `Ok` branch of the same type as the control. The scalar-`E` case is
    // the one shape that always worked, kept here as the discriminator.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let a: Result[String, String] = Err(f\"boom\");\n\
                 let ra = a.map(|x| x);\n\
                 match ra { Ok(x) => { println(x); } Err(e) => { println(e); } }\n\
                 let b: Result[String, String] = Err(f\"bang\");\n\
                 match b.map(|x| x) { Ok(x) => { println(x); } Err(e) => { println(e); } }\n\
                 let c: Result[String, String] = Err(f\"crash\");\n\
                 match c.map(|x: String| x.to_uppercase()) { Ok(x) => { println(x); } Err(e) => { println(e); } }\n\
                 let d: Result[String, Vec[i64]] = Err(vec![7, 8]);\n\
                 match d.map(|x| x) { Ok(x) => { println(x); } Err(e) => { println(f\"{e[1]}\"); } }\n\
                 let ok: Result[String, String] = Ok(f\"fine\");\n\
                 match ok.map(|x| x) { Ok(x) => { println(x); } Err(e) => { println(e); } }\n\
                 let sc: Result[String, i64] = Err(7);\n\
                 match sc.map(|x| x) { Ok(x) => { println(x); } Err(e) => { println(f\"{e}\"); } }\n\
             }",
        ) {
            assert_eq!(out, "boom\nbang\ncrash\n8\nfine\n7\n");
        }
}

/// B-2026-08-08-24 — this test was filed `#[ignore]`d with a repro that DOES
/// NOT REPRODUCE, and it is kept (un-ignored) as the control that proves so.
///
/// The row claimed an annotated `|x: String| x + "!"` mapper printed an
/// empty string. Bisecting it found the opposite: over an OWNED payload —
/// `Some(f"hi")`, exactly as written below — the shape is correct, and was
/// correct even at `dcdad611`, before B-2026-08-08-22 landed. The row was
/// written from a simplified receiver that was never re-measured.
///
/// The real trigger is the RECEIVER, not the annotation alone and not the
/// concat: `Vec.first()/.last()/.get()` mint `Option[ref T]`, and an owned
/// annotation over that borrowed payload is what miscompiled. That shape now
/// gets a type error (see the typechecker test
/// `owned_closure_param_annotation_over_borrowed_payload_is_rejected`), and
/// its correct spellings are exercised below in
/// `test_e2e_option_map_over_borrowed_payload_correct_spellings`.
///
/// This case must stay green: an owned payload takes an owned annotation.
#[test]
fn test_e2e_option_map_owned_payload_annotated_concat_is_correct() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let s: Option[String] = Some(f\"hi\");\n\
                     match s.map(|x: String| x + \"!\") { Some(x) => { println(x); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi!\n")
    );
}

/// B-2026-08-08-24 — the spellings that are CORRECT for a BORROWED payload
/// must keep running correctly, across every payload class whose miscompile
/// looked different (empty String / zero length / leaked stack address).
///
/// Un-annotated and `ref T`-annotated are the two legal forms; the owned
/// annotation that used to sit silently beside them is now a type error.
#[test]
fn test_e2e_option_map_over_borrowed_payload_correct_spellings() {
    for (label, decl, body, want) in [
        (
            "String payload, inferred",
            "let out: Vec[String] = vec![f\"hi\", f\"yo\"];",
            "out.first().map(|x| x.to_uppercase())",
            "HI\n",
        ),
        (
            "String payload, explicit `ref String`",
            "let out: Vec[String] = vec![f\"hi\", f\"yo\"];",
            "out.first().map(|x: ref String| x.to_uppercase())",
            "HI\n",
        ),
        (
            "String concat over a borrow",
            "let out: Vec[String] = vec![f\"hi\", f\"yo\"];",
            "out.first().map(|x: ref String| x + \"!\")",
            "hi!\n",
        ),
        (
            "`.last()` borrow",
            "let out: Vec[String] = vec![f\"yo\", f\"hi\"];",
            "out.last().map(|x: ref String| x + \"!\")",
            "hi!\n",
        ),
    ] {
        let src = format!(
            "fn main() {{\n    {decl}\n    \
                 match {body} {{ Some(x) => {{ println(x); }} None => {{}} }}\n}}\n"
        );
        assert_eq!(
            run_program(&src).as_deref(),
            Some(want),
            "{label} (`{body}`) must run correctly over a borrowed payload"
        );
    }
}

/// B-2026-08-09-6, THE SCALAR-`T` HALF — the sibling of
/// `test_e2e_result_map_preserves_a_heap_err_payload` above, and the harder
/// failure. `619a438a` fixed the heap-`T` shape by recording `E` for `map`,
/// and correctly noted that the entry is INERT for a scalar `T`: that shape
/// never reaches `compile_map_via_match_synthesis`, because the lowering
/// branches on `is_trivially_copyable_te(&inner_te)` — on `T` alone.
///
/// So `Result[i64, String]` took the hand-rolled path, whose absent branch
/// is a shallow copy of the receiver's words. The result then ALIASED the
/// receiver's `Err` buffer and both freed it: `free(): double free detected
/// in tcache 2`, a hard abort on an ordinary program.
///
/// Fixed by making the lowering choice consider BOTH halves — `map` passes
/// `E` through, so a heap `E` is just as much a payload as a heap `T`, and
/// the synthesis already owns the ownership machinery the hand-rolled path
/// lacks. That routes the alias out of existence rather than teaching a
/// fourth consumer to cope with it.
///
/// Case 2 is the newly-routed PRESENT branch (scalar `T` + heap `E` reaches
/// the synthesis for the first time) and case 4 is the scalar-`E` control
/// that must NOT change lowering.
#[test]
fn test_e2e_result_map_passes_a_heap_err_payload_through() {
    // 1. Scalar `T`, heap `E` — was a double-free abort.
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let r: Result[i64, String] = Err(f\"boom\");\n\
                     match r.map(|x| x + 1i64) { Ok(v) => { println(v); } Err(e) => { println(e); } }\n\
                 }"
            )
            .as_deref(),
            Some("boom\n")
        );
    // 2. Same shape, PRESENT branch — newly routed through match synthesis.
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let r: Result[i64, String] = Ok(41i64);\n\
                     match r.map(|x| x + 1i64) { Ok(v) => { println(v); } Err(e) => { println(e); } }\n\
                 }"
            )
            .as_deref(),
            Some("42\n")
        );
    // 3. A heap `E` that is a Vec, not a String.
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let r: Result[i64, Vec[i64]] = Err(vec![1i64, 2i64]);\n\
                     match r.map(|x| x + 1i64) { Ok(v) => { println(v); } Err(e) => { println(e.len()); } }\n\
                 }"
            )
            .as_deref(),
            Some("2\n")
        );
    // 4. Scalar `E` control — must keep the hand-rolled lowering.
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let r: Result[String, i64] = Err(7i64);\n\
                     match r.map(|x| x.to_uppercase()) { Ok(v) => { println(v); } Err(e) => { println(e); } }\n\
                 }"
            )
            .as_deref(),
            Some("7\n")
        );
    // 5. A mapper that CHANGES `T` while `E` rides through untouched.
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let r: Result[i64, String] = Err(f\"boom\");\n\
                     match r.map(|x| f\"n={x}\") { Ok(v) => { println(v); } Err(e) => { println(e); } }\n\
                 }"
            )
            .as_deref(),
            Some("boom\n")
        );
    // 6. Chained into `unwrap_or` — the span-key collision guard still holds
    //    now that BOTH combinators register an `E` for the same receiver.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let r: Result[i64, String] = Err(f\"boom\");\n\
                     println(r.map(|x| x + 1i64).unwrap_or(9i64));\n\
                 }"
        )
        .as_deref(),
        Some("9\n")
    );
    // 7. A fn-reference mapper rather than a closure literal.
    assert_eq!(
        run_program(
            "fn bump(x: i64) -> i64 { x + 1i64 }\n\
                 fn main() {\n\
                     let r: Result[i64, String] = Err(f\"boom\");\n\
                     match r.map(bump) { Ok(v) => { println(v); } Err(e) => { println(e); } }\n\
                 }"
        )
        .as_deref(),
        Some("boom\n")
    );
}

#[test]
fn e2e_map_try_insert_fallible_codegen() {
    // phase-8-stdlib-floor item 8 (B-2026-07-09-15): `Map.try_insert(k, v)`
    // lowers to `karac_map_try_insert` and returns `Result[Option[V],
    // AllocError]` — `Ok(None)` on a fresh insert, `Ok(Some(old))` on an
    // overwrite, `Err` on OOM. The host never OOMs, so verify the fresh vs.
    // overwrite discrimination + that growth across many inserts stays
    // correct (drives `try_resize`). Byte-identical to the `karac run`
    // interpreter oracle.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut m: Map[i64, i64] = Map.new();\n\
                 match m.try_insert(1_i64, 10_i64) {\n\
                     Ok(o) => match o { None => println(\"fresh\"), Some(v) => println(v) },\n\
                     Err(_) => println(\"oom\"),\n\
                 }\n\
                 match m.try_insert(1_i64, 20_i64) {\n\
                     Ok(o) => match o { None => println(\"fresh\"), Some(v) => println(v) },\n\
                     Err(_) => println(\"oom\"),\n\
                 }\n\
                 let mut i = 0_i64;\n\
                 while i < 100_i64 { let _ = m.try_insert(i, i * 2_i64); i = i + 1_i64; }\n\
                 println(m.len());\n\
                 match m.try_insert(50_i64, 999_i64) {\n\
                     Ok(o) => match o { None => println(\"fresh\"), Some(v) => println(v) },\n\
                     Err(_) => println(\"oom\"),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "fresh\n10\n100\n100\n");
    }
}

#[test]
fn e2e_map_try_insert_heap_value_codegen() {
    // `Map[i64, String].try_insert` — the `Ok` payload is `Option[String]`,
    // a heap value that must pack into the `Result` payload and round-trip
    // exactly like the panicking `Map.insert`'s `Option[String]`. Exercises
    // the multi-word `Option` reconstruction on the fallible path.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut m: Map[i64, String] = Map.new();\n\
                 match m.try_insert(1_i64, \"hello\") {\n\
                     Ok(o) => match o { None => println(\"fresh\"), Some(v) => println(v) },\n\
                     Err(_) => println(\"oom\"),\n\
                 }\n\
                 match m.try_insert(1_i64, \"world\") {\n\
                     Ok(o) => match o { None => println(\"fresh\"), Some(v) => println(v) },\n\
                     Err(_) => println(\"oom\"),\n\
                 }\n\
                 match m.get(1_i64) { Some(v) => println(v), None => println(\"none\") }\n\
             }",
    ) {
        assert_eq!(out, "fresh\nhello\nworld\n");
    }
}

#[test]
fn e2e_set_try_insert_fallible_codegen() {
    // `Set.try_insert(k) -> Result[bool, AllocError]`: `Ok(true)` newly
    // inserted, `Ok(false)` duplicate. Set is `Map[T, ()]`, routed through
    // the same `karac_map_try_insert`.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut s: Set[i64] = Set.new();\n\
                 match s.try_insert(7_i64) { Ok(b) => println(b), Err(_) => println(\"oom\") }\n\
                 match s.try_insert(7_i64) { Ok(b) => println(b), Err(_) => println(\"oom\") }\n\
                 let mut i = 0_i64;\n\
                 while i < 50_i64 { let _ = s.try_insert(i); i = i + 1_i64; }\n\
                 println(s.len());\n\
             }",
    ) {
        assert_eq!(out, "true\nfalse\n50\n");
    }
}

/// B-2026-09-04-16 — the sortedness marker is keyed by BARE BINDING NAME
/// and survived a function boundary, so an unrelated `Map`/`Set` that
/// happened to reuse a `SortedMap`/`SortedSet` binding's name in a LATER
/// function iterated in sorted order and rendered with the sorted prefix.
/// The twin need only be PRESENT — `twin()` below is never called — and
/// renaming either binding made the program correct.
///
/// Every line here is WRONG before the fix except the last two:
///   01 DIVERGED / SortedMap{7: 7} / 03 SortedMap{7: 7} / SortedSet{5}
/// which covers `keys()`, `println(m)` (the `compile_print` path) and
/// `f"{m}"` (the `synth_display` path — a different consult of the same
/// marker), and the `Set` half alongside the `Map` half.
///
/// Line 01 is deliberately ORDER-FREE: it fills two plain `Map`s with the
/// SAME keys in the SAME sequence and asserts their `keys()` AGREE, never
/// that either equals a particular permutation. Hash order is per-process
/// random (see CLAUDE.md), so an assertion naming a sequence would fail on
/// most runs; two maps in ONE process share the key and the growth path,
/// so their orders match whenever neither is being force-sorted. The
/// `.keys()` call has to sit INSIDE the colliding function — routing it
/// through a shared `fn ks(m: ref Map[..])` helper makes the receiver `m`
/// at the call site and the test then passes even unfixed (measured).
///
/// The last two lines are the don't-overshoot guard: a real `SortedMap`
/// must still sort and still say `SortedMap{`.
///
/// Twin: `sorted_marker_does_not_leak_across_functions_interp`.
#[test]
fn e2e_sorted_marker_does_not_leak_across_functions() {
    if let Some(out) = run_program(SORTED_MARKER_LEAK_SRC) {
        assert_eq!(out, SORTED_MARKER_LEAK_EXPECTED);
    }
}

/// B-2026-08-15-1 — an UNANNOTATED binding of a `SortedMap` / `SortedSet`.
///
/// The row above fixed how these RENDER; this is the spelling it could not
/// reach, because the binding was never registered as a collection at all.
/// `let m = mkm()` with no annotation landed in `self.variables` with a
/// slot and in NONE of the side-tables, so Display fell through to the
/// value-kind arms and printed the raw control pointer (a different number
/// every run) while `m.len()` was a hard codegen error. Two independent
/// gaps, and the first one alone produces a WORSE result than the bug:
///
///  * the `let` fallback's head-gate listed `Map` / `Set` but not their
///    sorted variants, so nothing was registered. Widening it repairs
///    `len()` and stops the pointer-print —
///  * — and then renders `{zz: 1, aa: 2}`: plain-`Map` prefix, INSERTION
///    order. `register_var_from_type_expr` writes the Map/Set tables for
///    both heads but never wrote `sorted_collection_vars`, the separate
///    ordering marker the annotated `let` path sets inline. So every
///    binding registered through the registrar — a place source, a struct
///    field, a synthetic element, a closure param — silently lost its
///    sortedness.
///
/// Lines 03 and 06 are what pin the second half: they iterate, so a
/// binding that registered but lost its marker prints `zz,aa,mm,` here and
/// is correct on every other line.
#[test]
fn test_e2e_unannotated_sorted_collection_binding_registers() {
    let src = r#"
struct Holder { sm: SortedMap[String, i64], ss: SortedSet[String] }

fn mkm() -> SortedMap[String, i64] {
    let mut m: SortedMap[String, i64] = SortedMap.new();
    let _ = m.insert("zz", 1);
    let _ = m.insert("aa", 2);
    let _ = m.insert("mm", 3);
    return m;
}
fn mks() -> SortedSet[String] {
    let mut s: SortedSet[String] = SortedSet.new();
    let _ = s.insert("zz");
    let _ = s.insert("aa");
    let _ = s.insert("mm");
    return s;
}

fn main() {
    let m1: SortedMap[String, i64] = mkm();
    println(f"01 {m1} {m1.len()}");

    let m2 = mkm();
    println(f"02 {m2} {m2.len()}");

    let mut acc = "";
    for k in m2.keys() { acc = acc + k + ","; }
    println(f"03 {acc}");

    let hit = m2.contains_key("mm");
    println(f"04 {hit} {m2.is_empty()}");

    let s2 = mks();
    let shit = s2.contains("aa");
    println(f"05 {s2} {s2.len()} {shit}");
    let mut sacc = "";
    for v in s2.iter() { sacc = sacc + v + ","; }
    println(f"06 {sacc}");

    let h = Holder { sm: mkm(), ss: mks() };
    let bm = h.sm;
    println(f"07 {bm} {bm.len()}");
    let bs = h.ss;
    println(f"08 {bs} {bs.len()}");

    let mut pm: Map[String, i64] = Map.new();
    let _ = pm.insert("zz", 1);
    println(f"09 {pm} {pm.len()}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            "01 SortedMap{aa: 2, mm: 3, zz: 1} 3\n\
                 02 SortedMap{aa: 2, mm: 3, zz: 1} 3\n\
                 03 aa,mm,zz,\n\
                 04 true false\n\
                 05 SortedSet{aa, mm, zz} 3 true\n\
                 06 aa,mm,zz,\n\
                 07 SortedMap{aa: 2, mm: 3, zz: 1} 3\n\
                 08 SortedSet{aa, mm, zz} 3\n\
                 09 {zz: 1} 1\n"
        ),
    );
}

#[test]
fn e2e_sorted_map_ordered_methods_codegen() {
    // B-2026-07-18-1: the ordered-only `SortedMap` methods — `min`/`max`/
    // `floor`/`ceiling` (-> Option[(K,V)]) and `range` (inclusive [lo,hi] ->
    // Vec[(K,V)]) — previously ran only under the interpreter ("codegen:
    // Map.min not yet implemented"). Now byte-identical to `karac run`.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let mut m: SortedMap[i64, i64] = SortedMap.new();\n\
                 let _ = m.insert(5_i64, 50_i64); let _ = m.insert(1_i64, 10_i64);\n\
                 let _ = m.insert(3_i64, 30_i64);\n\
                 match m.min() { Some(kv) => println(f\"{kv.0}={kv.1}\"), None => println(\"n\") }\n\
                 match m.max() { Some(kv) => println(f\"{kv.0}={kv.1}\"), None => println(\"n\") }\n\
                 match m.floor(4_i64) { Some(kv) => println(f\"{kv.0}\"), None => println(\"n\") }\n\
                 match m.ceiling(2_i64) { Some(kv) => println(f\"{kv.0}\"), None => println(\"n\") }\n\
                 match m.floor(0_i64) { Some(kv) => println(f\"{kv.0}\"), None => println(\"n\") }\n\
                 match m.ceiling(9_i64) { Some(kv) => println(f\"{kv.0}\"), None => println(\"n\") }\n\
                 let r = m.range(2_i64, 5_i64); println(f\"{r.len()}\");\n\
                 let mut e: SortedMap[i64, i64] = SortedMap.new();\n\
                 match e.min() { Some(kv) => println(f\"{kv.0}\"), None => println(\"empty\") }\n\
             }",
        ) {
            assert_eq!(out, "1=10\n5=50\n3\n3\nn\nn\n2\nempty\n");
        }
}

#[test]
fn e2e_map_try_insert_question_propagation_codegen() {
    // `Map.try_insert` composes with `?`: a helper returning
    // `Result[(), AllocError]` propagates any `Err` and discards the
    // `Option[V]` old value on success.
    if let Some(out) = run_program(
        "fn fill(m: mut ref Map[i64, i64]) -> Result[(), AllocError] {\n\
                 m.try_insert(1_i64, 100_i64)?;\n\
                 m.try_insert(2_i64, 200_i64)?;\n\
                 Ok(())\n\
             }\n\
             fn main() {\n\
                 let mut m: Map[i64, i64] = Map.new();\n\
                 match fill(mut m) {\n\
                     Ok(_) => println(m.len()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "2\n");
    }
}

#[test]
fn e2e_iter_adaptor_map_filter_collect_to_vec_codegen() {
    // B-2026-07-03-25: `<iter>.map(f)/.filter(p)....collect()` into a `Vec`
    // failed codegen ("no handler for method 'collect' on non-identifier
    // receiver") — codegen handled `collect` only on an identifier receiver
    // and on `chars().collect()`, so a lazy `map`/`filter` adaptor chain
    // (the book-documented idiom, ch10-closures-and-iterators) fell through.
    // The fix desugars the chain to a `for` loop that pushes each
    // surviving/transformed element onto a fresh Vec. Exercises: a single
    // `map` (the headline idiom), a `filter`, a `filter().map()` chain, a
    // type-changing `map` producing a heap `Vec[String]`, and a `map` over a
    // heap `Vec[String]` source that calls a method on the element (`.len()`)
    // — the case that requires the base-most param to inherit the loop var's
    // element type.
    if let Some(out) = run_program(
        r#"
fn main() {
    let s: Vec[i64] = Vec[1i64, 2i64, 3i64, 4i64, 5i64];
    let doubled: Vec[i64] = s.iter().map(|n| n * 2i64).collect();
    println(f"{doubled[0]} {doubled[4]}");
    let big: Vec[i64] = s.iter().filter(|x| x > 2i64).collect();
    println(f"{big.len()} {big[0]}");
    let chain: Vec[i64] = s.iter().filter(|x| x > 2i64).map(|y| y * 10i64).collect();
    println(f"{chain.len()} {chain[0]} {chain[2]}");
    let strs: Vec[String] = s.iter().map(|n| n.to_string()).collect();
    println(f"{strs[0]}{strs[4]}");
    let words: Vec[String] = Vec["apple".to_string(), "berry".to_string(), "fig".to_string()];
    let lens: Vec[i64] = words.iter().map(|w| w.len()).collect();
    println(f"{lens[0]} {lens[1]} {lens[2]}");
}
"#,
    ) {
        assert_eq!(out, "2 10\n3 3\n3 30 50\n15\n5 5 3\n");
    }
}

#[test]
fn e2e_iter_adaptor_named_fn_map_filter_collect_codegen() {
    // B-2026-07-04-2 sub-part 2: a NAMED-FUNCTION argument to
    // `map`/`filter`/... in a `collect()` chain (`<src>.iter().map(double)`)
    // fell through to the loud dispatch-fail under `karac build` (the
    // pipeline accepted only single-`Binding` closures), though `karac run`
    // handled it. The fix wraps the named fn in a synthetic body `<fn>(p)`,
    // so it lowers exactly like `.map(|x| double(x))`. Exercises a POD `map`,
    // a `filter`, a `filter().map()` chain, a type-changing `map` to a heap
    // `Vec[String]`, and a named fn CONSUMING a heap element (`.map(nlen)`
    // over `Vec[String]`) — the source survives (`.iter()` borrows, asserted
    // via `words.len()`). Distinct synthetic param names per stage (`nf_two`)
    // avoid collisions. A multi-param / destructuring closure still bails.
    if let Some(out) = run_program(
        r#"
fn double(n: i64) -> i64 { n * 2i64 }
fn big(n: i64) -> bool { n > 2i64 }
fn label(n: i64) -> String { n.to_string() }
fn nlen(s: String) -> i64 { s.len() }
fn main() {
    let v: Vec[i64] = Vec[1i64, 2i64, 3i64, 4i64];
    let a: Vec[i64] = v.iter().map(double).collect();
    println(f"{a.len()} {a[0]} {a[3]}");
    let b: Vec[i64] = v.iter().filter(big).collect();
    println(f"{b.len()} {b[0]} {b[1]}");
    let c: Vec[i64] = v.iter().filter(big).map(double).collect();
    println(f"{c.len()} {c[0]} {c[1]}");
    let d: Vec[String] = v.iter().map(label).collect();
    println(f"{d.len()} {d[0]}{d[3]}");
    let words: Vec[String] = Vec["alpha".to_string(), "be".to_string(), "gamma".to_string()];
    let e: Vec[i64] = words.iter().map(nlen).collect();
    println(f"{e.len()} {e[0]} {e[1]} {e[2]} {words.len()}");
}
"#,
    ) {
        assert_eq!(out, "4 2 8\n2 3 4\n2 6 8\n4 14\n3 5 2 5 3\n");
    }
}

/// B-2026-07-04-2 sub-part 1 (flat_map adaptor-carrying outer): an outer that
/// carries its own adaptor (`a.iter().map(f).flat_map(|p| p.iter())`,
/// `a.iter().filter(g).flat_map(|p| p.iter())`) pre-collects the outer to a
/// Vec[Vec[E]] temp and reuses the identity flat_map — it used to bail (only
/// an identity outer lowered). Gated to an inner that iterates the param as a
/// container (so the outer element type is derivable). ASAN twin:
/// asan_b04_2_flat_map_adaptor_outer_heap_no_leak.
#[test]
fn e2e_iter_adaptor_flat_map_adaptor_outer_codegen() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let a: Vec[Vec[i64]] = Vec[Vec[1i64, 2i64], Vec[3i64], Vec[4i64, 5i64]];
    let r: Vec[i64] = a.iter().filter(|v| v.len() > 0i64).flat_map(|p| p.iter()).collect();
    println(f"{r.len()} {r[0i64]} {r[2i64]} {r[4i64]}");
}
"#,
    ) {
        assert_eq!(out, "5 1 3 5\n");
    }
}

#[test]
fn e2e_iter_adaptor_flat_map_collect_codegen() {
    // B-2026-07-04-2 sub-part 1 (flat_map): `<outer>.flat_map(|v|
    // v.iter()).collect()` fell through to the loud dispatch-fail under
    // `karac build`, though `karac run` handled it. The fix lowers it to
    // nested loops `for v in outer { for x in v.iter() { acc.push(x) } }` —
    // iteration-based (not indexed), so `push` clones and the nested source
    // SURVIVES (asserted via `xs.len()`). Heap-safe (`Vec[Vec[String]]`);
    // leak-checked by `asan_b04_2_flat_map_heap_collect_no_leak`. A complex
    // inner (`|v| v.iter().map(…)`) or a downstream adaptor after flat_map
    // still bails.
    if let Some(out) = run_program(
        r#"
fn main() {
    let xs: Vec[Vec[i64]] = Vec[Vec[1i64, 2i64], Vec[3i64], Vec[4i64, 5i64]];
    let r: Vec[i64] = xs.iter().flat_map(|v| v.iter()).collect();
    println(f"{r.len()} {r[0]} {r[4]} {xs.len()}");
    let ys: Vec[Vec[String]] = Vec[
        Vec["aa".to_string(), "bb".to_string()],
        Vec["cc".to_string()]
    ];
    let s: Vec[String] = ys.iter().flat_map(|v| v.iter()).collect();
    println(f"{s.len()} {s[0]}{s[2]} {ys.len()}");
}
"#,
    ) {
        assert_eq!(out, "5 1 5 3\n3 aacc 2\n");
    }
}

#[test]
fn e2e_entry_chain_on_a_map_field() {
    // Codegen twin of the B-2026-08-18-34 oracle in tests/interpreter.rs.
    // The row was a check/run divergence -- `karac check` accepted
    // `h.buckets.entry(k).or_insert(d).push(v)` while BOTH executors refused
    // to dispatch it ("no handler for method 'push' on non-identifier
    // receiver") -- so the test that matters is that a real binary now
    // prints what the interpreter prints, for the same program.
    //
    // The `self.` leg is LeetCode 895, the kata that found it.
    if let Some(out) = run_program(
        "struct Holder { buckets: Map[i64, Vec[i64]], counts: Map[i64, i64] }\n\
             struct FreqStack { freq: Map[i64, i64], buckets: Map[i64, Vec[i64]], maxfreq: i64 }\n\
             impl FreqStack {\n\
                 fn push_val(mut ref self, v: i64) {\n\
                     let f = self.freq.get_or(v, 0) + 1;\n\
                     self.freq.insert(v, f);\n\
                     if f > self.maxfreq { self.maxfreq = f; }\n\
                     self.buckets.entry(f).or_insert(Vec.new()).push(v);\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut local: Map[i64, Vec[i64]] = Map.new();\n\
                 local.entry(1).or_insert(Vec.new()).push(7);\n\
                 local.entry(1).or_insert(Vec.new()).push(8);\n\
                 println(local[1].len());\n\
                 let mut h = Holder { buckets: Map.new(), counts: Map.new() };\n\
                 h.buckets.entry(1).or_insert(Vec.new()).push(7);\n\
                 h.buckets.entry(1).or_insert(Vec.new()).push(8);\n\
                 println(h.buckets[1].len());\n\
                 h.counts.entry(2).and_modify(|v| { *v = *v + 100; }).or_insert(7);\n\
                 println(h.counts[2]);\n\
                 h.counts.entry(2).and_modify(|v| { *v = *v + 100; }).or_insert(7);\n\
                 println(h.counts[2]);\n\
                 h.buckets.entry(3).or_insert_with(|| Vec.new()).push(42);\n\
                 println(h.buckets[3].len());\n\
                 let mut s = FreqStack { freq: Map.new(), buckets: Map.new(), maxfreq: 0 };\n\
                 s.push_val(5); s.push_val(7); s.push_val(5);\n\
                 println(s.buckets[1].len());\n\
                 println(s.buckets[2].len());\n\
             }",
    ) {
        assert_eq!(out, "2\n2\n7\n107\n1\n2\n1\n");
    }
}

#[test]
fn e2e_indexed_read_from_a_map_field() {
    // B-2026-08-18-36, the read half of B-2026-08-18-34's write. That row
    // made `h.buckets.entry(k).or_insert(Vec.new()).push(v)` work on a
    // struct FIELD; reading an element straight back out --
    // `h.buckets[k][i]`, the next line anyone writes -- still failed the
    // build while `--interp` returned the right element.
    //
    // The nested-index lowering resolved a field's element type through
    // `vec_inner_type_expr`, which takes the FIRST generic arg. For a map
    // the element is the SECOND (the VALUE type), so the head has to be
    // matched before the element type is resolved rather than after.
    //
    // The Vec-of-Vec leg is the regression control for that reordering.
    if let Some(out) = run_program(
        "struct Holder { buckets: Map[i64, Vec[i64]], byname: SortedMap[String, Vec[i64]] }\n\
             fn main() {\n\
                 let mut h = Holder { buckets: Map.new(), byname: SortedMap.new() };\n\
                 h.buckets.entry(1).or_insert(Vec.new()).push(43);\n\
                 h.buckets.entry(1).or_insert(Vec.new()).push(7);\n\
                 println(h.buckets[1][0]);\n\
                 println(h.buckets[1][1]);\n\
                 h.byname.entry(\"k\").or_insert(Vec.new()).push(6);\n\
                 println(h.byname[\"k\"][0]);\n\
                 let vv: Vec[Vec[i64]] = [[1, 2], [3, 4]];\n\
                 println(vv[1][0]);\n\
             }",
    ) {
        assert_eq!(out, "43\n7\n6\n3\n");
    }
}

#[test]
fn test_e2e_offset_of_first_field_is_0() {
    let out = run_program(
        "struct Point { x: i64, y: i64 }\n\
             fn main() { println(offset_of[Point](x)); }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0");
    }
}

#[test]
fn test_e2e_offset_of_second_field() {
    // `y` follows `x: i64` → offset 8.
    let out = run_program(
        "struct Point { x: i64, y: i64 }\n\
             fn main() { println(offset_of[Point](y)); }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "8");
    }
}

#[test]
fn test_e2e_offset_of_nested_path() {
    // `offset_of[Outer](inner.y)` = offset(inner inside Outer) + offset(y inside Inner).
    let out = run_program(
        "struct Inner { x: i32, y: i32 }\n\
             struct Outer { a: i32, inner: Inner, c: i32 }\n\
             fn main() { println(offset_of[Outer](inner.y)); }",
    );
    if let Some(out) = out {
        // a: i32 occupies bytes 0-3; inner: Inner starts at 4 (i32-aligned);
        // y is the second i32 field of Inner → +4 inside Inner → byte 8.
        assert_eq!(out.trim(), "8");
    }
}

#[test]
fn test_e2e_option_map_and_noncall_rhs_drop_paths() {
    // B-2026-06-10-6 Option[Map] + non-Call-RHS follow-ons. Pins OUTPUT
    // correctness: an `Option[Map]` built + read back through a match,
    // and a non-`Call` `if`-RHS yielding a fresh inline Option that's
    // read back. The memory side (no leak/double-free) is pinned by
    // `memory_sanitizer.rs::{asan_option_map_undestructured_freed,
    // asan_option_if_else_fresh_payload_freed}`.
    let out = run_program(
        r#"
fn mk_map() -> Option[Map[i64, i64]] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1i64, 42i64);
    Some(m)
}
fn main() {
    let om = mk_map();
    match om {
        Some(m) => { match m.get(1i64) { Some(x) => println(x), None => println(-1i64) } }
        None => { println(-2i64); }
    };
    let c = true;
    let mut a = String.new();
    a.push_str("fresh-if");
    let x = if c { Some(a) } else { None };
    match x { Some(s) => println(s), None => println("none") }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["42", "fresh-if"]);
    }
}

#[test]
fn test_e2e_bug7_shared_struct_nested_map_in_vec_return() {
    // Combined ownership path: the helper inserts the same `n` into
    // BOTH a Map and a Vec before returning it.  Each move-out site
    // emits an independent `rc_inc`, and the source's single
    // scope-exit `rc_dec` keeps the refcount above zero across all
    // three consumers (Map bucket, Vec buffer, caller's `r`).
    let out = run_program(
        r#"
shared struct Node { val: i64 }
fn helper() -> Node {
    let mut m: Map[i64, Node] = Map.new();
    let mut v: Vec[Node] = Vec.new();
    let n = Node { val: 42 };
    let _ = m.insert(1_i64, n);
    v.push(n);
    n
}
fn main() {
    let r = helper();
    println(r.val);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_ir_map_shared_value_drop_emits_per_bucket_rc_dec_walk() {
    // Regression for the 2026-05-16 leak: `Map[K, shared T]` values
    // were never rc_dec'd on map drop. The runtime helper
    // `karac_map_free_with_drop_vec` only handles Vec/String-shaped
    // values; shared-struct / shared-enum value types fell through
    // to plain `karac_map_free`, stranding the refcount and
    // leaking each live node's heap object. The fix is codegen-
    // time specialization at the `FreeMapHandle` cleanup site:
    // emit a per-bucket walk that calls `emit_rc_dec` on the
    // value-half pointer when V is shared.
    //
    // IR-level gates:
    //   1. The shared-val walk's distinctive block label is
    //      `cleanup.map.shared.walk.entry` — its presence proves
    //      the cleanup wired up the shared-val arm rather than
    //      falling through to plain `karac_map_free`.
    //   2. The bucket-iteration `loop.body` label proves the
    //      walk's loop structure was emitted (not just the null-
    //      guard skeleton).
    //   3. At least one `sub i64 %rc, 1` inside `main` proves
    //      `emit_rc_dec` ran on the value pointer. The check is
    //      a *minimum*-count gate so the test stays stable
    //      against future inlining / loop-unrolling.
    let ir = ir_for(
        r#"
shared struct Node { val: i64 }
fn main() {
    let mut m: Map[i64, Node] = Map.new();
    let _ = m.insert(1, Node { val: 42 });
    let _ = m.insert(2, Node { val: 7 });
    let _ = m.insert(3, Node { val: 9 });
}
"#,
    );
    assert!(
        ir.contains("cleanup.map.shared.walk.entry"),
        "Map[K, shared T] cleanup should emit shared-val rc_dec walk \
             (missing `cleanup.map.shared.walk.entry` block label)"
    );
    assert!(
        ir.contains("cleanup.map.shared.loop.body"),
        "Map[K, shared T] cleanup walk should include a per-bucket loop body \
             (missing `cleanup.map.shared.loop.body` block label)"
    );
    let dec_count = ir.matches("sub i64 %rc, 1").count();
    assert!(
        dec_count >= 1,
        "Map[K, shared T] cleanup should rc_dec each live value \
             (found {dec_count} `sub i64 %rc, 1` ops; expected ≥ 1)"
    );
}

#[test]
fn test_e2e_map_shared_value_drops_cleanly() {
    // End-to-end pairing for the IR test above: a program that
    // inserts shared-struct values into a Map and lets the map
    // go out of scope must not crash, must not leak (verified
    // by ASAN in the `memory_sanitizer` test file), and must
    // produce the expected stdout.
    let out = run_program(
        r#"
shared struct Node { val: i64 }
fn main() {
    let mut m: Map[i64, Node] = Map.new();
    let _ = m.insert(1, Node { val: 42 });
    let _ = m.insert(2, Node { val: 7 });
    println(m.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2");
    }
}

#[test]
fn test_ir_map_insert_overwrite_shared_discard_rc_dec() {
    // When `let _ = m.insert(k, v)` overwrites an existing key with
    // a shared-V map, the displaced bucket value's +1 transfers to
    // the synthesized `Some(old)` payload that the discard drops
    // on the floor. Without the fix the prior pointer's refcount
    // stays >0 forever — leaks one ref per overwrite.
    //
    // The fix emits an extra `sub i64 %rc` inside `map.ins.some`
    // when V is shared and the result is discarded. This test
    // gates the IR-level rc_dec presence by counting the `sub i64
    // %rc` ops emitted for the program — a Map.new + first
    // insert (no displacement) + second insert on same key
    // (displaces, fires the dec) + scope-exit Map drop walk
    // (dec on the still-live value).
    let ir = ir_for(
        r#"
shared struct S { val: i64 }
fn main() {
    let mut m: Map[i64, S] = Map.new();
    let _ = m.insert(1, S { val: 10 });
    let _ = m.insert(1, S { val: 20 });
}
"#,
    );
    // Expected `sub i64 %rc` ops in the module:
    //   - 1 dec inside `map.ins.some` from the second insert
    //     (the overwrite-leak fix)
    //   - 1 dec from the scope-exit Map-drop walk on the still-
    //     live final value
    // = 2 total. Without the fix, only the scope-exit dec fires,
    // leaving 1.
    let dec_count = ir.matches("sub i64 %rc").count();
    assert!(
        dec_count >= 2,
        "expected at least 2 `sub i64 %rc` ops (overwrite dec + \
             scope-exit dec); found {} in:\n{}",
        dec_count,
        ir
    );
}

#[test]
fn test_e2e_map_insert_overwrite_shared_no_leak() {
    // E2E: overwrite a shared-value Map entry repeatedly, then
    // drop the map. Without the fix, every overwrite leaks one
    // ref; the program's stdout was always correct (the freed
    // pointer's bytes still hold valid data), so this test
    // primarily pins program correctness — paired with the
    // IR-level gate above which catches a regression that
    // removes the dec.
    let out = run_program(
        r#"
shared struct S { val: i64 }
fn main() {
    let mut m: Map[i64, S] = Map.new();
    let _ = m.insert(1, S { val: 10 });
    let _ = m.insert(1, S { val: 20 });
    let _ = m.insert(1, S { val: 30 });
    println(m.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1");
    }
}

#[test]
fn test_ir_map_shared_key_drop_emits_per_bucket_rc_dec_walk() {
    // `Map[shared K, V]` scope-exit cleanup must emit a key-side
    // bucket walk parallel to the value-side walk from 9d878ae.
    // Without it, every key handle bit-copied into a bucket
    // strands its refcount when the Map drops.
    //
    // IR gate: the cleanup site emits the canonical "load slot
    // key pointer + rc_dec" sequence inside the bucket walk
    // loop. We pin both: the `cleanup.map.shared.key.ptr` load
    // (only emitted on the key path) and the presence of at
    // least one `sub i64 %rc` op (which the walk fires per
    // occupied bucket at runtime).
    let ir = ir_for(
        r#"
#[derive(Hash, Eq)]
shared struct K { id: i64 }
fn main() {
    let mut m: Map[K, i64] = Map.new();
    let k = K { id: 1 };
    let _ = m.insert(k, 42);
}
"#,
    );
    assert!(
        ir.contains("cleanup.map.shared.key.ptr"),
        "expected key-side walk label `cleanup.map.shared.key.ptr` \
             in IR (gates the new `is_val == false` half-walk path); \
             not found in:\n{}",
        ir
    );
    let dec_count = ir.matches("sub i64 %rc").count();
    assert!(
        dec_count >= 1,
        "expected at least one `sub i64 %rc` op for the key-side \
             walk; found {} in:\n{}",
        dec_count,
        ir
    );
}

#[test]
fn test_e2e_map_shared_key_drops_cleanly() {
    // E2E: build a Map[shared K, i64], insert several entries,
    // let it drop. Pre-fix the program would leak K's refcounts
    // (stdout still correct — the leaked memory's bytes are
    // valid — but the heap would balloon). The test pins the
    // program runs cleanly and prints the expected len.
    let out = run_program(
        r#"
#[derive(Hash, Eq)]
shared struct K { id: i64 }
fn main() {
    let mut m: Map[K, i64] = Map.new();
    let _ = m.insert(K { id: 1 }, 10);
    let _ = m.insert(K { id: 2 }, 20);
    let _ = m.insert(K { id: 3 }, 30);
    println(m.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn test_ir_struct_field_map_shared_value_drop_walk() {
    // `struct Owner { m: Map[i64, Node] }` where `Node` is a
    // shared struct. The synthesized `__karac_drop_struct_Owner`
    // walks the `m` field and routes to `karac_map_free_with_drop_vec`
    // — but without the value-side shared rc_dec walk emitted
    // beforehand, every live `Node` in the bucket array strands
    // its refcount when the owner drops. Item 4 fix: the struct-
    // drop synthesis now mirrors the `CleanupAction::FreeMapHandle`
    // ordering and emits `emit_map_shared_half_rc_dec_walk`
    // against the field's K/V halves before the runtime free.
    //
    // IR gate: the synthesized struct-drop fn contains the
    // canonical `cleanup.map.shared.val.ptr` label that marks
    // the val-side walk.
    let ir = ir_for(
        r#"
shared struct Node { val: i64 }
struct Owner { m: Map[i64, Node] }
fn main() {
    let o = Owner { m: Map.new() };
}
"#,
    );
    assert!(
        ir.contains("__karac_drop_struct_Owner"),
        "expected synthesized struct-drop fn `__karac_drop_struct_Owner` \
             in IR; not found in:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.map.shared.val.ptr"),
        "expected val-side walk label `cleanup.map.shared.val.ptr` \
             inside the struct-drop fn (gates the new shared-V walk \
             emitted before `karac_map_free_with_drop_vec`); not \
             found in:\n{}",
        ir
    );
}

#[test]
fn test_ir_plain_enum_map_payload_drop_walk() {
    // B-2026-07-23-11: a plain (non-shared) user enum whose variant payload
    // is a `Map`/`Set`(-family) collection previously classified `None` in
    // `enum_drop_kind_for_type_expr`, so `__karac_drop_<E>` emitted no free
    // for the handle word and the whole kv-table leaked at scope-exit drop
    // (contrast `Vec`/`String` payloads, which freed). The new `MapOrSet`
    // drop kind loads the handle at the payload word and routes it through
    // `karac_map_free_with_drop_vec` (the same runtime entrypoint the
    // tuple/struct Map drop uses). IR gate: the synthesized enum-drop fn
    // exists and calls the map-free runtime fn under the `drop.map.handle`
    // label.
    let ir = ir_for(
        r#"
enum V { Table(Map[String, i64]) }
fn main() {
    let mut mp: Map[String, i64] = Map.new();
    let _ = mp.insert("a", 1);
    let _t = V.Table(mp);
    println(1);
}
"#,
    );
    assert!(
        ir.contains("__karac_drop_V"),
        "expected synthesized enum-drop fn `__karac_drop_V` in IR; \
             not found in:\n{}",
        ir
    );
    assert!(
        ir.contains("drop.map.handle"),
        "expected the `MapOrSet` drop arm's `drop.map.handle` label in the \
             enum-drop fn; not found in:\n{}",
        ir
    );
    assert!(
        ir.contains("karac_map_free_with_drop_vec"),
        "expected the enum-drop fn to route the Map payload through \
             `karac_map_free_with_drop_vec`; not found in:\n{}",
        ir
    );
}

#[test]
fn e2e_return_map_moved_out_of_enum_payload() {
    // B-2026-07-23-14: returning a `Map`/`Set` value moved OUT of an enum
    // payload (`fn unwrap(v: V) -> Map[K,V] { match v { Table(m) => m } }`)
    // failed codegen module verification with `ret i64 %matchval` for a
    // `ptr`-typed return — the Map handle extracted from the enum's i64
    // payload word was bound as a raw i64 alloca and never `inttoptr`'d, so a
    // bare return tripped the verifier (method dispatch tolerated the i64).
    // Fix: the pattern-binding path now int-to-ptrs a `Map`/`Set`(-family)
    // payload word so the binding slot is pointer-typed, mirroring the
    // shared/File/DataFrame arms. Assert build succeeds AND ownership
    // transfers correctly (10 allocs = 10 frees is checked in the ASAN
    // sibling; here we pin the value).
    if let Some(out) = run_program(
        "enum V { Table(Map[String, i64]) }\n\
             fn unwrap(v: V) -> Map[String, i64] { match v { Table(m) => m } }\n\
             fn main() {\n\
                 let mut mp: Map[String, i64] = Map.new();\n\
                 let _ = mp.insert(\"a\", 1);\n\
                 let _ = mp.insert(\"b\", 2);\n\
                 let t = V.Table(mp);\n\
                 let m2 = unwrap(t);\n\
                 println(m2.len() as i64);\n\
             }",
    ) {
        assert_eq!(out.trim(), "2");
    }
}

#[test]
fn test_e2e_struct_owning_map_shared_drops_cleanly() {
    // E2E: an `Owner` struct owns a `Map[i64, Node]` where
    // `Node` is a shared struct. Constructing the field with
    // `Map.new()` inline (no source local to double-track)
    // and letting scope-exit run the synthesized struct drop
    // is the contained surface for item 4 — the local-then-
    // place pattern would trip a pre-existing Map-handle
    // move-suppression gap (struct-field construction
    // suppresses Vec/String/struct source cleanups but not
    // Map handles; double-free on the local's FreeMapHandle
    // + the struct's drop). The IR test above pins the
    // structural assertion (walk emitted in the drop fn);
    // this test exercises it at runtime on the empty-Map
    // path so a future regression that breaks the drop fn's
    // IR shape surfaces as a crash here.
    let out = run_program(
        r#"
shared struct Node { val: i64 }
struct Owner { m: Map[i64, Node] }
fn main() {
    let _o = Owner { m: Map.new() };
    println(42);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

/// B-2026-08-26-22 — `Map.reserve` / `Set.reserve` and their `try_` twins,
/// twinned against the interpreter.
///
/// The two backends do genuinely different work: the interpreter reserves
/// on a host `HashMap`, codegen calls `karac_map_reserve`, which widens its
/// own open-addressed table. Neither exposes a `capacity()` accessor, so
/// they are not obliged to agree on a bucket count — CONTENTS and `len()`
/// are the contract, and that is what this compares.
///
/// The negative-reserve leg is not padding. The Map path crosses an FFI
/// boundary, and typing that parameter `u64` rather than `i64` turns
/// `reserve(-5)` into a reservation of 18 quintillion entries; measured, it
/// aborted the process with `panic: out of memory`.
#[test]
fn e2e_map_and_set_reserve_agree_with_the_interpreter() {
    let cases: &[(&str, &str)] = &[
        (
            "Map.reserve keeps contents",
            "let mut m: Map[i64, i64] = Map.new();\n\
                 m.reserve(1000);\n\
                 let mut i = 0;\n\
                 while i < 200 { m.insert(i, i * 2); i = i + 1; }\n\
                 m.reserve(-5);\n\
                 m.reserve(0);\n\
                 println(f\"{m.len()} {m.get(7).unwrap()} {m.get(199).unwrap()}\");",
        ),
        (
            "Set.reserve keeps contents",
            "let mut s: Set[i64] = Set.new();\n\
                 s.reserve(500);\n\
                 let mut i = 0;\n\
                 while i < 100 { s.insert(i); i = i + 1; }\n\
                 s.reserve(-3);\n\
                 println(f\"{s.len()} {s.contains(7)} {s.contains(999)}\");",
        ),
        (
            "negative reserve is a no-op everywhere",
            "let mut v: Vec[i64] = Vec.new();\n\
                 v.push(1);\n\
                 v.reserve(-100);\n\
                 let mut s: String = String.new();\n\
                 s.push_str(\"k\");\n\
                 s.reserve(-100);\n\
                 let mut m: Map[i64, i64] = Map.new();\n\
                 m.insert(1, 1);\n\
                 m.reserve(-100);\n\
                 let mut t: Set[i64] = Set.new();\n\
                 t.insert(1);\n\
                 t.reserve(-100);\n\
                 println(f\"{v.len()} [{s}] {m.len()} {t.len()}\");",
        ),
        (
            "Vec.from_iter is collect",
            "let a: Vec[i64] = Vec.from_iter(0..4);\n\
                 let b: Vec[i64] = Vec.from_iter((0..6).map(|x| x * 2));\n\
                 let src: Vec[i64] = [5, 6, 7];\n\
                 let c: Vec[i64] = Vec.from_iter(src.iter().map(|x| x + 1));\n\
                 let d = Vec.from_iter(0..3);\n\
                 println(f\"{a[3]} {b[5]} {c[2]} {d.len()}\");",
        ),
    ];
    for (label, body) in cases {
        let src = format!("fn main() {{\n{body}\n}}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        let expected = interp_out.join("");
        if let Some(aot) = run_program(&src) {
            assert_eq!(aot, expected, "{label}: AOT and the interpreter disagree");
        }
    }
}

#[test]
fn test_sorted_map_codegen_now_compiles() {
    // B-2026-07-09-17: `SortedMap` used to be rejected at codegen
    // (interpreter-only, B3). It now LOWERS under `karac build` — sharing
    // `Map`'s `KaracMap` storage + `karac_map_sorted_keys` ordered
    // observation — so the constructor + storage methods must reach
    // `compile_to_ir` cleanly (the old fail-loud guard is inverted). Runtime
    // behaviour is covered byte-for-byte against the interpreter oracle by
    // `e2e_sorted_map_int_keys_values_entries_codegen` / `..._string_keys`.
    let mut parsed = karac::parse(
        "fn main() {\n\
                 let mut m: SortedMap[i64, String] = SortedMap.new();\n\
                 let _ = m.insert(1_i64, \"one\");\n\
                 let mut out: String = \"\";\n\
                 for (k, v) in m { out.push_str(v); }\n\
                 println(out);\n\
             }",
    );
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "SortedMap should typecheck: {:?}",
        typed.errors
    );
    karac::lower(&mut parsed.program, &typed);
    assert!(
        compile_to_ir(&parsed.program, None, None).is_ok(),
        "SortedMap should now compile to IR (B-2026-07-09-17)"
    );
}

#[test]
fn test_e2e_struct_tuple_map_leaf_drop_flags() {
    // #23 (phase-12 self-hosting) — a `Map` leaf inside a tuple inside a struct
    // field. Two coupled fixes land here: (1) a `Map` folded into a tuple
    // transfers its handle to the tuple's owner — the source binding's
    // `FreeMapHandle` is dropped at tuple construction (Part B) and a Map-owning
    // tuple VAR gets a `TypeExpr`-driven drop (Part A) / is suppressed when moved
    // into a struct field (Part C1) — so the owning struct's #21 `NestedTuple`
    // drop is the SOLE freer (was a double-free against the caller-retains source
    // binding that still freed); (2) the `NestedTuple` Map drop AND the regular
    // struct-field Map drop now compute the `karac_map_free_with_drop_vec` K/V
    // flags from the element types instead of hardcoding `(1, 1)` — the old flags
    // read offset-16 of an 8-byte scalar key as a bogus `cap` and freed the key
    // VALUE as a pointer, corrupting any occupied `Map[i64, i64]`
    // (`B-2026-06-13-18`). Correctness here is the map contents round-tripping
    // for scalar / String-key / Vec-value maps (read via the whole-tuple by-value
    // path, which is sound — `struct.tuple.0` element reads are a separate
    // pre-existing place-chain gap, see #25), a tuple var moved into a struct
    // field, and a plain `Map` struct field (the regular drop path). The leak +
    // double-free are covered by `asan_struct_tuple_map_leaf_no_double_free`.
    if let Some(out) = run_program(
        r#"
struct Hi { m: (Map[i64, i64], i64) }
struct Hs { m: (Map[String, i64], i64) }
struct Hv { m: (Map[i64, Vec[i64]], i64) }
struct Sp { m: Map[i64, i64] }
fn ci(p: (Map[i64, i64], i64)) -> i64 { let (mm, n) = p; mm.len() + n }
fn cs(p: (Map[String, i64], i64)) -> i64 { let (mm, n) = p; mm.len() + n }
fn cv(p: (Map[i64, Vec[i64]], i64)) -> i64 { let (mm, n) = p; mm.len() + n }
fn main() {
    // Scalar tuple-Map field, local source (the #23 double-free shape) + by-value consume.
    let mut a: Map[i64, i64] = Map.new();
    a.insert(1, 10); a.insert(2, 20);
    let hi = Hi { m: (a, 3) };
    println(ci(hi.m).to_string());
    // String-key tuple-Map field (drop_key must be 1 — free the key buffers).
    let mut s: Map[String, i64] = Map.new();
    s.insert("alpha".to_string(), 1); s.insert("beta".to_string(), 2);
    let hs = Hs { m: (s, 7) };
    println(cs(hs.m).to_string());
    // Vec-value tuple-Map field (drop_val must be 1 — free the value buffers).
    let mut v: Map[i64, Vec[i64]] = Map.new();
    let mut vv: Vec[i64] = Vec.new(); vv.push(9);
    v.insert(5, vv);
    let hv = Hv { m: (v, 0) };
    println(cv(hv.m).to_string());
    // Tuple VAR moved into a struct field (Part A registers its drop, Part C1
    // suppresses it on the move so only the struct's NestedTuple drop frees).
    let mut b: Map[i64, i64] = Map.new();
    b.insert(4, 40);
    let pair = (b, 8);
    let hi2 = Hi { m: pair };
    println(ci(hi2.m).to_string());
    // Plain Map[i64,i64] struct field — the regular MapOrSet drop path + flag fix.
    let mut c: Map[i64, i64] = Map.new();
    c.insert(6, 60);
    let sp = Sp { m: c };
    println(sp.m.len().to_string());
    println("ok");
}
"#,
    ) {
        assert_eq!(out, "5\n9\n1\n9\n1\nok\n");
    }
}

#[test]
fn test_e2e_tuple_index_map_set_receiver_method() {
    // #26 (phase-12 self-hosting, B-2026-06-14-6) — a method on a Map/Set
    // TUPLE element (`h.m.0.len()`). `Map`/`Set` lower to an opaque `ptr`
    // handle, and their runtime methods (`karac_map_*`) resolve the handle
    // via a NAMED slot (`compile_map_method` → `get_data_ptr`) — so only an
    // identifier receiver dispatched correctly. A tuple-index receiver fell
    // through to a generic path and read a GARBAGE handle (`h.m.0.len()`
    // printed e.g. `6132656048`); the FieldAccess peer `s.m.len()` already
    // worked. Fix: a `TupleIndex` sibling of `try_compile_field_receiver_method`
    // that GEPs to the element handle slot (`field_chain_place_ptr`) and
    // re-dispatches through a synth identifier — so len/get/contains_key/
    // is_empty AND in-place insert all resolve. Vec/scalar tuple elements are
    // unaffected (they already work via value extraction). The scalar second
    // element (`h.m.1`) is the regression guard.
    if let Some(out) = run_program(
        r#"
struct H { m: (Map[i64, i64], i64) }
struct Hs { s: (Set[i64], i64) }
fn mkm() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1, 10); m.insert(2, 20);
    return m;
}
fn mks() -> Set[i64] {
    let mut s: Set[i64] = Set.new();
    s.insert(3); s.insert(4);
    return s;
}
fn main() {
    let mut h = H { m: (mkm(), 7) };
    println(h.m.0.len().to_string());                 // 2
    println(h.m.0.get(1).unwrap_or(0).to_string());   // 10
    println(h.m.0.contains_key(2).to_string());       // true
    println(h.m.1.to_string());                       // 7 (scalar elem regression)
    // In-place mutation through the tuple element.
    h.m.0.insert(9, 90);
    println(h.m.0.len().to_string());                 // 3
    println(h.m.0.get(9).unwrap_or(0).to_string());   // 90
    // Set element methods.
    let hs = Hs { s: (mks(), 1) };
    println(hs.s.0.len().to_string());                // 2
    println(hs.s.0.contains(3).to_string());          // true
}
"#,
    ) {
        assert_eq!(out, "2\n10\ntrue\n7\n3\n90\n2\ntrue\n");
    }
}

#[test]
fn test_e2e_map_set_local_bind_from_place() {
    // #28 (phase-12 self-hosting, B-2026-06-14-9) — a Map/Set bound to a
    // LOCAL from a PLACE source with no annotation (`let mm = s.m` /
    // `let mm = h.m.0`). The let-binding's unannotated fallback registered
    // only `var_type_names` (not the Map/Set dispatch side-tables), so
    // `mm.len()` build-failed (`no handler for method 'len' on variable
    // 'mm'`). Fix: a Map/Set arm in the fallback registers the side-tables
    // from the typechecker's `pattern_binding_inner_types`. `mm` aliases the
    // source handle (caller-retains — no second `FreeMapHandle`), so the
    // owner is the sole freer; an in-place mutation through `mm` mutates the
    // shared map. The annotated form (`let mm: Map[..] = s.m`) already worked.
    if let Some(out) = run_program(
        r#"
struct S { m: Map[i64, i64] }
struct Hz { z: Set[i64] }
struct Ht { t: (Map[i64, i64], i64) }
fn mkm() -> Map[i64, i64] { let mut m: Map[i64, i64] = Map.new(); m.insert(1, 10); m.insert(2, 20); return m; }
fn mks() -> Set[i64] { let mut s: Set[i64] = Set.new(); s.insert(3); s.insert(4); return s; }
fn main() {
    // Map field bound to a local, no annotation.
    let s = S { m: mkm() };
    let mm = s.m;
    println(mm.len().to_string());                  // 2
    println(mm.get(1).unwrap_or(0).to_string());    // 10
    // Mutation through the bound local (mutates the shared handle).
    let s2 = S { m: mkm() };
    let mut mm2 = s2.m;
    mm2.insert(9, 90);
    println(mm2.len().to_string());                 // 3
    // Set field bound to a local.
    let z = Hz { z: mks() };
    let ss = z.z;
    println(ss.len().to_string());                  // 2
    println(ss.contains(3).to_string());            // true
    // Map TUPLE element bound to a local.
    let h = Ht { t: (mkm(), 7) };
    let tm = h.t.0;
    println(tm.len().to_string());                  // 2
}
"#,
    ) {
        assert_eq!(out, "2\n10\n3\n2\ntrue\n2\n");
    }
}

#[test]
fn test_e2e_option_unwrap_map_get_primitive() {
    // Slice OR (2026-05-16): `Option[T].unwrap()` dispatch lowering.
    // The receiver here is a `MethodCall` (`m.get(k)`), exercising
    // the receiver-shape-agnostic path that compiles the receiver
    // to a temporary SSA value rather than minting a synth
    // identifier.  Closes the previously unblockable `let x =
    // m.get(k).unwrap()` shape — which `karac build` rejected with
    // the "no handler for method 'unwrap' on non-identifier
    // receiver" fall-through diagnostic before this slice.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    let _ = m.insert(1, 42);
    let _ = m.insert(2, 100);
    let x = m.get(1).unwrap();
    let y = m.get(2).unwrap();
    println(x);
    println(y);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["42", "100"]);
    }
}

#[test]
fn test_ir_discarded_map_temp_emits_free() {
    // General owned-temp tracking, slice 2
    // (docs/spikes/general-owned-temp-tracking.md): a discarded fresh Map
    // handle (`make_map();`) is a plain pointer — indistinguishable from
    // any heap pointer by LLVM type, so slice 1 leaked it. The
    // lowering-pass `owned_temp_drops` hint table carries the producing
    // expression's `Map[K, V]` TypeExpr; `materialize_owned_temp` keys it
    // by span, materializes an `__owned_tmp` slot, and queues a
    // `FreeMapHandle` → `karac_map_free` (both halves primitive). Archive-
    // independent leak-closure gate (macOS ASAN has no LeakSanitizer).
    let src = r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 2_i64);
    return m;
}

fn main() {
    make_map();
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__owned_tmp"),
        "expected discarded Map temp materialized into __owned_tmp slot; got:\n{}",
        ir
    );
    assert!(
        ir.contains("karac_map_free"),
        "expected a FreeMapHandle drain (karac_map_free call) for the discarded Map temp; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_map_struct_value_binding_emits_val_drop_fn() {
    // Slice 3r (deferred gap (d)): a `Map[i64, Holder]` binding where the
    // struct value owns heap routes its scope-exit free through
    // `karac_map_free_with_val_drop_fn`, passing the synthesized
    // `__karac_drop_struct_Holder` per-value walk. The flag-based helper
    // (`karac_map_free_with_drop_vec`) can only free `{ptr,len,cap}`
    // overlays and leaked the struct's String field.
    let src = r#"
struct Holder { name: String, id: i64 }

fn main() {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: "a heap string padded out beyond thirty-six bytes!", id: 1 };
    m.insert(1, h);
    println(m.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("call void @karac_map_free_with_val_drop_fn"),
        "expected the struct-valued map's scope-exit free to route through \
             karac_map_free_with_val_drop_fn (the symbol is always declared — the \
             CALL is the signal); got:\n{}",
        ir
    );
    assert!(
        ir.contains("__karac_drop_struct_Holder"),
        "expected the synthesized per-value struct drop __karac_drop_struct_Holder \
             passed as the val_drop_fn; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_map_inner_map_value_emits_recursive_map_drop() {
    // Slice 3r: `Map[i64, Map[i64, String]]` — the value drop fn is the
    // upgraded `karac_drop_Map_i64_String` (the 0.c placeholder freed only
    // the handle), which itself routes the inner map through the
    // flag-based free so the deepest Strings drop.
    let src = r#"
fn main() {
    let mut inner: Map[i64, String] = Map.new();
    inner.insert(1, "a heap string padded out beyond thirty-six bytes!");
    let mut m: Map[i64, Map[i64, String]] = Map.new();
    m.insert(2, inner);
    println(m.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("call void @karac_map_free_with_val_drop_fn"),
        "expected the outer map's free CALL through karac_map_free_with_val_drop_fn; \
             got:\n{}",
        ir
    );
    assert!(
        ir.contains("karac_drop_Map_i64_String"),
        "expected the synthesized recursive inner-map drop karac_drop_Map_i64_String; \
             got:\n{}",
        ir
    );
}

#[test]
fn test_ir_inferred_map_struct_value_binding_gets_val_drop_fn() {
    // Slice 3r + the 3p `str`-spelling discipline: an UNANNOTATED
    // cross-fn binding (`let m = build();`) derives its value TypeExpr
    // from `type_to_type_expr` (which spells `str`, not `String`) — the
    // selection must still arm the per-value drop fn for the struct
    // value's String field.
    let src = r#"
struct Holder { name: String, id: i64 }

fn build() -> Map[i64, Holder] {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: "a heap string padded out beyond thirty-six bytes!", id: 1 };
    m.insert(1, h);
    return m;
}

fn main() {
    let m = build();
    println(m.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("call void @karac_map_free_with_val_drop_fn"),
        "expected the INFERRED struct-valued map binding to arm the per-value drop \
             CALL (val TypeExpr comes from type_to_type_expr — both spellings must \
             gate); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_map_get_moveout_arm_emits_payload_clone() {
    // Slice 3s (B-2026-07-01-12): `let s = match m.get(k) { Some(x) => x,
    // … }` — the escaping borrow-mode payload binding is deep-cloned
    // (`karac_clone_str` — the INFERRED spelling; `enum_inst_type_exprs`
    // renders `Option[str]`) so `s` owns an independent buffer and the
    // map keeps its stored value.
    let src = r#"
fn main() {
    let mut m: Map[i64, String] = Map.new();
    m.insert(7, "a heap string padded out beyond thirty-six bytes!");
    let s = match m.get(7) {
        Some(x) => x,
        None => "n".to_string(),
    };
    println(s.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("call void @karac_clone_str") || ir.contains("call void @karac_clone_String"),
        "expected the escaping Map.get payload binding deep-cloned \
             (karac_clone_str / karac_clone_String call); got:\n{}",
        ir
    );
    assert!(
        ir.contains("borrow.clone.tmp"),
        "expected the borrow-payload clone fixup temp; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_map_get_readonly_arm_emits_no_clone() {
    // Slice 3s perf gate: a READ-ONLY arm (`Some(x) => x.len()`) never
    // reaches the clone — the escape walk classifies receiver-position
    // uses as borrows, keeping the hot map-read shape zero-cost.
    let src = r#"
fn main() {
    let mut m: Map[i64, String] = Map.new();
    m.insert(7, "a heap string padded out beyond thirty-six bytes!");
    let n = match m.get(7) {
        Some(x) => x.len(),
        None => 0,
    };
    println(n);
}
"#;
    let ir = ir_for(src);
    assert!(
        !ir.contains("borrow.clone.tmp"),
        "a read-only arm must not clone the borrowed payload (escape walk \
             false); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_map_get_letbound_moveout_emits_payload_clone() {
    // B-2026-07-09-13: the SAME escaping-payload clone must fire when the
    // `Map.get` result is bound to an intermediate `let` and matched
    // through the binding (`let g = m.get(k); match g { Some(x) => x }`).
    // The `let` indirection made the scrutinee an identifier, which
    // `scrutinee_is_borrow_call` (method-call-only) didn't recognize, so the
    // clone was skipped and the escaping alias double-freed against the
    // Map's value drop. `borrow_accessor_let_payload` re-admits the binding
    // into the borrow protection, so the clone fires exactly as in the
    // direct form.
    let src = r#"
fn main() {
    let mut m: Map[i64, String] = Map.new();
    m.insert(7, "a heap string padded out beyond thirty-six bytes!");
    let g = m.get(7);
    let s = match g {
        Some(x) => x,
        None => "n".to_string(),
    };
    println(s.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("borrow.clone.tmp"),
        "the let-bound Map.get escaping payload must clone (borrow.clone.tmp) \
             just like the direct form; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_ref_arg_map_emits_free() {
    // Slice 2 part B: a fresh `Map[i64,i64]` handle passed to a `ref
    // Map[i64,i64]` param. The prior `ref_rvalue_arg` path only tracked
    // Vec/String temps, so the handle leaked. `queue_ref_rvalue_arg_cleanup`
    // now recognizes the `Map[K,V]` TypeExpr and queues a `FreeMapHandle`
    // → `karac_map_free` against the temp. Archive-independent leak gate.
    let src = r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 2_i64);
    return m;
}

fn show(m: ref Map[i64, i64]) {
    println(m.len());
}

fn main() {
    show(make_map());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("ref_rvalue_arg"),
        "expected the fresh Map handle materialized into a ref_rvalue_arg temp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("karac_map_free"),
        "expected a FreeMapHandle drain (karac_map_free) for the ref-arg Map temp; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_map_iter_emits_materialize_and_handle_free() {
    // Slice 3i: `for (k, v) in make_map().iter()` on a fresh-temp
    // `Map[String, i64]`. The for-loop peels `.iter()` and recurses on the
    // receiver `make_map()`, whose span collides with the `.iter()` MethodCall
    // — `expr_types` holds `Iterator[(String, i64)]`, so `owned_temp_drops`
    // has NO entry. Pre-fix the loop fell through `try_compile_for_vec_value`
    // (non-Vec → None) to the silent skip (body never ran, output 0). The
    // fresh-temp Map/Set gate now records the whole `Map[String, i64]`
    // span-keyed in `temp_recv_mapset_types`; codegen materializes the handle
    // into a `__for_mapset_` synth local, drives the map iterator, and frees
    // the handle + each stored String key via `karac_map_free_with_drop_vec`
    // at scope exit. Without the materialize the body is skipped; without the
    // per-entry drop the String keys leak (Linux LSan).
    let src = r#"
fn make_map() -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert("a heap map key padded out beyond thirty-six bytes ok", 10_i64);
    return m;
}

fn main() {
    let mut total = 0_i64;
    for (k, v) in make_map().iter() {
        total = total + v + k.len();
    };
    println(total);
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__for_mapset_"),
        "expected the fresh-temp Map[String, i64] iterable materialized into a \
             __for_mapset_ synth local (not the silent body-skip); got:\n{}",
        ir
    );
    assert!(
        ir.contains("karac_map_free_with_drop_vec"),
        "expected the materialized map temp's handle freed with per-entry String \
             drop (karac_map_free_with_drop_vec); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_map_get_emits_handle_free() {
    // Slice 3d: `make_map().get(k)` on a fresh-temp `Map[i64,i64]` receiver.
    // The map handle is a plain `ptr`, materialized into a `__mrecv_tmp`
    // slot; the read borrows the map, so the temp handle must be drop-tracked
    // with a `FreeMapHandle` (scalar K/V → plain `karac_map_free`) at the
    // enclosing frame's exit. Without it the whole map handle leaks
    // (LeakSanitizer on Linux CI). The `Some(v)` borrow is NOT independently
    // dropped (`scrutinee_is_borrow_call`).
    let src = r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
    return m;
}

fn main() {
    match make_map().get(1_i64) {
        Some(v) => println(v),
        None => println(0_i64),
    };
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__mrecv_tmp"),
        "expected the fresh Map receiver handle materialized into __mrecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("karac_map_free"),
        "expected a FreeMapHandle drain (karac_map_free) for the fresh-temp Map receiver; \
             got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_set_contains_emits_handle_free() {
    // Slice 3d: `make_set().contains(x)` on a fresh-temp `Set[i64]`. Like the
    // Map case the handle is materialized into `__mrecv_tmp` and drop-tracked
    // (`karac_map_free` — a Set is a map under the hood). `contains` returns
    // `bool` (no borrow), so the only obligation is freeing the handle once.
    let src = r#"
fn make_set() -> Set[i64] {
    let mut s: Set[i64] = Set.new();
    s.insert(7_i64);
    return s;
}

fn main() {
    println(make_set().contains(7_i64));
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__mrecv_tmp"),
        "expected the fresh Set receiver handle materialized into __mrecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("karac_map_free"),
        "expected a FreeMapHandle drain (karac_map_free) for the fresh-temp Set receiver; \
             got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_map_keys_emits_materialize_and_handle_free() {
    // Slice 3l: `make_map().keys()` on a fresh-temp `Map[i64,i64]`. `.keys()`
    // materializes a fresh `Vec[i64]` of the keys, but the MAP receiver is
    // itself a fresh owned temp that must be freed once it's been read. The
    // fresh-temp Map path materializes the handle into `__mrecv_tmp` and
    // drop-tracks it (`FreeMapHandle` — scalar K/V → plain `karac_map_free`)
    // at the enclosing frame's exit. Without it the map handle leaks
    // (LeakSanitizer on Linux CI). The returned `Vec[i64]` is owned by the
    // `let` binding and freed independently. This was a hard error pre-fix
    // ("no handler for method 'keys' on non-identifier receiver") — the
    // typechecker's fresh-temp Map gate didn't record `keys`/`values`, so
    // codegen's `temp_recv_mapset_types` lookup missed and the helper bailed.
    let src = r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
    return m;
}

fn main() {
    let ks: Vec[i64] = make_map().keys();
    println(ks.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__mrecv_tmp"),
        "expected the fresh Map receiver handle materialized into __mrecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("karac_map_free"),
        "expected a FreeMapHandle drain (karac_map_free) for the fresh-temp Map \
             `.keys()` receiver; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_map_entries_emits_materialize_and_handle_free() {
    // Slice 3m: `make_map().entries()` on a fresh-temp `Map[i64,i64]`.
    // `.entries()` materializes a fresh `Vec[(i64,i64)]`, but the MAP receiver
    // is a fresh owned temp — materialized into `__mrecv_tmp` and freed once
    // (`karac_map_free`, scalar K/V) at frame exit. The returned tuple Vec is
    // owned by the `let` binding. Sibling of the keys/values slice (3l); the
    // typechecker gate now records `entries` too.
    let src = r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
    return m;
}

fn main() {
    let es: Vec[(i64, i64)] = make_map().entries();
    println(es.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__mrecv_tmp"),
        "expected the fresh Map receiver handle materialized into __mrecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("karac_map_free"),
        "expected a FreeMapHandle drain (karac_map_free) for the fresh-temp Map \
             `.entries()` receiver; got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_set_vec_dedup_by_content() {
    // `Set[Vec[T]]` has HashSet value semantics: two equal-CONTENTS vecs
    // collapse to one element (B-2026-06-20-15). Before this fix the
    // typechecker rejected `Vec` as un-`Hash + Eq` (hard error in the
    // codegen path), and even past that gate codegen synthesized a
    // hash/eq over the `{ptr,len,cap}` HEADER (pointer identity), so two
    // equal-contents vecs landed in different buckets → `len() == 2`. The
    // interpreter (GC-by-Value-clone, structural `Value` eq) always
    // deduped, so this was a silent A/B divergence; codegen now walks the
    // element contents (`karac_hash_Vec_<elem>` / `karac_eq_Vec_<elem>`)
    // to match. Exercises insert dedup, a distinct-contents non-merge,
    // `contains`/`remove` over the same content eq, and the
    // same-prefix-different-length non-equality.
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[Vec[i64]] = Set.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(101i64); a.push(102i64); a.push(103i64);
    s.insert(a);
    let mut b: Vec[i64] = Vec.new();
    b.push(101i64); b.push(102i64); b.push(103i64);
    s.insert(b);
    println(s.len());           // 1 — deduped by content
    let mut c: Vec[i64] = Vec.new();
    c.push(101i64); c.push(102i64); c.push(999i64);
    s.insert(c);
    println(s.len());           // 2 — distinct contents stay separate
    let mut probe: Vec[i64] = Vec.new();
    probe.push(101i64); probe.push(102i64); probe.push(103i64);
    println(s.contains(probe)); // true
    let mut rem: Vec[i64] = Vec.new();
    rem.push(101i64); rem.push(102i64); rem.push(103i64);
    println(s.remove(rem));     // true
    println(s.len());           // 1
    let mut u: Set[Vec[i64]] = Set.new();
    let mut p: Vec[i64] = Vec.new(); p.push(7i64);
    u.insert(p);
    let mut q: Vec[i64] = Vec.new(); q.push(7i64); q.push(7i64);
    u.insert(q);
    println(u.len());           // 2 — same prefix, different length ≠ equal
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1\n2\ntrue\ntrue\n1\n2");
    }
}

#[test]
fn test_e2e_map_vec_key_dedup_by_content() {
    // The same hash/eq dispatch fix that fixes `Set[Vec[T]]` also makes a
    // `Vec[T]` MAP KEY behave by content: re-inserting an equal-contents
    // key overwrites the existing entry rather than adding a second bucket
    // (`len() == 1`), and a content-equal `get_or` probe finds it
    // (B-2026-06-20-15). `Set` lowers to `Map[T, ()]`, so both share the
    // per-element hash/eq emitters.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[Vec[i64], i64] = Map.new();
    let mut k1: Vec[i64] = Vec.new(); k1.push(1i64); k1.push(2i64);
    m.insert(k1, 10);
    let mut k2: Vec[i64] = Vec.new(); k2.push(1i64); k2.push(2i64);
    m.insert(k2, 20);           // same key by content → overwrites
    println(m.len());           // 1
    let mut probe: Vec[i64] = Vec.new(); probe.push(1i64); probe.push(2i64);
    println(m.get_or(probe, -1)); // 20
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1\n20");
    }
}

#[test]
fn test_e2e_map_set_moved_into_returned_enum_variant_no_uaf() {
    // Non-vacuous UAF gate (phase-6 line 562): a `Map`/`Set` local moved
    // into a returned enum variant (`return Some(m)`) must NOT be freed at
    // the source function's scope exit. Without the enum-variant
    // move-suppression, `make`'s scope exit frees the handle the returned
    // Option carries, so the caller's `m.len()` reads a freed handle in
    // the (non-instrumented) runtime → deterministically wrong/empty
    // output on a plain build (ASAN's quarantine masks it — see the
    // sibling `asan_..._clean` note). Looped to also exercise
    // per-iteration alloca reuse. `return Some(m)` routes through the
    // enum-variant arg suppression, not the return-stmt Identifier path
    // (`Some(m)` is a constructor, not a bare Identifier).
    let out = run_program(
        r#"
fn make_map(x: i64) -> Option[Map[i64, i64]] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(x, x * 2);
    return Some(m);
}
fn make_set(x: i64) -> Option[Set[i64]] {
    let mut s: Set[i64] = Set.new();
    s.insert(x);
    s.insert(x + 100);
    return Some(s);
}
fn main() {
    let mut i: i64 = 0;
    while i < 4 {
        let mo: Option[Map[i64, i64]] = make_map(i);
        match mo {
            Some(m) => println(i + m.len()),
            None => println(-1),
        }
        let so: Option[Set[i64]] = make_set(i);
        match so {
            Some(s) => {
                if s.contains(i) { println(s.len()); } else { println(-1); }
            }
            None => println(-1),
        }
        i = i + 1;
    }
    println(99);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1\n2\n2\n2\n3\n2\n4\n2\n99");
    }
}

#[test]
fn test_ir_struct_destructure_set_bound_field_freed() {
    // A bound `Set` field is freed via its binding's scope-exit cleanup.
    // Scoped to `main`'s body so the `declare @karac_map_free` line + any
    // free inside `make()` don't false-positive.
    let src = format!(
            "{SET_DESTRUCTURE_PRELUDE}fn main() {{\n    let Bag {{ tags, count }} = make();\n    println(tags.len() + count);\n}}\n"
        );
    let ir = ir_for_with_ownership(&src);
    let n = main_body_ir(&ir)
        .matches("call void @karac_map_free")
        .count();
    assert_eq!(
        n,
        1,
        "bound Set field must be freed exactly once in main (got {n}); IR:\n{}",
        main_body_ir(&ir)
    );
}

#[test]
fn test_ir_struct_destructure_set_unbound_field_freed() {
    // An explicitly-discarded `Set` field (`tags: _`) is stashed in a
    // synthetic discard slot and freed there.
    let src = format!(
            "{SET_DESTRUCTURE_PRELUDE}fn main() {{\n    let Bag {{ tags: _, count }} = make();\n    println(count);\n}}\n"
        );
    let ir = ir_for_with_ownership(&src);
    let main_ir = main_body_ir(&ir);
    assert!(
        main_ir.contains("__destructure_discard_") && main_ir.contains("karac_map_free"),
        "unbound Set field must be discard-freed via karac_map_free in main; got:\n{ir}"
    );
}

#[test]
fn test_e2e_map_index_set_fresh_and_overwrite() {
    // m[k] = v on a missing key inserts; on an existing key overwrites.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m[1_i64] = 10_i64;
    m[2_i64] = 20_i64;
    println(m[1_i64]);
    println(m[2_i64]);
    m[1_i64] = 99_i64;
    println(m[1_i64]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "20", "99"]);
    }
}

#[test]
fn test_e2e_map_keys_returns_vec() {
    // m.keys() materializes Vec[K] containing every key. Iteration order
    // is unspecified, so sum the keys and verify total.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(10_i64, 1_i64);
    m.insert(20_i64, 2_i64);
    m.insert(30_i64, 3_i64);
    let ks: Vec[i64] = m.keys();
    println(ks.len());
    let mut sum: i64 = 0;
    for k in ks {
        sum = sum + k;
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "60"]);
    }
}

#[test]
fn test_e2e_map_values_returns_vec() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
    m.insert(2_i64, 200_i64);
    m.insert(3_i64, 300_i64);
    let vs: Vec[i64] = m.values();
    println(vs.len());
    let mut sum: i64 = 0;
    for v in vs {
        sum = sum + v;
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "600"]);
    }
}

#[test]
fn test_e2e_map_entries_returns_vec_of_tuples() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 10_i64);
    m.insert(2_i64, 20_i64);
    let es: Vec[(i64, i64)] = m.entries();
    println(es.len());
    let mut k_sum: i64 = 0;
    let mut v_sum: i64 = 0;
    for (k, v) in es {
        k_sum = k_sum + k;
        v_sum = v_sum + v;
    }
    println(k_sum);
    println(v_sum);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "3", "30"]);
    }
}

#[test]
fn test_e2e_map_tuple_key_overwrite_returns_old() {
    // Re-inserting under the same compound key returns the prior value —
    // exercises the eq-fn path (the runtime must find the existing slot).
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[(String, i64), i64] = Map.new();
    let first = m.insert(("k", 1_i64), 10_i64);
    match first {
        Some(x) => println(x),
        None => println(0_i64),
    }
    let second = m.insert(("k", 1_i64), 20_i64);
    match second {
        Some(x) => println(x),
        None => println(0_i64),
    }
    println(m.len());
}
"#,
    );
    let out = out.expect("tuple-key overwrite codegen should not bail");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["0", "10", "1"]);
}

#[test]
fn test_e2e_map_nested_tuple_key() {
    // `Map[(String, (i64, i64)), V]` — exercises the recursive emission
    // path: the outer tuple-hash recurses into the inner tuple-hash, which
    // recurses into the per-element primitive hash fns. Validates that
    // `karac_hash_tuple_String_tuple_i64_i64` emits exactly once and works
    // end-to-end.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[(String, (i64, i64)), i64] = Map.new();
    m.insert(("p", (1_i64, 2_i64)), 12_i64);
    m.insert(("p", (3_i64, 4_i64)), 34_i64);
    m.insert(("q", (1_i64, 2_i64)), 99_i64);
    println(m.len());
    let v = m.get(("p", (1_i64, 2_i64)));
    match v {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
    let v2 = m.get(("q", (1_i64, 2_i64)));
    match v2 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
    let v3 = m.get(("p", (9_i64, 9_i64)));
    match v3 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
}
"#,
    );
    let out = out.expect("nested tuple-key codegen should not bail");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["3", "12", "99", "-1"]);
}

#[test]
fn test_e2e_map_primitive_struct_key() {
    // `#[derive(Hash, Eq)]` struct of primitives lowers to a packed-by-
    // field LLVM struct with no padding (here: `{ i64, i64 }`, 16 bytes).
    // The existing byte-loop FNV-1a path hashes the raw struct bytes,
    // and the byte-by-byte eq compares them — both correct for the
    // primitive-only case.
    let out = run_program(
        r#"
#[derive(Hash, Eq)]
struct Point {
    x: i64,
    y: i64,
}

fn main() {
    let mut m: Map[Point, i64] = Map.new();
    m.insert(Point { x: 1_i64, y: 2_i64 }, 12_i64);
    m.insert(Point { x: 3_i64, y: 4_i64 }, 34_i64);
    println(m.len());
    let v = m.get(Point { x: 1_i64, y: 2_i64 });
    match v {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
    let v2 = m.get(Point { x: 9_i64, y: 9_i64 });
    match v2 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
}
"#,
    );
    let out = out.expect("primitive-struct-key codegen should not bail");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["2", "12", "-1"]);
}

#[test]
fn test_e2e_map_unit_enum_key() {
    // Unit-variant enum used as a Map key. Layout is `{ i64 tag }` — the
    // existing primitive hash/eq path (byte-by-byte over sizeof(K))
    // already does the right thing once the typechecker permits the
    // `K: Hash + Eq` bound.
    let out = run_program(
        r#"
#[derive(Hash, Eq)]
enum Color { Red, Green, Blue }

fn main() {
    let mut m: Map[Color, i64] = Map.new();
    m.insert(Color.Red,   100_i64);
    m.insert(Color.Green, 200_i64);
    m.insert(Color.Blue,  300_i64);
    println(m.len());
    let v1 = m.get(Color.Red);
    match v1 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
    let v2 = m.get(Color.Green);
    match v2 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
    let v3 = m.get(Color.Blue);
    match v3 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
}
"#,
    );
    let out = out.expect("unit-enum-key codegen should not bail");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["3", "100", "200", "300"]);
}

#[test]
fn test_e2e_map_compound_key_cache_reuse() {
    // Two distinct Map variables in one program share the same compound
    // key shape `(String, i64)`. Cache reuse means `karac_hash_tuple_*`
    // and `karac_eq_tuple_*` are emitted exactly once and called by both
    // map-new sites — duplicate emission would surface as a `module
    // already has a function named ...` panic during codegen, so this
    // test failing to compile is the cache regression signal.
    let out = run_program(
        r#"
fn main() {
    let mut m1: Map[(String, i64), i64] = Map.new();
    let mut m2: Map[(String, i64), i64] = Map.new();
    m1.insert(("a", 1_i64), 10_i64);
    m2.insert(("a", 1_i64), 99_i64);
    println(m1.len());
    println(m2.len());
    let v1 = m1.get(("a", 1_i64));
    match v1 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
    let v2 = m2.get(("a", 1_i64));
    match v2 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
}
"#,
    );
    let out = out.expect("compound-key cache reuse codegen should not bail");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["1", "1", "10", "99"]);
}

#[test]
fn test_e2e_map_keys_empty() {
    // Empty map → empty Vec; len=0, no iteration body runs.
    let out = run_program(
        r#"
fn main() {
    let m: Map[i64, i64] = Map.new();
    let ks: Vec[i64] = m.keys();
    println(ks.len());
    println(ks.is_empty());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "true"]);
    }
}

#[test]
fn test_e2e_map_clear() {
    // clear() empties the map; subsequent insert/lookup work normally.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 10_i64);
    m.insert(2_i64, 20_i64);
    m.insert(3_i64, 30_i64);
    println(m.len());
    m.clear();
    println(m.len());
    println(m.is_empty());
    m.insert(7_i64, 70_i64);
    println(m[7_i64]);
    println(m.contains_key(1_i64));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "0", "true", "70", "false"]);
    }
}

#[test]
fn test_e2e_map_bare_literal_with_annotation() {
    // Bare ["k": v] form with explicit Map type annotation.
    let out = run_program(
        r#"
fn main() {
    let m: Map[String, i64] = ["x": 10_i64, "y": 20_i64];
    println(m.len());
    println(m["x"]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "10"]);
    }
}

#[test]
fn test_e2e_map_index_panics_on_missing() {
    // Indexing a Map with a missing key panics at runtime.
    let captured = run_program_capturing(
        r#"
fn main() {
    let m: Map[i64, i64] = Map.new();
    let x = m[42_i64];
    println(x);
    println(99_i64);
}
"#,
    );
    if let Some(c) = captured {
        // Panic message printed to stdout (printf), then exit(1) — so
        // the trailing prints never run.
        assert!(
            c.stderr.contains("panic: Map index: key not present"),
            "expected panic message, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("99"),
            "code after panicking index should not run"
        );
    }
}

// ── Map.entry / Entry[K, V] codegen (canonical: phase-8-stdlib-floor.md
//    "Map.entry(k) + Entry[K, V] enum") ──────────────────────────────────

#[test]
fn test_e2e_map_entry_or_insert_vacant() {
    // Vacant key → or_insert pushes (key, default), map state changes.
    // The chain return is mut ref V — discarded here; the post-chain
    // get() call reads the inserted value.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.entry(7_i64).or_insert(42_i64);
    let v = m.get(7_i64);
    match v {
        Some(x) => println(x),
        None => println(0_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_map_entry_or_insert_occupied_passthrough() {
    // Occupied key → or_insert is a no-op write; map keeps existing value.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(3_i64, 99_i64);
    m.entry(3_i64).or_insert(0_i64);
    let v = m.get(3_i64);
    match v {
        Some(x) => println(x),
        None => println(-1_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

#[test]
fn test_e2e_map_entry_and_modify_runs_when_occupied() {
    // Occupied → and_modify's closure fires with mut ref V; the body's
    // mutation propagates back through the slot pointer.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(4_i64, 10_i64);
    m.entry(4_i64).and_modify(|v| { v += 1; });
    let v = m.get(4_i64);
    match v {
        Some(x) => println(x),
        None => println(0_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "11");
    }
}

#[test]
fn test_e2e_map_entry_and_modify_skips_when_vacant() {
    // Vacant → closure does not fire; map stays empty.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.entry(8_i64).and_modify(|v| { v += 1; });
    println(m.is_empty());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "true");
    }
}

#[test]
fn test_e2e_map_entry_and_modify_chain_or_insert() {
    // Canonical chain: vacant → or_insert seeds 1; subsequent calls
    // → and_modify increments. Three calls produce a final value of 3.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.entry(5_i64).and_modify(|v| { v += 1; }).or_insert(1_i64);
    m.entry(5_i64).and_modify(|v| { v += 1; }).or_insert(1_i64);
    m.entry(5_i64).and_modify(|v| { v += 1; }).or_insert(1_i64);
    let v = m.get(5_i64);
    match v {
        Some(x) => println(x),
        None => println(0_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn test_e2e_sorted_map_entry_chain_key_ordered() {
    // SortedMap reuses Map's KaracMap-backed entry chain (shared storage;
    // ascending order applies only at iteration). The and_modify/or_insert
    // count chain plus a trailing `or_insert(...).push(...)` mut-ref write
    // all resolve through the same codegen as Map; `keys()` emits ascending.
    let out = run_program(
        r#"
fn main() {
    let mut counts: SortedMap[String, i64] = SortedMap.new();
    let words = ["pear", "fig", "pear", "apple", "fig", "pear"];
    let mut i = 0i64;
    while i < words.len() {
        counts.entry(words[i].to_string()).and_modify(|c| { c += 1; }).or_insert(1);
        i = i + 1;
    }
    let mut groups: SortedMap[i64, Vec[i64]] = SortedMap.new();
    groups.entry(2_i64).or_insert(Vec.new()).push(20);
    groups.entry(1_i64).or_insert_with(|| Vec.new()).push(10);
    groups.entry(2_i64).or_insert(Vec.new()).push(40);
    for k in counts.keys() {
        println(f"{k}={counts.get_or(k.clone(), 0)}");
    }
    for g in groups.keys() {
        println(f"g{g}={groups.get_or(g, Vec.new()).len()}");
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "apple=1\nfig=2\npear=3\ng1=1\ng2=2");
    }
}

#[test]
fn test_e2e_map_entry_or_insert_trailing_push() {
    // Canonical kata idiom from `design.md § Entry[K, V]`:
    // `bucket.entry(k).or_insert(Vec.new()).push(v)`.
    // Exercises the new entry-chain-receiver method dispatch:
    // the chain produces a `*mut Vec[i64]` slot pointer; the
    // trailing `.push(v)` mutates the in-storage Vec via the
    // synth identifier; subsequent chains on the same key see
    // the accumulated contents through the same slot pointer
    // (verifies both the vacant-install and occupied-passthrough
    // paths). A trailing `.len()` read on the same chain shape
    // confirms read-side method dispatch through the slot.
    let out = run_program(
        r#"
fn main() {
    let mut bucket: Map[i64, Vec[i64]] = Map.new();
    bucket.entry(1_i64).or_insert(Vec.new()).push(10_i64);
    bucket.entry(1_i64).or_insert(Vec.new()).push(20_i64);
    bucket.entry(1_i64).or_insert(Vec.new()).push(30_i64);
    bucket.entry(2_i64).or_insert(Vec.new()).push(99_i64);
    let n1 = bucket.entry(1_i64).or_insert(Vec.new()).len();
    let n2 = bucket.entry(2_i64).or_insert(Vec.new()).len();
    println(n1);
    println(n2);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "1"]);
    }
}

#[test]
fn test_e2e_map_entry_or_insert_deref_compound_assign_counter() {
    // Tier D flagship: `*m.entry(k).or_insert(0) += 1` — the entry chain
    // lowers to a slot pointer; the compound-assign loads / adds / stores
    // back through it. `get_or` reads the result. A=3, B=2, C=1.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let words = ["a", "b", "a", "c", "a", "b"];
    for w in words {
        *m.entry(w.to_string()).or_insert(0_i64) += 1;
    }
    println(m.get_or("a".to_string(), 0_i64));
    println(m.get_or("b".to_string(), 0_i64));
    println(m.get_or("c".to_string(), 0_i64));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "2", "1"]);
    }
}

#[test]
fn test_e2e_map_entry_or_insert_two_step_mut_ref() {
    // Two-step counter: bind the `mut ref V` to a local, then write through
    // it. `*r += 1` and the deref-elided `r += 1` both store back to the
    // slot; `*r` reads it. 10 → 11 → 12.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let r = m.entry("a".to_string()).or_insert(10_i64);
    *r += 1;
    r += 1;
    println(*r);
    println(m.get_or("a".to_string(), 0_i64));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["12", "12"]);
    }
}

#[test]
fn test_e2e_map_get_or_hit_and_default() {
    // `Map.get_or(k, default)` codegen: returns the stored value on a hit
    // and the default on a miss.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("x".to_string(), 7_i64);
    println(m.get_or("x".to_string(), -1_i64));
    println(m.get_or("y".to_string(), -1_i64));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["7", "-1"]);
    }
}

#[test]
fn test_e2e_map_get_remove_with_vec_value_payload() {
    // Bisected from the LeetCode 3629 codegen-vs-interpreter divergence.
    // Pins `Map.get` and `Map.remove` returning `Option[Vec[i64]]`:
    // the `Some(v) => v.len()` arm must reconstruct the full Vec
    // (3 LLVM words: ptr, len, cap) from the Option's payload fields.
    // Before the per-payload-word fix, `coerce_to_i64` truncated the
    // Vec to a single word and the destructure read undef for fields
    // 2 and 3 — producing garbage `len()` reads and reordered output.
    // After the fix, both paths round-trip the full payload.
    let out = run_program(
        r#"
fn main() {
    let mut bucket: Map[i64, Vec[i64]] = Map.new();
    bucket.entry(2_i64).or_insert(Vec.new()).push(10_i64);
    bucket.entry(2_i64).or_insert(Vec.new()).push(20_i64);
    bucket.entry(2_i64).or_insert(Vec.new()).push(30_i64);
    bucket.entry(3_i64).or_insert(Vec.new()).push(99_i64);

    match bucket.get(2_i64) {
        Some(v) => { println(v.len()); },
        None    => { println(-1_i64); },
    }
    match bucket.get(3_i64) {
        Some(v) => { println(v.len()); },
        None    => { println(-1_i64); },
    }

    match bucket.remove(2_i64) {
        Some(indices) => {
            for j in indices.into_iter() {
                println(j);
            }
        },
        None => { println(-1_i64); },
    }

    println(bucket.len());

    match bucket.remove(2_i64) {
        Some(_) => { println(99_i64); },
        None    => { println(0_i64); },
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "1", "10", "20", "30", "1", "0"]);
    }
}

#[test]
fn test_e2e_map_clone_preserves_entry() {
    // Cloned Map carries the source's single entry; lookup on the clone
    // resolves to the cloned value.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(7_i64, 42_i64);
    let n: Map[i64, i64] = m.clone();
    let v = n.get(7_i64);
    match v {
        Some(x) => println(x),
        None => println(0_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_map_clone_independent_after_source_insert() {
    // Inserting into the source after cloning doesn't affect the clone
    // — independent map handles, separate bucket arrays.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
    let n: Map[i64, i64] = m.clone();
    m.insert(2_i64, 200_i64);
    println(m.len());
    println(n.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "1"]);
    }
}

#[test]
fn test_e2e_set_clone_preserves_membership() {
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(5_i64);
    let t: Set[i64] = s.clone();
    println(t.contains(5_i64));
    println(t.contains(99_i64));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["true", "false"]);
    }
}

#[test]
fn test_e2e_set_clear() {
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(1_i64);
    s.insert(2_i64);
    s.insert(3_i64);
    println(s.len());
    s.clear();
    println(s.len());
    println(s.is_empty());
    s.insert(99_i64);
    println(s.contains(99_i64));
    println(s.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "0", "true", "true", "1"]);
    }
}

#[test]
fn test_e2e_set_difference_independent_after_source_mutation() {
    // The result set owns its keys — mutating `a` after the difference
    // doesn't reach back into `d`. Membership snapshot is preserved.
    let out = run_program(
        r#"
fn main() {
    let mut a: Set[i64] = Set.new();
    a.insert(1_i64);
    a.insert(2_i64);
    a.insert(3_i64);
    let mut b: Set[i64] = Set.new();
    b.insert(2_i64);
    let d: Set[i64] = a.difference(b);
    a.insert(99_i64);
    a.remove(1_i64);
    println(d.len());
    println(d.contains(1_i64));
    println(d.contains(3_i64));
    println(d.contains(99_i64));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "true", "true", "false"]);
    }
}

/// Map+VecDeque co-existence regression (2026-05-16).
///
/// `let m: Map[i64, i64] = Map.new()` followed by
/// `let q: VecDeque[i64] = VecDeque.new()` (or the reverse) used to
/// corrupt each other on `e4ca725`:
///
/// 1. `llvm_type_for_type_expr` lowered `VecDeque[T]` to `i64` (only
///    `Vec` had a fast-path; the baked `struct VecDeque[T] {}` shape
///    is empty and never reaches `struct_types` from codegen's
///    side). The auto-par escape-slot return struct then sized
///    `q`'s slot at 8 bytes — but the branch fn stored the real
///    24-byte `{ptr, len, cap}` aggregate, overflowing 16 bytes
///    into the adjacent `m` alloca. Symptom: `q.len()` returned a
///    pointer-sized integer; `q.pop_front()` either looped forever
///    on the trashed `len` or read garbage.
/// 2. After (1) was fixed, `q.push_back(x)` was still raced against
///    sibling `q.len()` / `q.pop_front()` reads inside a second
///    auto-par group, because the analyzer's
///    `method_effects_imply_receiver_mutation` lookup found no
///    non-pure verb seeded for `push_back` / `pop_*` (the
///    `VecDeque.*` keys weren't in `inferred_effects` and the
///    bare-method-name `STDLIB_METHOD_MAP` had no `push_back` /
///    `pop_front` / `push_front` / `pop_back` entries). The captured
///    `q` was bit-copied into the branch env, so each branch saw
///    the pre-spawn snapshot. Symptom: `q.len()` printed 0 even
///    though `q.push_back(42)` ran first in source order.
///
/// This test exercises both orderings of Map / VecDeque
/// construction plus a push_back → len → pop_front trailing
/// sequence on each. Compiles through the full pipeline
/// (concurrency_analyze included) so the auto-par dispatch runs
/// for real.
#[test]
fn test_e2e_map_and_vec_deque_coexistence_no_corruption() {
    use karac::codegen::{compile_to_object_with_options, link_executable};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn run(src: &str, id: u64) -> Option<String> {
        let mut parsed = karac::parse(src);
        if !parsed.errors.is_empty() {
            panic!("parse errors: {:?}", parsed.errors);
        }
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);

        let obj_path = format!("/tmp/karac_e2e_mapvd_{}_{}.o", std::process::id(), id);
        let exe_path = format!("/tmp/karac_e2e_mapvd_{}_{}", std::process::id(), id);

        if let Err(e) = compile_to_object_with_options(
            &parsed.program,
            &obj_path,
            None,
            Some(&analysis),
            None,
            None,
        ) {
            panic!("codegen failed: {e}");
        }
        // Link or exec failure → soft skip (matches the rest of the
        // codegen E2E suite — runtime archive may be missing in CI).
        super::common::link_or_skip(link_executable(&obj_path, &exe_path))?;
        let output = output_with_hang_watchdog(std::process::Command::new(&exe_path))?;
        let _ = std::fs::remove_file(&obj_path);
        let _ = std::fs::remove_file(&exe_path);
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    }

    // Order 1: Map declared first, then VecDeque. Interleave the
    // insert / push_back, then read q.len(), then pop_front. Pre-fix
    // this hung (corrupted len overflowed into the loop count of
    // memmove) or printed garbage; post-fix prints `1\n42`.
    let id1 = COUNTER.fetch_add(1, Ordering::Relaxed);
    let src1 = r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    let mut q: VecDeque[i64] = VecDeque.new();
    let _ = m.insert(1, 100);
    q.push_back(42);
    println(q.len());
    if let Some(v) = q.pop_front() {
        println(v);
    } else {
        println(-1);
    }
}
"#;
    if let Some(out) = run(src1, id1) {
        assert_eq!(
            out, "1\n42\n",
            "Map + VecDeque co-exist (Map-first): len + pop_front must agree with push_back"
        );
    }

    // Order 2: VecDeque ops BEFORE Map.insert — same end state.
    // Pre-fix this printed `4378427392` (a pointer-sized integer
    // — `q4`'s slot read off the end of the alloca because the
    // par-group return struct under-sized `q`'s field).
    let id2 = COUNTER.fetch_add(1, Ordering::Relaxed);
    let src2 = r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    let mut q: VecDeque[i64] = VecDeque.new();
    q.push_back(42);
    let _ = m.insert(1, 100);
    println(q.len());
}
"#;
    if let Some(out) = run(src2, id2) {
        assert_eq!(
            out, "1\n",
            "Map + VecDeque co-exist (VecDeque-first): len must reflect the push_back"
        );
    }

    // Order 3: no .len() read — exercises `pop_front` as the
    // sole post-mutation read so the regression is detected even
    // when the program never asks for the explicit length. Pre-fix
    // this hung inside `memmove` (the trashed len was on the order
    // of 2^32, so the tail-shift's byte count was billions). Post-
    // fix prints `42`.
    let id3 = COUNTER.fetch_add(1, Ordering::Relaxed);
    let src3 = r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    let mut q: VecDeque[i64] = VecDeque.new();
    let _ = m.insert(1, 100);
    q.push_back(42);
    if let Some(v) = q.pop_front() {
        println(v);
    } else {
        println(-1);
    }
}
"#;
    if let Some(out) = run(src3, id3) {
        assert_eq!(
            out, "42\n",
            "Map + VecDeque co-exist (no len read): pop_front must return the pushed value"
        );
    }
}

// ── Map.new() / Set.new() as module-binding initialisers
//    (phase-8-stdlib-floor.md "Map.new() / Set.new() as module-binding
//    initialisers"). Unlike Vec.new(), the empty value is NOT a
//    zero-shaped constant — `karac_map_new` installs hash seeds + a
//    vtable — so codegen emits a placeholder `null` ptr global and fills
//    it from a `__karac_static_init` prologue that runs before main's
//    body. Each test asserts the typecheck gate is CLEAN (run_program
//    ignores typecheck errors, so runtime output alone passes vacuously)
//    AND the binary produces the right values. ──

#[test]
fn test_e2e_modbind_map_new_insert_and_get() {
    // Module-scope `Map.new()`: insert in one fn, read in another —
    // the handle lives in a global filled by the static-init prologue,
    // so the write is observable across calls.
    let src = "let mut REGISTRY: Map[String, i64] = Map.new();\n\
                   fn put() { REGISTRY.insert(\"answer\", 42); }\n\
                   fn main() {\n\
                       put();\n\
                       println(REGISTRY.get(\"answer\").unwrap_or(0));\n\
                       println(REGISTRY.get(\"missing\").unwrap_or(-1));\n\
                   }";
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "module-scope Map.new() must typecheck clean, got: {:?}",
        typed.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
    let output = run_program(src).expect("compile + run failed");
    assert_eq!(output, "42\n-1\n");
}

#[test]
fn test_e2e_modbind_set_new_insert_and_contains() {
    // Module-scope `Set.new()`: insert in one fn, query membership in
    // main. Set reuses `karac_map_new` with val_size = 0.
    let src = "let mut SEEN: Set[i64] = Set.new();\n\
                   fn mark() { SEEN.insert(7); }\n\
                   fn main() {\n\
                       mark();\n\
                       SEEN.insert(9);\n\
                       println(SEEN.contains(7));\n\
                       println(SEEN.contains(9));\n\
                       println(SEEN.contains(3));\n\
                   }";
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "module-scope Set.new() must typecheck clean, got: {:?}",
        typed.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
    let output = run_program(src).expect("compile + run failed");
    assert_eq!(output, "true\ntrue\nfalse\n");
}

#[test]
fn test_e2e_reassign_heap_field_and_map_var() {
    // B-2026-07-15-25: reassigning a struct's heap field now drops the old
    // value before overwriting (no leak) and suppresses a moved-binding
    // source (no double-free); Map/Set VARIABLE reassignment frees the old
    // handle and suppresses the moved source too. This output test guards the
    // observable result across a moved-binding field RHS (`h.v = v2`), a
    // fresh field RHS (`h.v = […]`), a String field reassign, and a Map var
    // reassign; the sibling LSan test guards the memory safety.
    let output = run_program(
        "struct H { v: Vec[i64] }\n\
             struct S { s: String }\n\
             fn main() {\n\
                 let mut h = H { v: [1, 2] };\n\
                 let mut v2: Vec[i64] = Vec.new();\n\
                 v2.push(5); v2.push(6); v2.push(7);\n\
                 h.v = v2;\n\
                 println(h.v.len());\n\
                 h.v = [9, 8, 7, 6];\n\
                 println(h.v.len());\n\
                 let mut sh = S { s: (11).to_string() };\n\
                 sh.s = (222222).to_string();\n\
                 println(sh.s);\n\
                 let mut m: Map[i64, i64] = Map.new();\n\
                 m.insert(1, 10);\n\
                 let mut m2: Map[i64, i64] = Map.new();\n\
                 m2.insert(2, 20); m2.insert(3, 30);\n\
                 m = m2;\n\
                 println(m.len());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "3\n4\n222222\n2\n");
}

#[test]
fn test_e2e_field_map_get_unwrap_heap_value() {
    // B-2026-07-16-1: `<struct-field-map>.get(k).unwrap()` of a heap value
    // (`Map[_, String]` / `Map[_, Vec]` FIELD) double-freed the value buffer —
    // both inline (`println(h.ms.get(1).unwrap())`) AND bound
    // (`let v = h.ms.get(1).unwrap()`) — against the struct's scope-exit map
    // drop. The get/unwrap borrow-elision detectors (B-2026-07-14-15 bound,
    // B-2026-07-15-26 inline cap-zero) only recognised a bare-IDENTIFIER map
    // receiver; a field-access receiver (`h.ms`, `self.ms`) fell through, so
    // the unwrapped value kept its real `cap` and its consumer's free-guard
    // freed the map's stored buffer. Fixed by resolving the field-access map's
    // value type from the struct field (with the owning struct's mono subst)
    // in the shared `map_receiver_value_type_expr`, so the unwrap zeroes the
    // borrow view's `cap` uniformly. Covers a `String` value across an inline
    // println / a `[i].method()` receiver / a fn arg / a bound read, a `Vec`
    // value indexed inline (`h.mv.get(1).unwrap()[i]`) and bound, and a
    // `self.field` receiver via a method. Sibling LSan test guards the memory
    // safety.
    let output = run_program(
        "struct Holder { tag: i64, ms: Map[i64, String], mv: Map[i64, Vec[i64]] }\n\
             impl Holder {\n\
                 fn probe(ref self) -> i64 {\n\
                     let a = self.ms.get(1).unwrap().len();\n\
                     let b = self.mv.get(1).unwrap()[0];\n\
                     return a + b;\n\
                 }\n\
             }\n\
             fn takes(s: String) -> i64 { s.len() }\n\
             fn main() {\n\
                 let mut ms: Map[i64, String] = Map.new();\n\
                 ms.insert(1, \"hello\");\n\
                 ms.insert(2, \"worldwide\");\n\
                 let mut mv: Map[i64, Vec[i64]] = Map.new();\n\
                 mv.insert(1, [10, 20, 30]);\n\
                 let h = Holder { tag: 7, ms: ms, mv: mv };\n\
                 println(h.ms.get(1).unwrap());\n\
                 println(h.ms.get(2).unwrap().len());\n\
                 println(takes(h.ms.get(1).unwrap()));\n\
                 let v = h.ms.get(2).unwrap();\n\
                 println(v);\n\
                 println(h.mv.get(1).unwrap()[1]);\n\
                 let row = h.mv.get(1).unwrap();\n\
                 println(row[2]);\n\
                 println(h.probe());\n\
                 println(h.ms.get(1).unwrap());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "hello\n9\n5\nworldwide\n20\n30\n15\nhello\n");
}

#[test]
fn test_e2e_map_get_unwrap_struct_heap_value() {
    // B-2026-07-20-7: `let a = m.get(k).unwrap()` where the map VALUE is a
    // user STRUCT that owns heap (a `String`/`Vec` field). Unlike the bare
    // `String`/`Vec` value (B-2026-07-14-15), a struct value fell through the
    // borrow-elision detector, so the shallow struct copy's scope-exit
    // struct-drop AND the map's own value-drop freed the inner buffer twice
    // → `free(): double free detected` (SIGABRT) under JIT/AOT while interp
    // was correct. Now `is_nonshared_struct_value_map_get_unwrap` marks the
    // binding borrow-elided (map stays sole owner). Covers a String-field
    // struct (i64 and String keys), a two-field struct (String + i64), and
    // two reads from one map. Sibling LSan test guards the memory safety.
    let output = run_program(
        "struct Acct { name: String, txns: i64 }\n\
             fn main() {\n\
                 let mut m: Map[String, Acct] = Map.new();\n\
                 m.insert(\"a\".to_string(), Acct { name: \"alice\".to_string(), txns: 2 });\n\
                 m.insert(\"b\".to_string(), Acct { name: \"bob\".to_string(), txns: 1 });\n\
                 let a = m.get(\"a\").unwrap();\n\
                 let b = m.get(\"b\").unwrap();\n\
                 println(f\"{a.name}:{a.txns},{b.name}:{b.txns}\");\n\
                 let mut mi: Map[i64, Acct] = Map.new();\n\
                 mi.insert(7, Acct { name: \"carol\".to_string(), txns: 9 });\n\
                 let c = mi.get(7).unwrap();\n\
                 println(f\"{c.name}:{c.txns}\");\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "alice:2,bob:1\ncarol:9\n");
}

#[test]
fn test_e2e_discarded_map_remove_shared_value() {
    // B-2026-07-19-16 output leg: discarded / bound `Map.remove` over a
    // `Map[K, shared V]` — the discarded form now releases the moved-out
    // value's ref at the `;` (`try_track_discarded_shared_option`), the
    // unannotated bound form registers its scope-exit release (case b2),
    // and a displacing insert / absent-key remove stay balanced. The
    // sibling LSan test guards the leak/double-free halves; this pins the
    // observable behavior (len bookkeeping + bound payload reads) across
    // backends.
    let output = run_program(
        "shared struct DNode { key: i64, val: i64 }\n\
             fn main() {\n\
                 let mut m: Map[i64, DNode] = Map.new();\n\
                 m.insert(1, DNode { key: 1, val: 10 });\n\
                 m.insert(1, DNode { key: 1, val: 11 });\n\
                 m.insert(2, DNode { key: 2, val: 20 });\n\
                 m.remove(1);\n\
                 m.remove(99);\n\
                 println(m.len());\n\
                 let removed = m.remove(2);\n\
                 match removed {\n\
                     Some(n) => println(n.val),\n\
                     None => println(-1),\n\
                 }\n\
                 println(m.len());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "1\n20\n0\n");
}

/// B-2026-09-15-25 — `VecDeque[e1, …]` and `SortedMap[k: v, …]` are two of
/// the five names design.md § Collection Literals says the prefix form
/// supports; neither parsed until this row, so no backend had ever seen
/// one. The moment the parser could produce them, the `VecDeque` spelling
/// was born RUN-VS-BUILD SPLIT: `--interp` ran it, and codegen met it with
/// `no handler for expression kind PrefixCollectionLiteral`, because
/// `compile_vec_prefix_literal` is reached from a `type_name == "Vec"`
/// dispatch and lowering only renamed the `ArrayLiteral` spelling.
///
/// The `SortedMap` half compiled from the start (it lowers to
/// `ExprKind::MapLiteral`, and sortedness is a per-BINDING marker the
/// annotation and the recorded type both feed), which is exactly why it
/// belongs in the same fixture: the two halves of one syntax took
/// different paths to the backend and only one of them arrived.
///
/// The ordering assertion is `SortedMap`'s alone. Its walk is in KEY
/// order, seed-independent and identical on every backend (CLAUDE.md
/// § `Map` / `Set` iteration order) — the source writes its keys out of
/// order here, so insertion order and key order disagree and only a real
/// ordered map prints them sorted.
#[test]
fn test_e2e_vecdeque_and_sortedmap_prefix_literals() {
    let out = run_program_capturing(
        r#"
fn main() {
    let m = SortedMap["c": 3, "a": 1, "b": 2];
    for (k, v) in m { print(k); print(v); }
    println(m);
    let ann: SortedMap[String, i64] = SortedMap["z": 26, "y": 25];
    for (k, v) in ann { print(k); }
    let empty: SortedMap[String, i64] = SortedMap[];
    println(empty.len());
    let d = VecDeque[1, 2, 3];
    println(d.len());
    let mut dq = VecDeque[4, 5];
    dq.push_front(3);
    for x in dq { print(x); }
    println("");
    let ed: VecDeque[i64] = VecDeque[];
    println(ed.len());
}
"#,
    );
    if let Some(c) = out {
        // Byte-identical to what `karac run --interp` prints for the same
        // source — that equality is the property under test, not the
        // string itself.
        assert_eq!(
            c.stdout, "a1b2c3SortedMap{a: 1, b: 2, c: 3}\nyz0\n3\n345\n0\n",
            "compiled output diverged from the interpreter"
        );
    }
}

#[test]
fn test_e2e_vec_sorted_immutable_returns_new_vec() {
    // B-2026-07-19-15 — `Vec[T].sorted()` returns a NEW sorted Vec and
    // leaves the receiver UNSORTED. Desugars to `{ let mut tmp = v.clone();
    // tmp.sort(); tmp }`, so every element-type comparator the in-place
    // `sort` supports applies: signed int, unsigned int (unsigned order —
    // high-bit value sorts last, not first), String (byte-lexicographic),
    // and float. Byte-identical to the interpreter.
    //
    // B-2026-08-12-9: the FLOAT leg now goes through a generic. The direct
    // `Vec[f64].sorted()` is a type error (B-2026-08-11-7's total-order
    // gate), which had this whole test grandfathered past the check gate
    // as pinning "an emitter production can never run". That premise was
    // wrong: the gate reads the element type at the CALL SITE, so a
    // generic instantiated at `T = f64` passes it and monomorphizes
    // straight to the float comparator. Written this way the program
    // passes `karac check`, the test comes off the grandfather list, and
    // it pins a path a user can actually reach.
    let out = run_program(
        r#"
fn gsorted[T](v: ref Vec[T]) -> Vec[T] { v.sorted() }
fn main() {
    let v: Vec[i64] = [3, 1, 2, 5, 4];
    let s: Vec[i64] = v.sorted();
    println(s.get(0)); println(s.get(4));
    // receiver stays unsorted
    println(v.get(0));
    let u: Vec[u64] = [1u64 << 63, 5u64, 1u64 << 62];
    let us: Vec[u64] = u.sorted();
    println(f"{us[0]}");
    let w: Vec[String] = ["banana", "apple", "cherry"];
    let ws: Vec[String] = w.sorted();
    println(ws.get(0));
    println(w.get(0));
    let f: Vec[f64] = [2.5, 1.1, 3.3];
    let fs: Vec[f64] = gsorted(f);
    println(fs.get(0));
    // NaN must sort LAST and must not derange the rest — the comparator was
    // `OLT`/`OGT`, which answers "equal" for every NaN comparison and so is
    // intransitive; `[2.0, NaN, 1.0]` came back untouched under codegen while
    // the interpreter sorted it correctly.
    let n: Vec[f64] = [2.0, 0.0 / 0.0, 1.0];
    let ns: Vec[f64] = gsorted(n);
    println(f"{ns[0]} {ns[1]} {ns[2]}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "Some(1)\nSome(5)\nSome(3)\n5\nSome(apple)\nSome(banana)\nSome(1.1)\n\
                 1 2 NaN\n"
        );
    }
}

#[test]
fn test_e2e_vec_sorted_by_immutable_comparator_sort() {
    // B-2026-07-20-8 — `Vec[T].sorted_by(cmp: Fn(T,T)->Ordering)` returns a
    // NEW Vec sorted by the user comparator, leaving the receiver unsorted.
    // Desugars to `{ let mut tmp = v.clone(); tmp.sort_by(cmp); tmp }` (the
    // comparator sibling of `sorted`), so both `sort_by` paths — the
    // capture-free inline-closure mono fast path and the runtime callback
    // thunk — apply to the clone. Covers a struct-field comparator, a
    // DESCENDING scalar comparator, receiver immutability, heap `String`
    // elements, and N=100 (crossing the mono/runtime N=64 threshold).
    // Byte-identical to the interpreter. (`String.sorted_by` stays a LOUD
    // interp-only bail — the runtime char-sort has no comparator variant.)
    let out = run_program(
        r#"
struct P { id: i64, w: i64 }
fn main() {
    let mut v: Vec[P] = Vec.new();
    v.push(P { id: 3, w: 30 });
    v.push(P { id: 1, w: 10 });
    v.push(P { id: 2, w: 20 });
    let s = v.sorted_by(|a, b| a.w.cmp(b.w));
    for p in s { println(p.id); }
    let first = v.get(0).unwrap();
    println(f"{first.id}:{first.w}");
    let n = [4, 9, 1, 7];
    let d = n.sorted_by(|a, b| b.cmp(a));
    println(f"{d[0]}:{d[3]}:{n[0]}");
    let w = ["pear".to_string(), "fig".to_string(), "apple".to_string()];
    let ws = w.sorted_by(|a, b| a.cmp(b));
    println(ws.get(0).unwrap());
    println(w.get(0).unwrap());
    let mut big: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 100 { big.push((i * 37) % 100); i = i + 1; }
    let bs = big.sorted_by(|a, b| a.cmp(b));
    println(f"{bs.get(0).unwrap()}:{bs.get(99).unwrap()}:{bs.len()}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1\n2\n3\n3:30\n9:1:4\napple\npear\n0:99:100\n");
    }
}

#[test]
fn test_e2e_vec_sorted_by_key_immutable_key_sort() {
    // B-2026-08-11-23 — `sorted_by_key` was the ONLY hole in the
    // six-method sort family: `sort`, `sort_by`, `sort_by_key`, `sorted`
    // and `sorted_by` all compiled, and this one passed `karac check` and
    // then died at build with "not yet supported in codegen". That shape
    // is worse than a plain missing method for the Mend loop, where
    // `check` is the primary tool — a program that checks clean and fails
    // at build leaves the repair loop nothing to act on.
    //
    // It now shares an arm with `sorted_by`: same desugar, different inner
    // method (`{ let mut tmp = v.clone(); tmp.sort_by_key(f); tmp }`), so
    // everything the in-place `sort_by_key` lowering supports carries over
    // through the clone. Covered here:
    //  * the immutable contract — the RECEIVER must be left unsorted;
    //  * a key that is not the element (`|p| p.age`), which is the whole
    //    point of `by_key` and the thing a `sorted`/`sorted_by` test
    //    cannot exercise;
    //  * a FLOAT key, which routes through `karac_float_cmp` — NaN must
    //    sort last (B-2026-08-11-17), proving that dispatch survives the
    //    clone desugar;
    //  * a String key on a heap-bearing element, whose clone-and-drop is
    //    ASAN-covered by `asan_vec_sorted_by_key_heap_elements`.
    let out = run_program(
        r#"
struct P { name: String, age: i64 }
fn main() {
    let v: Vec[i64] = Vec.new();
    let mut v: Vec[i64] = v;
    v.push(3); v.push(1); v.push(2);
    let s = v.sorted_by_key(|x| x);
    for x in v.iter() { println(x); }
    for x in s.iter() { println(x); }
    let mut ps: Vec[P] = Vec.new();
    ps.push(P { name: "c padded beyond the small-string threshold", age: 30 });
    ps.push(P { name: "a padded beyond the small-string threshold", age: 10 });
    ps.push(P { name: "b padded beyond the small-string threshold", age: 20 });
    let byage = ps.sorted_by_key(|p| p.age);
    for p in byage.iter() { println(p.age); }
    let mut fs: Vec[f64] = Vec.new();
    fs.push(2.5); fs.push(0.0 / 0.0); fs.push(1.5);
    let fsorted = fs.sorted_by_key(|x| x);
    for x in fsorted.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec![
                // receiver untouched, then the sorted copy
                "3", "1", "2", "1", "2", "3", // by a non-element key
                "10", "20", "30", // float key: NaN last
                "1.5", "2.5", "NaN",
            ]
        );
    }
}

/// Sortedness at the same boundaries, including the degenerate lengths
/// the merge sort short-circuits (0 and 1 never allocate scratch).
#[test]
fn test_e2e_mono_sort_by_sorted_at_every_boundary() {
    let out = run_program(
        r#"
fn inversions(n: i64) -> i64 {
    let mut v: Vec[i64] = Vec.new();
    let mut i: i64 = 0;
    while i < n {
        v.push((i * 7919) % 1000);
        i = i + 1;
    }
    v.sort_by(|a, b| a.cmp(b));
    let mut bad: i64 = 0;
    let mut j: i64 = 1;
    while j < v.len() {
        if v[j - 1] > v[j] { bad = bad + 1; }
        j = j + 1;
    }
    bad
}

fn main() {
    println(inversions(0));
    println(inversions(1));
    println(inversions(2));
    println(inversions(33));
    println(inversions(65));
    println(inversions(2000));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "0", "0", "0", "0", "0"]);
    }
}

#[test]
fn e2e_map_and_set_honour_a_hand_written_hash_and_eq() {
    // B-2026-08-26-10's remaining half. A key type carrying its own
    // `impl Hash` was hashed STRUCTURALLY, so an impl that deliberately
    // ignored a field still split two keys equal under it into different
    // buckets — and with no derive present the key was refused outright.
    //
    // Both impls here key on `id` ALONE while `tag` varies, so every
    // assertion below fails under structural hashing or structural equality:
    // the two `id: 1` inserts would stay separate, the `get` with an unseen
    // tag would miss, and the `Set` would hold two elements. That is the
    // point of choosing a comparator that disagrees with the derived one.
    let out = run_program(
        r#"
struct Item { id: i64, tag: i64 }
impl PartialEq for Item { fn eq(ref self, other: ref Item) -> bool { self.id == other.id } }
impl Eq for Item {}
impl Hash for Item { fn hash[H: Hasher](ref self, hasher: mut ref H) { hasher.write_i64(self.id) } }
fn main() {
    let mut m: Map[Item, i64] = Map.new();
    m.insert(Item { id: 1, tag: 100 }, 10);
    m.insert(Item { id: 2, tag: 200 }, 20);
    m.insert(Item { id: 1, tag: 999 }, 11);
    println(m.len());
    match m.get(Item { id: 1, tag: 0 }) { Some(v) => println(v), None => println(-1) }
    println(m.contains_key(Item { id: 2, tag: 7 }));
    m.remove(Item { id: 2, tag: 12345 });
    println(m.len());
    let mut s: Set[Item] = Set.new();
    s.insert(Item { id: 5, tag: 1 });
    s.insert(Item { id: 5, tag: 2 });
    println(s.len());
    println(s.contains(Item { id: 5, tag: 3 }));
}
"#,
    );
    let out = out.expect("a Map keyed by a hand-written Hash + Eq must build");
    assert_eq!(
        out.trim().lines().collect::<Vec<_>>(),
        vec![
            "2",    // the second `id: 1` insert OVERWROTE the first
            "11",   // ...with the newer value, found by a tag never inserted
            "true", // contains_key ignores tag too
            "1",    // remove found the entry by id alone
            "1",    // Set deduped two elements differing only in tag
            "true", // and contains agrees
        ]
    );
}

#[test]
fn test_e2e_ewmap_trait_bound_map_zip() {
    // S6c: `map` / `zip_with` on the `ElementwiseMap` trait surface, through
    // a bound-generic `fn f[C: ElementwiseMap[i64]]`. Both return `Self = C`
    // (a fresh container) — the codegen enabler is `augment_subst_from_
    // handle_params`: binding the handle type param `C` to `ptr` in the mono
    // subst so the mono's RETURN type lowers to the container pointer, not
    // the `i64` default (else "Function return type does not match operand
    // type of return inst"). The mono handle param still registers as a
    // Column/Tensor, so `c.map(...)` / `a.zip_with(b, ...)` reach the same
    // inline-closure kernels the concrete surface uses. Covers map on both
    // containers + zip_with on Column, result bound and reduced.
    let src = r#"
fn doubled[C: ElementwiseMap[i64]](c: ref C) -> C {
    c.map(|x| x * 2)
}
fn combine[C: ElementwiseMap[i64]](a: ref C, b: ref C) -> C {
    a.zip_with(b, |x, y| x + y)
}
fn main() {
    let col: Column[i64] = Column.from_vec([1, 2, 3]);
    let t: Tensor[i64, [3]] = Tensor.from([10, 20, 5]);
    let a: Column[i64] = Column.from_vec([1, 2, 3]);
    let b: Column[i64] = Column.from_vec([10, 20, 30]);
    let dc: Column[i64] = doubled(col);
    let dt: Tensor[i64, [3]] = doubled(t);
    let z: Column[i64] = combine(a, b);
    println(f"{dc.sum()} {dt.sum()}");
    println(f"{z.sum()}");
}
"#;
    // doubled col = [2,4,6] → 12; doubled t = [20,40,10] → 70; combine → [11,22,33] → 66.
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "12 70\n66\n");
}

#[test]
fn e2e_tracing_log_set_exporter_noop_silences() {
    // Registering `NoOpExporter` routes compiled `Log.*` to its empty
    // `export_event` (indirect dispatch through the runtime fn-ptr)
    // instead of stdout — so the program emits nothing.
    let out = run_program(
        r#"fn main() {
                Log.set_exporter(NoOpExporter {});
                Log.info("i");
                Log.error("e");
                println("done");
            }"#,
    );
    assert_eq!(out.as_deref(), Some("done\n"));
}

#[test]
fn e2e_tracing_log_reset_restores_default() {
    // `Log.reset()` clears the min level (and any sink), so a level
    // dropped before the reset emits to stdout again afterward.
    let out = run_program(
        r#"fn main() {
                Log.set_min_level("error");
                Log.info("dropped");
                Log.reset();
                Log.info("kept");
            }"#,
    );
    assert_eq!(out.as_deref(), Some("[info] kept\n"));
}

#[test]
fn test_e2e_chained_map_zip_reduce_parity() {
    // Fused chained `zip_with(...).sum()` AND `map(...).sum()` over exact
    // values — byte-identical to the interpreter twin (and to the
    // materialize path the fusion replaces). `dot` = 1·2+2·0+3·1+4·1 = 9;
    // `map(*0.5).sum()` = (2+4+6+8)·0.5 = 10.
    let out = run_program(
        "fn dot(a: ref Tensor[f32, [4]], b: ref Tensor[f32, [4]]) -> f32 \
             { a.zip_with(b, |x, y| x * y).sum() }\n\
             fn main() {\n\
                 let a: Tensor[f32, [4]] = Tensor.from([1.0, 2.0, 3.0, 4.0]);\n\
                 let b: Tensor[f32, [4]] = Tensor.from([2.0, 0.0, 1.0, 1.0]);\n\
                 println(dot(a, b));\n\
                 let s: Tensor[f32, [4]] = Tensor.from([2.0, 4.0, 6.0, 8.0]);\n\
                 println(s.map(|x| x * 0.5).sum());\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "9\n10\n",
            "fused chained map/zip reduce must match the interpreter twin",
        );
    }
}

#[test]
fn test_ir_fused_transcendental_map_reduce_vectorizes() {
    // The CHAINED fused form `t.map(|x| x.exp()).sum()` routes through
    // emit_fused_map_reduce; a transcendental in the body would
    // otherwise scalarize the exp call and block the reassoc auto-vec.
    // try_emit_vectorized_fused_map_reduce emits a <8 x float> vector-
    // accumulator loop instead (the polynomial + packed fadd).
    let ir = ir_for(
        "fn d(t: ref Tensor[f32, [64]]) -> f32 { t.map(|x| x.exp()).sum() }\n\
             fn main() { let t: Tensor[f32, [64]] = Tensor.ones([64]); println(d(t)); }\n",
    );
    assert!(
        ir.contains("<8 x float>"),
        "fused transcendental map-reduce must lift to a <8 x float> accumulator loop"
    );

    // A PURE-ARITHMETIC fused reduce (the embeddings dot shape) must
    // NOT hit the transcendental vectorizer — it stays on the scalar +
    // `reassoc` auto-vec path (fadd reassoc, no hand-emitted vector
    // accumulator from this pass).
    let ir_dot = ir_for(
        "fn d(a: ref Tensor[f32, [64]], b: ref Tensor[f32, [64]]) -> f32 \
             { a.zip_with(b, |x, y| x * y).sum() }\n\
             fn main() {\n\
                 let a: Tensor[f32, [64]] = Tensor.ones([64]);\n\
                 let b: Tensor[f32, [64]] = Tensor.ones([64]);\n\
                 println(d(a, b));\n\
             }\n",
    );
    assert!(
        ir_dot.contains("fadd reassoc"),
        "pure-arith fused dot must stay on the scalar+reassoc path; IR:\n{}",
        &ir_dot[..ir_dot.len().min(3000)]
    );
}

#[test]
fn test_e2e_fused_transcendental_map_reduce_accuracy() {
    // The vectorized fused map-reduce (`t.map(|x| x.exp()).sum()`) must
    // stay within the documented f32-polynomial tolerance of the true
    // sum. Self-checking so it holds on BOTH backends (interp: f64 libm
    // + ordered fold; AOT: SIMD polynomial + reassociated vector fold).
    // Length 20 = two full width-8 chunks + a 4-element tail.
    let out = run_program(
        "fn main() {\n\
                 let mut t: Tensor[f32, [20]] = Tensor.zeros(vec![20]);\n\
                 for i in 0..20 { t[i] = (i) as f32 * 0.1f32 - 1.0f32; }\n\
                 let got: f32 = t.map(|x| x.exp()).sum();\n\
                 let mut want: f32 = 0.0f32;\n\
                 for i in 0..20 { want = want + ((i) as f32 * 0.1f32 - 1.0f32).exp(); }\n\
                 let d: f32 = got - want;\n\
                 let ad: f32 = if d < 0.0f32 { 0.0f32 - d } else { d };\n\
                 println(ad < 0.01f32);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "true\n",
            "vectorized fused exp-sum must stay within 1e-2 of the reference on both backends",
        );
    }
}

#[test]
fn test_e2e_vec_map_param_deep_copy() {
    // `Vec[Map[i64, i64]]` owned param tail-returned: the defensive
    // copy must deep-clone each map handle, not alias it (Cluster 1).
    // Value-correctness companion to the ASAN double-free pin — the
    // cloned maps must carry the original entries.
    let out = run_program(
        r#"
fn id(v: Vec[Map[i64, i64]]) -> Vec[Map[i64, i64]] {
    v
}

fn main() {
    let mut v: Vec[Map[i64, i64]] = Vec.new();
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1i64, 10i64);
    m.insert(2i64, 20i64);
    v.push(m);
    let mut m2: Map[i64, i64] = Map.new();
    m2.insert(3i64, 30i64);
    v.push(m2);
    let r = id(v);
    println(r.len());
    match r[0].get(1i64) { Some(x) => println(x), None => println(-1i64) }
    match r[0].get(2i64) { Some(x) => println(x), None => println(-1i64) }
    match r[1].get(3i64) { Some(x) => println(x), None => println(-1i64) }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2\n10\n20\n30");
    }
}

#[test]
fn test_e2e_vec_map_returned_from_helper() {
    // The headline `Vec[Map]` ownership bug (Cluster 1): a Map pushed
    // into a Vec aliased a handle owned by the origin `m` binding, which
    // freed it at the helper's scope exit — so the returned Vec dangled
    // and the read in `main` saw a freed/reused handle (AOT printed
    // `-1`, interp `777`). The fix transfers ownership to the Vec at the
    // push and frees the handle at the Vec's drop. ASAN twin:
    // `memory_sanitizer.rs::asan_vec_map_returned_from_helper_no_uaf`.
    let out = run_program(
        r#"
fn make() -> Vec[Map[i64, i64]] {
    let mut v: Vec[Map[i64, i64]] = Vec.new();
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1i64, 777i64);
    v.push(m);
    v
}

fn main() {
    let v = make();
    match v[0].get(1i64) { Some(x) => println(x), None => println(-1i64) }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "777");
    }
}

#[test]
fn test_e2e_struct_field_tuple_and_map_drop_bodies() {
    // B-2026-08-02-18 — TUPLE-held and Map-VALUE-held Drop values in
    // struct fields: the owner-death bodies walk now has a tuple arm
    // (reusing the __karac_dropelems_tuple walker on the field slot) and
    // a Map/Set arm (reusing __karac_dropelems_map), on both backends.
    // The generic block additionally pins B-2026-08-02-19: the mono
    // parent's NestedTuple memory drop resolves `(T, i64)` through the
    // subst, so the tuple-held String is freed (the asan twin proves the
    // memory half; here the body line proves dispatch).
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct DuoR { pair: (Res, i64), tag: i64 }
struct Duo[T] { pair: (T, i64), tag: i64 }
struct MapHold { m: Map[i64, Res], tag: i64 }
fn main() {
    println("a");
    {
        let d = DuoR { pair: (Res { id: 1, name: f"aa{1}" }, 5), tag: 7 };
        println(d.tag);
    }
    println("b");
    {
        let g: Duo[Res] = Duo { pair: (Res { id: 2, name: f"bb{2}" }, 6), tag: 8 };
        println(g.tag);
    }
    println("c");
    {
        let mut h = MapHold { m: Map.new(), tag: 9 };
        h.m.insert(1i64, Res { id: 3, name: f"cc{3}" });
        println(h.tag);
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "a\n7\ndrop 1 aa1\nb\n8\ndrop 2 bb2\nc\n9\ndrop 3 cc3\nend"
        );
    }
}

#[test]
fn test_e2e_struct_field_set_and_sortedmap_bodies() {
    // B-2026-08-02-21 (output regression pin — the asan twin owns the
    // LSan pin, which is where the -21 defect was observable): a
    // Set[Res] field's element bodies and a SortedMap[i64, Res] field's
    // value bodies fire at owner death (the B-2026-08-02-18 table walk),
    // values in sorted key order. The -21 fix itself is memory-only:
    // pre-fix these bodies already fired while the SortedMap field's
    // whole handle tree and the Set elements' String buffers leaked
    // (Sorted heads missing from FieldDrop::MapOrSet; no per-key drop
    // channel in the field arm).
    let out = run_program(
        r#"
#[derive(Hash, Eq)]
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct SetHold { s: Set[Res], tag: i64 }
struct SmHold { m: SortedMap[i64, Res], tag: i64 }
fn main() {
    println("a");
    {
        let mut h = SetHold { s: Set.new(), tag: 1 };
        let _ = h.s.insert(Res { id: 3, name: f"s{3}" });
        println(h.tag);
    }
    println("mid");
    {
        let mut g = SmHold { m: SortedMap.new(), tag: 2 };
        let _ = g.m.insert(2, Res { id: 4, name: f"m{4}" });
        let _ = g.m.insert(1, Res { id: 5, name: f"n{5}" });
        println(g.tag);
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "a\n1\ndrop 3 s3\nmid\n2\ndrop 5 n5\ndrop 4 m4\nend"
        );
    }
}

#[test]
fn test_e2e_struct_field_and_map_value_tuple_run_element_drop() {
    // B-2026-08-03-7 — the two tuple positions B-2026-08-03-3 did not reach,
    // both silent on BOTH backends. A struct field holding a tuple and a Map
    // value holding a tuple each classified as body-free because their
    // selectors read only the tuple's element HEAD NAMES, so an inner
    // `Option[Res]` read as the body-free "Option" — even though the
    // emitters, once selected, already know how to descend. The struct-field
    // leg ALSO leaked the payload's heap: the `NestedTuple` field classifier
    // shares `type_expr_has_drop_heap`'s deliberate Option/Result blind spot,
    // so the field registered no memory drop either.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct W { p: (Option[Res], i64) }
fn main() {
    println("struct-field-tuple:");
    { let w = W { p: (Option.Some(Res { id: 1, name: f"a{1}" }), 10) }; println(w.p.1); }
    println("map-value-tuple:");
    {
        let mut m: Map[i64, (Option[Res], i64)] = Map.new();
        m.insert(5, (Option.Some(Res { id: 2, name: f"bb{2}" }), 20));
        println(m.len());
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "struct-field-tuple:\n10\ndrop 1 a1\n\
                 map-value-tuple:\n1\ndrop 2 bb2\nend"
        );
    }
}

#[test]
fn test_e2e_sorted_container_destroys_in_key_order() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq, Ord, PartialOrd)]
struct K { id: i64 }
struct V { id: i64 }
impl Drop for K { fn drop(mut ref self) { println(f"K{self.id}") } }
impl Drop for V { fn drop(mut ref self) { println(f"V{self.id}") } }
fn main() {
    { let mut m: SortedMap[K, V] = SortedMap.new();
      let mut i = 1;
      while i <= 4 { m.insert(K { id: 5 - i }, V { id: i }); i = i + 1; }
      println("--map--"); }
    { let mut s: SortedSet[K] = SortedSet.new();
      let mut j = 1;
      while j <= 4 { s.insert(K { id: 50 - j }); j = j + 1; }
      println("--set--"); }
}
"#
        ),
        Some(
            "K1\nK2\nK3\nK4\nV4\nV3\nV2\nV1\n--map--\n\
                 K46\nK47\nK48\nK49\n--set--\n"
                .to_string()
        )
    );
}

#[test]
fn test_e2e_user_impl_drop_on_a_map_key_fires() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
struct K { id: i64 }
struct V { id: i64 }
impl Drop for K { fn drop(mut ref self) { println(f"dropK {self.id}") } }
impl Drop for V { fn drop(mut ref self) { println(f"dropV {self.id}") } }
struct Holder { m: Map[K, V] }
fn main() {
    let mut a: Map[K, V] = Map.new();
    a.insert(K { id: 1 }, V { id: 1 });
    println("--before-clear--")
    a.clear();
    println("--after-clear--")
    { let mut b: Map[K, V] = Map.new();
      b.insert(K { id: 2 }, V { id: 2 });
      let h = Holder { m: b };
      println("--field-in-scope--"); }
    println("--field-gone--")
}
"#
        ),
        Some(
            "--before-clear--\ndropK 1\ndropV 1\n--after-clear--\n\
                 dropK 2\ndropV 2\n--field-in-scope--\n--field-gone--\n"
                .to_string()
        )
    );
}

/// B-2026-08-27-3 — the compiled twin of
/// `clearing_a_set_runs_its_elements_drop_body`, same bytes.
///
/// `Set.clear` is its own codegen arm, and it called plain
/// `karac_map_clear` — which zeroes the status bytes and does nothing
/// else. So it ran no element bodies (the gap B-2026-08-26-41 closed for
/// `Map.clear` but could not reach here) AND released no element memory
/// (B-2026-08-27-3 proper). The two are one arm's worth of missing work,
/// which is why they are fixed together: the leak is invisible to this
/// test and the missing body is invisible to a sanitizer, so neither
/// assertion alone would have found both.
///
/// The element walk comes from `emit_map_val_user_drop_bodies_fn`, NOT the
/// `_key_` twin, even though a Set's element lives in the key half: the
/// key-half entry point declines for `Set`/`SortedSet` on purpose, rather
/// than emit the same walk a second time under a second name. Reaching for
/// the name that matches the HALF is the natural mistake here and it
/// silently emits nothing.
#[test]
fn test_e2e_set_clear_runs_the_element_drop_body() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
struct E { n: i64 }
impl Drop for E { fn drop(mut ref self) { println(f"dropE {self.n}") } }
fn main() {
    let mut s: Set[E] = Set.new();
    s.insert(E { n: 1 });
    println("--before--");
    s.clear();
    println(f"--after len={s.len()}--");
}
"#
        ),
        Some("--before--\ndropE 1\n--after len=0--\n".to_string())
    );
}

/// The disarm half of B-2026-08-26-41, compiled. A bound-local key moved
/// into a map must run its body ONCE, at teardown, over live data — not at
/// the move site over a moved-from slot, which is what `karac build` did
/// (printing an empty field where `--interp` printed the value) and which
/// the walk alone would have turned into a double fire.
#[test]
fn test_e2e_moved_map_key_drop_body_fires_once() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
struct K { s: String }
impl Drop for K { fn drop(mut ref self) { println(f"dropK {self.s}") } }
fn main() {
    let mut m: Map[K, i64] = Map.new();
    let k = K { s: "kk" };
    m.insert(k, 1);
    println("--after-insert--")
    println(f"len={m.len()}")
}
"#
        ),
        Some("--after-insert--\nlen=1\ndropK kk\n".to_string())
    );
}

/// B-2026-08-18-26 — READ METHODS on a freshly returned `Set`/`Map`, which
/// silently produced garbage: `mk_set().len()` printed 0 for a three-element
/// set, `mk_map().len()` printed 152 for a two-entry map, `is_empty()`
/// printed raw bytes, and six such calls in one program SEGFAULTED. The
/// program ran and exited 0, which is what made it worse than a build error.
///
/// TWO INDEPENDENT DEFECTS, one per half of this test.
///
/// The VALUES were wrong because the typechecker's fresh-temp Map/Set
/// side-table roster listed `get`/`contains_key`/`iter`/`keys`/`values`/
/// `entries` and `contains`/`iter` — precisely the methods that answered
/// correctly — while `len` and `is_empty` were absent, so no entry was
/// recorded, codegen's fresh-temp path declined, and the receiver fell
/// through to a lowering that reads the HANDLE (a plain pointer) as an
/// inline aggregate.
///
/// The `B` COUNTER is the second defect and does not follow from the first:
/// once those two names were admitted, the helper still ran behind arms
/// that compile the receiver and fall through, so the producer was emitted
/// TWICE — `mk_set` ran twice per call under `karac build` against once
/// under `--interp`, and only the second handle was drop-tracked. Exactly
/// one `B` per call is therefore the load-bearing assertion here; the
/// companion ASAN fixture pins the freeing of the handle that stranded.
#[test]
fn freshtemp_mapset_read_methods_answer_and_evaluate_once() {
    let src = "fn mk_set(n: i64) -> Set[i64] {\n\
                       println(\"B\");\n\
                       let mut s: Set[i64] = Set.new();\n\
                       let mut i = 0;\n\
                       while i < n { s.insert(i); i = i + 1; }\n\
                       return s;\n\
                   }\n\
                   fn mk_map(n: i64) -> Map[String, i64] {\n\
                       let mut m: Map[String, i64] = Map.new();\n\
                       let mut i = 0;\n\
                       while i < n { m.insert(f\"k{i}\", i); i = i + 1; }\n\
                       return m;\n\
                   }\n\
                   fn main() {\n\
                       println(mk_set(3).len().to_string());\n\
                       println(mk_set(3).is_empty().to_string());\n\
                       println(mk_set(3).contains(2).to_string());\n\
                       println(mk_map(2).len().to_string());\n\
                       println(mk_map(2).contains_key(\"k0\").to_string());\n\
                   }\n";
    let Some(out) = run_program(src) else {
        return;
    };
    assert_eq!(
        out, "B\n3\nB\nfalse\nB\ntrue\n2\ntrue\n",
        "a read method on a fresh Map/Set temp must answer from the real handle \
             and evaluate its receiver exactly once"
    );
}

/// B-2026-08-18-12 — a BUILTIN method called on `self` inside an
/// `impl Trait for Map[K, V]` or `impl Trait for Set[T]` body.
///
/// The main Map/Set dispatch lives inside an `ExprKind::Identifier` arm,
/// so a `SelfValue` receiver never reached it and every builtin method on
/// such a body failed with "no handler for method 'len' on non-identifier
/// receiver" — while the identical body over a `Vec[i64]` or `Slice[i64]`
/// head compiled and ran. That asymmetry was not a decision: a
/// Vec/Slice/String receiver compiles to a STRUCT VALUE, which the
/// `len`/`is_empty`/`count` intercept reads the length field straight out
/// of, and a Map/Set handle is a bare pointer the same intercept declines.
/// So Vec looked handled and Map/Set looked unimplemented, when neither
/// had a `self` arm at all.
///
/// Both receiver modes and several methods, because one method passing
/// would not distinguish "the arm is there" from "this one method happens
/// to be intercepted somewhere else".
#[test]
fn a_builtin_method_on_a_map_or_set_self_receiver_compiles() {
    let map_setup = "let mut c: Map[String, i64] = Map.new();\n                          c.insert(\"a\", 5); c.insert(\"b\", 7);";
    let set_setup = "let mut c: Set[i64] = Set.new();\n c.insert(5); c.insert(7);";
    for (label, head, self_mode, body, setup, want) in [
        (
            "Map.len, ref self",
            "Map[String, i64]",
            "ref self",
            "return self.len().to_string();",
            map_setup,
            "2\n",
        ),
        (
            "Map.len, owned self",
            "Map[String, i64]",
            "self",
            "return self.len().to_string();",
            map_setup,
            "2\n",
        ),
        (
            "Map.contains_key",
            "Map[String, i64]",
            "ref self",
            "if self.contains_key(\"a\") { return \"yes\"; } return \"no\";",
            map_setup,
            "yes\n",
        ),
        (
            "Map.get",
            "Map[String, i64]",
            "ref self",
            "return self.get(\"a\").unwrap_or(0).to_string();",
            map_setup,
            "5\n",
        ),
        (
            "Set.len, ref self",
            "Set[i64]",
            "ref self",
            "return self.len().to_string();",
            set_setup,
            "2\n",
        ),
        (
            "Set.len, owned self",
            "Set[i64]",
            "self",
            "return self.len().to_string();",
            set_setup,
            "2\n",
        ),
        (
            "Set.contains",
            "Set[i64]",
            "ref self",
            "if self.contains(5) { return \"yes\"; } return \"no\";",
            set_setup,
            "yes\n",
        ),
        (
            "Set.is_empty",
            "Set[i64]",
            "ref self",
            "if self.is_empty() { return \"yes\"; } return \"no\";",
            set_setup,
            "no\n",
        ),
    ] {
        let src = format!(
            "trait P {{ fn p({self_mode}) -> String; }}\n\
                 impl P for {head} {{ fn p({self_mode}) -> String {{ {body} }} }}\n\
                 fn main() {{ {setup} println(c.p()); }}\n"
        );
        let Some(out) = run_program(&src) else {
            return;
        };
        assert_eq!(out, want, "{label}");
    }
}

/// The `Map` head, on both receiver modes. An args-less `Map` rejected a
/// String key outright as "index must be an integer or range".
///
/// The OWNED spelling is here for a second reason. It was briefly the one
/// shape this fix left out, because routing it did not fail — it
/// SEGFAULTED, the receiver's slot appearing not to hold the handle the
/// index path loads. That turned out to be a symptom of B-2026-08-18-11,
/// where the caller passed the ADDRESS of an owned pointer-shaped
/// receiver instead of its value, so the slot really did hold the wrong
/// thing. Both modes are pinned so neither half can regress alone.
#[test]
fn a_map_impl_head_can_read_by_its_own_key_type() {
    for self_mode in ["self", "ref self"] {
        let src = format!(
            "trait Get {{ fn get_a({self_mode}) -> i64; }}\n\
                 impl Get for Map[String, i64] {{\n\
                     fn get_a({self_mode}) -> i64 {{ return self[\"a\"]; }}\n\
                 }}\n\
                 fn main() {{\n\
                     let mut m: Map[String, i64] = Map.new();\n\
                     m.insert(\"a\", 42);\n\
                     println(m.get_a().to_string());\n\
                 }}\n"
        );
        let Some(out) = run_program(&src) else {
            return;
        };
        assert_eq!(out, "42\n", "`{self_mode}` receiver: wrong map read");
    }
}

/// B-2026-08-21-10 — `is_sorted()` end to end on all three sequence
/// receivers.
///
/// The `u64` cases are the ones with teeth. Codegen compares adjacent
/// pairs through the `karac_cmp_<T>` family rather than an open-coded
/// `<=`, precisely so an element past `i64::MAX` is read UNSIGNED — an
/// open-coded signed compare answers `false` for `[1, u64::MAX]`, which is
/// both wrong and a disagreement with `sort()`'s own output. Each receiver
/// reaches the comparator by a different route (`Vec` directly, `Slice`
/// through the header-into-Vec-view republish, `Array` through the
/// unrolled fixed-array arm), so all three are swept.
#[test]
fn test_e2e_is_sorted_across_receivers() {
    let src = r#"
fn main() {
    let a: Vec[i64] = [1, 2, 2, 5];
    println(a.is_sorted());
    let b: Vec[i64] = [1, 3, 2];
    println(b.is_sorted());
    let e: Vec[i64] = [];
    println(e.is_sorted());
    let one: Vec[i64] = [42];
    println(one.is_sorted());

    // An element past i64::MAX: unsigned order says sorted, signed says not.
    let big: Vec[u64] = [1u64, 18446744073709551615u64];
    println(big.is_sorted());

    let s: Vec[String] = ["apple", "banana", "cherry"];
    println(s.is_sorted());
    let s2: Vec[String] = ["cherry", "apple"];
    println(s2.is_sorted());

    let sl: Slice[i64] = a[0..3];
    println(sl.is_sorted());
    let sl2: Slice[i64] = b[0..3];
    println(sl2.is_sorted());
    let slu: Slice[u64] = big[0..2];
    println(slu.is_sorted());

    let arr: Array[i64, 4] = [1, 2, 3, 4];
    println(arr.is_sorted());
    let arr2: Array[i64, 3] = [3, 1, 2];
    println(arr2.is_sorted());
    let arru: Array[u64, 2] = [1u64, 18446744073709551615u64];
    println(arru.is_sorted());
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(concat!(
            "true\nfalse\ntrue\ntrue\n",
            "true\n",
            "true\nfalse\n",
            "true\nfalse\ntrue\n",
            "true\nfalse\ntrue\n"
        ))
    );
}

/// `sort()` and `is_sorted()` must agree about the sequence `sort` just
/// produced. They are separate lowerings that each pick an element
/// ordering, so a signed/unsigned mismatch between them would show up here
/// as `false` right after a sort — the shape B-2026-07-04-8 fixed for
/// `sort` alone.
#[test]
fn test_e2e_is_sorted_agrees_with_sort() {
    let src = r#"
fn main() {
    let mut v: Vec[u64] = [9223372036854775809u64, 3u64, 18446744073709551615u64, 1u64];
    println(v.is_sorted());
    v.sort();
    println(v.is_sorted());
    println(v[0]);
    println(v[3]);

    let mut w: Vec[String] = ["pear", "apple", "fig"];
    println(w.is_sorted());
    w.sort();
    println(w.is_sorted());
    println(w[0]);
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("false\ntrue\n1\n18446744073709551615\nfalse\ntrue\napple\n")
    );
}

/// design.md § Contracts' worked example, compiled. The spec writes
/// `requires haystack.is_sorted()` on `binary_search` and
/// `invariant self.data.is_sorted()` on `SortedVec`; neither compiled
/// before B-2026-08-21-10, so the feature was documented against a method
/// that did not exist.
#[test]
fn test_e2e_is_sorted_backs_the_spec_contract_example() {
    let src = r#"
fn find(haystack: ref Vec[i64], needle: i64) -> Option[i64]
    requires haystack.is_sorted()
{
    haystack.binary_search(needle)
}

struct SortedVec {
    data: Vec[i64],

    invariant self.data.is_sorted()
}

fn main() {
    let v: Vec[i64] = [1, 3, 5, 7];
    match find(v, 5) { Some(i) => { println(i); } None => { println("none"); } }
    match find(v, 4) { Some(i) => { println(i); } None => { println("none"); } }
    let sv = SortedVec { data: [2, 4, 6] };
    println(sv.data.len());
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("2\nnone\n3\n"));
}

#[test]
fn e2e_array_used_as_a_map_key_reads_back_on_every_surface() {
    // B-2026-09-13-1 -- the cross-surface twin of
    // `asan_array_used_as_a_map_key_frees_its_elements`.
    //
    // A key is read on every lookup, by the hash/equality compare, so the
    // key half's use-after-free is directly observable here in a way the
    // value half's was not: before the fix the map held pointers into
    // buffers the source local had already freed, and `karac_map_get`
    // compared against them. That it usually still FOUND the entry is the
    // hazard -- freed bytes often survive -- so these cells assert the
    // looked-up value, not merely that a lookup happened.
    //
    // Every lookup is by an explicitly typed probe. An array literal in an
    // argument position does not take the expected element type and infers
    // as `Vec[String]` (the B-2026-09-10-38 family), so `m.get([..])` does
    // not type-check while `m.insert([..], v)` does.
    for (label, src, want) in [
            // 1 -- source-LOCAL keys, map outliving them, looked up by value.
            (
                "map-key-array-local-source-lookup",
                "fn main() {\n\
                 \x20\x20\x20\x20let mut m: Map[Array[String, 2], i64] = Map.new();\n\
                 \x20\x20\x20\x20let mut i: i64 = 0;\n\
                 \x20\x20\x20\x20while i < 3 {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20let k: Array[String, 2] = [f\"kaaaaaaa{i}\", f\"kbbbbbbb{i}\"];\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20m.insert(k, i * 10);\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1;\n\
                 \x20\x20\x20\x20}\n\
                 \x20\x20\x20\x20let p0: Array[String, 2] = [f\"kaaaaaaa0\", f\"kbbbbbbb0\"];\n\
                 \x20\x20\x20\x20let p2: Array[String, 2] = [f\"kaaaaaaa2\", f\"kbbbbbbb2\"];\n\
                 \x20\x20\x20\x20match m.get(p0) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(v) => { println(f\"s:{v}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"s:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 \x20\x20\x20\x20match m.get(p2) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(v) => { println(f\"t:{v}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"t:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 \x20\x20\x20\x20println(f\"u:{m.len()}\");\n\
                 }\n",
                "s:0\nt:20\nu:3\n",
            ),
            // 2 -- DUPLICATE keys. The second insert must REPLACE rather than
            //      add, which is only true if the stored key still compares
            //      equal -- i.e. if its buffers are intact.
            (
                "map-key-array-duplicate-replaces",
                "fn mk(n: i64) -> Array[String, 2] { return Array[f\"kaaaaaaa{n}\", f\"kbbbbbbb{n}\"]; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let mut m: Map[Array[String, 2], i64] = Map.new();\n\
                 \x20\x20\x20\x20let k1 = mk(1);\n\
                 \x20\x20\x20\x20let k2 = mk(1);\n\
                 \x20\x20\x20\x20m.insert(k1, 11);\n\
                 \x20\x20\x20\x20m.insert(k2, 22);\n\
                 \x20\x20\x20\x20let p: Array[String, 2] = mk(1);\n\
                 \x20\x20\x20\x20match m.get(p) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(v) => { println(f\"s:{v}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"s:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 \x20\x20\x20\x20println(f\"t:{m.len()}\");\n\
                 }\n",
                "s:22\nt:1\n",
            ),
            // 3 -- `try_insert`, both halves, the second entry point.
            (
                "map-key-array-try-insert",
                "fn mk(n: i64) -> Array[String, 2] { return Array[f\"kaaaaaaa{n}\", f\"kbbbbbbb{n}\"]; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let mut m: Map[Array[String, 2], i64] = Map.new();\n\
                 \x20\x20\x20\x20let k1 = mk(1);\n\
                 \x20\x20\x20\x20let k2 = mk(1);\n\
                 \x20\x20\x20\x20match m.try_insert(k1, 11) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Ok(o) => { println(\"s:ok\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Err(e) => { println(\"s:err\"); }\n\
                 \x20\x20\x20\x20}\n\
                 \x20\x20\x20\x20match m.try_insert(k2, 22) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Ok(o) => { println(\"t:ok\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Err(e) => { println(\"t:err\"); }\n\
                 \x20\x20\x20\x20}\n\
                 \x20\x20\x20\x20let p: Array[String, 2] = mk(1);\n\
                 \x20\x20\x20\x20match m.get(p) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(v) => { println(f\"u:{v}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"u:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 }\n",
                "s:ok\nt:ok\nu:22\n",
            ),
            // 4 -- `try_insert` on the VALUE half from a source local: the
            //      B-2026-09-12-13 correction, which aborted before this row.
            (
                "map-value-array-try-insert-local-read",
                "fn main() {\n\
                 \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
                 \x20\x20\x20\x20let e: Array[String, 2] = [f\"vaaaaaaa0\", f\"vbbbbbbb0\"];\n\
                 \x20\x20\x20\x20match m.try_insert(1, e) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Ok(o) => { println(\"s:ok\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Err(x) => { println(\"s:err\"); }\n\
                 \x20\x20\x20\x20}\n\
                 \x20\x20\x20\x20match m.get(1) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { println(f\"t:{a[1]}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"t:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 }\n",
                "s:ok\nt:vbbbbbbb0\n",
            ),
            // CONTROL: a `String` key at the same duplicate shape, whose own
            // no-adopt reclaim the array one sits beside.
            (
                "map-key-string-duplicate-control",
                "fn main() {\n\
                 \x20\x20\x20\x20let mut m: Map[String, i64] = Map.new();\n\
                 \x20\x20\x20\x20let k1 = f\"kaaaaaaa1\";\n\
                 \x20\x20\x20\x20let k2 = f\"kaaaaaaa1\";\n\
                 \x20\x20\x20\x20m.insert(k1, 11);\n\
                 \x20\x20\x20\x20m.insert(k2, 22);\n\
                 \x20\x20\x20\x20match m.get(f\"kaaaaaaa1\") {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(v) => { println(f\"s:{v}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"s:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 }\n",
                "s:22\n",
            ),
            // CONTROL: scalar elements, where both the drop fn and the reclaim
            // must decline and the map still behaves.
            (
                "map-key-scalar-array-control",
                "fn main() {\n\
                 \x20\x20\x20\x20let mut m: Map[Array[i64, 2], i64] = Map.new();\n\
                 \x20\x20\x20\x20m.insert([11, 22], 1);\n\
                 \x20\x20\x20\x20m.insert([11, 22], 2);\n\
                 \x20\x20\x20\x20let p: Array[i64, 2] = [11, 22];\n\
                 \x20\x20\x20\x20match m.get(p) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(v) => { println(f\"s:{v}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"s:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 }\n",
                "s:2\n",
            ),
        ] {
            let Some(out) = run_program(src) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

#[test]
fn e2e_array_held_as_a_map_value_reads_back_on_every_surface() {
    // B-2026-09-12-13 -- the cross-surface twin of
    // `asan_array_held_as_a_map_value_frees_its_elements`.
    //
    // Unlike the struct-field row's twin, a PRE-FIX TREE FAILS THIS ONE,
    // and that is the point: the source-local spelling was not merely
    // leaking, it was reading out of freed buffers. The exact first cell
    // printed `s:s:H0V` with six valgrind errors before the fix. So these
    // cells are a real output gate here rather than only a guard against
    // an over-eager retraction.
    //
    // Every lookup is by KEY. A `Map` walk is per-process hash order, so
    // a bare `for (k, v) in m` would diverge between runs of one binary
    // for reasons that have nothing to do with this row.
    for (label, src, want) in [
            // 1 -- the source-LOCAL spelling, map outliving the local. The
            //      use-after-free face: pre-fix this printed garbage.
            (
                "map-value-array-local-source-read",
                "fn main() {\n\
                 \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
                 \x20\x20\x20\x20let mut i: i64 = 0;\n\
                 \x20\x20\x20\x20while i < 2 {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20let e: Array[String, 2] = [f\"aaaaaaaa{i}\", f\"bbbbbbbb{i}\"];\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20m.insert(i, e);\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1;\n\
                 \x20\x20\x20\x20}\n\
                 \x20\x20\x20\x20match m.get(0) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { println(f\"s:{a[0]}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"s:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 \x20\x20\x20\x20match m.get(1) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { println(f\"t:{a[1]}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"t:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 }\n",
                "s:aaaaaaaa0\nt:bbbbbbbb1\n",
            ),
            // 2 -- the temp-literal spelling, which read back correctly even
            //      pre-fix and only leaked. Both spellings must agree.
            (
                "map-value-array-literal-read",
                "fn main() {\n\
                 \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
                 \x20\x20\x20\x20m.insert(1, [f\"aaaaaaaa0\", f\"bbbbbbbb0\"]);\n\
                 \x20\x20\x20\x20match m.get(1) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { println(f\"s:{a[0]}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"s:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 4 -- the NESTED array value.
            (
                "map-value-nested-array-read",
                "fn main() {\n\
                 \x20\x20\x20\x20let mut m: Map[i64, Array[Array[String, 2], 2]] = Map.new();\n\
                 \x20\x20\x20\x20m.insert(1, [[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]]);\n\
                 \x20\x20\x20\x20match m.get(1) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { println(\"s:ok\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"s:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 }\n",
                "s:ok\n",
            ),
            // 5 -- `SortedMap`, the ordered sibling on the same storage.
            (
                "sortedmap-value-array-read",
                "fn main() {\n\
                 \x20\x20\x20\x20let mut m: SortedMap[i64, Array[String, 2]] = SortedMap.new();\n\
                 \x20\x20\x20\x20let e: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20m.insert(1, e);\n\
                 \x20\x20\x20\x20match m.get(1) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { println(f\"s:{a[1]}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"s:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 }\n",
                "s:bbbbbbbb0\n",
            ),
            // 6 -- the map RETURNED from the fn that built it, read by the
            //      caller: the stored buffers must outlive the callee frame.
            (
                "map-value-array-returned-read",
                "fn mk() -> Map[i64, Array[String, 2]] {\n\
                 \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
                 \x20\x20\x20\x20let e: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20m.insert(1, e);\n\
                 \x20\x20\x20\x20return m;\n\
                 }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let m = mk();\n\
                 \x20\x20\x20\x20match m.get(1) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { println(f\"s:{a[0]}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"s:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // CONTROL: a `Vec[String]` value from a source local -- the type
            // that already resolved a per-value drop, so `insert` already
            // retracted its source. It must read back unchanged.
            (
                "map-value-vec-local-read-control",
                "fn main() {\n\
                 \x20\x20\x20\x20let mut inner: Vec[String] = Vec.new();\n\
                 \x20\x20\x20\x20inner.push(f\"aaaaaaaa0\");\n\
                 \x20\x20\x20\x20let mut m: Map[i64, Vec[String]] = Map.new();\n\
                 \x20\x20\x20\x20m.insert(1, inner);\n\
                 \x20\x20\x20\x20match m.get(1) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(v) => { println(f\"s:{v[0]}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"s:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // CONTROL: scalar elements, where the new arm must decline and the
            // value still reads back.
            (
                "map-value-scalar-array-read-control",
                "fn main() {\n\
                 \x20\x20\x20\x20let mut m: Map[i64, Array[i64, 2]] = Map.new();\n\
                 \x20\x20\x20\x20m.insert(1, [11, 22]);\n\
                 \x20\x20\x20\x20match m.get(1) {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { println(f\"s:{a[1]}\"); }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"s:missing\"); }\n\
                 \x20\x20\x20\x20}\n\
                 }\n",
                "s:22\n",
            ),
        ] {
            let Some(out) = run_program(src) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

#[test]
/// B-2026-09-23-40 — a heap field bound inside an enum-variant sub-pattern of a
/// `Map.get` / `SortedMap.get` payload (`Some(B.S(w)) => { held = w; }`) and
/// then MOVED was an alias into the map's live value, so the new owner and the
/// map both freed it: `free(): double free detected` on the JIT, `-O0` and
/// `-O2`. The struct and tuple destructures of the same payload already got
/// their own copy; the variant destructure now does too, for each ESCAPING
/// binding. Legs: a two-field variant, a struct payload pushed whole, `if let`,
/// `SortedMap`, the binding returned from a match arm, a move in a loop, a
/// `Vec[String]` payload, and a read-only control. This fixture program also
/// NEVER PARSED before B-2026-09-23-33, which is how the fault stayed hidden.
fn test_e2e_map_get_variant_payload_move_matches_interp() {
    let out = run_program(
        r#"
enum B { S(String), C }
enum B2 { S(String, i64), C }
enum Bv { S(Vec[String]), C }
struct Wide { a: String, n: i64 }
enum W { V(Wide), E }
fn mk(i: i64) -> String { f"payload-{i}-long-enough-to-heap" }
fn leg_two_field() {
    let mut m: Map[String, B2] = Map.new();
    let _ = m.insert("k", B2.S(mk(1), 3));
    let mut held = String.new(); let mut k = 0;
    match m.get("k") { None => {} Some(B2.S(w, n)) => { held = w; k = n; } Some(B2.C) => {} }
    println(f"tf {held} {k} {m.len()}");
}
fn leg_struct() {
    let mut m: Map[String, W] = Map.new();
    let _ = m.insert("k", W.V(Wide { a: mk(2), n: 7 }));
    let mut out: Vec[Wide] = [];
    match m.get("k") { None => {} Some(W.V(x)) => { out.push(x); } Some(W.E) => {} }
    println(f"st {out.len()} {out[0].a} {out[0].n}");
}
fn leg_if_let() {
    let mut m: Map[String, B] = Map.new();
    let _ = m.insert("k", B.S(mk(3)));
    let mut held = String.new();
    if let Some(B.S(w)) = m.get("k") { held = w; }
    println(f"il {held}");
}
fn leg_sorted() {
    let mut m: SortedMap[String, B] = SortedMap.new();
    let _ = m.insert("k", B.S(mk(4)));
    let mut out: Vec[String] = [];
    match m.get("k") { None => {} Some(B.S(w)) => { out.push(w); } Some(B.C) => {} }
    println(f"so {out[0]}");
}
fn pick(m: ref Map[String, B]) -> String {
    match m.get("k") { None => String.new(), Some(B.S(w)) => w, Some(B.C) => String.new() }
}
fn leg_return() {
    let mut m: Map[String, B] = Map.new();
    let _ = m.insert("k", B.S(mk(5)));
    let s = pick(m);
    println(f"rt {s}");
}
fn leg_loop_move() {
    let mut m: Map[String, B] = Map.new();
    let _ = m.insert("k", B.S(mk(6)));
    let mut out: Vec[String] = [];
    let mut i = 0;
    while i < 3 { match m.get("k") { None => {} Some(B.S(w)) => { out.push(w); } Some(B.C) => {} } i = i + 1; }
    println(f"lm {out.len()} {out[2]}");
}
fn leg_vec_payload() {
    let mut m: Map[String, Bv] = Map.new();
    let _ = m.insert("k", Bv.S([mk(7), mk(8)]));
    let mut held: Vec[String] = [];
    match m.get("k") { None => {} Some(Bv.S(w)) => { held = w; } Some(Bv.C) => {} }
    println(f"vp {held.len()} {held[1]}");
}
fn leg_read_only() {
    let mut m: Map[String, B] = Map.new();
    let _ = m.insert("k", B.S(mk(9)));
    let mut n = 0; let mut i = 0;
    while i < 4 { match m.get("k") { None => {} Some(B.S(w)) => { n = n + w.len(); } Some(B.C) => {} } i = i + 1; }
    println(f"ro {n}");
}
fn main() {
    leg_two_field();
    leg_struct();
    leg_if_let();
    leg_sorted();
    leg_return();
    leg_loop_move();
    leg_vec_payload();
    leg_read_only();
    println("done");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "tf payload-1-long-enough-to-heap 3 1\nst 1 payload-2-long-enough-to-heap 7\nil payload-3-long-enough-to-heap\nso payload-4-long-enough-to-heap\nrt payload-5-long-enough-to-heap\nlm 3 payload-6-long-enough-to-heap\nvp 2 payload-8-long-enough-to-heap\nro 116\ndone\n",
            "every leg must match --interp; got {out:?}"
        );
    }
}
