//! Map, Set, SortedMap, SortedSet -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer map_set::
//!
//! New fixtures about Map, Set, SortedMap, SortedSet belong in this file.

use super::*;

/// B-2026-09-06-60 — the MEMORY half, and the row's own symptom: the same
/// program under ASAN + LSan, where the pre-fix build double-freed the `Map`
/// handle and the `String`. One owner and one free per object on every
/// spelling, `Drop`-free struct included.
#[test]
fn asan_map_field_param_by_transfer() {
    assert_clean_asan_run(
            "struct Q { id: i64, name: String, tbl: Map[i64, i64] }\n\
             impl Drop for Q { fn drop(mut ref self) { println(f\"  dQ{self.id}\") } }\n\
             struct QNoDrop { id: i64, name: String, tbl: Map[i64, i64] }\n\
             struct P { id: i64, name: String, xs: Vec[i64] }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"  dP{self.id}\") } }\n\
             \n\
             fn mkq(i: i64) -> Q { let mut t = Map[i64, i64].new(); t.insert(i, i); return Q { id: i, name: f\"h{i}\", tbl: t }; }\n\
             fn mkqn(i: i64) -> QNoDrop { let mut t = Map[i64, i64].new(); t.insert(i, i); return QNoDrop { id: i, name: f\"h{i}\", tbl: t }; }\n\
             fn mkp(i: i64) -> P { return P { id: i, name: f\"p{i}\", xs: [i] }; }\n\
             \n\
             fn norebind(q: Q) -> i64 { return q.id; }\n\
             fn readmap(q: Q) -> i64 { return q.tbl.len(); }\n\
             fn rebind(q: Q) -> i64 { let m = q; return m.id; }\n\
             fn nodrop(q: QNoDrop) -> i64 { return q.id; }\n\
             fn copyable(p: P) -> i64 { let m = p; return m.id; }\n\
             \n\
             fn main() {\n\
             \x20   println(\"temp_arg\"); println(f\"  v={norebind(mkq(1))}\");\n\
             \x20   println(\"temp_arg_reads_map\"); println(f\"  v={readmap(mkq(2))}\");\n\
             \x20   println(\"temp_arg_rebind\"); println(f\"  v={rebind(mkq(3))}\");\n\
             \x20   println(\"no_drop_struct\"); println(f\"  v={nodrop(mkqn(4))}\");\n\
             \x20   println(\"copy_supported\"); println(f\"  v={copyable(mkp(5))}\");\n\
             \x20   println(\"no_call\"); let a = mkq(6); println(f\"  v={a.id}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "temp_arg",
                "  dQ1",
                "  v=1",
                "temp_arg_reads_map",
                "  dQ2",
                "  v=1",
                "temp_arg_rebind",
                "  dQ3",
                "  v=3",
                "no_drop_struct",
                "  v=4",
                "copy_supported",
                "  dP5",
                "  v=5",
                "no_call",
                "  v=6",
                "  dQ6",
                "end"
            ],
            "map_field_param_by_transfer",
        );
}

/// A container-element heap read handed out of a BRANCH VALUE gets an owner
/// at the merge (B-2026-08-28-44).
///
/// An arm tail that reads `p[1].word` deep-clones — the container keeps its
/// own buffer — and registers that clone's cleanup so a NON-consuming read
/// does not leak. Handing the value out of the arm is a consuming position,
/// so the arm-tail suppressor neutralizes that cleanup; correct, because the
/// arm's slot must not free what escaped it. Nothing at the merge then owned
/// the escaping value, and the clone was simply lost: 3 bytes in 1
/// allocation under LSan.
///
/// BOTH BRANCH FORMS ARE HERE because both leak, and that is worth pinning.
/// The row this fixture closes was filed claiming the `if` spelling was
/// clean and only `match` leaked; re-measuring found the opposite split on
/// its own fixtures and NO split at all once the consumer is held fixed.
/// The axis is the CONSUMER, not the construct, so the matrix is spelled out
/// rather than sampled.
///
/// The two LEAKING consumer positions:
///   - `println(<branch>)`, which borrows and never takes the clone over
///   - a by-value call argument, which under this codegen leaves the caller
///     owning the buffer and so never takes it over either
///
/// The three OWNING positions are the controls, and they are not decoration
/// — each is a DOUBLE FREE if the merge owner is added without the takeover
/// half, because the destination owns the same buffer:
///   - a `let` binding
///   - a `Vec.push` argument
///   - a `return` (the `pick` calls), which hands the value to the caller
///
/// The DISCARDED statement rows are the fourth corner: `discarded_branch_-
/// spans` means no clone is made there at all, so the merge must find
/// nothing to own. A merge owner armed on a discard would free a buffer the
/// container still holds, which the trailing reads of `p` would then catch
/// as a use-after-free rather than a leak.
///
/// Both `p[0].word` and `p[1].word` stay readable at the end, which is what
/// keeps "the read cloned" honest: if the read ever stopped cloning, these
/// would turn into a use-after-free instead of quietly passing.
/// B-2026-08-29-63 — a by-value own-heap struct param is owned by TRANSFER,
/// not by an entry COPY, wherever every call site in the program hands it a
/// binding the caller owns and never reads again.
///
/// The fixture calls three such callees 40 times each — a param that dies
/// inside, one handed straight back, and one forwarded through a second hop.
/// Pre-fix each call deep-copied the 32-element `Vec[i64]` field, so the
/// program allocated ~120 buffers nothing ever read; post-fix it allocates
/// none of them. The ceiling is what makes this a regression test rather
/// than a restatement: the pre-fix compiler runs this fixture correctly and
/// cleanly, just at nearly twice the allocations. Measured under ASAN —
/// **251 malloc calls with the transfer off, 131 with it on**, a flat 120
/// (3 per iteration) that were pure copy. The 180 ceiling sits between them
/// with headroom on both sides, so it neither flakes nor passes vacuously;
/// `KARAC_MOVE_STRUCT_PARAMS=0` reproduces the pre-fix number exactly and is
/// how this was RED-verified.
#[test]
fn asan_by_value_struct_param_is_owned_by_transfer_not_entry_copy() {
    assert_clean_asan_run_max_allocs(
        r#"
struct Res { id: i64, buf: Vec[i64] }

fn mk(n: i64) -> Res {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 32 { v.push(i + n); i = i + 1; }
    return Res { id: n, buf: v };
}

fn eat(r: Res) -> i64 { return r.buf[1]; }
fn hand(r: Res) -> Res { return r; }
fn inner(r: Res) -> i64 { return r.buf[2]; }
fn outer(r: Res) -> i64 { return inner(r); }

fn main() {
    let mut total = 0;
    let mut k = 0;
    while k < 40 {
        let a = mk(k);
        total = total + eat(a);
        let b = mk(k);
        let hb = hand(b);
        total = total + hb.buf[1];
        let c = mk(k);
        total = total + outer(c);
        k = k + 1;
    }
    println(f"total={total}");
}
"#,
        &["total=2500"],
        "b63-by-value-struct-param-transfer",
        180,
    );
}

/// B-2026-09-05-31 — the MEMORY gate for a NAMED LOCAL handed to a generic
/// whole-param callee, the cell the sibling fixture above had to leave out.
///
/// A generic callee ENTRY-COPIES a copy-supported heap struct param and
/// returns the COPY, so `let g = mk(88); let _ = passG(g);` leaves two
/// objects and one owner: the binding freed its original, nothing freed
/// the copy. Measured pre-fix on this exact program — 152 allocs / 134
/// frees, 288 B definitely lost in 6 blocks — and unbounded, three rounds
/// accumulating rather than cancelling.
///
/// THE PAYLOAD IS `Vec[String]` ON PURPOSE and the fixture is worthless
/// without it. With the row's original `String` + `Vec[i64]` the entry
/// copy is dead once the result is discarded, LLVM removes the malloc, and
/// the whole leak is invisible at the DEFAULT optimization level — which
/// is why it sat filed as a `KARAC_OPT_LEVEL=0` curiosity. A `Vec[String]`
/// copy goes through runtime calls the optimizer will not touch, so the
/// leak survives to the column this test actually runs.
///
/// In the other direction this is the DOUBLE-FREE gate, and cell `g` is
/// the one that matters: `D` owns a `shared` field, so copy support
/// declines and the callee takes the binding's own object rather than a
/// copy. Admitting that shape would free one object twice — strictly worse
/// than the leak — so the fix's registration and its caller-side
/// stand-down are gated on the SAME entry-copy predicate, and this cell is
/// what holds them together. Cell `h` is a struct with no user `Drop`,
/// which leaked its copy too and owes memory without a body.
#[test]
fn asan_generic_whole_param_named_local_frees_the_entry_copy() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, names: Vec[String] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, names: [f"a{i}", f"b{i}"] }; }
shared struct Sh { v: i64 }
struct D { s: Sh, tag: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.tag}") } }
fn mkd(i: i64) -> D { return D { s: Sh { v: i }, tag: f"x{i}" }; }
struct P { id: i64, tag: String }
fn mkp(i: i64) -> P { return P { id: i, tag: f"p{i}" }; }
fn passG[T](x: T) -> T { println("inP"); return x; }
fn passN(x: R) -> R { println("inN"); return x; }
fn maybeG[T](x: T, k: bool) -> T { println("inM"); if k { return x; } return x; }
fn scalarG[T](x: T) -> i64 { println("inS"); return 3; }
fn round() {
  let g1 = mk(82); let _ = passG(g1);            println("a");
  let g2 = mk(88); let o2 = passG(g2);           println(f"k{o2.id}");
  let g3 = mk(89); let _ = maybeG(g3, true);     println("c");
  let g4 = mk(90); let o4 = maybeG(g4, false);   println(f"m{o4.id}");
  let g5 = mk(91); let _ = passN(g5);            println("e");
  let g6 = mk(92); let _ = scalarG(g6);          println("f");
  let d7 = mkd(93); let _ = passG(d7);           println("g");
  let p8 = mkp(94); let _ = passG(p8);           println("h");
}
fn main() { let mut i = 0; while i < 3 { round(); i = i + 1; } println("done"); }
"#,
        &[
            "inP", "dR82", "a", "inP", "k88", "dR88", "inM", "dR89", "c", "inM", "m90", "dR90",
            "inN", "dR91", "e", "inS", "dR92", "f", "inP", "dDx93", "g", "inP", "h", "inP", "dR82",
            "a", "inP", "k88", "dR88", "inM", "dR89", "c", "inM", "m90", "dR90", "inN", "dR91",
            "e", "inS", "dR92", "f", "inP", "dDx93", "g", "inP", "h", "inP", "dR82", "a", "inP",
            "k88", "dR88", "inM", "dR89", "c", "inM", "m90", "dR90", "inN", "dR91", "e", "inS",
            "dR92", "f", "inP", "dDx93", "g", "inP", "h", "done",
        ],
        "b0905-31-generic-whole-param-named-local",
        100,
    );
}

/// B-2026-09-06-2 — the MEMORY gate for a generic METHOD that hands its
/// by-value param back. The E2E twin reads the body counts; this one reads
/// the 240 B in 5 blocks that valgrind measured lost per evaluation
/// pre-fix, unbounded in a loop (24,000 B in 500 blocks at the DEFAULT
/// optimization level), and — in the other direction — it is what would
/// catch the fix overshooting into a double free.
///
/// Cell `g` is the one that holds the gates together. `D` owns a direct
/// `shared` field, so copy support declines and the callee takes the
/// binding's OWN object rather than a copy: admitting it would free one
/// object twice, and standing the binding's body down would leave nothing
/// running it. Both halves of the fix are gated on the same entry-copy
/// predicate for exactly that reason, and this cell is what proves they
/// stayed identical rather than merely compatible.
///
/// The payload is `Vec[String]` for the reason B-2026-09-05-31 recorded:
/// with a `String` + `Vec[i64]` struct the discarded entry copy is dead and
/// LLVM deletes it, hiding the leak everywhere except `-O0`. Three rounds
/// so a per-round imbalance accumulates rather than cancelling.
#[test]
fn asan_generic_method_whole_param_frees_the_entry_copy() {
    assert_clean_asan_run_min_allocs(
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
fn round() {
  let h = H { n: 1 };
  let g1 = mk(82); let _ = h.keep(g1);          println("a");
  let g2 = mk(88); let o2 = h.keep(g2);         println(f"k{o2.id}");
  let g3 = mk(89); let _ = h.maybe(g3, true);   println("c");
  let g4 = mk(90); let o4 = h.maybe(g4, false); println(f"m{o4.id}");
  let g5 = mk(91); let _ = h.keepN(g5);         println("e");
  let g6 = mk(92); let _ = h.scalar(g6);        println("f");
  let d7 = mkd(93); let _ = h.keep(d7);         println("g");
  let _ = h.keep(mk(94));                       println("h");
}
fn main() { let mut i = 0; while i < 3 { round(); i = i + 1; } println("done"); }
"#,
        &[
            "inK", "dR82", "a", "inK", "k88", "dR88", "inM", "dR89", "c", "inM", "m90", "dR90",
            "inN", "dR91", "e", "inS", "dR92", "f", "inK", "dDx93", "g", "inK", "dR94", "h", "inK",
            "dR82", "a", "inK", "k88", "dR88", "inM", "dR89", "c", "inM", "m90", "dR90", "inN",
            "dR91", "e", "inS", "dR92", "f", "inK", "dDx93", "g", "inK", "dR94", "h", "inK",
            "dR82", "a", "inK", "k88", "dR88", "inM", "dR89", "c", "inM", "m90", "dR90", "inN",
            "dR91", "e", "inS", "dR92", "f", "inK", "dDx93", "g", "inK", "dR94", "h", "done",
        ],
        "b0906-2-generic-method-whole-param",
        110,
    );
}

#[test]
fn asan_branch_nested_param_rebind_frees_the_entry_copy() {
    assert_clean_asan_run_min_allocs(
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
fn round() {
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
}
fn main() { let mut i = 0; while i < 3 { round(); i = i + 1; } println("done"); }
"#,
        &[
            "dR21", "brF=0", "dR22", "brT=22", "dR23", "faF=7", "in24", "dR24", "faT=7", "dR25",
            "elF=25", "dR26", "elT=4", "s=h27", "dR27", "rdF=6", "n=h28", "dR28", "rdT=5", "dR29",
            "arF=0", "dR30", "arT=30", "dR31", "tw00=3", "dR32", "tw10=2", "dR33", "tw11=33",
            "dR34", "top=34", "after", "dR35", "got36", "dR36", "dR21", "brF=0", "dR22", "brT=22",
            "dR23", "faF=7", "in24", "dR24", "faT=7", "dR25", "elF=25", "dR26", "elT=4", "s=h27",
            "dR27", "rdF=6", "n=h28", "dR28", "rdT=5", "dR29", "arF=0", "dR30", "arT=30", "dR31",
            "tw00=3", "dR32", "tw10=2", "dR33", "tw11=33", "dR34", "top=34", "after", "dR35",
            "got36", "dR36", "dR21", "brF=0", "dR22", "brT=22", "dR23", "faF=7", "in24", "dR24",
            "faT=7", "dR25", "elF=25", "dR26", "elT=4", "s=h27", "dR27", "rdF=6", "n=h28", "dR28",
            "rdT=5", "dR29", "arF=0", "dR30", "arT=30", "dR31", "tw00=3", "dR32", "tw10=2", "dR33",
            "tw11=33", "dR34", "top=34", "after", "dR35", "got36", "dR36", "done",
        ],
        "b0905-37-branch-nested-param-rebind",
        200,
    );
}

#[test]
fn asan_shared_struct_field_map_recursive_value_no_leak() {
    // B-2026-08-09-1 — the SHARED struct-drop arm had no per-value drop-fn
    // channel at all, while its non-shared twin has routed every recursive
    // value type through `map_val_drop_fn_for_type_expr` since
    // B-2026-07-27-2. `map_drop_flags` frees exactly ONE level of
    // `{ptr,len,cap}` per side, so a value owning heap below that level was
    // released short:
    //
    //   shared struct field -> definitely lost: 96 bytes in 1 block
    //   non-shared struct   -> All heap blocks were freed
    //   plain local binding -> All heap blocks were freed
    //
    // Three spellings of the same container, one of them leaking — which is
    // what made it look like noise rather than a defect in whatever program
    // hit it. All three spellings are in this fixture so a regression that
    // un-wires the channel cannot hide behind the two that were always
    // clean.
    assert_clean_asan_run(
        r#"
shared struct SharedOwner { mut m: Map[i64, Vec[Vec[String]]] }
struct PlainOwner { mut m: Map[i64, Vec[Vec[String]]] }
fn nested(tag: String) -> Vec[Vec[String]] {
    let mut inner: Vec[String] = Vec.new();
    inner.push(tag);
    inner.push("a second reasonably long string in the inner vec");
    let mut outer: Vec[Vec[String]] = Vec.new();
    outer.push(inner);
    return outer;
}
fn main() {
    let so = SharedOwner { m: Map.new() };
    so.m.insert(1i64, nested("shared owner field value string"));
    println(so.m.len());

    let mut po = PlainOwner { m: Map.new() };
    po.m.insert(2i64, nested("plain owner field value string"));
    println(po.m.len());

    let mut local: Map[i64, Vec[Vec[String]]] = Map.new();
    local.insert(3i64, nested("plain local binding value string"));
    println(local.len());
}
"#,
        &["1", "1", "1"],
        "shared_struct_field_map_recursive_value",
    );
}

#[test]
fn asan_map_of_weak_value_cycle_freed_no_leak() {
    // B-2026-08-08-29 — `Map[K, weak V]`. `Vec` got the whole weak-element
    // contract (store downgrade, no strong count, scope-exit weak drain);
    // `Map` had the typecheck relaxation and NONE of the codegen, so an
    // insert stored a STRONG pointer into a slot the author declared
    // `weak`. Three consequences, all measured against the `Map[i64, N]`
    // strong twin, which was clean throughout:
    //
    //   one entry, no cycle   -> definitely lost: 24 bytes (the box)
    //   Map-mediated cycle    -> 376 bytes (32 direct + 344 indirect)
    //   overwrite / remove    -> one orphaned weak count each, because the
    //                            displaced value rides a `Some(old)` the
    //                            discard never holds
    //
    // So writing `weak` LEAKED where omitting it did not — the annotation
    // doing the opposite of its purpose, silently and with the right answer
    // printed. This fixture covers all four shapes in one program: a
    // struct-FIELD `Map[i64, weak N]` closing a two-node cycle (the
    // shared-struct drop arm, which has no general val-drop channel and
    // needed its own weak arm), plus a local map that is overwritten and
    // then drained by `remove`.
    //
    // The `impl Drop` bodies are the positive half: they only fire if the
    // cycle is actually collected, so a regression that reinstates the
    // strong store fails on OUTPUT here, not only under LSan.
    assert_clean_asan_run(
        r#"
shared struct N { mut v: i64, mut kids: Map[i64, weak N] }
impl Drop for N {
    fn drop(mut ref self) { println(f"drop {self.v}"); }
}
fn cycle() {
    let p = N { v: 1i64, kids: Map.new() };
    let c = N { v: 2i64, kids: Map.new() };
    c.kids.insert(0i64, p);
    p.kids.insert(0i64, c);
}
fn churn() -> i64 {
    let mut m: Map[i64, weak N] = Map.new();
    let a = N { v: 10i64, kids: Map.new() };
    let b = N { v: 20i64, kids: Map.new() };
    m.insert(7i64, a);
    m.insert(7i64, b);
    let _ = m.remove(7i64);
    return a.v + b.v + m.len();
}
fn main() {
    cycle();
    println(churn());
}
"#,
        &["drop 2", "drop 1", "drop 20", "drop 10", "30"],
        "map_of_weak_value_cycle_freed",
    );
}

#[test]
fn asan_option_map_heap_payload_no_leak() {
    // B-2026-07-12-11 heap half: `Option.map` over a heap payload routes
    // through match-synthesis for correct ownership. A read-only mapper
    // frees the payload buffer exactly once (the receiver owns the
    // moved-out payload); a MOVE mapper (`|x| x`) transfers the buffer to
    // the result without a receiver double-free; a heap-RETURNING mapper
    // (annotated `to_uppercase`) frees the source and owns the fresh result;
    // and a CHAINED `map(f).unwrap_or(d)` (Slice-1 span fix) is exercised
    // too. 40 iterations so any leak / double-free trips LSan / ASAN.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let mut v: Vec[i64] = Vec.new();
        v.push(1i64); v.push(2i64); v.push(3i64);
        let opt: Option[Vec[i64]] = Some(v);
        total = total + opt.map(|xs| xs.len()).unwrap_or(0i64);
        let s: Option[String] = Some(f"hello");
        let same = s.map(|x| x);
        match same { Some(x) => { total = total + x.len(); } None => {} }
        let up: Option[String] = Some(f"hi");
        match up.map(|x: String| x.to_uppercase()) { Some(x) => { total = total + x.len(); } None => {} }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // 40 * (3 + 5 + 2) = 400
        &["400"],
        "option_map_heap_payload_no_leak",
    );
}

#[test]
fn asan_let_bound_map_result_owns_its_heap_payload() {
    // B-2026-08-09-4 — a HEAP-inner `.map(f)` lowers through
    // `compile_map_via_match_synthesis`, whose result OWNS its payload: the
    // present branch is freshly produced by the mapper, the absent branch is
    // moved out of the receiver, and the receiver's own cleanup is
    // suppressed by the synthesized match. `map_passthrough_armed_source`
    // nonetheless claimed every `.map` over an armed binding as an ALIAS of
    // the receiver — correct only for the SCALAR-inner hand-rolled lowering,
    // which really does shallow-copy the receiver's Err/None words
    // (B-2026-08-07-3) — so the fresh payload was left with no owner and
    // leaked once per evaluation.
    //
    // Every `let`/discard form of the map result is here, because they leak
    // through two different registrars (the let-site tracker and the
    // discarded-temp registrar) and a fix to one says nothing about the
    // other.
    //
    // ARM SHAPE IS LOAD-BEARING, and is why this row hid for as long as it
    // did. A `Some(x) => x.len()` arm reads only the length word, so at the
    // default `-O2` LLVM proves the copied BUFFER dead and deletes the
    // malloc outright — the leak becomes unobservable and the default
    // sanitizer leg reports clean. The arms below compare or index the
    // payload, which loads its bytes, so they are red at BOTH opt levels;
    // the tag-only arm and the two discards keep `-O0`-only shapes in the
    // fixture so `scripts/asan-o0-leg.sh` gates that half too.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        // Payload BYTES read — red at both opt levels.
        let s: Option[String] = Some(f"hello");
        let same = s.map(|x| x);
        match same { Some(x) => { if x == "hello" { total = total + 5i64; } } None => {} }
        let u: Option[String] = Some(f"hello");
        let up = u.map(|x: String| x.to_uppercase());
        match up { Some(x) => { if x == "HELLO" { total = total + 5i64; } } None => {} }
        let v: Option[Vec[i64]] = Some(vec![1i64, 2i64, 3i64]);
        let vm = v.map(|xs| xs);
        match vm { Some(xs) => { total = total + xs[0]; } None => {} }
        let r: Result[String, String] = Ok(f"okv");
        let rm = r.map(|x| x);
        match rm { Ok(x) => { if x == "okv" { total = total + 3i64; } } Err(_) => {} }
        // Tag-only read and the two discard forms — `-O0`-only, because the
        // optimizer deletes a copy whose bytes are never loaded.
        let n: Option[String] = Some(f"hello");
        let unused = n.map(|x| x);
        match unused { Some(_) => { total = total + 1i64; } None => {} }
        let d: Option[String] = Some(f"hello");
        d.map(|x| x);
        let e: Option[String] = Some(f"hello");
        let _ = e.map(|x| x);
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // 40 * (5 + 5 + 1 + 3 + 1) = 600
        &["600"],
        "let_bound_map_result_owns_its_heap_payload",
    );
}

#[test]
fn asan_result_map_heap_err_payload_freed_exactly_once() {
    // B-2026-08-09-6, leak half — the miscompile half is pinned by
    // `test_e2e_result_map_preserves_a_heap_err_payload` (tests/codegen.rs).
    // A heap-`T` `.map` synthesizes `Err(e) => Err(e)`, but the typechecker
    // never recorded `E` for `map`, so codegen had no type with which to
    // seed the `e` binding and treated the payload as a scalar word: it was
    // neither carried into the result (empty output) nor freed (4 bytes per
    // evaluation).
    //
    // Both directions matter here, so the fixture reads the payload back
    // AND loops: a fix that over-corrected into a double free would abort
    // under ASAN rather than pass quietly. Covers String and Vec `Err`
    // payloads, an identity and a heap-returning mapper (which never runs
    // on the `Err` branch but picks the lowering), and the `Ok` branch of
    // the same type as the control.
    //
    // Stated precisely, because it changes what this pin proves: PRE-FIX it
    // dies on the MISCOMPILE before LSan gets to report (the scalar-word
    // `Err` gives the `Vec` payload a garbage length, so the run aborts with
    // `panic: vec index out of bounds`, exit 1). The leak was measured
    // separately, on the reduced probe under valgrind: 4 bytes definitely
    // lost per evaluation, at both opt levels. So this fixture gates the
    // pair; it does not isolate the leak on its own.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let a: Result[String, String] = Err(f"boom");
        let ra = a.map(|x| x);
        match ra { Ok(x) => { total = total + x.len(); } Err(e) => { if e == "boom" { total = total + 4i64; } } }
        let b: Result[String, String] = Err(f"bang");
        match b.map(|x: String| x.to_uppercase()) { Ok(x) => { total = total + x.len(); } Err(e) => { if e == "bang" { total = total + 4i64; } } }
        let c: Result[String, Vec[i64]] = Err(vec![7i64, 8i64]);
        let rc = c.map(|x| x);
        match rc { Ok(x) => { total = total + x.len(); } Err(e) => { total = total + e[1]; } }
        let ok: Result[String, String] = Ok(f"fine");
        let ro = ok.map(|x| x);
        match ro { Ok(x) => { if x == "fine" { total = total + 4i64; } } Err(_) => {} }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // 40 * (4 + 4 + 8 + 4) = 800
        &["800"],
        "result_map_heap_err_payload_freed_exactly_once",
    );
}

#[test]
fn asan_map_try_insert_heap_value_overwrite_no_double_free() {
    // B-2026-07-09-15: `Map[i64, String].try_insert` on the fallible path
    // must have the SAME single-drop contract as the panicking `insert`.
    // On an overwrite the runtime copies the OLD value out into the
    // `Some(old)` payload (the match binding owns + drops it once) and the
    // bucket adopts the NEW value (the map's handle-drop frees it once).
    // ASan flags a double-free if either the old value is also freed by the
    // map, or the new value's adoption double-copies. Looped so any
    // per-iteration imbalance accumulates; f-strings force non-foldable heap.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[i64, String] = Map.new();
    let mut i: i64 = 0i64;
    while i < 4i64 {
        let _ = m.try_insert(i, f"val-{i}-padding-padding-padding");
        i = i + 1;
    }
    let mut j: i64 = 0i64;
    while j < 4i64 {
        match m.try_insert(j, f"new-{j}-padding-padding-padding") {
            Ok(o) => match o { Some(v) => println(v), None => println("none") },
            Err(_) => println("oom"),
        }
        j = j + 1i64;
    }
    match m.get(2i64) { Some(v) => println(v), None => println("none") }
}
"#,
        &[
            "val-0-padding-padding-padding",
            "val-1-padding-padding-padding",
            "val-2-padding-padding-padding",
            "val-3-padding-padding-padding",
            "new-2-padding-padding-padding",
        ],
        "asan_map_try_insert_heap_value_overwrite_no_double_free",
    );
}

#[test]
fn asan_map_try_insert_heap_key_duplicate_no_leak() {
    // `Map[String, i64].try_insert` with a DUPLICATE heap key: the incoming
    // key is deep-copied (owned-param defensive copy), but on the update
    // path the map keeps its stored key and does NOT adopt the incoming
    // one — so the deep-copied buffer is orphaned and must be freed exactly
    // once (the no-adopt leak fix, B-2026-06-20-9 sibling). LSan flags the
    // leak if the free is missing; ASan flags a double-free if it aliases
    // the map's stored key. Every key uses the SAME literal so every insert
    // after the first is an update.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut i: i64 = 0i64;
    while i < 5i64 {
        let k: String = f"stable-key-padding-padding";
        match m.try_insert(k, i) {
            Ok(o) => match o { Some(v) => println(v), None => println("fresh") },
            Err(_) => println("oom"),
        }
        i = i + 1;
    }
    println(m.len());
}
"#,
        &["fresh", "0", "1", "2", "3", "1"],
        "asan_map_try_insert_heap_key_duplicate_no_leak",
    );
}

#[test]
fn asan_set_try_insert_heap_element_duplicate_no_leak() {
    // `Set[String].try_insert` with a DUPLICATE heap element: same no-adopt
    // contract as Set.insert (B-2026-06-20-12) on the fallible path — the
    // deep-copied incoming element must be freed once on the duplicate
    // branch, never aliasing the stored element.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut s: Set[String] = Set.new();
    let mut i: i64 = 0i64;
    while i < 5i64 {
        let e: String = f"stable-elem-padding-padding";
        match s.try_insert(e) {
            Ok(b) => println(b),
            Err(_) => println("oom"),
        }
        i = i + 1;
    }
    println(s.len());
}
"#,
        &["true", "false", "false", "false", "false", "1"],
        "asan_set_try_insert_heap_element_duplicate_no_leak",
    );
}

#[test]
fn asan_sorted_map_ordered_methods_no_leak() {
    // B-2026-07-18-1: the ordered-only `SortedMap` codegen methods. Exercises
    // the memory behavior of my new code on its CLEAN paths: min/max/floor/
    // ceiling over i64 keys/values (sorted-keys scratch alloc/free + Option
    // construction — inline 2-word payload, no boxing) plus `range` over
    // STRING keys/values (deep-cloned (String,String) tuples into the result
    // Vec — LSan flags a leak if a clone / the sorted-key scratch is unfreed,
    // ASan a double-free if a clone aliases a map buffer). Looped so any
    // imbalance accumulates. The String min/max/floor/ceiling path (a boxed
    // Option[(String,String)] payload consumed via a whole-tuple `Some(kv)`
    // binding) was the driver for B-2026-07-18-15 (now FIXED: the box drop
    // runs the tuple's per-element inner-heap drop) — it is exercised here
    // too, so this test now also gates that fix. (The narrow residual — an
    // *unbound* `Some(_)` boxed heap-tuple, which carries no static type via
    // its wildcard pattern — stays deferred; spike §1, oversized-enum-payload.)
    assert_clean_asan_run(
        r#"
fn main() {
    let mut n: i64 = 0i64;
    while n < 3i64 {
        let mut m: SortedMap[i64, i64] = SortedMap.new();
        let _ = m.insert(5i64, 50i64);
        let _ = m.insert(1i64, 10i64);
        let _ = m.insert(3i64, 30i64);
        match m.min() { Some(kv) => println(f"{kv.0}={kv.1}"), None => println("n") }
        match m.max() { Some(kv) => println(f"{kv.0}={kv.1}"), None => println("n") }
        match m.floor(4i64) { Some(kv) => println(f"{kv.0}"), None => println("n") }
        match m.ceiling(2i64) { Some(kv) => println(f"{kv.0}"), None => println("n") }
        let ri = m.range(2i64, 5i64);
        println(f"{ri.len()}");

        let mut s: SortedMap[String, String] = SortedMap.new();
        let _ = s.insert(f"kb-{n}-pad-pad", f"vb-{n}-pad-pad");
        let _ = s.insert(f"ka-{n}-pad-pad", f"va-{n}-pad-pad");
        let _ = s.insert(f"kc-{n}-pad-pad", f"vc-{n}-pad-pad");
        // Whole-tuple `Some(kv)` consumption of the boxed Option[(String,String)]
        // payload — the B-2026-07-18-15 leak shape.
        match s.min() { Some(kv) => println(f"{kv.0}={kv.1}"), None => println("n") }
        match s.max() { Some(kv) => println(f"{kv.0}={kv.1}"), None => println("n") }
        match s.floor(f"kbb-{n}-pad") { Some(kv) => println(f"{kv.0}"), None => println("n") }
        match s.ceiling(f"kaa-{n}-pad") { Some(kv) => println(f"{kv.0}"), None => println("n") }
        let rs = s.range(f"ka-{n}-pad-pad", f"kb-{n}-pad-pad");
        println(f"{rs.len()}");
        n = n + 1i64;
    }
}
"#,
        &[
            "1=10",
            "5=50",
            "3",
            "3",
            "2",
            "ka-0-pad-pad=va-0-pad-pad",
            "kc-0-pad-pad=vc-0-pad-pad",
            "kb-0-pad-pad",
            "kb-0-pad-pad",
            "2",
            "1=10",
            "5=50",
            "3",
            "3",
            "2",
            "ka-1-pad-pad=va-1-pad-pad",
            "kc-1-pad-pad=vc-1-pad-pad",
            "kb-1-pad-pad",
            "kb-1-pad-pad",
            "2",
            "1=10",
            "5=50",
            "3",
            "3",
            "2",
            "ka-2-pad-pad=va-2-pad-pad",
            "kc-2-pad-pad=vc-2-pad-pad",
            "kb-2-pad-pad",
            "kb-2-pad-pad",
            "2",
        ],
        "asan_sorted_map_ordered_methods_no_leak",
    );
}

#[test]
fn asan_shared_enum_map_payload_move_out_no_double_free() {
    // B-2026-07-08-22: a `shared enum` variant carrying an owning heap
    // payload (`Full(Map[K,V])` / `Full(Set[T])`), matched with a binding that
    // MOVES the payload out, double-freed it — once via the moved binding's
    // scope-exit cleanup, once via the enum box's rc-drop (which frees the Map
    // handle unconditionally). The match-arm move must zero the handle word in
    // the box so the rc-drop's free no-ops on the null handle. Loops so a leak
    // or double-free is observable; the `Empty`/`_`-arm drops exercise the
    // no-move path (must still free exactly once — no leak).
    assert_clean_asan_run(
        r#"
shared enum Store { Empty, Full(Map[i64, u64]) }
fn build(k: i64) -> Store {
    let mut m: Map[i64, u64] = Map.new();
    let _ = m.insert(k, 9u64);
    let _ = m.insert(k + 1i64, 10u64);
    Store.Full(m)
}
fn drop_no_move(k: i64) -> i64 {
    let x = build(k);
    match x { Store.Full(_) => 99i64, Store.Empty => 0i64 }
}
fn main() {
    let mut i: i64 = 0i64;
    while i < 40i64 {
        let s = build(i);
        match s {
            Store.Full(m) => { println(m.len()); }
            Store.Empty => { println(0); }
        }
        let _ = drop_no_move(i);
        i = i + 1;
    }
}
"#,
        &["2"; 40],
        "asan_shared_enum_map_payload_move_out_no_double_free",
    );
}

#[test]
fn asan_heap_enumerate_map_collect_no_double_free() {
    // B-2026-07-04-4: `<Vec[String]>.iter().enumerate().map(|p| …).collect()`
    // — a heap `enumerate` whose `(i64, String)` tuple flows into a terminal
    // `map`. The desugar binds the tuple DIRECTLY to the map's param (single
    // owning binding, no aliasing `let p = __ietup` copy), and the source
    // loop var gets a synthetic name so it can't collide with that param.
    // The map pushes a value transformed from the tuple. Exercises the
    // headline `map(|p| p.1)` (extract the heap component into `Vec[String]`),
    // a POD-producing `map(|p| p.0 + p.1.len())`, and `skip().enumerate().map`
    // — reading an element each round to expose any double-free/UAF.
    assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "alpha-enum-map-payload-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "bravo-enum-map-payload-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "charlie-enum-map-payload-cccccccccccccccccc".to_string()
        ];
        let a: Vec[String] = w.iter().enumerate().map(|p| p.1).collect();
        let b: Vec[i64] = w.iter().enumerate().map(|p| p.0 + p.1.len()).collect();
        let c: Vec[String] = w.iter().skip(1i64).enumerate().map(|p| p.1).collect();
        let a2: String = a[2].clone();
        let c0: String = c[0].clone();
        println(f"{a.len()} {a2} {b[0]} {b[2]} {c.len()} {c0}");
        round = round + 1i64;
    }
}
"#,
            [
                "3 charlie-enum-map-payload-cccccccccccccccccc 43 45 2 bravo-enum-map-payload-bbbbbbbbbbbbbbbbbbbb",
            ]
            .repeat(40)
            .as_slice(),
            "asan_heap_enumerate_map_collect_no_double_free",
        );
}

#[test]
fn asan_b04_4_heap_enumerate_downstream_map_collect_no_double_free() {
    // B-2026-07-04-4 case D and siblings: a HEAP `enumerate` whose tuple
    // reaches a `map(|p| p.1)` AFTER a passthrough stage (`take`/`skip`) or a
    // `filter`. The `Enumerate` arm now searches PAST `take`/`skip`/`step_by`
    // to bind the tuple directly to the map's param, so the map extracts the
    // String field from the SINGLE owning binding — no `let p = __ietup`
    // whole-tuple bit-copy (the previous double-free). Each collects a
    // `Vec[String]`; reads an element each round.
    assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "alpha-b044-payload-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "bravo-b044-payload-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "charlie-b044-payload-cccccccccccccccc".to_string(),
            "delta-b044-payload-dddddddddddddddddddd".to_string()
        ];
        let fm: Vec[String] = w.iter().enumerate().filter(|p| p.0 > 0i64).map(|p| p.1).collect();
        let tm: Vec[String] = w.iter().enumerate().take(2i64).map(|p| p.1).collect();
        let sm: Vec[String] = w.iter().enumerate().skip(1i64).map(|p| p.1).collect();
        let fm2: String = fm[2].clone();
        let tm0: String = tm[0].clone();
        let sm2: String = sm[2].clone();
        println(f"{fm.len()}:{fm2} {tm.len()}:{tm0} {sm.len()}:{sm2}");
        round = round + 1i64;
    }
}
"#,
            [
                "3:delta-b044-payload-dddddddddddddddddddd 2:alpha-b044-payload-aaaaaaaaaaaaaaaaaaaa 3:delta-b044-payload-dddddddddddddddddddd",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_4_heap_enumerate_downstream_map_collect_no_double_free",
        );
}

#[test]
fn asan_b05_1_nonterminal_map_tuple_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let mut v: Vec[String] = Vec[
            "b05-map-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "b05-map-bravo-bbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()
        ];
        let r: Vec[(i64, String)] = v.iter().enumerate().map(|p| p).filter(|q| q.0 >= 0i64).collect();
        println(f"{r.len()} {r[0i64].0} {v.len()}");
        round = round + 1i64;
    }
}
"#,
        ["2 0 2"].repeat(30).as_slice(),
        "asan_b05_1_nonterminal_map_tuple_no_double_free",
    );
}

#[test]
fn asan_b05_1_retuple_map_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let mut v: Vec[String] = Vec[
            "b05-retup-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "b05-retup-bravo-bbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()
        ];
        let r: Vec[(i64, String)] = v.iter().enumerate().map(|p| (p.0, p.1)).filter(|q| q.0 >= 0i64).collect();
        println(f"{r.len()} {r[0i64].0} {r[1i64].0} {v.len()}");
        round = round + 1i64;
    }
}
"#,
        ["2 0 1 2"].repeat(30).as_slice(),
        "asan_b05_1_retuple_map_no_double_free",
    );
}

#[test]
fn asan_b04_2_flat_map_adaptor_outer_heap_no_leak() {
    // B-2026-07-04-2 sub-part 1 (flat_map adaptor-carrying outer): an outer
    // that carries its own adaptor (`a.iter().filter(g).flat_map(|p| p.iter())
    // .collect()`) pre-collects the outer to a Vec[Vec[String]] temp, then
    // reuses the identity flat_map. The temp is dropped at block exit; each
    // flattened element clones into the result. 30x heap payloads.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let a: Vec[Vec[String]] = Vec[
            Vec["flat-outer-alpha-aaaaaaaaaaaaaaaaaaaaaa".to_string(),
                "flat-outer-bravo-bbbbbbbbbbbbbbbbbbbbbb".to_string()],
            Vec["flat-outer-charlie-cccccccccccccccccccc".to_string()]
        ];
        let r: Vec[String] = a.iter().filter(|v| v.len() > 0i64).flat_map(|p| p.iter()).collect();
        println(f"{r.len()} {r[0i64]} {r[2i64]}");
        round = round + 1i64;
    }
}
"#,
        ["3 flat-outer-alpha-aaaaaaaaaaaaaaaaaaaaaa flat-outer-charlie-cccccccccccccccccccc"]
            .repeat(30)
            .as_slice(),
        "asan_b04_2_flat_map_adaptor_outer_heap_no_leak",
    );
}

#[test]
fn asan_b04_2_named_fn_map_collect_no_leak() {
    // B-2026-07-04-2 sub-part 2: a NAMED-FUNCTION `map` arg over a heap
    // source. The fix wraps `<fn>` in a synthetic body `<fn>(p)`, so each
    // element flows through `tag`/`nlen` and is pushed. `tag(s) -> s` returns
    // the String (entry-copied under caller-retains), `nlen(s) -> s.len()`
    // reads it; `.iter()` borrows, so the source Vec survives. Loops 40× with
    // >=45-byte payloads for LSan reachability; reads a mapped element each
    // round to expose any double-free/UAF.
    assert_clean_asan_run(
        r#"
fn tag(s: String) -> String { s }
fn nlen(s: String) -> i64 { s.len() }
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "named-fn-map-payload-alpha-aaaaaaaaaaaaaaaaaa".to_string(),
            "named-fn-map-payload-bravo-bbbbbbbbbbbbbbbbbb".to_string(),
            "named-fn-map-payload-charlie-cccccccccccccccc".to_string()
        ];
        let mapped: Vec[String] = w.iter().map(tag).collect();
        let lens: Vec[i64] = w.iter().map(nlen).collect();
        let m1: String = mapped[1i64].clone();
        println(f"{mapped.len()} {lens[0]} {lens[2]} {w.len()} {m1}");
        round = round + 1i64;
    }
}
"#,
        ["3 45 45 3 named-fn-map-payload-bravo-bbbbbbbbbbbbbbbbbb"]
            .repeat(40)
            .as_slice(),
        "asan_b04_2_named_fn_map_collect_no_leak",
    );
}

#[test]
fn asan_b04_2_flat_map_heap_collect_no_leak() {
    // B-2026-07-04-2 sub-part 1 (flat_map): `<outer>.flat_map(|v|
    // v.iter()).collect()` over a HEAP `Vec[Vec[String]]`. The nested-loop
    // lowering `for v in outer { for x in v.iter() { acc.push(x) } }`
    // iterates and clones on `push`, so the nested source survives and the
    // flattened accumulator owns independent copies — no leak, no
    // double-free. 40× with ≥44-byte payloads for LSan reachability; reads a
    // flattened element each round.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let xs: Vec[Vec[String]] = Vec[
            Vec["flat-map-payload-alpha-aaaaaaaaaaaaaaaaaaaa".to_string(),
                "flat-map-payload-bravo-bbbbbbbbbbbbbbbbbbbb".to_string()],
            Vec["flat-map-payload-charlie-cccccccccccccccccc".to_string()]
        ];
        let r: Vec[String] = xs.iter().flat_map(|v| v.iter()).collect();
        let r2: String = r[2i64].clone();
        println(f"{r.len()} {xs.len()} {r2}");
        round = round + 1i64;
    }
}
"#,
        ["3 2 flat-map-payload-charlie-cccccccccccccccccc"]
            .repeat(40)
            .as_slice(),
        "asan_b04_2_flat_map_heap_collect_no_leak",
    );
}

/// B-2026-07-04-9(a): entry-copy-THEN-whole-drop of a `Vec[struct-with-heap]`
/// FIELD double-frees (exit 133 / ASAN). A struct `AttrN` with a
/// `Vec[ArgN]` field (`ArgN` owning `Option[String]`/`Option[shared]`) is
/// passed BY VALUE to `count(a)` — which is copy-supported, so `a` is
/// entry-copied — reads `a.args.len()`, and the copy is WHOLE-dropped at
/// return (no element-by-element consume). The prior entry-copy was a
/// SHALLOW bit-copy of the `Vec[ArgN]` field, so the callee's whole-drop and
/// the caller's for-loop element drop free the SAME element buffers. Distinct
/// from `asan_attr_node_list_drop_consume_and_plain`, which consumes the args
/// Vec element-by-element (self-balancing) — the two must BOTH stay clean:
/// the whole-drop path needs an element-DEEP entry-copy, and that copy must
/// NOT strand the consume path's source drain (the regression that reverted
/// two prior attempts). Payloads ≥36 B for LSan reachability. Run:
/// `scripts/lsan-local.sh "b04_9a_vec_struct_field_entrycopy_wholedrop"`.
#[test]
fn asan_b04_9a_vec_struct_field_entrycopy_wholedrop_no_double_free() {
    assert_clean_asan_run(
        r#"
shared enum Val { Nothing, Ident(String), Num(i64) }
struct ArgN { name: Option[String], value: Option[Val] }
struct AttrN { path: Vec[String], args: Vec[ArgN], string_value: Option[String] }

// Entry-copy-THEN-whole-drop: `a` is entry-copied, `a.args.len()` read, then
// the copy is WHOLE-dropped at return while the caller's for-loop element `a`
// also drops. A shallow field bit-copy => the same element buffers freed twice.
fn count(a: AttrN) -> i64 { a.args.len() }

fn build() -> Vec[AttrN] {
    let mut v: Vec[AttrN] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut args: Vec[ArgN] = Vec.new();
        args.push(ArgN {
            name: Some("note_argument_name_key_gamma_ccccccccccc".to_string()),
            value: Some(Val.Ident("clone_derive_identifier_value_ddddddddd".to_string())),
        });
        args.push(ArgN { name: None, value: Some(Val.Num(42)) });
        let mut path: Vec[String] = Vec.new();
        path.push("diagnostic_namespace_segment_alpha_aaaaa".to_string());
        v.push(AttrN {
            path: path,
            args: args,
            string_value: Some("string_value_payload_epsilon_eeeeeeeeee".to_string()),
        });
        i = i + 1;
    }
    v
}

fn main() {
    let items = build();
    let mut total = 0;
    for a in items { total = total + count(a); }
    println(total);
}
"#,
        &["12"],
        "b04_9a_vec_struct_field_entrycopy_wholedrop",
    );
}

/// B-2026-09-12-28 — the MEMORY twin of
/// `e2e_map_get_wide_enum_payload_survives_stack_boxing`.
///
/// A `Map.get` on a value type wider than the 3-word `Option` area is
/// heap-boxed; for a fresh-temp scrutinee that box now comes from an
/// entry-block alloca instead, and `track_freshtemp_boxed_enum_scrutinee`
/// declines to queue its free. Two failures live here and neither is
/// visible in the printed answer:
///
///  - the suppression MISSES and the alloca is passed to `free()` — on
///    macOS libmalloc that is an abort in `mfm_free`, and under ASAN an
///    attempting-free-on-address-which-was-not-malloc'd report;
///  - the suppression fires for a box that was NOT stack-allocated, and
///    the box leaks (or its payload is freed twice through the binding).
///
/// The cells walk the arms that differ in who ends up owning the payload:
/// read-only, moved into a longer-lived local, moved into a `mut ref`
/// accumulator that outlives the construct, unbound, and a miss.
#[test]
fn asan_map_get_stack_boxed_payload_has_one_owner() {
    const H: &str = "enum B { S(String), C }\n\
             struct Wide { a: String, b: String, n: i64 }\n\
             enum W { V(Wide), E }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn mk() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn fill(m: mut ref Map[String, B]) { let _ = m.insert(\"k\", B.S(mk())) }\n";
    for (label, body, want) in [
            // Read-only arm in a LOOP — the kata:288 shape. A leaked box grows
            // without bound; a double-freed one aborts on the second pass.
            (
                "read-only-in-a-loop",
                "fn main() {\n\
                 \x20  let mut m: Map[String, B] = Map.new(); fill(mut m);\n\
                 \x20  let mut i = 0; let mut n = 0;\n\
                 \x20  while i < 4 {\n\
                 \x20    match m.get(\"k\") { None => {} Some(B.S(w)) => { n = n + w.len() } Some(B.C) => {} }\n\
                 \x20    i = i + 1 }\n\
                 \x20  println(f\"n:{n}\"); println(\"end\") }\n",
                vec!["end"],
            ),
            // The payload MOVES into a local that outlives the construct.
            (
                "moved-into-outliving-local",
                "fn main() {\n\
                 \x20  let mut m: Map[String, B] = Map.new(); fill(mut m);\n\
                 \x20  let mut held = String.new();\n\
                 \x20  match m.get(\"k\") { None => {} Some(B.S(w)) => { held = w } Some(B.C) => {} }\n\
                 \x20  println(f\"h:{held.len()}\"); println(\"end\") }\n",
                vec!["end"],
            ),
            // ...and into a `mut ref` accumulator, the escape shape.
            (
                "moved-into-mut-ref-acc",
                "fn take(m: ref Map[String, B], acc: mut ref Vec[String]) {\n\
                 \x20  match m.get(\"k\") { None => {} Some(B.S(w)) => { acc.push(w) } Some(B.C) => {} } }\n\
                 fn main() {\n\
                 \x20  let mut m: Map[String, B] = Map.new(); fill(mut m);\n\
                 \x20  let mut acc: Vec[String] = [];\n\
                 \x20  take(ref m, mut acc);\n\
                 \x20  println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                vec!["len:1", "end"],
            ),
            // Payload never bound — nothing takes the inner heap.
            (
                "unbound-payload",
                "fn main() {\n\
                 \x20  let mut m: Map[String, B] = Map.new(); fill(mut m);\n\
                 \x20  match m.get(\"k\") { None => println(\"n\"), Some(_) => println(\"s\") }\n\
                 \x20  println(\"end\") }\n",
                vec!["s", "end"],
            ),
            // A MISS boxes nothing at all — the suppression must not disarm an
            // unrelated drop on the way past.
            (
                "miss-boxes-nothing",
                "fn main() {\n\
                 \x20  let mut m: Map[String, B] = Map.new(); fill(mut m);\n\
                 \x20  match m.get(\"absent\") { None => println(\"none\"), Some(B.S(w)) => println(w), Some(B.C) => println(\"c\") }\n\
                 \x20  println(\"end\") }\n",
                vec!["none", "end"],
            ),
            // A payload wider than one box, read repeatedly.
            (
                "wider-payload-repeated",
                "fn main() {\n\
                 \x20  let mut wm: Map[String, W] = Map.new();\n\
                 \x20  let _ = wm.insert(\"w\", W.V(Wide { a: mk(), b: mk(), n: 7 }));\n\
                 \x20  let mut i = 0;\n\
                 \x20  while i < 3 {\n\
                 \x20    match wm.get(\"w\") { None => {} Some(W.V(x)) => { println(f\"n:{x.n}\") } Some(W.E) => {} }\n\
                 \x20    i = i + 1 }\n\
                 \x20  println(\"end\") }\n",
                vec!["n:7", "end"],
            ),
        ] {
            let src = format!("{H}{body}");
            assert_clean_asan_run(&src, &want, label);
        }
}

// ── B-2026-06-12-5: push_str of a fresh-owned String RANGE-SLICE temp ──
//
// `buffer.push_str(src[a..b])` — the lexer's idiomatic zero-copy token-text
// shape — passes a `String[a..b]` range-index slice, which `compile_index`
// → `compile_string_slice` lowers to a *freshly* `karac_string_slice`-
// allocated owned `{ptr,len,cap}` (cap > 0), exactly like `.substring(a,b)`.
// But a range slice is an `Index`, not a `Call`/`MethodCall`, so the pre-fix
// `expr_yields_fresh_owned_temp` gate missed it and `free_fresh_owned_str_arg`
// never fired — the slice buffer leaked once per call (measured 34 MiB at 2M
// iters vs 2.5 MiB clean). The fix broadens the gate with
// `expr_is_fresh_owned_string_slice`; this run guards that free against
// double-free / UAF (the `cap > 0` guard + place-safe gate keep a borrowed
// view or a `ref String` identifier untouched).

#[test]
fn asan_freshtemp_mapset_len_frees_its_handle() {
    // B-2026-08-18-26 — `mk_set().len()` reached codegen's fresh-temp
    // Map/Set path only after `len`/`is_empty` were added to the
    // typechecker's side-table roster. That path is also what DROP-TRACKS
    // the handle, so the fix has a memory obligation as well as a
    // correctness one: before it the handle was never registered (leak
    // territory), and a fix that registered it twice would double-free.
    //
    // IN A LOOP so the accounting cannot round away, and with a
    // `Map[String, i64]` alongside the scalar `Set` because a String key
    // gives the handle per-entry heap of its own to free. The bound
    // receiver on the last two lines is the control: that spelling always
    // worked, so if it starts failing the fix broke the path it reused.
    assert_clean_asan_run(
        r#"
fn mk_set(n: i64) -> Set[i64] {
    let mut s: Set[i64] = Set.new();
    let mut i = 0;
    while i < n { s.insert(i); i = i + 1; }
    return s;
}

fn mk_map(n: i64) -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    let mut i = 0;
    while i < n { m.insert(f"k{i}", i); i = i + 1; }
    return m;
}

fn main() {
    let mut total = 0;
    let mut k = 0;
    while k < 8 {
        total = total + mk_set(3).len();
        total = total + mk_map(2).len();
        if mk_set(3).is_empty() { total = total + 100; }
        if mk_map(2).is_empty() { total = total + 100; }
        k = k + 1;
    }
    let bound = mk_map(4);
    println(total);
    println(bound.len());
}
"#,
        &["40", "4"],
        "freshtemp_mapset_len_frees_its_handle",
    );
}

#[test]
fn asan_freshtemp_map_get_no_double_free() {
    // Slice 3d: `make_map().get(k)` on a fresh-temp `Map[i64,i64]` receiver
    // in a loop. `get` returns `Option[ref V]` borrowing a value slot inside
    // the map handle, which the `__mrecv_tmp` `FreeMapHandle` frees at frame
    // exit. Two hazards: (1) DOUBLE-FREE — the `Some(v)` arm binds a borrow
    // (`scrutinee_is_borrow_call` must suppress its independent drop for a
    // temp receiver); (2) LEAK — the whole map handle must be freed once per
    // iteration. macOS ASAN catches (1); Linux LSan catches (2). The loop
    // forces per-iteration accumulation.
    assert_clean_asan_run(
        r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
    m.insert(2_i64, 200_i64);
    m.insert(3_i64, 300_i64);
    return m;
}

fn main() {
    let mut i = 1;
    while i < 4 {
        match make_map().get(i) {
            Some(v) => println(v),
            None => println(0_i64),
        };
        i = i + 1;
    };
}
"#,
        &["100", "200", "300"],
        "freshtemp_map_get_no_double_free",
    );
}

#[test]
fn asan_map_get_unwrap_struct_heap_value_no_double_free() {
    // B-2026-07-20-7: `let a = m.get(k).unwrap()` on a Map whose VALUE is a
    // user STRUCT owning a heap `String` field. The shallow struct copy the
    // get materializes aliases the map's stored String; registering an owned
    // scope-exit struct-drop on the binding freed that buffer a SECOND time
    // against the map's own value-drop → double-free (`free(): double free
    // detected`, caught by macOS ASAN) — and a mis-fired elision would leak
    // the map's buffer (Linux LSan). Now `is_nonshared_struct_value_map_get_-
    // unwrap` borrow-elides the binding (map stays sole owner). Rebuild +
    // drop the map each iteration so any per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
struct Acct { name: String, txns: i64 }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let mut m: Map[String, Acct] = Map.new();
        m.insert("a".to_string(), Acct { name: "alice".to_string(), txns: 2 });
        m.insert("b".to_string(), Acct { name: "bobby".to_string(), txns: 3 });
        let a = m.get("a").unwrap();
        let b = m.get("b").unwrap();
        acc = acc + a.name.len() + a.txns + b.name.len() + b.txns;
        i = i + 1;
    }
    println(acc);
}
"#,
        &["600"],
        "map_get_unwrap_struct_heap_value_no_double_free",
    );
}

#[test]
fn asan_discarded_map_remove_shared_value_no_leak() {
    // B-2026-07-19-16: a DISCARDED `m.remove(k);` on a `Map[K, shared V]`
    // leaked the removed value's RC box — the runtime tombstones the slot
    // and hands the value's ref back in `Some(old)`, and no discard
    // tracker released it. Now `try_track_discarded_shared_option` queues
    // the tag-guarded `RcDecOption` at the `;`. Also covers the guards
    // around it: a DISPLACING `m.insert(k, v2);` must NOT get a second
    // dec (the insert lowering releases the old value inline — an extra
    // dec would double-free), an absent-key remove is a `None` no-op, and
    // an UNANNOTATED `let r = m.remove(k)` registers its scope-exit
    // release via the new map-receiver leg (case b2). Loop so any
    // per-iteration imbalance accumulates under LSan/ASAN.
    assert_clean_asan_run(
        r#"
shared struct DNode { key: i64, val: i64 }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let mut m: Map[i64, DNode] = Map.new();
        m.insert(1, DNode { key: 1, val: 10 });
        m.insert(1, DNode { key: 1, val: 11 });
        m.insert(2, DNode { key: 2, val: 20 });
        m.remove(1);
        m.remove(99);
        let removed = m.remove(2);
        match removed {
            Some(n) => { acc = acc + n.val; }
            None => { acc = acc - 1; }
        }
        acc = acc + m.len();
        i = i + 1;
    }
    println(acc);
}
"#,
        &["800"],
        "discarded_map_remove_shared_value",
    );
}

/// B-2026-08-05-33 predicate (b) — a MAP-BEARING struct passed by value.
///
/// `make_aggregate_param_callee_owned` declined the entry copy (a Map field
/// cannot be duplicated by the outer-buffer copy, which its own comment
/// records as "left on caller-retains"), and the caller had ALREADY
/// retracted its drop for the move. The value was then owned by nobody:
/// 25,360 B over 40 calls on a DEFAULT -O2 build, confirmed under valgrind
/// through the ordinary CLI.
///
/// The fix is ownership by TRANSFER — register the callee's drop without an
/// entry copy, since the caller moved the value in and there is no original
/// left to protect. This test is the leak half; the double-free half is
/// guarded by the whole -O0 sweep, whose failure SET must stay at baseline
/// (an over-broad widening here aborts rather than leaks, and the -O2 suite
/// does not see it — that is recorded in the row).
///
/// Floored per B-2026-08-04-17: runtime-derived payload, and the Map is
/// read through `len()` on the callee side so the entry cannot be a dead
/// allocation at -O2.
#[test]
fn asan_map_bearing_struct_by_value_param_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
struct MapH { m: Map[i64, String] }

fn sink(h: MapH) -> i64 { h.m.len() }

fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        let mut m: Map[i64, String] = Map.new();
        let mut s: String = String.new();
        s.push_str("payload-");
        s.push_str(n.to_string());
        s.push_str("-padded-out-to-force-heap");
        m.insert(1, s);
        let h = MapH { m: m };
        acc = acc + sink(h);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["40"],
        "map_bearing_struct_by_value_param",
        100,
    );
}

#[test]
fn asan_letbound_map_get_moveout_no_double_free() {
    // B-2026-07-09-13: `let g = m.get(k); match g { Some(v) => <move v> }`.
    // `Map.get` returns an `Option[V]` whose String payload ALIASES the
    // bucket's stored value; moving `v` out and dropping it (here via
    // `println` consuming the returned String) freed that buffer a second
    // time against the Map's own value drop (`karac_map_free_with_drop_vec`)
    // — a double-free the DIRECT `match m.get(k)` form was already protected
    // from, but the intermediate `let g` binding hid the alias property.
    // `borrow_accessor_let_payload` re-admits the binding into the
    // clone-on-escape protection so the escaping payload is an independent
    // buffer. Covers the bare-String value, a `#[derive(Hash)]`-struct KEY
    // (the shape that first surfaced the glibc tcache abort), and the
    // `if let` sibling; looped so any per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct P { x: i64, y: i64 }
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(1i64, f"val-{i}-padding-padding-padding");
        m.insert(2i64, f"other-{i}-padding-padding-padding");
        let g = m.get(1i64);
        match g {
            Some(v) => println(v),
            None => println("miss"),
        };
        let mut sm: Map[P, String] = Map.new();
        sm.insert(P { x: 1i64, y: 2i64 }, f"struct-{i}-padding-padding-padding");
        sm.insert(P { x: 3i64, y: 4i64 }, f"skey-{i}-padding-padding-padding");
        let sg = sm.get(P { x: 1i64, y: 2i64 });
        if let Some(v) = sg {
            println(v);
        };
        i = i + 1;
    }
}
"#,
        &[
            "val-0-padding-padding-padding",
            "struct-0-padding-padding-padding",
            "val-1-padding-padding-padding",
            "struct-1-padding-padding-padding",
        ],
        "letbound_map_get_moveout_no_double_free",
    );
}

#[test]
fn asan_freshtemp_map_contains_key_set_contains_no_double_free() {
    // Slice 3d: the `bool`-returning reads — `Map.contains_key` and
    // `Set.contains` — on fresh-temp receivers. No borrow escapes, so the
    // sole obligation is freeing the handle once per call. A `FreeMapHandle`
    // that double-freed would crash here (macOS ASAN); a missing one leaks
    // the handle (Linux LSan).
    assert_clean_asan_run(
        r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(5_i64, 50_i64);
    return m;
}

fn make_set() -> Set[i64] {
    let mut s: Set[i64] = Set.new();
    s.insert(7_i64);
    s.insert(9_i64);
    return s;
}

fn main() {
    let mut i = 0;
    while i < 2 {
        println(make_map().contains_key(5_i64));
        println(make_set().contains(7_i64));
        println(make_set().contains(42_i64));
        i = i + 1;
    };
}
"#,
        &["true", "true", "false", "true", "true", "false"],
        "freshtemp_map_contains_key_set_contains_no_double_free",
    );
}

#[test]
fn asan_freshtemp_map_keys_values_scalar_no_double_free() {
    // Slice 3l: `make_map().keys()` / `.values()` on a fresh-temp
    // `Map[i64,i64]`, looped. `.keys()`/`.values()` materialize a fresh
    // `Vec[i64]`, but the MAP receiver is a fresh owned temp — the fresh-temp
    // Map path materializes it into `__mrecv_tmp` and frees the handle once
    // (`karac_map_free`) at frame exit. The returned Vec is owned by the
    // binding / for-loop. Scalar K/V → no per-entry heap. A leaked handle
    // (Linux LSan) or a double-freed handle (macOS ASAN) is the hazard; the
    // loop re-materializes each pass.
    assert_clean_asan_run(
        r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
    m.insert(2_i64, 200_i64);
    return m;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let ks: Vec[i64] = make_map().keys();
        println(ks.len());
        let mut s = 0;
        for v in make_map().values() { s = s + v; }
        println(s);
        i = i + 1;
    };
}
"#,
        &["2", "300", "2", "300", "2", "300"],
        "freshtemp_map_keys_values_scalar_no_double_free",
    );
}

#[test]
fn asan_freshtemp_map_entries_scalar_no_double_free() {
    // Slice 3m: `make_map().entries()` on a fresh-temp `Map[i64,i64]`, looped.
    // `.entries()` materializes a fresh `Vec[(i64,i64)]`; the MAP receiver is a
    // fresh owned temp freed once (`karac_map_free`) at frame exit. Scalar K/V
    // → no per-entry heap. A leaked handle (Linux LSan) or a double-freed
    // handle (macOS ASAN) is the hazard; the loop re-materializes each pass.
    assert_clean_asan_run(
        r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
    m.insert(2_i64, 200_i64);
    return m;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let es: Vec[(i64, i64)] = make_map().entries();
        println(es.len());
        i = i + 1;
    };
}
"#,
        &["2", "2", "2"],
        "freshtemp_map_entries_scalar_no_double_free",
    );
}

#[test]
fn asan_option_map_undestructured_freed() {
    // `Option[Map[i64,i64]]` dropped without destructuring → the
    // scope-exit `FreeInlineOptionMapPayload` frees the `Some` handle
    // (and its bucket storage) via `emit_free_one_map_handle`.
    assert_clean_asan_run(
        r#"
fn mk() -> Option[Map[i64, i64]] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1i64, 10i64);
    Some(m)
}
fn main() {
    let om = mk();
    println("done");
}
"#,
        &["done"],
        "option_map_undestructured_freed",
    );
}

// ── Set[T]: scope-exit free ─────────────────────────────────────
// Set lowers to Map[T, ()] and shares the karac_map_free cleanup
// action. Verify the FreeMapHandle entry registered by
// compile_set_new_stmt fires on scope exit, and that the Set's
// backing buckets + heap-bearing String elements are released.

#[test]
fn asan_set_new_insert_scope_exit_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(1_i64);
    s.insert(2_i64);
    s.insert(3_i64);
    println(s.len());
}
"#,
        &["3"],
        "set_new_insert_scope_exit_free",
    );
}

#[test]
fn asan_map_clone_independent_handle() {
    // Both maps allocate their own bucket arrays; both must be freed
    // exactly once on scope exit. ASAN catches handle-aliasing (one
    // map pointing at another's storage).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 10_i64);
    m.insert(2_i64, 20_i64);
    let n: Map[i64, i64] = m.clone();
    println(m.len());
    println(n.len());
}
"#,
        &["2", "2"],
        "map_clone_independent_handle",
    );
}

// Regression for the kata 133 (`clone_graph` BFS) perf cliff
// introduced by commit 2bd2dba ("per-iteration cleanup + null-
// guarded RcDec for body-local lets", 2026-05-17). The per-iter
// cleanup fires `rc_dec` on every body-local shared-struct let on
// every loop iteration. `let n = visited.get(k).unwrap()` binds
// an aliasing handle to the Map's stored ref because the runtime
// `karac_map_get` byte-copies the bucket's value pointer without
// touching its refcount, and the let-site's `rhs_yields_fresh_ref`
// path treats MethodCall RHS as "fresh +1 ref" so it skips the
// receive-side rc_inc. Pre-fix, the per-iter dec on `n` drove the
// bucket's ref to zero, freeing the Node while the Map still held
// a dangling pointer. Subsequent allocations reused the freed
// chunk and every subsequent get-then-bind returned a node
// aliasing the latest reuse — observable here as `visited.get(0).val`
// reading the wrong value, and in kata 133 as a ~100× malloc-
// freelist thrash on the next clone_graph call. The fix
// (`compile_map_method` "get" arm) emits an rc_inc on the loaded
// pointer when V is a shared struct, aligning Map.get with the
// calling convention that shared-returning callees hand the
// caller a fresh +1 ref. The Vec[Node] field in the Node type is
// load-bearing for the repro — it bumps the heap allocation to
// 40 bytes, putting it in a freelist bucket the next alloc reuses
// deterministically; with no Vec field the 16-byte Node lands in
// a sparser bucket and the corruption pattern doesn't surface.
//
// B-2026-07-14-3: the neighbor edges form a CHAIN (`node0->..->node4`),
// NOT a ring. The original `(i + 1) % k` wrap built a reference CYCLE
// (`node4 -> node0`); a `Vec[shared Node]` element is an OWNING ref (its
// drop rc-dec's each element), so a cycle is uncollectable under RC and
// leaks by construction — the whole map. That leak surfaced only on arm64
// (LSan) once the reader-binding over-retain below was fixed; on x86 the
// ring stayed reachable through a stale stack slot (an LSan false-negative),
// which is why the ring "passed" there. The chain still exercises the exact
// reader shape this test targets (`let a/b = m.get(k).unwrap()` + a
// `neighbors.push(b)` consume) without conflating it with the separate,
// known RC-cannot-collect-cycles limitation. The reader-binding over-retain
// itself was a codegen double-inc: `m.get(k)` on a shared value rc-inc's the
// aliased bucket ptr (get arm, maps.rs), and the consuming `.unwrap()`
// let-site rc-inc'd it a SECOND time (`rhs_yields_fresh_ref` classed the
// unwrap as non-fresh) — +2 per bind vs one per-iter dec, leaking every
// node. Fixed in `rhs_yields_fresh_ref` (a shared-map-get `.unwrap()` IS
// fresh — get already delivered the +1).
#[test]
fn asan_map_get_shared_value_in_loop_no_alias_collapse() {
    assert_clean_asan_run(
        r#"
shared struct Node {
    val: i64,
    mut neighbors: Vec[Node],
}

fn main() {
    let mut visited: Map[i64, Node] = Map.new();
    let k: i64 = 5;
    for i in 0..k {
        let fresh = Node { val: i, neighbors: Vec.new() };
        let _ = visited.insert(i, fresh);
    }
    // The push-into-Vec[Node] step is what triggers the per-iter
    // cleanup of `a` and `b` to free the Map's only ref; without
    // this second loop the bug doesn't surface because the inserts'
    // per-iter cleanup is already balanced by the existing
    // `suppress_source_vec_cleanup_for_arg` rc_inc.
    for i in 0..k {
        let a = visited.get(i).unwrap();
        if i + 1 < k {
            let b = visited.get(i + 1).unwrap();
            a.neighbors.push(b);
        }
    }
    // Read with let-bindings (not inline chains). The inline
    // `Map.get(k).unwrap().val` shape is covered separately in
    // `asan_map_get_unwrap_field_inline_chain` — together the
    // two tests pin both common reader shapes.
    let n0 = visited.get(0_i64).unwrap();
    let n1 = visited.get(1_i64).unwrap();
    let n4 = visited.get(4_i64).unwrap();
    println(n0.val);
    println(n1.val);
    println(n4.val);
}
"#,
        &["0", "1", "4"],
        "map_get_shared_value_in_loop_no_alias_collapse",
    );
}

#[test]
fn asan_set_clone_independent_handle() {
    // Set[i64] clone — both sets free independent bucket arrays.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(7_i64);
    s.insert(8_i64);
    let t: Set[i64] = s.clone();
    println(s.contains(7_i64));
    println(t.contains(8_i64));
}
"#,
        &["true", "true"],
        "set_clone_independent_handle",
    );
}

/// B-2026-08-06-6 — the sanitizer gate on a moved-out `Map` field.
///
/// The defect was a USE-AFTER-FREE, which is ASAN's own business: the
/// owner's struct drop freed the map storage and the destination then read
/// and freed it again. The codegen twin catches it as a segfault; this
/// catches it as the memory error it is, and would also catch the opposite
/// mistake (over-nulling, which leaks instead).
///
/// NOT VACUOUS (B-2026-08-04-17): opaque `env.args().len()` seed, every map
/// value and tag built from it at runtime, byte-level `contains` read, and
/// 40 iterations x six maps so nothing folds away.
#[test]
fn asan_map_field_move_out_neutralizes_the_source() {
    assert_clean_asan_run_min_allocs(
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
        i = i + 1;
    }
    println(acc);
}
"#,
        &["480"],
        "map_field_move_out_neutralizes_the_source",
        // 40 iterations x six maps, each with heap keys/values plus a tag
        // String; the floor sits far above the 3 of a folded-away run.
        400,
    );
}

/// B-2026-08-06-1 — the MEMORY half of the bare-`T` Map/Set field fix, and
/// the half that matters, since the defect was a pure leak in most shapes
/// and the codegen twin cannot see a leak at all.
///
/// The drop classifier's bare-generic-param rescue loop recognised only
/// String / Vec / VecDeque heads, so a `Box[Map[i64, String]]` field was
/// classified no-heap and NOTHING freed it — 25,830 bytes over 40 rounds,
/// identically at -O2 and -O0. Teaching it the Map/Set head is only half
/// the fix: both move neutralizers classify by the DECLARED field name,
/// which for a bare param is the erased `T`, so a freed-but-not-neutralized
/// field turns the leak into a double free the moment the field or its
/// owner is moved. Both sides now resolve the bare param through the same
/// instantiation the drop was synthesized against.
///
/// This catches BOTH failure directions: under-freeing shows up as the
/// original leak, over-nulling as a leak in the never-moved control, and a
/// disagreement between the two as a use-after-free.
///
/// NOT VACUOUS (B-2026-08-04-17): opaque `env.args().len()` seed, every map
/// key/value and every String built from it at runtime, a byte-level
/// `contains` read, and 40 iterations x eleven containers so nothing folds
/// away.
#[test]
fn asan_bare_generic_param_map_field_is_freed_and_neutralized() {
    assert_clean_asan_run_min_allocs(
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
        i = i + 1;
    }
    println(acc);
}
"#,
        &["1040"],
        "bare_generic_param_map_field_is_freed_and_neutralized",
        // 40 rounds x eleven containers, each with heap keys/values plus
        // the wrapper Strings — a real run measures ~2,240 allocations, so
        // this floor sits far above anything a folded-away run could hit.
        800,
    );
}

/// B-2026-08-09-2 — the ANNOTATED read of a `Map[K, weak V]`.
///
/// `let o: Option[N] = m.get(k)` is the spelling that binds an
/// `Option[shared N]` and so queues a scope-exit `RcDecOption`. The upgrade
/// hands out a box pointer with NO strong retain
/// (`emit_weak_field_upgrade`), so without the balancing acquire that dec
/// has nothing to match and the referent's 24-byte box is lost — measured
/// under valgrind against a compiler with only the `expr_is_weak_field_read`
/// arm reverted, while the STRONG `Map[i64, N]` twin of the same program is
/// clean. The un-annotated `let o = m.get(k)` does NOT reach it (a different
/// binding case, no queued dec), which is why this fixture spells the type.
#[test]
fn asan_map_of_weak_annotated_read_balances_its_acquire_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct N { mut v: i64 }

fn build(seed: i64) -> i64 {
    let mut m: Map[i64, weak N] = Map.new();
    let a: N = N { v: seed };
    m.insert(1i64, a);
    let o: Option[N] = m.get(1i64);
    let p: Option[N] = m.get(1i64);
    let mut hits: i64 = 0;
    match o { Some(x) => { hits = hits + x.v; } None => {} }
    match p { Some(y) => { hits = hits + y.v; } None => {} }
    return hits + a.v;
}

// Overwrites the dead frame the handles lived in, so LSan cannot mistake
// stack residue for reachability (B-2026-08-08-4).
fn churn(n: i64) -> i64 {
    if n <= 0 { return 0; }
    let pad: i64 = n * 3;
    return pad + churn(n - 1);
}

fn main() {
    let seed: i64 = env.args().len();
    let hits = build(seed);
    let noise = churn(2000);
    println(hits + noise - noise);
}
"#,
        // seed == 1: two reads of v==1 plus a.v.
        &["3"],
        "map_of_weak_annotated_read_balances_its_acquire",
        // The referent's box plus the map's own allocation.
        2,
    );
}

#[test]
fn asan_struct_tuple_map_leaf_no_double_free() {
    // #23 (phase-12 self-hosting) — a `Map` leaf inside a tuple inside a struct
    // field, the one corruption-class residual of #21. `Map`s are caller-retains
    // (the origin binding frees; the callee never does), so #21's `NestedTuple`
    // struct drop added a SECOND freer for a Map tuple leaf and a local `Map`
    // folded into a tuple-in-struct-field double-freed (origin binding's
    // `FreeMapHandle` + the struct drop). The fix transfers the handle to the
    // tuple's owner at construction (drop the origin's `FreeMapHandle` — Part B),
    // gives a Map-owning tuple VAR a `TypeExpr`-driven drop (Part A) and
    // suppresses it when moved into a struct field (Part C1), so the struct's
    // drop is the sole freer. Coupled fix: the Map drop's
    // `karac_map_free_with_drop_vec` K/V flags are now derived from the element
    // types — the old hardcoded `(1, 1)` read offset-16 of an 8-byte scalar key
    // as a bogus `cap` and freed the key VALUE as a pointer (corruption on any
    // OCCUPIED `Map[i64, i64]`; `B-2026-06-13-18`, which also fixed the same bug
    // in the regular struct-field Map drop — covered here by the `Sp` field).
    //
    // Loop-stressed matrix: scalar tuple-Map field (origin-suppress + flag fix),
    // by-value consume (caller-retains, no second free), String-key field
    // (drop_key=1, leak if mis-flagged), Vec-value field (drop_val=1), a tuple
    // var moved into a struct field, a bare tuple var owning a Map (Part A drop),
    // and a plain `Map[String, i64]` struct field (regular drop path). The
    // f-string keys keep each entry's heap non-foldable so Linux LSan sees a real
    // leak if any flag/transfer is wrong; the inserts make the scalar maps
    // occupied so the old `(1, 1)` flags would abort here.
    assert_clean_asan_run(
        r#"
struct Hi { m: (Map[i64, i64], i64) }
struct Hs { m: (Map[String, i64], i64) }
struct Hv { m: (Map[i64, Vec[i64]], i64) }
struct Sp { m: Map[String, i64] }
fn ci(p: (Map[i64, i64], i64)) -> i64 { let (mm, n) = p; mm.len() + n }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        // Scalar tuple-Map field, local source, dropped undestructured
        // (#23 double-free + the occupied-scalar (1,1)-flag corruption).
        let mut a: Map[i64, i64] = Map.new();
        a.insert(i, i); a.insert(i + 100, i);
        let hi = Hi { m: (a, i) };

        // Same, but consumed by value (caller-retains; struct drop is sole freer).
        let mut a2: Map[i64, i64] = Map.new();
        a2.insert(i, i);
        let hi2 = Hi { m: (a2, i) };
        acc = acc + ci(hi2.m);

        // String-key tuple-Map field (drop_key=1 — free the key buffers, no leak).
        let mut s: Map[String, i64] = Map.new();
        s.insert(f"k-{i}", i); s.insert(f"j-{i}", i);
        let hs = Hs { m: (s, i) };

        // Vec-value tuple-Map field (drop_val=1 — free the value buffers).
        let mut v: Map[i64, Vec[i64]] = Map.new();
        let mut vv: Vec[i64] = Vec.new(); vv.push(i);
        v.insert(i, vv);
        let hv = Hv { m: (v, i) };

        // Tuple VAR moved into a struct field (Part A drop + Part C1 suppression).
        let mut b: Map[i64, i64] = Map.new();
        b.insert(i, i);
        let pair = (b, i);
        let hi3 = Hi { m: pair };

        // Bare tuple var owning a Map, dropped undestructured (Part A drop).
        let mut d: Map[i64, i64] = Map.new();
        d.insert(i, i);
        let t = (d, i);

        // Plain Map[String,i64] struct field — the regular MapOrSet drop + flag fix.
        let mut c: Map[String, i64] = Map.new();
        c.insert(f"c-{i}", i);
        let sp = Sp { m: c };

        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
        &["done"],
        "struct_tuple_map_leaf_no_double_free",
    );
}

#[test]
fn asan_tuple_index_map_receiver_no_leak() {
    // #26 (phase-12 self-hosting, B-2026-06-14-6) — methods on a Map TUPLE
    // element (`h.m.0.len()` / `.get` / `.insert`) route through a synth
    // identifier aliasing the owning struct's handle slot (the tuple element
    // is GEP'd in place via `field_chain_place_ptr`). The synth must NOT take
    // ownership: the owning `h` is the sole freer of the Map, and reads (len/
    // get/contains_key) borrow the handle while an in-place insert mutates it.
    // Loop-stressed with String keys (heap, non-foldable via f-strings) so
    // Linux LSan catches a leak if the synth mis-registers a second freer, and
    // an in-place insert each iteration so a copy-instead-of-alias would drop
    // the mutation (and leak/UAF the copy).
    assert_clean_asan_run(
        r#"
struct H { m: (Map[String, i64], i64) }
fn mkm(i: i64) -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert(f"k{i}", i); m.insert(f"j{i}", i);
    return m;
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        let mut h = H { m: (mkm(i), i) };
        // Read methods through the tuple element (borrow the handle).
        acc = acc + h.m.0.len();
        if h.m.0.contains_key(f"k{i}") { acc = acc + 1; }
        // In-place mutation through the tuple element.
        h.m.0.insert(f"x{i}", i);
        acc = acc + h.m.0.len();
        acc = acc + h.m.1;
        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
        &["done"],
        "tuple_index_map_receiver_no_leak",
    );
}

#[test]
fn asan_map_local_bind_from_place_no_double_free() {
    // #28 (phase-12 self-hosting, B-2026-06-14-9) — a Map bound to a LOCAL
    // from a place source (`let mm = s.m`) now registers its dispatch
    // side-tables. The binding ALIASES the source handle (Maps are
    // caller-retains), so `mm` must NOT register a second `FreeMapHandle`:
    // the owning `s` is the sole freer. The fix only populates the dispatch
    // tables (`register_var_from_type_expr`), and the let path's
    // `track_map_var` is gated on a fresh-handle RHS (clone/union/…) which a
    // place source is not — so no second cleanup is queued. Loop-stressed with
    // String keys (heap, non-foldable) so Linux LSan catches a leak if the
    // owner's free were suppressed, and ASAN catches a double-free if `mm`
    // wrongly took a cleanup. Includes an in-place mutation through `mm`.
    assert_clean_asan_run(
        r#"
struct S { m: Map[String, i64] }
fn mkm(i: i64) -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert(f"k{i}", i); m.insert(f"j{i}", i);
    return m;
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        let s = S { m: mkm(i) };
        let mut mm = s.m;
        acc = acc + mm.len();
        if mm.contains_key(f"k{i}") { acc = acc + 1; }
        // In-place mutation through the bound local (mutates the shared handle).
        mm.insert(f"x{i}", i);
        acc = acc + mm.len();
        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
        &["done"],
        "map_local_bind_from_place_no_double_free",
    );
}

// ── Map.remove heap-VALUE move-out under churn (2026-06-20) ───
// The VALUE side of the `Map.remove` ownership class: `Map.remove`
// moves the bucket's `{ptr,len,cap}` Vec/String value OUT (bitwise
// copy into the `Some(old)` payload) and tombstones the slot WITHOUT
// freeing it — the match binding becomes the sole owner and frees it
// once at arm exit, while the eventual `karac_map_free_with_drop_vec`
// walks only OCCUPIED slots (the tombstoned ones are skipped). The
// stored-KEY side was fixed in B-2026-06-20-10 (the `drop_key` ABI).
//
// B-2026-06-20-14 logged an INTERMITTENT, seed-flavoured SEGV for
// `asan_match_arm_vec_binding_freed_on_arm_exit` (below) on this same
// value path. A follow-up audit (this batch) found the emitted IR for
// that shape is single-owner and provably correct — one `@free` per
// removed value, guarded by `cap > 0`, and the scope-exit map-free
// touches no tombstoned slot — with no HashMap-order-dependent
// decision anywhere in the path (no shared types are involved). Across
// ~2400 real Linux ASAN+LSan runs (verified positive control) it did
// not reproduce. The original symptom is consistent with a transient
// stale-archive ABI mismatch during the `drop_key` rollout (codegen
// passing the 4th arg to a not-yet-rebuilt 3-arg `karac_map_remove_old`
// → garbage register), not a latent codegen bug. These three churn
// stress cases lock the value path down permanently: large heap churn
// exhausts the ASAN quarantine, so any real double-free/UAF surfaces as
// a live-chunk corruption rather than a benign quarantined catch.

#[test]
fn asan_map_remove_vec_value_moveout_under_churn() {
    // Vec[i64] value, repeatedly grown then removed-and-bound across
    // 200 keys × 20 generations — the core move-out-then-free path at
    // a scale that cycles the allocator.
    assert_clean_asan_run(
        r#"
fn inner() -> i64 {
    let mut bucket: Map[i64, Vec[i64]] = Map.new();
    let mut i = 0i64;
    while i < 200 {
        let mut j = 0i64;
        while j < 20 {
            bucket.entry(i).or_insert(Vec.new()).push(i + j);
            j = j + 1;
        }
        i = i + 1;
    }
    let mut acc = 0i64;
    let mut k = 0i64;
    while k < 200 {
        match bucket.remove(k) {
            Some(indices) => {
                acc = acc + indices.len();
            },
            None => {},
        }
        k = k + 1;
    }
    acc
}
fn main() {
    let mut s = 0i64;
    let mut iter = 0i64;
    while iter < 20 {
        s = s + inner();
        iter = iter + 1;
    }
    println(s);
}
"#,
        &["80000"],
        "map_remove_vec_value_moveout_under_churn",
    );
}

#[test]
fn asan_map_remove_vec_value_indexed_readback() {
    // Read every element of the moved-out Vec INSIDE the arm before it
    // is freed — exercises the reconstructed `{ptr,len,cap}` binding's
    // data buffer for reads, not just `len()`, under heap churn.
    assert_clean_asan_run(
        r#"
fn inner() -> i64 {
    let mut bucket: Map[i64, Vec[i64]] = Map.new();
    let mut i = 0i64;
    while i < 100 {
        let mut j = 0i64;
        while j < 16 {
            bucket.entry(i).or_insert(Vec.new()).push(i * 16i64 + j);
            j = j + 1;
        }
        i = i + 1;
    }
    let mut total = 0i64;
    let mut k = 0i64;
    while k < 100 {
        match bucket.remove(k) {
            Some(indices) => {
                let mut p = 0i64;
                while p < indices.len() {
                    total = total + indices[p];
                    p = p + 1;
                }
            },
            None => {},
        }
        k = k + 1;
    }
    total
}
fn main() {
    let mut s = 0i64;
    let mut iter = 0i64;
    while iter < 20 {
        s = s + inner();
        iter = iter + 1;
    }
    println(s);
}
"#,
        &["25584000"],
        "map_remove_vec_value_indexed_readback",
    );
}

#[test]
fn asan_set_vec_keys_no_leak() {
    // `Set[Vec[i64]]` — Set of vecs. Each inserted Vec is the bucket's
    // KEY; the recursive-drop runtime helper frees each key's data
    // buffer before deallocating the bucket storage.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut s: Set[Vec[i64]] = Set.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(1i64);
    a.push(2i64);
    s.insert(a);
    let mut b: Vec[i64] = Vec.new();
    b.push(3i64);
    s.insert(b);
    println(s.len());
}
"#,
        &["2"],
        "set_vec_keys_no_leak",
    );
}

#[test]
fn asan_set_vec_duplicate_element_dedup_no_leak_no_double_free() {
    // NEW ownership surface opened by the `Set[Vec[T]]` content-dedup fix
    // (B-2026-06-20-15): once two equal-CONTENTS vecs collapse, the second
    // `insert` takes the EXISTS (duplicate) path of `karac_map_insert_old`,
    // which keeps the bucket's existing element and does NOT adopt the
    // incoming one — so the incoming `{ptr,len,cap}` buffer must be freed
    // exactly once on the exists branch (B-2026-06-20-12's diamond), while
    // the bucket's adopted (first) buffer is freed exactly once at set drop.
    // Before this fix the dedup never happened (every insert was a fresh
    // bucket), so this exists-branch path was unreachable for `Set[Vec]`.
    // `b` is a moved local binding: its source scope-exit free is suppressed
    // (so it can't double-free with the exists-branch free) and the vacant
    // case would adopt it (so the exists-branch free can't run there). ≥6
    // i64s ⇒ a ≥48-byte data buffer (LSan misses sub-36-byte reachable
    // buffers). Must be clean under BOTH macOS ASAN (no double-free) and the
    // Linux LSan gate (no leak).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut s: Set[Vec[i64]] = Set.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(601i64); a.push(602i64); a.push(603i64);
    a.push(604i64); a.push(605i64); a.push(606i64);
    s.insert(a);
    let mut b: Vec[i64] = Vec.new();
    b.push(601i64); b.push(602i64); b.push(603i64);
    b.push(604i64); b.push(605i64); b.push(606i64);
    s.insert(b);
    println(s.len());
}
"#,
        &["1"],
        "set_vec_duplicate_element_dedup_no_leak_no_double_free",
    );
}

// ── Vec[Map] / Vec[Set] owned-param defensive copy recurses into
//    map/set handles (Cluster 1) ──
// `emit_vecstr_defensive_copy` deep-copies the OUTER buffer of an owned
// `Vec` param at a retaining consume site, then rewrites each element to
// own its own heap. It recursed String/Vec elements but FLAT-COPIED
// Map/Set elements — the copy and the source aliased the same opaque map
// handles, so both the source's and the copy's scope-exit
// `karac_map_free_with_drop_vec` freed the same map (double-free). Map
// handles aren't LLVM-type-sniffable, but the element TypeExpr is
// available, so the fix routes Map/Set elements through the synthesized
// `karac_clone_<T>` deep-clone per element. Tail-returning the owned
// `Vec[Map]` param is the canonical retaining site: the caller frees the
// moved-in original AND the returned copy.

#[test]
fn asan_vec_map_param_deep_copy_no_double_free() {
    // `Vec[Map[i64, i64]]` owned param tail-returned. Pre-fix the
    // returned copy's element handles aliased the original's maps; both
    // freed at main scope exit → double-free. The read-backs prove the
    // cloned maps carry the same entries.
    assert_clean_asan_run(
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
    match r[1].get(3i64) { Some(x) => println(x), None => println(-1i64) }
}
"#,
        &["2", "10", "30"],
        "vec_map_param_deep_copy_no_double_free",
    );
}

#[test]
fn asan_vec_set_param_deep_copy_no_double_free() {
    // `Vec[Set[i64]]` sibling — Set lowers to `Map[T, ()]`, so the same
    // handle-aliasing double-free applies; the clone routes through
    // `emit_map_clone_fn` with the unit value half.
    assert_clean_asan_run(
        r#"
fn id(v: Vec[Set[i64]]) -> Vec[Set[i64]] {
    v
}

fn main() {
    let mut v: Vec[Set[i64]] = Vec.new();
    let mut s: Set[i64] = Set.new();
    s.insert(1i64);
    s.insert(2i64);
    v.push(s);
    let r = id(v);
    println(r.len());
    println(r[0].contains(1i64));
    println(r[0].contains(9i64));
}
"#,
        &["1", "true", "false"],
        "vec_set_param_deep_copy_no_double_free",
    );
}

#[test]
fn asan_map_entry_or_insert_counter_and_get_or_clean() {
    // Tier D entry write-through end to end: `*m.entry(k).or_insert(0) += 1`
    // builds a frequency table keyed by ≥36-byte Strings (LSan-visible if a
    // key buffer leaked), then `get_or` reads the counts back. The map owns
    // and frees each key exactly once; the scalar counter values carry no
    // heap. No leak / double-free / UAF.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let words = [
        "alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "beta-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ];
    for w in words {
        *m.entry(w.to_string()).or_insert(0_i64) += 1;
    }
    println(m.get_or("alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 0_i64));
    println(m.get_or("beta-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(), 0_i64));
}
"#,
        &["3", "1"],
        "map_entry_or_insert_counter_and_get_or_clean",
    );
}

// ── Residual map-key no-adopt ownership (B-2026-06-20-9) ──
// Map key methods route the key buffer through ownership paths the
// fresh-temp-only handling of `B-2026-06-20-8` missed. `karac_map_entry`
// ADOPTS the key (bit-copies its `{ptr,len,cap}`) only on the VACANT
// insert; `and_modify`'s lookup variant, `get`/`get_or`/`remove`/
// `contains_key`, and `insert` on the EXISTS path never adopt. For a
// moved local binding / owned param / place key on a no-adopt path the
// buffer was orphaned (leak); on `entry`'s vacant path the key was adopted
// AND freed by the un-suppressed source (double-free). The fix mirrors
// `Map.insert`'s consume-site dance in the entry chain (suppress source +
// defensive-copy owned params + free on the no-adopt branch) and adds the
// fresh-temp key free to `get`/`remove`/`contains_key` and the exists-path
// key free to `insert`.

#[test]
fn asan_map_entry_moved_binding_key_no_double_free() {
    // `entry().or_insert` VACANT path with a moved LOCAL String binding.
    // The map adopts the buffer; the source binding's scope-exit free must
    // be suppressed, else both free it (double-free — caught even on macOS
    // ASAN without LSan).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut k = String.new();
    k.push_str("fresh-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    *m.entry(k).or_insert(0_i64) += 1_i64;
    println(m.len());
}
"#,
        &["1"],
        "map_entry_moved_binding_key_no_double_free",
    );
}

#[test]
fn asan_map_entry_moved_binding_key_occupied_no_leak() {
    // `entry().or_insert` OCCUPIED (no-adopt) path with a moved local
    // binding (pre-inserted via `insert`). The entry chain frees the
    // orphaned ≥36-byte key buffer (the source was suppressed).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("dup-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 5_i64);
    let mut k = String.new();
    k.push_str("dup-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    *m.entry(k).or_insert(0_i64) += 1_i64;
    println(m.get_or("dup-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 0_i64));
}
"#,
        &["6"],
        "map_entry_moved_binding_key_occupied_no_leak",
    );
}

#[test]
fn asan_map_entry_owned_param_key_no_double_free() {
    // `entry().or_insert` with an OWNED String PARAM key, exercised on both
    // the vacant (first call) and occupied (second call, same key) paths
    // via a `mut ref Map`. The param key is defensive-copied so the bucket
    // owns a private buffer; the caller frees the original. Vacant: copy
    // adopted, original freed by caller (no double-free). Occupied: copy
    // orphaned → freed by the entry chain, original freed by caller
    // (no leak, no double-free). Churn between calls exposes a UAF if a
    // bucket ever aliased a freed buffer.
    assert_clean_asan_run(
        r#"
fn bump(m: mut ref Map[String, i64], k: String) {
    *m.entry(k).or_insert(0_i64) += 1_i64;
}

fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut a1 = String.new();
    a1.push_str("param-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    bump(mut m, a1);
    let mut a2 = String.new();
    a2.push_str("param-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    bump(mut m, a2);
    let mut churn: Vec[String] = Vec.new();
    let mut i = 0i64;
    while i < 16i64 {
        let mut t = String.new();
        t.push_str("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx");
        churn.push(t);
        i = i + 1;
    }
    println(m.get_or("param-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 0_i64));
}
"#,
        &["2"],
        "map_entry_owned_param_key_no_double_free",
    );
}

#[test]
fn asan_map_entry_and_modify_moved_binding_key_no_leak() {
    // Bare `entry().and_modify` (the lookup-only variant — NEVER adopts)
    // with a moved local binding key, occupied. The entry chain always
    // frees the orphaned key on this path.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("am-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 10_i64);
    let mut k = String.new();
    k.push_str("am-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    m.entry(k).and_modify(|v| { *v += 5_i64; });
    println(m.get_or("am-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 0_i64));
}
"#,
        &["15"],
        "map_entry_and_modify_moved_binding_key_no_leak",
    );
}

#[test]
fn asan_map_get_moved_binding_key_no_leak() {
    // Moved local binding key into `get` (never adopts). `get` does NOT
    // suppress the source, so the source binding's scope-exit free releases
    // the buffer exactly once.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("look-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 9_i64);
    let mut k = String.new();
    k.push_str("look-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    match m.get(k) { Some(x) => println(x), None => println(-1_i64) }
}
"#,
        &["9"],
        "map_get_moved_binding_key_no_leak",
    );
}

#[test]
fn asan_map_get_fresh_temp_key_no_leak() {
    // Fresh-temp (`.clone()`) key into `get`. `get` now frees its
    // fresh-temp key (mirroring `get_or`); pre-fix it leaked one buffer
    // per call.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let base = "clone-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    m.insert(base.clone(), 4_i64);
    match m.get(base.clone()) { Some(x) => println(x), None => println(-1_i64) }
}
"#,
        &["4"],
        "map_get_fresh_temp_key_no_leak",
    );
}

#[test]
fn asan_map_remove_contains_fresh_temp_key_no_leak() {
    // Fresh-temp keys into `contains_key` (present) and `remove` (the
    // incoming key argument), both lookup-only — neither retains the
    // incoming key, so each now frees its fresh-temp key buffer. The
    // `remove` here targets an ABSENT key (miss path) on purpose, to
    // isolate the INCOMING-key residual: the distinct present-key STORED
    // key leak is exercised by the `asan_map_remove_present_*` tests below
    // (closed by the drop-flag ABI in B-2026-06-20-10).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let base = "rc-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    m.insert(base.clone(), 7_i64);
    if m.contains_key(base.clone()) {
        println("present");
    }
    match m.remove("absent-key-bbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()) {
        Some(x) => println(x),
        None => println(-1_i64),
    }
    println(m.len());
}
"#,
        &["present", "-1", "1"],
        "map_remove_contains_fresh_temp_key_no_leak",
    );
}

// ── Present-key remove STORED key/value ownership (B-2026-06-20-10) ──
// Completes the map-key-ownership class B-2026-06-20-9 started. A
// present-key `remove` of a HEAP key tombstones the bucket, and
// `karac_map_free_with_drop_vec` only walks OCCUPIED slots — so the
// bucket's STORED key buffer was orphaned (leak) until the runtime
// learned to free it. `Map.remove` / `Set.remove` lower to
// `karac_map_remove_old`, which now takes a codegen-set `drop_key` flag
// (`llvm_ty_is_vec_struct(key_ty)`) and frees the stored key on the
// tombstone path — the value half is MOVED OUT to the caller, so it is
// never freed here. ≥36-byte keys per the LSan-reachability rule (LSan
// misses short, still-reachable String buffers).

#[test]
fn asan_map_remove_present_heap_key_no_leak() {
    // `Map[String, i64].remove(present)` → Some. The incoming fresh-temp
    // key is freed by the no-adopt path (B-2026-06-20-9); the bucket's
    // STORED String key is freed by the runtime drop-flag (this fix).
    // Pre-fix the stored key buffer leaked under the Linux LSan gate.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("present-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 7_i64);
    match m.remove("present-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()) {
        Some(x) => println(x),
        None => println(-1_i64),
    }
    println(m.len());
}
"#,
        &["7", "0"],
        "map_remove_present_heap_key_no_leak",
    );
}

#[test]
fn asan_map_remove_present_heap_key_and_vec_value_no_leak() {
    // `Map[String, Vec[i64]].remove(present)` → Some(vec). Exercises BOTH
    // sides at once: the STORED String key is freed by the drop-flag
    // (this fix), while the Vec value is MOVED OUT into the match-arm
    // binding and freed exactly once at arm exit (the runtime must NOT
    // free a returned value). Pre-fix the stored key leaked; a naive
    // "free both" runtime change would instead double-free the value.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, Vec[i64]] = Map.new();
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    v.push(20_i64);
    m.insert("vec-value-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), v);
    match m.remove("vec-value-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()) {
        Some(got) => println(got.len()),
        None => println(-1_i64),
    }
    println(m.len());
}
"#,
        &["2", "0"],
        "map_remove_present_heap_key_and_vec_value_no_leak",
    );
}

#[test]
fn asan_map_insert_moved_binding_duplicate_key_no_leak() {
    // Moved local binding key into `insert` on the EXISTS (duplicate-key)
    // path. `karac_map_insert_old` keeps the bucket's existing key and does
    // not adopt the incoming one; `insert` suppressed the source — so the
    // incoming buffer is orphaned and now freed on the exists branch.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut k1 = String.new();
    k1.push_str("ins-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    m.insert(k1, 1_i64);
    let mut k2 = String.new();
    k2.push_str("ins-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    m.insert(k2, 2_i64);
    println(m.len());
    println(m.get_or("ins-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 0_i64));
}
"#,
        &["1", "2"],
        "map_insert_moved_binding_duplicate_key_no_leak",
    );
}

#[test]
fn asan_map_insert_owned_param_duplicate_key_no_leak() {
    // Owned String PARAM key into `insert` on the EXISTS path via a
    // `mut ref Map`. The defensive copy is orphaned on the exists branch
    // and freed there; the caller frees each original param.
    assert_clean_asan_run(
        r#"
fn store(m: mut ref Map[String, i64], k: String) {
    m.insert(k, 1_i64);
}

fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut a1 = String.new();
    a1.push_str("ins-param-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    store(mut m, a1);
    let mut a2 = String.new();
    a2.push_str("ins-param-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    store(mut m, a2);
    println(m.len());
}
"#,
        &["1"],
        "map_insert_owned_param_duplicate_key_no_leak",
    );
}

#[test]
fn asan_map_insert_duplicate_heap_value_discarded_no_leak() {
    // `Map[String, String]`, same key inserted twice with DISCARDED
    // `Option[V]` results. On the second (exists) insert the displaced OLD
    // String value is handed back as a `Some(old)` payload no one holds;
    // the discarded-Option-value cleanup already releases it. Companion to
    // the exists-path KEY free (the incoming duplicate key is freed by the
    // new exists-branch handling). Confirms both the key and the displaced
    // value are released exactly once on a duplicate heap-valued insert.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, String] = Map.new();
    let k = "vkey-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    m.insert(k.clone(), "first-vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv".to_string());
    m.insert(k.clone(), "second-wwwwwwwwwwwwwwwwwwwwwwwwwwww".to_string());
    println(m.get_or(k.clone(), "x".to_string()));
}
"#,
        &["second-wwwwwwwwwwwwwwwwwwwwwwwwwwww"],
        "map_insert_duplicate_heap_value_discarded_no_leak",
    );
}

// ── Set INCOMING-element NO-ADOPT ownership (B-2026-06-20-12) ──
// Completes the map/set key-ownership class: B-2026-06-20-9 (c7b72bd4)
// fixed the INCOMING key for Map's no-adopt paths but never applied it to
// Set (`collections.rs` had zero `free_fresh_owned_str_arg` calls), and
// B-2026-06-20-10 (a1b59c5e) fixed only the STORED element on a present-key
// remove. The remaining gap is the INCOMING element argument: a fresh-owned
// temp (`s.remove("x".to_string())`) or a moved binding on a no-adopt path
// leaked one element buffer per call. Set lowers to `Map[T, ()]`, so these
// arms call `karac_map_remove_old` / `karac_map_contains` / `karac_map_entry`
// (insert). ≥36-byte elements per the LSan-reachability rule (LSan misses
// short, still-reachable String/Vec buffers).

#[test]
fn asan_set_remove_present_fresh_temp_element_no_leak() {
    // `Set[String].remove(present)` with a fresh-temp element. TWO distinct
    // buffers must be freed exactly once: the bucket's STORED element (via
    // the runtime `drop_key` flag, B-2026-06-20-10) and the INCOMING fresh
    // temp (via `free_fresh_owned_str_arg`, this fix). Pre-fix the incoming
    // buffer leaked under the Linux LSan gate.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut s: Set[String] = Set.new();
    s.insert("set-remove-element-aaaaaaaaaaaaaaaaaaaa".to_string());
    if s.remove("set-remove-element-aaaaaaaaaaaaaaaaaaaa".to_string()) {
        println("removed");
    }
    println(s.len());
}
"#,
        &["removed", "0"],
        "set_remove_present_fresh_temp_element_no_leak",
    );
}

#[test]
fn asan_set_contains_present_fresh_temp_element_no_leak() {
    // `Set[String].contains(present)` with a fresh-temp element. The lookup
    // hashes/compares but never retains the incoming element, so the fresh
    // temp must be freed after the call. Pre-fix it leaked one buffer per
    // call (LSan-only).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut s: Set[String] = Set.new();
    s.insert("set-contains-element-aaaaaaaaaaaaaaaaaa".to_string());
    if s.contains("set-contains-element-aaaaaaaaaaaaaaaaaa".to_string()) {
        println("present");
    }
    println(s.len());
}
"#,
        &["present", "1"],
        "set_contains_present_fresh_temp_element_no_leak",
    );
}

#[test]
fn asan_set_insert_moved_binding_duplicate_element_no_leak() {
    // Moved local binding element into `Set[String].insert` on the EXISTS
    // (duplicate) path. `karac_map_insert_old` keeps the bucket's existing
    // element and does NOT adopt the incoming one, while the insert arm
    // suppressed the source binding's scope-exit free — so the incoming
    // buffer is orphaned and now freed on the exists branch.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut s: Set[String] = Set.new();
    let mut a1 = String.new();
    a1.push_str("set-dup-element-aaaaaaaaaaaaaaaaaaaaaaaa");
    s.insert(a1);
    let mut a2 = String.new();
    a2.push_str("set-dup-element-aaaaaaaaaaaaaaaaaaaaaaaa");
    s.insert(a2);
    println(s.len());
}
"#,
        &["1"],
        "set_insert_moved_binding_duplicate_element_no_leak",
    );
}

#[test]
fn asan_set_remove_absent_fresh_temp_vec_element_no_leak() {
    // `Set[Vec[i64]]` sibling on the lookup-only path: confirms the incoming
    // free's vec-struct gate (`free_fresh_owned_str_arg` → `cap > 0` free)
    // also fires for an actual `Vec` element, not just `String`. `make_vec()`
    // returns a fresh-owned temp; `remove` looks it up (ABSENT here — the
    // element `[701..706]` differs in both length and contents from the
    // set's lone `[1]`, so the explicit miss isolates the INCOMING-element
    // residual without depending on content equality, independent of the
    // `Set[Vec]` content-dedup now in place via B-2026-06-20-15) and never
    // retains it, so the returned Vec buffer must be freed after the call.
    // ≥6 i64s ⇒ a ≥48-byte data buffer (LSan misses sub-36-byte reachable
    // buffers). Pre-fix it leaked one buffer per call.
    assert_clean_asan_run(
        r#"
fn make_vec() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(701i64);
    v.push(702i64);
    v.push(703i64);
    v.push(704i64);
    v.push(705i64);
    v.push(706i64);
    v
}

fn main() {
    let mut s: Set[Vec[i64]] = Set.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(1i64);
    s.insert(a);
    if s.remove(make_vec()) {
        println("removed");
    } else {
        println("absent");
    }
    println(s.len());
}
"#,
        &["absent", "1"],
        "set_remove_absent_fresh_temp_vec_element_no_leak",
    );
}

// ── Vec[Map] ownership: a Map moved into a Vec transfers ownership
//    to the Vec (Cluster 1) ──
// The headline bug: a `Map` pushed into a `Vec` aliased a handle still
// owned (and freed at scope exit) by the origin `m` binding, while the
// Vec's own drop never freed map elements. So a `Vec[Map]` built from a
// local map and RETURNED dangled — the local's `FreeMapHandle` freed the
// handle the returned Vec still pointed at (use-after-free on the next
// read; AOT printed `-1` vs interp's correct value). The fix makes the
// Vec OWN its map elements: the push suppresses the source's
// `FreeMapHandle` (ownership transfer) and the Vec drop frees each
// element handle (`track_vec_of_maps_var`). Both halves are required —
// either alone is a leak or a double-free.

#[test]
fn asan_vec_map_returned_from_helper_no_uaf() {
    // `make()` builds a `Vec[Map]` from a local map and returns it; the
    // read in `main` walks the returned map. Pre-fix the local's
    // scope-exit `FreeMapHandle` freed the handle inside `make`, so the
    // read in `main` is a heap-use-after-free (allocation churn between
    // forces the freed chunk's reuse). The value-correctness twin is
    // `tests/codegen.rs::test_e2e_vec_map_returned_from_helper`.
    assert_clean_asan_run(
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
    let mut churn: Vec[Map[i64, i64]] = Vec.new();
    let mut i = 0i64;
    while i < 16i64 {
        let mut c: Map[i64, i64] = Map.new();
        c.insert(99i64, 1i64);
        churn.push(c);
        i = i + 1;
    }
    match v[0].get(1i64) { Some(x) => println(x), None => println(-1i64) }
}
"#,
        &["777"],
        "vec_map_returned_from_helper_no_uaf",
    );
}

#[test]
fn asan_vec_map_push_then_read_same_scope_single_free() {
    // `v.push(m)` then a same-scope read + scope exit. The Vec now owns
    // the handle (push suppressed the source `m`'s `FreeMapHandle`); on
    // scope exit ONLY the Vec's element drop frees it. A missing
    // suppression here is a double-free (both `m` and the Vec free the
    // same handle) — this pins the all-frames suppression scan (the
    // moved binding's `FreeMapHandle` can sit one frame below the
    // push's transient arg frame).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[Map[i64, i64]] = Vec.new();
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1i64, 777i64);
    v.push(m);
    match v[0].get(1i64) { Some(x) => println(x), None => println(-1i64) }
}
"#,
        &["777"],
        "vec_map_push_then_read_same_scope_single_free",
    );
}

#[test]
fn asan_vec_map_loop_built_returned_no_uaf() {
    // Loop-built `Vec[Map]` (the realistic scope-stack shape) returned
    // and indexed — exercises N>1 element handles through the drop loop.
    assert_clean_asan_run(
        r#"
fn make() -> Vec[Map[i64, i64]] {
    let mut v: Vec[Map[i64, i64]] = Vec.new();
    let mut i = 0i64;
    while i < 5i64 {
        let mut m: Map[i64, i64] = Map.new();
        m.insert(i, i * 100i64);
        v.push(m);
        i = i + 1;
    }
    v
}

fn main() {
    let v = make();
    println(v.len());
    match v[3].get(3i64) { Some(x) => println(x), None => println(-1i64) }
}
"#,
        &["5", "300"],
        "vec_map_loop_built_returned_no_uaf",
    );
}

#[test]
fn asan_generic_struct_literal_arg_entry_copy_no_leak() {
    // B-2026-08-06-2 (B). `take(Box { v: <fresh String> })` where
    // `fn take[T](b: Box[T]) -> T`. The monomorph ENTRY-DEEP-COPIES its
    // by-value param and returns the COPY, so the caller's struct-literal
    // temp is orphaned — and the caller registered no drop for it, because
    // a generic struct whose heap sits behind a bare `T` has no name-keyed
    // struct drop for `track_struct_var` to find. One buffer leaked per
    // call (140 B / 4 blocks on this fixture before the fix).
    //
    // The two spellings that must NOT gain a caller drop ride along, since
    // both are indistinguishable at the call site and both would DOUBLE
    // FREE if the caller took ownership:
    //
    //   * the NAMED argument, whose binding already owns the buffer;
    //   * `takeu[U](b: Box[U])`, whose fn-level type param is named
    //     differently from the struct's. `mono_struct_type_from_active_subst`
    //     finds no `T` binding there, falls back to the base layout, and the
    //     monomorph takes the OWN-BY-TRANSFER arm instead of entry-copying —
    //     so the callee holds the only reference.
    //
    // That is why the caller asks the CALLEE's own predicate
    // (`mono_entry_copies_aggregate_param`, under the callee's substitution)
    // rather than a caller-side look-alike, which cannot see the difference.
    assert_clean_asan_run(
        r#"
struct Box[T] { v: T }
fn take[T](b: Box[T]) -> T { b.v }
fn takeu[U](b: Box[U]) -> U { b.v }
fn main() {
    // Runtime-derived payload — a constant-folded one is a dead allocation the
    // optimizer deletes, and the fixture then passes vacuously against the
    // unfixed compiler (B-2026-08-04-17).
    let n: i64 = env.args().len();
    let mut i: i64 = 0;
    while i < 4 {
        println(take(Box { v: "generic-literal-payload-past-inline".repeat(n) }).len());
        let bx = Box { v: "generic-named-payload-past-inlin".repeat(n) };
        println(take(bx).len());
        println(takeu(Box { v: "diverging-param-name-payload-xy".repeat(n) }).len());
        i = i + 1;
    }
}
"#,
        &[
            "35", "32", "31", "35", "32", "31", "35", "32", "31", "35", "32", "31",
        ],
        "generic_struct_literal_arg_entry_copy",
    );
}

#[test]
fn asan_result_map_half_struct_field_freed_and_move_paired() {
    // B-2026-08-07-19 — the `Result` twin of B-2026-08-07-12 leg 1. A
    // `Result[Map[K,V], E]` STRUCT FIELD leaked its whole handle tree (720 B
    // / 10 iterations at BOTH opt levels, no call anywhere in the program):
    // a Map/Set handle is a single word, so it is neither the inline
    // `{ptr,len,cap}` overlay nor wide enough to be boxed, and its head is
    // in neither `struct_types` nor `enum_layouts` — the promotion loop's
    // two Result gates admit no such half and the field got no drop.
    //
    // THE MOVE LEGS ARE THE POINT OF THIS FIXTURE, not the `let`. Widening
    // a field's drop obliges every move site to neutralize the source with
    // it. Measured with the classifier widened and the move sites left
    // alone, every spelling that pattern-matches the field went from CLEAN
    // to 470 valgrind errors with invalid frees at both opt levels. So the
    // pattern arm, the whole-struct move, and both callee spellings are all
    // carried here; the `let`-move spelling stayed clean throughout (it
    // binds the field to a local, whose own cleanup runs) and rides along as
    // the arm that must not change.
    //
    // The `Err`-side handle and the sibling-field row are included because
    // the gate is per-FIELD: if it failed for the struct as a whole, the
    // `Option[String]` sibling would leak with the Map.
    assert_clean_asan_run(
        r#"
struct S { s: Result[Map[i64, String], i64] }
struct SErr { s: Result[i64, Map[i64, String]] }
struct Sib { p: Option[String], s: Result[Map[i64, String], i64] }
struct Ctl { s: Result[String, i64] }

fn mk(n: i64) -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(n, "mapv-padded-out-to-force-a-real-heap-buffer".repeat(1));
    m
}
fn take(s: S) -> i64 {
    match s.s { Ok(inner) => { inner.len() } Err(e) => { e } }
}

fn main() {
    // Runtime-derived so the payload is a real allocation at -O2 too
    // (B-2026-08-04-17).
    let n: i64 = env.args().len();
    let mut i: i64 = 0;
    while i < 3 {
        // plain let — the leaking shape this row was filed for
        let a: S = S { s: Result.Ok(mk(n + i)) };
        println(1);
        // pattern move out of the field
        let b: S = S { s: Result.Ok(mk(n + i)) };
        match b.s { Ok(inner) => { println(inner.len()); } Err(e) => { println(e); } }
        // field let-move, then match the local
        let c: S = S { s: Result.Ok(mk(n + i)) };
        let r: Result[Map[i64, String], i64] = c.s;
        match r { Ok(inner) => { println(inner.len()); } Err(e) => { println(e); } }
        // whole-struct move, then pattern
        let d: S = S { s: Result.Ok(mk(n + i)) };
        let e: S = d;
        match e.s { Ok(inner) => { println(inner.len()); } Err(x) => { println(x); } }
        // move into a callee, both spellings
        let f: S = S { s: Result.Ok(mk(n + i)) };
        println(take(f));
        println(take(S { s: Result.Ok(mk(n + i)) }));
        // Err-side handle
        let g: SErr = SErr { s: Result.Err(mk(n + i)) };
        println(1);
        // sibling field must not leak with it
        let h: Sib = Sib { p: Option.Some("sib-padded-out-to-force-a-real-heap-buffer".repeat(1)), s: Result.Ok(mk(n + i)) };
        println(1);
        // control: the Result shape that already worked
        let k: Ctl = Ctl { s: Result.Ok("ctl-padded-out-to-force-a-real-heap-buffer".repeat(1)) };
        match k.s { Ok(inner) => { println(inner.len()); } Err(x) => { println(x); } }
        i = i + 1;
    }
}
"#,
        &[
            "1", "1", "1", "1", "1", "1", "1", "1", "42", "1", "1", "1", "1", "1", "1", "1", "1",
            "42", "1", "1", "1", "1", "1", "1", "1", "1", "42",
        ],
        "result_map_half_struct_field",
    );
}

/// B-2026-08-13-5 — a heap field read off a SLICE element or a MAP value is
/// deep-cloned too, so it no longer double-frees.
///
/// B-2026-08-12-27 clones this read for an owning `Vec` and excluded the
/// other two containers in a comment: "a slice element is borrowed and a map
/// value is side-table owned; cloning either would hand back a buffer whose
/// original still has its own owner, i.e. a leak rather than a fix." The
/// measurement contradicts it in BOTH directions, which is why the arm is
/// now container-agnostic:
///
///   - NOT cloning was not the safe side. `fn take(xs: Slice[Pair]) ->
///     String { xs[0].word }` and `let w = m[k].word` both DOUBLE-FREED,
///     because the un-cloned read escapes into an owning destination which
///     then frees a buffer the lender still owns.
///   - Cloning is not a leak, and the proof was one line away the whole
///     time: the WHOLE-element read (`let p = xs[0]` / `let p = m[k]`) has
///     always cloned for these same containers and is clean, because the
///     clone carries its own scope cleanup and a consuming destination takes
///     that cleanup over instead of duplicating it.
///
/// THE LOOP LEGS ARE THE LEAK HALF, and they are why this is not just a
/// double-free fixture: 200 iterations of a consuming read on both containers, and a second
/// bound read beside it (a bare `xs[0].word.len()` cannot be written — a
/// chained field receiver is a separate deferred gap). If the clone's cleanup did not fire per iteration, LSan
/// reports 200 blocks; if a consuming destination did not take the clone
/// over, this aborts instead.
///
/// The `nested` leg carries B-2026-08-13-4's chain through a slice, since
/// the two fixes compose: the hop walk resolves the field and the container
/// gate now admits the borrow.
#[test]
fn asan_slice_and_map_elem_field_read_cloned_not_aliased() {
    assert_clean_asan_run(
        r#"
struct Pair { word: String, n: i64 }
struct Deep { inner: Pair, tag: i64 }
fn take(xs: Slice[Pair]) -> String { let w = xs[0].word; w }
fn nested(xs: Slice[Deep]) -> String { let w = xs[0].inner.word; w }
fn tally(xs: Slice[Pair]) -> i64 {
    let mut acc = 0i64;
    let mut i = 0i64;
    while i < 200 {
        let w = xs[0].word;
        let peek = xs[0].word;
        acc = acc + w.len() + peek.len();
        i = i + 1;
    }
    acc
}
fn main() {
    let k = env.args().len() as i64;
    let mut ps: Vec[Pair] = Vec.new();
    ps.push(Pair { word: f"a{k}", n: 1 });
    let borrowed = take(ps[0..1]);
    let kept = ps[0].word;
    let mut acc = borrowed.len() + kept.len();
    acc = acc + tally(ps[0..1]);
    let mut ds: Vec[Deep] = Vec.new();
    ds.push(Deep { inner: Pair { word: f"b{k}", n: 2 }, tag: 3 });
    let deep = nested(ds[0..1]);
    let kept_deep = ds[0].inner.word;
    acc = acc + deep.len() + kept_deep.len();
    let mut m: Map[String, Pair] = Map.new();
    m.insert(f"k{k}", Pair { word: f"c{k}", n: 4 });
    let mapped = m[f"k{k}"].word;
    let kept_map = m[f"k{k}"].word;
    acc = acc + mapped.len() + kept_map.len();
    let mut i = 0i64;
    while i < 200 {
        let w = m[f"k{k}"].word;
        acc = acc + w.len();
        i = i + 1;
    }
    println(acc > 0);
}
"#,
        &["true"],
        "slice_and_map_elem_field_read_cloned_not_aliased",
    );
}

/// B-2026-08-02-17 — a GENERIC parent's `Map[i64, T]` field: the
/// struct-literal init built the handle from the DECLARED bare-`T`
/// value TE (val_size = 8 instead of the instantiated struct's 32),
/// so inserts truncated the value and its String buffer leaked at
/// owner death; the drop synthesis also classified the value drop
/// from bare `T`. Both now resolve through the literal's / mono
/// subst — LSan-clean, and the non-generic control's behavior is
/// unchanged.
#[test]
fn asan_generic_parent_map_field_value_freed() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Store[T] { m: Map[i64, T], tag: i64 }
fn main() {
    println("a");
    {
        let mut s: Store[Res] = Store { m: Map.new(), tag: 1 };
        s.m.insert(1i64, Res { id: 8, name: f"mm{8}" });
        println(f"n {s.m.len()}");
    }
    println("end");
}
"#,
        // B-2026-08-02-18 — the Map-field VALUE bodies walk now fires at
        // owner death, so the expectation gains the drop line (memory
        // behavior unchanged: still LSan-clean).
        &["a", "n 1", "drop 8 mm8", "end"],
        "generic_parent_map_field_value_freed",
    );
}

/// B-2026-08-02-21 — struct-field tables of Drop-struct elements:
/// (1) a `Set[Res]` field's element String buffers are freed at owner
/// death via the per-key drop channel (B-2026-08-01-18's binding-death
/// channel, now wired into the field arm); (2) a `SortedMap[i64, Res]`
/// field's WHOLE handle tree is freed — the Sorted heads were missing
/// from the FieldDrop::MapOrSet classification, so the field registered
/// no drop at all. LSan-clean, and the element/value bodies fire (the
/// SortedMap values in sorted key order — the b170 sorted walker).
#[test]
fn asan_struct_field_set_and_sortedmap_freed() {
    assert_clean_asan_run(
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
        &[
            "a",
            "1",
            "drop 3 s3",
            "mid",
            "2",
            "drop 5 n5",
            "drop 4 m4",
            "end",
        ],
        "struct_field_set_and_sortedmap_freed",
    );
}

/// B-2026-08-02-12 — `Map.new()` / `Set.new()` in EXPRESSION position
/// (push args, `Vec.filled` fill values) used to lower to a NULL
/// handle (assoc-call `i64 0` default), segfaulting at the first
/// element use — this run crashes outright pre-fix. Post-fix the
/// memory pairing needs pinning: the handle is real, its String KEY
/// buffer transfers into the map (freed exactly once by the map's
/// key-walking free — the wrong-key-size handle silently skipped
/// that free), filled slots deep-clone independently, and the Vec's
/// scope-exit element walk frees every handle.
#[test]
fn asan_expression_position_map_handles_freed() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[Map[String, i64]] = Vec.new();
    v.push(Map.new());
    let _ = v[0].insert(f"k{1}", 5);
    match v[0].get(f"k{1}") { Some(x) => println(f"got {x}"), None => println("miss") }
    let mut g: Vec[Map[String, i64]] = Vec.filled(2, Map.new());
    let _ = g[0].insert(f"a{2}", 7);
    println(f"g0 {g[0].len()} g1 {g[1].len()}");
    println("end");
}
"#,
        &["got 5", "g0 1 g1 0", "end"],
        "expression_position_map_handles_freed",
    );
}

/// B-2026-08-01-28 — the -24 double-free through the Map.insert /
/// Set.insert / field-move-out consume arms (all three aborted with
/// free(): double free pre-fix; the map/set arms now deep-copy the
/// staged key/value slots and a consuming field read of a loop element
/// routes through the borrowed-receiver defensive copy). ASAN pins the
/// double-free half; LSan (Linux CI) the no-leak half of the copies.
#[test]
fn asan_for_loop_elem_map_set_field_consumes_freed() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, Ord)]
struct P { a: i64, s: String }
struct Header { name: String, value: String }
fn main() {
    let mut hs: Vec[Header] = Vec.new();
    hs.push(Header { name: f"n{1}", value: f"v{1}" });
    let mut m: Map[i64, Header] = Map.new();
    let mut i = 0;
    for h in hs {
        let _ = m.insert(i, h);
        i = i + 1;
    }
    let mut ps: Vec[P] = Vec.new();
    ps.push(P { a: 1, s: f"x{1}" });
    let mut set: Set[P] = Set.new();
    for p in ps {
        set.insert(p);
    }
    println(set.len());
    let mut src: Vec[Header] = Vec.new();
    src.push(Header { name: f"a{7}", value: f"b{7}" });
    let mut names: Vec[String] = Vec.new();
    for h in src {
        names.push(h.name);
    }
    for n in names { println(n); }
    println("end");
}
"#,
        &["1", "a7", "end"],
        "for_loop_elem_map_set_field_consumes_freed",
    );
}

/// B-2026-08-01-18 — a Set/SortedSet ELEMENT (or Map KEY) that is a
/// struct owning heap fields leaked those fields' buffers at scope
/// exit: the handle free released the bucket storage but never walked
/// element struct fields (the key-half sibling of the slice-3r
/// per-VALUE drop fn). The fix threads `key_drop_fn` through the
/// FreeMapHandle classification and open-codes an occupied-bucket walk
/// before the storage release. Drop-bearing elements keep exactly one
/// body fire (the NLL dropelems walker); the scope-exit walk is
/// memory-only. LSan (Linux CI) is the leak gate; ASAN guards the new
/// walk against use-after-free / double-free of the field buffers.
#[test]
fn asan_set_element_struct_field_buffers_freed() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, Ord)]
struct P { a: i64, s: String }
#[derive(Hash, Eq, Ord)]
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn main() {
    println("a");
    let mut set: Set[P] = Set.new();
    set.insert(P { a: 2, s: f"zz{2}" });
    set.insert(P { a: 1, s: f"bb{1}" });
    let mut ss: SortedSet[P] = SortedSet.new();
    ss.insert(P { a: 4, s: f"dd{4}" });
    ss.insert(P { a: 3, s: f"cc{3}" });
    let mut m: Map[P, i64] = Map.new();
    let _ = m.insert(P { a: 5, s: f"k{5}" }, 50);
    println("b");
    let mut ds: SortedSet[Res] = SortedSet.new();
    ds.insert(Res { id: 7, name: f"q{7}" });
    ds.insert(Res { id: 6, name: f"p{6}" });
    println("end");
}
"#,
        &["a", "b", "drop 6 p6", "drop 7 q7", "end"],
        "set_element_struct_field_buffers_freed",
    );
}

#[test]
fn asan_ref_arg_map_freed() {
    // Slice 2 part B: a fresh `Map[i64,i64]` handle passed to a `ref
    // Map[i64,i64]` param. The prior `ref_rvalue_arg` path only tracked
    // Vec/String-shaped temps, so a fresh Map handle passed by `ref`
    // leaked its whole control block. `queue_ref_rvalue_arg_cleanup`
    // recognizes the `Map[K,V]` TypeExpr via the hint table and queues a
    // `FreeMapHandle`. Loop amplifies any imbalance into a deterministic
    // macOS double-free fault; Linux catches the leak.
    assert_clean_asan_run(
        r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 2_i64);
    m.insert(3_i64, 4_i64);
    return m;
}

fn show(m: ref Map[i64, i64]) {
    println(m.len());
}

fn main() {
    let mut i = 0;
    while i < 8 {
        show(make_map());
        i = i + 1;
    }
}
"#,
        &["2", "2", "2", "2", "2", "2", "2", "2"],
        "ref_arg_map_freed",
    );
}

#[test]
fn asan_enum_mapset_payload_no_leak_no_double_free() {
    // B-2026-07-23-11: a user enum whose variant payload is a `Map`/`Set`
    // (-family) collection. The enum drop walker previously had no Map/Set
    // arm (`enum_drop_kind_for_type_expr` → `None`), so a pure
    // construct-then-drop leaked the whole kv-table (valgrind
    // "definitely-lost" from `karac_map_new`). The `MapOrSet` drop kind frees
    // the handle via `karac_map_free_with_drop_vec`, and the symmetric
    // entry-copy (`deep_copy_enum_heap_payload_in_place`) deep-clones the
    // handle so a by-value enum param owns an independent table — no
    // double-free across the call boundary. Exercises four paths in a loop so
    // any per-iteration imbalance accumulates into a fault: (1) construct +
    // scope-exit drop (the ledger repro), (2) `ref V` read (source retains,
    // enum drop frees), (3) by-value consume (entry-copy deep-clone → caller
    // and callee each free their own), (4) a heap-VALUED `Map[String,String]`
    // so the `(drop_key, drop_val)` flags release per-entry buffers, and a
    // `Set[String]` element. On Linux CI LeakSanitizer faults if the handle
    // (or a heap key/value) leaks; ASAN faults on a double-free.
    assert_clean_asan_run(
        r#"
enum V { Table(Map[String, i64]) }
enum S { Names(Set[String]) }
enum W { Pairs(Map[String, String]) }
fn size(v: ref V) -> i64 { match v { Table(m) => m.len() as i64 } }
fn consume(v: V) -> i64 { match v { Table(m) => m.len() as i64 } }
fn main() {
    let mut total: i64 = 0;
    let mut i = 0;
    while i < 40 {
        let mut mp: Map[String, i64] = Map.new();
        mp.insert("a", 1);
        mp.insert("b", 2);
        let t = V.Table(mp);           // (1) construct
        total = total + size(t);       // (2) ref read; t dropped at loop end

        let mut mp2: Map[String, i64] = Map.new();
        mp2.insert("x", 9);
        let t2 = V.Table(mp2);
        total = total + consume(t2);   // (3) by-value consume (deep-clone entry)

        let mut names: Set[String] = Set.new();
        names.insert("alice".to_string());
        names.insert("bob".to_string());
        let sn = S.Names(names);       // Set[String] payload
        total = total + match sn { Names(nm) => nm.len() as i64 };

        let mut pm: Map[String, String] = Map.new();
        pm.insert("k".to_string(), "value_payload".to_string());
        let w = W.Pairs(pm);           // (4) heap-valued Map
        total = total + match w { Pairs(pp) => pp.len() as i64 };

        i = i + 1;
    }
    println(total);
}
"#,
        &["240"],
        "enum_mapset_payload_no_leak_no_double_free",
    );
}

#[test]
fn asan_enum_map_payload_moved_out_and_returned_no_leak_no_double_free() {
    // B-2026-07-23-14: a `Map` moved OUT of an enum payload and RETURNED from
    // a function (`fn unwrap(v: V) -> Map[K,V] { match v { Table(m) => m } }`).
    // The build previously failed module verification (`ret i64` for a `ptr`
    // return); once that is fixed, the ownership must also be right — the
    // returned handle transfers to the caller (`m2`), the source `v` is
    // move-suppressed, and the caller frees it exactly once. Loops so any
    // per-iteration imbalance (a missed free on the returned binding, or a
    // double-free from the suppressed source) accumulates into a fault under
    // LeakSanitizer / AddressSanitizer.
    assert_clean_asan_run(
        r#"
enum V { Table(Map[String, i64]) }
fn unwrap(v: V) -> Map[String, i64] { match v { Table(m) => m } }
fn main() {
    let mut total: i64 = 0;
    let mut i = 0;
    while i < 40 {
        let mut mp: Map[String, i64] = Map.new();
        mp.insert("a", 1);
        mp.insert("b", 2);
        let t = V.Table(mp);
        let m2 = unwrap(t);
        total = total + (m2.len() as i64);
        i = i + 1;
    }
    println(total);
}
"#,
        &["80"],
        "enum_map_payload_moved_out_and_returned_no_leak_no_double_free",
    );
}

#[test]
fn asan_struct_destructure_set_bound_and_unbound_no_double_free() {
    // Set leg of B follow-up #3 (closes the Set remaining-leak gap): a
    // fresh-temp struct destructure where one `Set` field is bound (`a`,
    // freed via its binding) and another discarded (`b: _`, freed via a
    // synthetic discard slot). Set lowers to `Map[T, ()]`, so both route
    // through `karac_map_free`. Looped so a double-free or missed/extra
    // free of either handle faults under ASAN's quarantine. macOS has no
    // LeakSanitizer, so the leak closure is pinned by the IR tests; this
    // is the double-free gate.
    assert_clean_asan_run(
        r#"
struct Pair { a: Set[i64], b: Set[i64], n: i64 }
fn mk(x: i64) -> Pair {
    let mut sa: Set[i64] = Set.new();
    sa.insert(x);
    let mut sb: Set[i64] = Set.new();
    sb.insert(x * 2);
    return Pair { a: sa, b: sb, n: x };
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let Pair { a, b: _, n } = mk(i);
        println(a.len() + n);
        i = i + 1;
    }
    println(99);
}
"#,
        &["1", "2", "3", "4", "5", "99"],
        "struct_destructure_set_bound_and_unbound_no_double_free",
    );
}

#[test]
fn asan_set_local_moved_into_returned_struct_no_uaf() {
    // Pre-existing UAF (fixed 2026-06-08): a `Set` local moved into a
    // struct LITERAL that the function returns was freed at the source
    // function's scope exit — the Vec path had move-suppression, Map/Set
    // didn't — so the returned struct's handle dangled. Without the fix
    // this crashed even without ASAN (SIGSEGV / abort); here the caller
    // reads the moved-in handle (`contains`) in a loop, so a dangling /
    // double-freed handle faults under ASAN. Set lowers to `Map[T, ()]`.
    assert_clean_asan_run(
        r#"
struct Bag { tags: Set[i64], count: i64 }
fn make(x: i64) -> Bag {
    let mut s: Set[i64] = Set.new();
    s.insert(x);
    return Bag { tags: s, count: x };
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let b = make(i);
        if b.tags.contains(i) { println(b.count); } else { println(-1); }
        i = i + 1;
    }
    println(99);
}
"#,
        &["0", "1", "2", "3", "4", "99"],
        "set_local_moved_into_returned_struct_no_uaf",
    );
}

#[test]
fn asan_map_local_moved_into_returned_struct_no_uaf() {
    // Map sibling of the Set UAF above (was an abort/double-free, exit 134).
    // The caller derefs the moved-in handle via `b.m.len()`, so a dangling
    // handle faults under ASAN.
    assert_clean_asan_run(
        r#"
struct Box { m: Map[i64, i64], n: i64 }
fn make(x: i64) -> Box {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(x, x * 2);
    return Box { m: m, n: x };
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let b = make(i);
        println(b.n + b.m.len());
        i = i + 1;
    }
    println(99);
}
"#,
        &["1", "2", "3", "4", "5", "99"],
        "map_local_moved_into_returned_struct_no_uaf",
    );
}

#[test]
fn asan_map_set_moved_into_returned_enum_variant_clean() {
    // ASAN-cleanliness guard for the enum-variant sibling of
    // `asan_map_local_moved_into_returned_struct_no_uaf` (phase-6 line
    // 562): a `Map`/`Set` local moved into an enum variant (`Some(m)`)
    // that the function returns must not free the handle at the source's
    // scope exit (the move-suppression at the enum-variant constructor),
    // and the match-arm leaf binding must dispatch `.len()`/`.contains()`
    // (the new dispatch wiring). Looped to exercise per-iteration alloca
    // reuse. This pins that the new dispatch + suppression paths stay
    // ASAN-clean and never double-free.
    //
    // NOTE — this ASAN test is NOT the non-vacuous proof of the UAF fix.
    // The bug here is a *single-free* UAF whose dangling read lands inside
    // the (non-ASAN-instrumented) runtime archive's `karac_map_len`, and
    // ASAN's free-quarantine preserves the freed bytes, so ASAN reads the
    // intact data and reports clean with OR without the fix. The
    // deterministic non-vacuous gate is the plain-build E2E
    // `test_e2e_match_arm_map_set_method_dispatch` in tests/codegen.rs,
    // which prints wrong/empty output without the suppression and `3\n2`
    // with it. This test guards the memory-safety dimension (no NEW
    // double-free introduced) and the looped dispatch path.
    assert_clean_asan_run(
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
    while i < 5 {
        match make_map(i) {
            Some(m) => println(i + m.len()),
            None => println(-1),
        }
        match make_set(i) {
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
        &["1", "2", "2", "2", "3", "2", "4", "2", "5", "2", "99"],
        "map_set_moved_into_returned_enum_variant_clean",
    );
}

// ── General owned-temp tracking, slice 2: Map / RC / nested-elem ──
// (docs/spikes/general-owned-temp-tracking.md). Map handles and RC
// boxes are plain pointers — slice 1 (LLVM-type Vec/String detection)
// leaked them; the lowering-pass `owned_temp_drops` hint table now lets
// `materialize_owned_temp` classify and drop them. The 8-iteration loop
// amplifies any per-iteration imbalance into a deterministic double-free
// fault under macOS ASAN; Linux `detect_leaks=1` is the leak oracle.

#[test]
fn asan_discarded_map_temp_freed() {
    // A discarded fresh `Map[i64, i64]` handle: no binding to drop it,
    // recognized only via the hint table's `Map[K, V]` TypeExpr. Both
    // halves primitive → `karac_map_free` (no per-entry vec walk). Faults
    // on macOS if the handle is double-freed; leaks on Linux if untracked.
    assert_clean_asan_run(
        r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 2_i64);
    m.insert(3_i64, 4_i64);
    return m;
}

fn main() {
    let mut i = 0;
    while i < 8 {
        make_map();
        i = i + 1;
    }
    println("done");
}
"#,
        &["done"],
        "discarded_map_temp_freed",
    );
}

#[test]
fn asan_returned_map_explicit_return_no_double_free() {
    // Regression for the explicit-`return m;` map-suppression gap fixed
    // alongside slice 2 (src/codegen/exprs.rs `ExprKind::Return`): the
    // tail-expression path suppressed a returned Map's `FreeMapHandle`,
    // but the explicit-`return` path did not — so a callee returning a
    // map via `return m;` freed the handle *and* returned it, and the
    // caller's binding then freed the dangling pointer (double-free under
    // AOT). Here the callee uses `return m;` and the caller binds and
    // reads it; without the fix this double-frees. Sibling to the
    // discarded-map case, pinning the *bound* return shape.
    assert_clean_asan_run(
        r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 2_i64);
    return m;
}

fn main() {
    let m2 = make_map();
    println(m2.len());
}
"#,
        &["1"],
        "returned_map_explicit_return_no_double_free",
    );
}

#[test]
fn asan_shared_struct_map_shared_value_field_no_leak() {
    // B-2026-07-13-12. A `shared struct Owner { cache: Map[i64, Node] }`
    // (Node shared) dropped the Map's bucket storage via the type-erased
    // `karac_map_free_with_drop_vec` but NEVER dec'd the shared VALUES — the
    // shared-struct RC-drop's MapOrSet arm lacked the per-bucket
    // `emit_map_shared_half_rc_dec_walk` that the non-shared struct-drop peer
    // already runs. One ref leaked per live entry (LSan: 16-byte Node boxes).
    // 50 iters × 2 entries: the walk now dec's each shared value before the
    // bucket free.
    assert_clean_asan_run(
        r#"
shared struct Node { mut val: i64 }
shared struct Owner { mut cache: Map[i64, Node] }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        let o = Owner { cache: Map.new() };
        o.cache.insert(1, Node { val: 7 });
        o.cache.insert(2, Node { val: 8 });
        total = total + 1;
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        &["50"],
        "shared_struct_map_shared_value_field_no_leak",
    );
}

#[test]
fn asan_shared_enum_map_shared_value_payload_drop_no_leak() {
    // B-2026-07-13-15 (the shared-enum sibling of B-2026-07-13-12). A
    // `shared enum Store { Full(Map[i64, Node]) }` (Node shared) dropped with
    // its payload intact released the Map's bucket storage via the type-erased
    // `karac_map_free_with_drop_vec` but never dec'd the shared VALUES —
    // `emit_shared_enum_field_drop`'s Map/Set arm lacked the per-bucket
    // `emit_map_shared_half_rc_dec_walk` the struct-drop peer runs. One ref
    // leaked per live entry (LSan: 16-byte Node boxes). 50 iters × 2 entries:
    // the arm now walks each shared value before the bucket free.
    assert_clean_asan_run(
        r#"
shared struct Node { mut val: i64 }
shared enum Store { Empty, Full(Map[i64, Node]) }
fn build(n: i64) -> Store {
    let mut m: Map[i64, Node] = Map.new();
    m.insert(1, Node { val: n });
    m.insert(2, Node { val: n + 1 });
    Store.Full(m)
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        let s = build(i);
        total = total + 1;
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        &["50"],
        "shared_enum_map_shared_value_payload_drop_no_leak",
    );
}

#[test]
fn asan_map_key_user_drop_bodies_fire_once() {
    // B-2026-08-26-41 — keys twin of
    // `asan_map_value_user_drop_bodies_fire_once`. A map key's MEMORY was
    // already reclaimed by `emit_map_key_drop_fn_walk`; this row added the
    // BODY walk on top of it, so the hazard is running a user body over a
    // slot the memory walk is about to free, or freeing the key's heap
    // field twice (once from the body's own field drops, once from the
    // bucket teardown).
    //
    // The key carries a `String` so there is a real buffer behind each
    // entry, and the map is built and torn down repeatedly so a
    // double-free or UAF is immediate rather than probabilistic. The
    // printed count is the oracle: 300 bodies, no more and no less — a
    // double fire shows up as a wrong number, not just as an ASAN report.
    let label = "map_key_user_drop_bodies";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let src = r#"
#[derive(Hash, Eq, PartialEq)]
struct K { s: String }
struct V { s: String }
impl Drop for K { fn drop(mut ref self) { karac_bump() } }
impl Drop for V { fn drop(mut ref self) { karac_bump() } }
let mut FIRED: i64 = 0;
fn karac_bump() { FIRED = FIRED + 1; }
fn main() {
    let mut round = 0;
    while round < 30 {
        let mut m: Map[K, V] = Map.new();
        let mut i = 0;
        while i < 5 {
            let k = K { s: f"key-{i}-padding-padding" };
            m.insert(k, V { s: f"val-{i}-padding-padding" });
            i = i + 1;
        }
        round = round + 1;
    }
    println(f"fired={FIRED}");
}
"#;
    let Some((stdout, status)) = run_under_asan(src, label) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN run failed (status {status:?}); stdout:\n{stdout}"
    );
    // 30 rounds x 5 entries x 2 halves = 300 bodies. The COUNT is the
    // oracle a sanitizer cannot provide: a double fire is not a memory
    // error, so ASAN would pass it silently.
    assert_eq!(stdout.trim(), "fired=300", "[{label}] stdout:\n{stdout}");
}

#[test]
fn asan_map_weak_value_param_rebind_is_unchanged() {
    // CONTROL — passes both with and without the fix, and pins the reason
    // the defect was Vec-only. A `Map[K, weak V]` drains its values through
    // the SAME `__karac_weak_slot_drop`, so the same double-drain was
    // available to it; what saves it is that a Map param's copy goes
    // through `karac_clone_Map`, whose weak-value branch already emits the
    // per-entry `karac_weak_downgrade` (maps.rs). The Vec copy was the
    // outlier, not the weak tier as a whole.
    //
    // It also guards the direction a fix like this fails: an over-retain
    // here would be a quiet leak, and this test would catch it.
    assert_clean_asan_run(
        r#"
shared struct N { v: i64 }

fn probe(m: Map[i64, weak N]) -> i64 {
    let work = m;
    match work.get(1) { Some(x) => { x.v } None => { 0 - 1 } }
}

fn main() {
    let a: N = N { v: 7 };
    let mut m: Map[i64, weak N] = Map.new();
    m.insert(1, a);
    println(probe(m));
    println(a.v);
}
"#,
        &["7", "7"],
        "map_weak_value_param_rebind_unchanged",
    );
}

#[test]
fn asan_map_of_vec_weak_param_stash_drains_every_slot() {
    // The shape B-2026-09-08-1's fix REGRESSED, now correct. It was green
    // before that fix by CANCELLATION rather than by correctness -- the
    // copy failed to retain (that row) and this nested destination failed
    // to drain (this one) -- so the two errors summed to zero, and removing
    // one exposed the other.
    //
    // Kept as its own fixture because a cancelling pair reports as a clean
    // run: nothing but a deliberate case records that the pair existed.
    assert_clean_asan_run(
        r#"
shared struct N { v: i64 }

fn stash(xs: Vec[weak N]) -> i64 {
    let mut m: Map[i64, Vec[weak N]] = Map.new();
    m.insert(1, xs);
    match m.get(1) { Some(inner) => { match inner[0] { Some(x) => { x.v } None => { 0 - 1 } } } None => { 0 - 2 } }
}

fn main() {
    let a: N = N { v: 7 };
    let mut w: Vec[weak N] = Vec.new();
    w.push(a);
    println(stash(w));
    println(a.v);
}
"#,
        &["7", "7"],
        "map_of_vec_weak_param_stash",
    );
}

// B-2026-08-07-20 — a SHARED-owning struct never freed its `Option[Map]` /
// `Option[Set]` field. 4,320 B in 60 blocks at BOTH opt levels for plain
// `let` bindings with no call anywhere in the program (stash-proven red).
//
// The gap is an INTERSECTION, which is why neither sibling row reaches it:
// the promotion gate in `emit_struct_drop_synthesis_impl` has two arms and
// this shape closes both, each for its own correct reason. The copy-support
// arm is closed by the `Map` field (a handle the outer-buffer copy cannot
// duplicate — the same fact that keeps `Se` below off that arm), and the
// own-by-transfer arm by the `shared` field (B-2026-08-05-32 keeps a
// shared-owning struct on caller-retains). Neither refusal is about
// ownership. `shared_owning_struct_sole_field_owner` asks that question
// directly and supplies a third arm.
//
// The controls are the diagnosis: `Sb` (`Option[String]`) and `Sc`
// (`Option[Vec]`) pass through the FIRST arm and were always clean, so "a
// shared-owning struct never frees its Option fields" is false; `Se` (no
// shared field) passes through the SECOND since B-2026-08-07-12 leg 1, so
// "an `Option[Map]` field is never freed" is false too. `Sd` is the
// direct-shared spelling of `Sa` and leaked identically; `Sf` is the `Set`
// twin. The three move-out spellings (`match a.m`, `let qm = a.m`, and the
// whole-struct destructure `let Sa { n, m } = r`) carry the pairing: the
// destructure needed its own `Option[Map]` transfer + leaf tracker, without
// which the newly-armed source drop and the leaf's consumer both freed
// (940 valgrind errors, measured).
#[test]
fn asan_shared_owning_struct_option_map_field_freed() {
    assert_clean_asan_run(
        r#"
shared struct Node { v: i64 }
struct Sa { n: Option[Node], m: Option[Map[String, i64]] }
struct Sd { n: Node, m: Option[Map[String, i64]] }
struct Sb { n: Option[Node], m: Option[String] }
struct Sc { n: Option[Node], m: Option[Vec[i64]] }
struct Se { m: Option[Map[String, i64]] }
struct Sf { n: Option[Node], s: Option[Set[String]] }
fn mk(i: i64) -> Map[String, i64] {
    let mut mm: Map[String, i64] = Map.new();
    mm.insert("map_key_long_enough_to_force_a_heap_allocation".to_string(), i);
    mm
}
fn mkset(i: i64) -> Set[String] {
    let mut ss: Set[String] = Set.new();
    ss.insert("set_key_long_enough_to_force_a_heap_allocation".to_string());
    ss
}
fn main() {
    let mut t: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        let a: Sa = Sa { n: Option.Some(Node { v: i }), m: Option.Some(mk(i)) };
        match a.n { Some(nn) => { t = t + nn.v; } None => {} }
        let d: Sd = Sd { n: Node { v: i }, m: Option.Some(mk(i)) };
        t = t + d.n.v;
        let b: Sb = Sb { n: Option.Some(Node { v: i }), m: Option.Some("a_string_long_enough_to_force_heap_alloc_x".to_string()) };
        match b.m { Some(x) => { t = t + x.len(); } None => {} }
        let c: Sc = Sc { n: Option.Some(Node { v: i }), m: Option.Some(Vec.new()) };
        match c.m { Some(x) => { t = t + x.len(); } None => {} }
        let e: Se = Se { m: Option.Some(mk(i)) };
        match e.m { Some(x) => { t = t + x.len(); } None => {} }
        let f: Sf = Sf { n: Option.Some(Node { v: i }), s: Option.Some(mkset(i)) };
        match f.s { Some(x) => { t = t + x.len(); } None => {} }
        let p: Sa = Sa { n: Option.Some(Node { v: i }), m: Option.Some(mk(i)) };
        match p.m { Some(x) => { t = t + x.len(); } None => {} }
        let q: Sa = Sa { n: Option.Some(Node { v: i }), m: Option.Some(mk(i)) };
        let qm = q.m;
        match qm { Some(x) => { t = t + x.len(); } None => {} }
        let r: Sa = Sa { n: Option.Some(Node { v: i }), m: Option.Some(mk(i)) };
        let Sa { n, m } = r;
        match n { Some(nn) => { t = t + nn.v; } None => {} }
        match m { Some(x) => { t = t + x.len(); } None => {} }
        let u: Sa = Sa { n: Option.Some(Node { v: i }), m: Option.Some(mk(i)) };
        let Sa { n: un, m: um } = u;
        match un { Some(nn) => { t = t + nn.v; } None => {} }
        i = i + 1;
    }
    println(t);
}
"#,
        &["1700"],
        "shared_owning_struct_option_map_field_freed",
    );
}

// B-2026-08-07-20's SCOPE GUARD, and the reason the fix above carries a
// whole-program condition rather than only a type condition.
//
// The caller-retains argument holds inside ONE frame. At a call boundary a
// shared-owning struct stays caller-retains (B-2026-08-05-32), so the callee
// gets a shallow copy, registers no drop, and zeroes any field it moves out
// in ITS copy — a write the caller's frame never sees. Arming the caller's
// drop for a promoted field therefore double-frees against a callee that
// moves that field out: measured at 470 valgrind errors / 26 invalid frees
// at both opt levels, in all three spellings (`match a.m { Some(x) => .. }`,
// `let mm = a.m`, and the escaping `Some(x) => x`), and NOT fixable by
// deep-copying the param at entry — a `Map`/`Set` handle is the one thing
// `deep_copy_owned_struct_param_field_move` cannot duplicate.
//
// So the disjunct declines for a field a by-value callee's body could take
// out, and this fixture is what holds that line: all three callees here move
// `a.m`, so that field keeps today's (correct) behavior — the callee owns the
// move-out and nothing double-frees.
//
// B-2026-08-08-6 narrowed the decline from the whole TYPE to the individual
// FIELD (`struct_by_value_param_body_takes_field`), so this fixture no longer
// stands for "any struct that could reach a call boundary at all". `Sa`'s
// OTHER promoted field is now armed whenever no callee body takes it — see
// `asan_shared_owning_struct_untouched_option_map_field_freed`, which pins
// the leak that decline used to preserve. What this fixture still pins is the
// moved-out field itself.
#[test]
fn asan_shared_owning_struct_option_map_by_value_param_single_owner() {
    assert_clean_asan_run(
        r#"
shared struct Node { v: i64 }
struct Sa { n: Option[Node], m: Option[Map[String, i64]] }
fn mk(i: i64) -> Map[String, i64] {
    let mut mm: Map[String, i64] = Map.new();
    mm.insert("map_key_long_enough_to_force_a_heap_allocation".to_string(), i);
    mm
}
fn take_match(a: Sa) -> i64 { match a.m { Some(x) => { x.len() } None => { 0 } } }
fn take_let(a: Sa) -> i64 { let mm = a.m; match mm { Some(x) => { x.len() } None => { 0 } } }
fn take_escape(a: Sa) -> Option[Map[String, i64]] { a.m }
fn main() {
    let mut t: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        let a: Sa = Sa { n: Option.Some(Node { v: i }), m: Option.Some(mk(i)) };
        t = t + take_match(a);
        let b: Sa = Sa { n: Option.Some(Node { v: i }), m: Option.Some(mk(i)) };
        t = t + take_let(b);
        let c: Sa = Sa { n: Option.Some(Node { v: i }), m: Option.Some(mk(i)) };
        let esc = take_escape(c);
        match esc { Some(x) => { t = t + x.len(); } None => {} }
        i = i + 1;
    }
    println(t);
}
"#,
        &["60"],
        "shared_owning_struct_option_map_by_value_param",
    );
}

// B-2026-08-08-6 — the residual leak the scope guard above preserved, and
// the reason its decline is now per FIELD rather than per type.
//
// `struct_used_as_bare_by_value_param` asks a SIGNATURE question, so one
// callee that could move one promoted field out declined the caller-retains
// drop for EVERY promoted field of the struct — including fields no callee
// ever mentions, whose payload was then freed by nobody. `reads_n` here
// reads only `a.n`; the `Option[Map]` field never reaches a call boundary at
// all, and it leaked 720 direct / 5,740 indirect bytes over 10 iterations at
// both opt levels. `struct_by_value_param_body_takes_field` consults the
// callee BODIES instead, so `m` arms its drop while `n` keeps the behaviour
// the fixture above pins.
//
// The second and third shapes are the pairing check. Widening a drop obliges
// the move sites to widen with it, so a caller-side `let mm = y.m` and a
// `match w.m` on the SAME struct that is also a by-value param elsewhere are
// exercised here: if the newly-armed drop and the move-site zeroing
// disagreed, these would double-free rather than leak. The field CLASS is
// untouched by this row — B-2026-08-07-20 already paired the two for these
// payloads — which is why widening WHICH STRUCTS qualify is safe.
#[test]
fn asan_shared_owning_struct_untouched_option_map_field_freed() {
    assert_clean_asan_run(
        r#"
shared struct Node { v: i64 }
struct Sa { n: Option[Node], m: Option[Map[String, i64]] }
fn mk(i: i64) -> Map[String, i64] {
    let mut mm: Map[String, i64] = Map.new();
    mm.insert("map_key_long_enough_to_force_a_heap_allocation".to_string(), i);
    mm
}
fn reads_n(a: Sa) -> i64 { match a.n { Some(nn) => { nn.v } None => { 0 } } }
fn main() {
    let mut t: i64 = 0;
    let mut i: i64 = 0;
    while i < 10 {
        let x: Sa = Sa { n: Option.Some(Node { v: i }), m: Option.Some(mk(i)) };
        t = t + reads_n(x);
        let y: Sa = Sa { n: Option.Some(Node { v: i }), m: Option.Some(mk(i)) };
        let mm = y.m;
        match mm { Some(z) => { t = t + z.len(); } None => {} }
        let w: Sa = Sa { n: Option.Some(Node { v: i }), m: Option.Some(mk(i)) };
        match w.m { Some(z) => { t = t + z.len(); } None => {} }
        i = i + 1;
    }
    println(t);
}
"#,
        &["65"],
        "shared_owning_struct_untouched_option_map_field",
    );
}

#[test]
fn asan_mapval_inner_map_insert_get_no_uaf() {
    // Slice 3r leg 1 (gap (d) sibling): `m.insert(k, inner)` where `inner`
    // is a Map binding never suppressed the source's `FreeMapHandle` — the
    // inner handle was freed at the builder's scope exit and the outer
    // map's stored handle dangled (SIGSEGV on `m.get(k)` read-back; the
    // suppression walk had arms for Vec/String/shared/enum/struct/tuple
    // but none for a Map/Set-handle binding). Fixed with a branch-safe
    // null-store of the source slot (`karac_map_free*` null-checks).
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Map[i64, Map[i64, String]] {
    let mut inner: Map[i64, String] = Map.new();
    inner.insert(n, f"inner payload padded out beyond thirty-six bytes {n}");
    let mut m: Map[i64, Map[i64, String]] = Map.new();
    m.insert(n, inner);
    m
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let m = build(i);
        match m.get(i) {
            Some(inner) => {
                match inner.get(i) {
                    Some(s) => { println(s.len()); },
                    None => { println("inner-missing"); },
                }
            },
            None => { println("outer-missing"); },
        }
        i = i + 1;
    };
}
"#,
        &["50", "50", "50"],
        "mapval_inner_map_insert_get_no_uaf",
    );
}

#[test]
fn asan_mapval_struct_double_get_no_double_free() {
    // Slice 3r leg 2: `Option[Holder]` is a WIDE payload (4 words > the
    // 3-word inline area), so a `match m.get(k)` scrutinee boxes the
    // bit-copied value — and `track_freshtemp_boxed_enum_scrutinee` armed
    // the box drop's INNER struct walk, freeing the `name` buffer the box
    // merely borrows from the bucket. The second `get` double-freed it
    // (exit 133 pre-fix). A borrow-call scrutinee now gets a box-only free.
    assert_clean_asan_run(
        r#"
struct Holder {
    name: String,
    id: i64,
}
fn build(n: i64) -> Map[i64, Holder] {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {n}", id: n };
    m.insert(n, h);
    m
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let m = build(i);
        match m.get(i) {
            Some(h) => { println(h.name.len() + h.id); },
            None => { println("missing"); },
        }
        match m.get(i) {
            Some(h) => { println(h.id); },
            None => { println("missing"); },
        }
        i = i + 1;
    };
}
"#,
        &["51", "0", "52", "1", "53", "2"],
        "mapval_struct_double_get_no_double_free",
    );
}

#[test]
fn asan_mapval_struct_scope_exit_drop_no_leak() {
    // Slice 3r leg 3 (deferred gap (d)): a struct value's heap content was
    // never freed by the map's scope-exit cleanup — `val_is_vec` only
    // covers the `{ptr,len,cap}` overlay, and `Holder` isn't that shape.
    // The FreeMapHandle arm now routes through
    // `karac_map_free_with_val_drop_fn` with the synthesized
    // `karac_drop_*` value drop. LSan-RED pre-fix (one name buffer per
    // build). Runtime f-string payloads per the 3p spelling-trap
    // discipline.
    assert_clean_asan_run(
        r#"
struct Holder {
    name: String,
    id: i64,
}
fn build(n: i64) -> Map[i64, Holder] {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {n}", id: n };
    m.insert(n, h);
    m
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let m = build(i);
        println(m.len());
        i = i + 1;
    };
}
"#,
        &["1", "1", "1"],
        "mapval_struct_scope_exit_drop_no_leak",
    );
}

#[test]
fn asan_mapval_nested_vec_scope_exit_drop_no_leak() {
    // Slice 3r leg 3 (deferred gap (d), the `Map[K, Vec[Vec[T]]]` value
    // leg): `val_is_vec = 1` freed only the value's OUTER buffer; the
    // middle Vec's element buffers and their strings leaked (176 bytes per
    // build pre-fix). The per-value drop fn is the recursive
    // `karac_drop_Vec_<elem>` from the slice-3n family.
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Map[i64, Vec[Vec[String]]] {
    let mut inner: Vec[String] = Vec.new();
    inner.push(f"nested payload padded out beyond thirty-six bytes {n}");
    let mut outer: Vec[Vec[String]] = Vec.new();
    outer.push(inner);
    let mut m: Map[i64, Vec[Vec[String]]] = Map.new();
    m.insert(n, outer);
    m
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let m = build(i);
        println(m.len());
        i = i + 1;
    };
}
"#,
        &["1", "1", "1"],
        "mapval_nested_vec_scope_exit_drop_no_leak",
    );
}

#[test]
fn asan_mapval_inner_map_scope_exit_drop_no_leak() {
    // Slice 3r leg 3 (deferred gap (d)): an inner-Map VALUE — once leg 1's
    // insert move-suppression makes the bucket the handle's owner — must be
    // freed by the outer map's cleanup via the per-value drop fn
    // (`karac_drop_Map_*`, which recursively releases the inner map's own
    // String values). LSan-RED after leg 1 alone (inner handle + its
    // stored strings leak per build).
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Map[i64, Map[i64, String]] {
    let mut inner: Map[i64, String] = Map.new();
    inner.insert(n, f"inner payload padded out beyond thirty-six bytes {n}");
    let mut m: Map[i64, Map[i64, String]] = Map.new();
    m.insert(n, inner);
    m
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let m = build(i);
        println(m.len());
        i = i + 1;
    };
}
"#,
        &["1", "1", "1"],
        "mapval_inner_map_scope_exit_drop_no_leak",
    );
}

#[test]
fn asan_mapval_vec_of_struct_valued_maps_no_leak() {
    // Slice 3r: `Vec[Map[i64, Holder]]` — the Vec's element-drop loop
    // frees each handle via `emit_free_one_map_handle` with the same
    // per-value drop fn a standalone binding gets
    // (`vec_elem_map_drop_for_type_expr` → `map_temp_cleanup_parts`).
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn build(n: i64) -> Map[i64, Holder] {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {n}", id: n };
    m.insert(n, h);
    m
}
fn main() {
    let mut v: Vec[Map[i64, Holder]] = Vec.new();
    v.push(build(1));
    v.push(build(2));
    println(v.len());
}
"#,
        &["2"],
        "mapval_vec_of_struct_valued_maps_no_leak",
    );
}

#[test]
fn asan_mapval_iterate_struct_values_no_double_free() {
    // Slice 3r: `for (k, v) in m` over a struct-valued map with the
    // per-value drop armed — the loop binding is a bit-copy of the
    // bucket's value; it must alias (not own), or every iteration
    // double-frees against the map's scope-exit value drop.
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {7}", id: 7 };
    m.insert(7, h);
    let mut total = 0;
    for (k, v) in m {
        total = total + k + v.id + (v.name.len() as i64);
    }
    println(total);
}
"#,
        &["65"],
        "mapval_iterate_struct_values_no_double_free",
    );
}

#[test]
fn asan_mapval_overwrite_displaced_struct_value_no_leak() {
    // Slice 3r: inserting over an existing key displaces the OLD value
    // into the discarded `Option[Holder]` result (boxed — Holder is a
    // wide payload); the fresh-temp boxed-Option machinery must free
    // both the box and the displaced value's interior heap.
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut m: Map[i64, Holder] = Map.new();
    let h1 = Holder { name: f"first payload padded out beyond thirty-six bytes {7}", id: 7 };
    let h2 = Holder { name: f"second payload padded out beyond thirty-six bytes {8}", id: 8 };
    m.insert(7, h1);
    m.insert(7, h2);
    println(m.len());
}
"#,
        &["1"],
        "mapval_overwrite_displaced_struct_value_no_leak",
    );
}

#[test]
fn asan_mapval_triple_nested_map_value_no_leak() {
    // Slice 3r: `Map[i64, Map[i64, Map[i64, String]]]` — the upgraded
    // `karac_drop_Map_<K>_<V>` recurses through
    // `map_val_drop_fn_for_type_expr` per level (the 0.c placeholder
    // freed only the handle), so the deepest strings drop.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut inner2: Map[i64, String] = Map.new();
    inner2.insert(1, f"deepest payload padded out beyond thirty-six bytes {1}");
    let mut inner1: Map[i64, Map[i64, String]] = Map.new();
    inner1.insert(2, inner2);
    let mut m: Map[i64, Map[i64, Map[i64, String]]] = Map.new();
    m.insert(3, inner1);
    println(m.len());
}
"#,
        &["1"],
        "mapval_triple_nested_map_value_no_leak",
    );
}

#[test]
fn asan_mapval_remove_struct_value_no_leak() {
    // Slice 3r: `m.remove(k)` moves the stored value out into a
    // DISCARDED `Option[Holder]` (boxed — wide payload); the
    // discarded-boxed-Option tracker must free the box AND the
    // payload's interior heap (`try_track_discarded_boxed_option`).
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {7}", id: 7 };
    m.insert(7, h);
    m.remove(7);
    println(m.len());
}
"#,
        &["0"],
        "mapval_remove_struct_value_no_leak",
    );
}

#[test]
fn asan_mapval_clear_struct_value_no_leak() {
    // Slice 3r: `m.clear()` on a struct-valued map — the clear arm now
    // routes through `karac_map_clear_with_val_drop_fn` (the clear
    // sibling of the scope-exit per-value walk); the flag-based clear
    // leaked every stored value's heap (64 bytes here, visible even to
    // macOS `leaks`).
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {7}", id: 7 };
    m.insert(7, h);
    m.clear();
    println(m.len());
}
"#,
        &["0"],
        "mapval_clear_struct_value_no_leak",
    );
}

#[test]
fn asan_vec_sorted_by_key_heap_elements() {
    // B-2026-08-11-23 — `sorted_by_key` desugars to `{ let mut tmp =
    // v.clone(); tmp.sort_by_key(f); tmp }`, so with heap-bearing elements
    // it creates a SECOND owner of every String in the vector and then
    // returns the clone while the receiver stays live. That is the shape
    // that leaks or double-frees if the clone's element ownership is
    // wrong, and it is not covered by the in-place `sort_by_key` tests
    // (no clone) nor by `sorted_by` (which never reads a field for a key).
    //
    // Both vectors are read after the call so neither can be optimized
    // away, and the strings are padded past the small-string threshold so
    // each one is a real heap allocation rather than an inline buffer.
    assert_clean_asan_run(
        r#"
struct P { name: String, age: i64 }
fn main() {
    let mut ps: Vec[P] = Vec.new();
    ps.push(P { name: f"carol padded beyond thirty-six bytes junk {1}", age: 30 });
    ps.push(P { name: f"alice padded beyond thirty-six bytes junk {1}", age: 10 });
    ps.push(P { name: f"bob padded beyond thirty-six bytes junk {1}", age: 20 });
    let byage = ps.sorted_by_key(|p| p.age);
    for p in byage.iter() { println(p.age); }
    for p in ps.iter() { println(p.age); }
    let byname = ps.sorted_by_key(|p| p.name);
    println(byname.len());
}
"#,
        &["10", "20", "30", "30", "10", "20", "3"],
        "vec_sorted_by_key_heap_elements",
    );
}

#[test]
fn asan_ewmap_trait_bound_map_zip_freed_no_leak() {
    // S6c: `map` / `zip_with` on the `ElementwiseMap` trait — the fresh
    // result container is allocated INSIDE a bound-generic fn and RETURNED
    // (`-> C`) to the caller. This is a new drop shape: the callee's
    // scope-exit cleanup must NOT free the returned container (it's moved
    // out on return), and the caller's `let`-binding must free it exactly
    // once. Also asserts the `ref` operands (`c` / `a` / `b`) aren't
    // double-freed across the generic boundary. Looped for LSan.
    assert_clean_asan_run(
        r#"
fn doubled[C: ElementwiseMap[i64]](c: ref C) -> C {
    c.map(|x| x * 2)
}
fn combine[C: ElementwiseMap[i64]](a: ref C, b: ref C) -> C {
    a.zip_with(b, |x, y| x + y)
}
fn inner() -> i64 {
    let col: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let dc: Column[i64] = doubled(col);
    let t: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);
    let dt: Tensor[i64, [4]] = doubled(t);
    let a: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let b: Column[i64] = Column.from_vec([10, 20, 30, 40]);
    let z: Column[i64] = combine(a, b);
    dc.sum() + dt.sum() + z.sum()
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
        &["3000"], // (20 + 20 + 110) * 20
        "asan_ewmap_trait_bound_map_zip_freed_no_leak",
    );
}

/// B-2026-07-03-30 (Vec-element drain) — a struct field `Vec[Map[i64, String]]`,
/// plain-dropped: each element Map's buckets (and their String values) drain
/// via the recursive drop family (`emit_drop_fn_for_type_expr`), which
/// `vec_elem_agg_drop_for_type_expr` alone did not reach for a direct Map
/// element.
#[test]
fn asan_struct_vec_map_field_plain_drop_drains_elements() {
    assert_clean_asan_run(
        r#"
struct A { rows: Vec[Map[i64, String]] }
fn main() {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut rows: Vec[Map[i64, String]] = Vec.new();
        let mut m: Map[i64, String] = Map.new();
        m.insert(1, "struct_vec_map_field_string_value_payload_x".to_string());
        rows.push(m);
        v.push(A { rows: rows });
        i = i + 1;
    }
    println(v.len());
}
"#,
        &["6"],
        "struct_vec_map_field_plain_drop_drains_elements",
    );
}

#[test]
fn asan_map_get_unwrap_heap_value_no_double_free() {
    // B-2026-07-14-15: `let r = m.get(k).unwrap()` on a Map whose VALUE is a
    // NON-shared heap type (`Vec`/`String`) double-freed — `map.get` returns
    // a BORROW, so `r` shallow-aliased the map's buffer while being registered
    // as an owned Vec/String (scope-exit drop), and both `r`'s drop and the
    // map's value-drop freed the same buffer. `r` is now treated as a
    // borrow-elided alias (no owned drop; the map stays sole owner). Covers a
    // `Vec` value and a `String` value; must be double-free-clean.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut mv: Map[String, Vec[i64]] = Map.new();
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    mv.insert("a", v);
    let rv = mv.get("a").unwrap();
    println(rv.len());

    let mut ms: Map[String, String] = Map.new();
    ms.insert("k", "hello world");
    let rs = ms.get("k").unwrap();
    println(rs.len());
}
"#,
        &["2", "11"],
        "map_get_unwrap_heap_value_no_double_free",
    );
}

#[test]
fn asan_option_map_set_field_owned_by_transfer_no_leak_no_double_free() {
    // B-2026-08-07-12 leg 1 — an `Option[Map]`/`Option[Set]` STRUCT FIELD
    // is freed by the owning struct's drop, and every way of moving it out
    // neutralizes the source exactly once.
    //
    // The field had no drop arm at all: a `Map`/`Set` handle is a single
    // word, so it is neither the inline `{ptr,len,cap}` overlay
    // `option_payload_inline_recursive_drop_ok` matches nor wide enough to
    // be boxed, and its head is in neither `struct_types` nor
    // `enum_layouts`. It fell through all three payload predicates. The
    // GATE above them is the other half: it admitted only a copy-supported
    // struct, and since B-2026-08-05-33 own-by-transfer is a second way to
    // have exactly one owner — which is why `Sib` here carries a sibling
    // `Option[String]`, a field that leaked purely because the gate fails
    // per-STRUCT rather than per-field (1,351 B / 40 of this fixture's
    // pre-fix total is that String, and it is clean the moment the Map
    // field is deleted).
    //
    // THE MOVE ARMS ARE THE DOUBLE-FREE HALF and must be read with the
    // leak arms, not separately: widening the drop without widening the
    // move-site zero is a double free, per the pairing rule in
    // `place_optres_field_move_info_ex`. `mo` covers the PATTERN leg
    // (narrow class), `wm` the WHOLE-MOVE leg, `taken` the let-move leg,
    // and `eat_om` the pattern leg reached inside a callee. Measured: with
    // the classifier widened and the pattern leg left alone, the three
    // pattern/whole shapes went from clean to 470 valgrind errors with
    // invalid frees at BOTH opt levels.
    //
    // STASH-PROVEN RED at HEAD: 14,400 B / 200 blocks at -O2 and 15,751 B
    // / 240 at -O0. The stdout is 20440 BEFORE and after, so an
    // output-only E2E cannot observe this defect at all — the sanitizer
    // twin is the whole test, which is why there is no codegen.rs peer.
    // Expected total DERIVED, not read off a run: the per-iteration
    // contributions are the disjoint bits 1+2+4+8+16+32+64+128+256 = 511
    // (every `inner.len()` is 1, one entry per map), over 40 iterations =
    // 20440.
    assert_clean_asan_run_min_allocs(
        r#"
struct Om { s: Option[Map[i64, String]] }
struct Os { s: Option[Set[i64]] }
struct Sib { p: Option[String], m: Option[Map[i64, String]] }
fn ignore_om(x: Om) -> i64 { 1 }
fn consume_opt(o: Option[Map[i64, String]]) -> i64 {
    match o {
        Option.Some(inner) => { inner.len() }
        Option.None => { 0 }
    }
}
fn eat_om(x: Om) -> i64 {
    match x.s {
        Option.Some(inner) => { inner.len() }
        Option.None => { 0 }
    }
}
fn mk_map(k: i64) -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(k, f"mapv-{k}-padded-out-to-force-a-real-heap-buffer");
    m
}
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a: Om = Om { s: Option.Some(mk_map(n + i)) };
        acc = acc + 1;
        let mut st: Set[i64] = Set.new();
        st.insert(n + i);
        let b: Os = Os { s: Option.Some(st) };
        acc = acc + 2;
        acc = acc + ignore_om(Om { s: Option.Some(mk_map(n + i)) }) * 4;
        let named: Om = Om { s: Option.Some(mk_map(n + i)) };
        acc = acc + ignore_om(named) * 8;
        let sib: Sib = Sib { p: Option.Some(f"sib-{n + i}-padded-out-well-past-inline"), m: Option.Some(mk_map(n + i)) };
        acc = acc + 16;
        let mo: Om = Om { s: Option.Some(mk_map(n + i)) };
        match mo.s {
            Option.Some(inner) => { acc = acc + inner.len() * 32; }
            Option.None => { acc = acc + 0; }
        }
        let wm: Om = Om { s: Option.Some(mk_map(n + i)) };
        acc = acc + consume_opt(wm.s) * 64;
        let lm: Om = Om { s: Option.Some(mk_map(n + i)) };
        let taken = lm.s;
        acc = acc + consume_opt(taken) * 128;
        acc = acc + eat_om(Om { s: Option.Some(mk_map(n + i)) }) * 256;
        i = i + 1;
    }
    println(acc);
}
"#,
        &["20440"],
        "option_map_set_field_owned_by_transfer",
        100,
    );
}

#[test]
fn asan_struct_move_out_map_set_enum_field_no_double_free() {
    // B-2026-07-15-23: moving a struct carrying a Map/Set handle field or an
    // enum-with-heap-payload field double-freed the handle / enum buffer at
    // scope exit — SIGSEGV (Map/Set) or `free(): double free` (enum). A Map/Set
    // field is a bare `ptr` the struct's `FieldDrop::MapOrSet` frees
    // UNCONDITIONALLY (`karac_map_free_with_drop_vec`, null-safe), but neither
    // `zero_struct_move_caps` (whole-struct move `let g = f`) nor the nested-
    // field suppressor's LLVM-type-driven `zero_aggregate_field_caps`
    // (`let bound = o.inner`) nulled it; the enum leaf was likewise invisible to
    // the LLVM-type walk. Fixed by (1) a Map/Set null-the-handle arm in
    // `zero_struct_move_caps` and (2) routing named-struct fields through
    // `zero_struct_move_caps` in `suppress_struct_field_move_into_literal`.
    // Verify the whole round-trip is double-free AND leak clean under ASAN/LSan
    // (the delicate move-suppression tension: nulling too eagerly would strand
    // the map storage). Loops so a leak accumulates. Covers Map + Set whole-
    // struct move, Map nested-field move, and an enum-with-String-payload
    // nested-field move. Was ASAN-dirty (2500 errors) pre-fix — the double-free
    // fires at scope exit AFTER the prints, invisible to an output-only harness.
    assert_clean_asan_run(
        r#"
enum Tok { Word(String), Num(i64) }
struct MHolder { m: Map[i64, i64] }
struct SHolder { s: Set[i64] }
struct EInner { t: Tok }
struct EOuter { inner: EInner }
struct MInner { m: Map[i64, i64] }
struct MOuter { inner: MInner }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 100 {
        let mut mp: Map[i64, i64] = Map.new();
        mp.insert(i, i * 10); mp.insert(i + 1, i * 20);
        let f: MHolder = MHolder { m: mp };
        let g: MHolder = f;
        total = total + g.m.len();
        let mut mp2: Map[i64, i64] = Map.new();
        mp2.insert(i, 1); mp2.insert(i + 1, 2); mp2.insert(i + 2, 3);
        let o: MOuter = MOuter { inner: MInner { m: mp2 } };
        let bound: MInner = o.inner;
        total = total + bound.m.len();
        let mut st: Set[i64] = Set.new();
        st.insert(i); st.insert(i + 1);
        let sf: SHolder = SHolder { s: st };
        let sg: SHolder = sf;
        total = total + sg.s.len();
        let mut b: String = String.new();
        b.push_str("padded well past inline width to force heap alloc here");
        let eo: EOuter = EOuter { inner: EInner { t: Tok.Word(b) } };
        let eb: EInner = eo.inner;
        match eb.t {
            Tok.Word(w) => { total = total + w.len(); },
            Tok.Num(n) => { total = total + n; },
        }
        i = i + 1;
    }
    println(total);
}
"#,
        &["6100"],
        "struct_move_out_map_set_enum_field_no_double_free",
    );
}

#[test]
fn asan_mono_bare_t_heap_field_before_map_no_double_free() {
    // B-2026-07-15-24: a generic struct with a bare generic-param field bound
    // to a WIDER heap type (`Vec`/`String` = a 3-word `{ptr,len,cap}` triple)
    // placed BEFORE a Map/Vec field. The base `struct_types` layout erases the
    // bare-T field to one i64 word, so the mono widening shifted every
    // following field's offset — the drop-synthesis + move-suppression GEPs
    // targeted the base offset, leaving the Map handle / following Vec live and
    // double-freeing at scope exit (SIGSEGV / `free(): double free`; the reads
    // already used the mono layout, so only the drop crashed). Fixed by GEPing
    // those fields with the per-monomorph layout (`mono_struct_type_from_subst`
    // threaded into `emit_struct_drop_synthesis` + `zero_struct_move_caps_mono`)
    // and generalizing the bare-T heap-field classifier from single-field to
    // any position, plus deriving the moved-out binding's mono instantiation
    // (`field_move_out_struct_inst`) for `let bound = o.inner`. Loops so a
    // per-iteration double-free trips ASAN and any strand accumulates as a
    // leak. Covers a `Vec[String]` bare-T before Map (drains inner elements, no
    // move), two bare-T `Vec` fields before Map with a whole-struct move, and a
    // nested field move-out. Only the crash surfaces at scope exit AFTER the
    // reads, invisible to an output-only harness.
    assert_clean_asan_run(
        r#"
struct SBefore[T] { a: T, m: Map[i64, i64] }
struct TwoHeap[T] { a: T, b: T, m: Map[i64, i64] }
struct GInner[T] { a: T, m: Map[i64, i64] }
struct GOuter[T] { inner: GInner[T] }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 100 {
        let mut m1: Map[i64, i64] = Map.new();
        m1.insert(i, i + 11);
        let vsa: Vec[String] = ["padded aaaaaaaaaaaaaaaaaaaaaaa", "padded bbbbbbbbbbbbbbbbbbbbbbb"];
        let x1 = SBefore { a: vsa, m: m1 };
        total = total + x1.a[1].len();
        total = total + x1.m.get(i).unwrap();
        let mut m2: Map[i64, i64] = Map.new();
        m2.insert(i, 22);
        let v1: Vec[i64] = [1, 2, 3];
        let v2: Vec[i64] = [4, 5, 6, 7];
        let th = TwoHeap { a: v1, b: v2, m: m2 };
        let th2 = th;
        total = total + th2.a[2] + th2.b[3];
        total = total + th2.m.get(i).unwrap();
        let mut mp: Map[i64, i64] = Map.new();
        mp.insert(i, 99);
        let vv: Vec[i64] = [7, 8, 9];
        let gi = GInner { a: vv, m: mp };
        let o = GOuter { inner: gi };
        let bound = o.inner;
        total = total + bound.a[0];
        total = total + bound.m.get(i).unwrap();
        i = i + 1;
    }
    println(total);
}
"#,
        &["22850"],
        "mono_bare_t_heap_field_before_map_no_double_free",
    );
}

#[test]
fn asan_filter_map_heap_collect_no_leak() {
    // B-2026-07-19-14: `<filter_map-chain>.collect()` routes through the
    // fused for-loop lowering that pushes into a fresh `Vec[String]`
    // accumulator. Each iteration builds new heap `String` payloads (the
    // uppercased words that pass the filter) AND discards the filtered-out
    // elements' would-be payloads (the `None` arm produces nothing) — so a
    // missing drop on either the collected buffer at scope exit, or the
    // dropped source elements, or the per-iteration accumulator would
    // accumulate into a visible LSan leak. Loop many times so any
    // per-iteration strand is unmistakable. Also exercises the FOR-LOOP
    // path (no collect) over the same chain in the second half.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let v: Vec[String] = ["hi".to_string(), "".to_string(), "yo".to_string(), "there".to_string()];
        let w: Vec[String] = v.iter().filter_map(|s| if s.len() > 0 { Some(s.to_uppercase()) } else { None }).collect();
        let mut j: i64 = 0;
        while j < w.len() {
            acc = acc + w[j].len();
            j = j + 1;
        }
        let u: Vec[String] = ["a".to_string(), "".to_string(), "bb".to_string()];
        for t in u.iter().filter_map(|s| if s.len() > 0 { Some(s.to_uppercase()) } else { None }) {
            acc = acc + t.len();
        }
        i = i + 1;
    }
    println(acc);
}
"#,
        &["480"],
        "filter_map_heap_collect_no_leak",
    );
}

#[test]
fn asan_vec_sorted_heap_clone_no_leak_no_double_free() {
    // B-2026-07-19-15 — `Vec[String].sorted()` deep-CLONES the receiver
    // (each String buffer duplicated), sorts the clone, and returns it; the
    // receiver stays intact. The clone's buffers and the receiver's buffers
    // must EACH free exactly once — a bug is a per-iteration leak (clone not
    // freed) or a double-free (clone aliasing the receiver's buffers).
    // Loop a heap `Vec[String]`, sort a fresh clone, and use BOTH so both
    // reach scope-exit drop each iteration.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let v: Vec[String] = ["delta".to_string(), "alpha".to_string(), "charlie".to_string(), "bravo".to_string()];
        let s: Vec[String] = v.sorted();
        // sorted clone: alpha, bravo, charlie, delta — use each
        let mut j: i64 = 0;
        while j < s.len() { acc = acc + s[j].len(); j = j + 1; }
        // receiver survives, unsorted — use each
        let mut k: i64 = 0;
        while k < v.len() { acc = acc + v[k].len(); k = k + 1; }
        i = i + 1;
    }
    println(acc);
}
"#,
        // per iter: (5+5+7+5) clone + (5+5+7+5) receiver = 44; ×40 = 1760
        &["1760"],
        "vec_sorted_heap_clone_no_leak_no_double_free",
    );
}

#[test]
fn asan_result_struct_wrapper_map_field_moved_to_callee_no_use_after_free() {
    // B-2026-08-11-29 — `match mk() { Ok(s) => take(s) }` where `S` has a
    // `Map`/`Set` field. The by-value struct param is CALLEE-owned, so
    // `take` ends in `__karac_drop_struct_S` and frees the Map; the arm's
    // source payload drop stayed armed and freed it again. It surfaced as a
    // plain SEGV (exit 139) on a default build, with `karac check` clean and
    // the interpreter correct — valgrind names it precisely: an invalid read
    // in `karac_map_free_with_drop_vec` over a block already freed by main.
    //
    // ASAN is the gate that separates the fix from a relabelling, because
    // the repair is "stop the SOURCE freeing": suppress one too many and the
    // Map leaks instead. LSan (the Linux leg) is what catches that, and the
    // `Vec`-field arm below is here because it was ALREADY suppressed
    // through a different predicate — it must stay exactly as clean.
    assert_clean_asan_run(
        r#"
enum E { Missing(String) }
struct Sm { a: Map[String, i64] }
struct Ss { a: Set[String] }
struct Sv { a: Vec[String] }
fn mk_map() -> Result[Sm, E] {
    let mut m: Map[String, i64] = Map.new();
    m.insert("k", 1);
    Ok(Sm { a: m })
}
fn mk_set() -> Result[Ss, E] {
    let mut s: Set[String] = Set.new();
    s.insert("k");
    Ok(Ss { a: s })
}
fn mk_vec() -> Result[Sv, E] {
    let mut v: Vec[String] = Vec.new();
    v.push("k");
    Ok(Sv { a: v })
}
fn take_map(s: Sm) -> i64 { return s.a.len(); }
fn take_set(s: Ss) -> i64 { return s.a.len(); }
fn take_vec(s: Sv) -> i64 { return s.a.len(); }
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 30 {
        match mk_map() { Ok(s) => { total = total + take_map(s); }, Err(_e) => { total = total + 100; } }
        match mk_set() { Ok(s) => { total = total + take_set(s); }, Err(_e) => { total = total + 100; } }
        match mk_vec() { Ok(s) => { total = total + take_vec(s); }, Err(_e) => { total = total + 100; } }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        &["90"],
        "result_struct_wrapper_map_field_moved_to_callee_no_use_after_free",
    );
}

#[test]
fn asan_vec_sorted_by_heap_clone_no_leak_no_double_free() {
    // B-2026-07-20-8 — `Vec[String].sorted_by(cmp)` is the comparator
    // sibling of `sorted()`: deep-clone the receiver, `sort_by(cmp)` the
    // clone (runtime callback thunk for String elements), return it; the
    // receiver stays intact. Same invariant: the clone's buffers and the
    // receiver's buffers each free exactly once (a bug is a per-iteration
    // leak or a double-free). Loop, sort DESCENDING via the comparator so
    // the result provably ran the user closure, and use both vectors.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let v: Vec[String] = ["delta".to_string(), "alpha".to_string(), "charlie".to_string(), "bravo".to_string()];
        let s: Vec[String] = v.sorted_by(|a, b| b.cmp(a));
        // descending clone: delta first — fold its head length in twice so a
        // wrong order changes the oracle
        acc = acc + s[0].len();
        let mut j: i64 = 0;
        while j < s.len() { acc = acc + s[j].len(); j = j + 1; }
        let mut k: i64 = 0;
        while k < v.len() { acc = acc + v[k].len(); k = k + 1; }
        i = i + 1;
    }
    println(acc);
}
"#,
        // per iter: head "delta"(5) + clone 22 + receiver 22 = 49; ×40 = 1960
        &["1960"],
        "vec_sorted_by_heap_clone_no_leak_no_double_free",
    );
}

#[test]
fn asan_reassign_heap_field_and_map_set_var_no_leak_no_double_free() {
    // B-2026-07-15-25: reassigning a struct's heap-owning FIELD (`h.v = …`,
    // `h.s = …`, `h.m = …`) used to overwrite the slot with NO drop of the
    // old value — leaking it for a fresh RHS and double-freeing / SIGSEGVing
    // for a moved-binding RHS (the source's own cleanup then freed the
    // now-shared buffer). Separately, Map/Set VARIABLE reassignment
    // (`m = m2`, `set_a = set_b`) was uncovered by the Vec-only eager-free +
    // move-suppression, so it leaked the old handle AND double-freed the
    // source. Fixed by (a) dropping the old field value in
    // `compile_field_store`'s plain-owned-struct-field branch + suppressing
    // the moved source in the Assign FieldAccess arm, and (b) an eager
    // old-handle free + source suppression for Map/Set vars in the Assign
    // var-reassign arm. Loop every shape (fresh AND moved-binding RHS) so a
    // per-iteration strand accumulates into a visible LSan leak, and a
    // double-free surfaces as SIGABRT under ASAN.
    assert_clean_asan_run(
        r#"
struct Hv { v: Vec[i64] }
struct Hs { s: String }
struct Hm { m: Map[i64, i64] }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let mut a: Vec[i64] = Vec.new();
        a.push(i); a.push(i + 1);
        let mut hv = Hv { v: a };
        let mut b: Vec[i64] = Vec.new();
        b.push(i); b.push(i + 1); b.push(i + 2);
        hv.v = b;
        hv.v = [i, i + 1, i + 2, i + 3];
        acc = acc + hv.v.len();

        let mut hs = Hs { s: i.to_string() };
        let ns = (i * 100).to_string();
        hs.s = ns;
        acc = acc + hs.s.len();

        let mut m1: Map[i64, i64] = Map.new();
        m1.insert(1, i);
        let mut hm = Hm { m: m1 };
        let mut m2: Map[i64, i64] = Map.new();
        m2.insert(2, i); m2.insert(3, i);
        hm.m = m2;
        acc = acc + hm.m.len();

        let mut mv: Map[i64, i64] = Map.new();
        mv.insert(9, i);
        let mut mv2: Map[i64, i64] = Map.new();
        mv2.insert(8, i); mv2.insert(7, i);
        mv = mv2;
        acc = acc + mv.len();

        let mut sa: Set[i64] = Set.new();
        sa.insert(i);
        let mut sb: Set[i64] = Set.new();
        sb.insert(i + 1); sb.insert(i + 2);
        sa = sb;
        acc = acc + sa.len();

        i = i + 1;
    }
    println(acc);
}
"#,
        &["548"],
        "reassign_heap_field_and_map_set_var_no_leak_no_double_free",
    );
}

#[test]
fn asan_field_map_get_unwrap_heap_value_no_double_free() {
    // B-2026-07-16-1: `<struct-field-map>.get(k).unwrap()` of a heap value
    // (`Map[_, String]` / `Map[_, Vec]` FIELD) double-freed the value buffer,
    // both bound AND inline, against the struct's scope-exit map drop. The
    // get/unwrap borrow-elision detectors (B-2026-07-14-15 bound registration,
    // B-2026-07-15-26 inline cap-zero) recognised only a bare-identifier map
    // receiver; a field-access receiver (`h.ms`, `self.ms`) fell through, so
    // the unwrapped value kept its real `cap` and its consumer's free-guard
    // released the map's stored buffer — freed again by the map drop. Fixed by
    // resolving the field-access map's value type from the struct field
    // (`map_receiver_value_type_expr`, mono-subst aware) so the unwrap zeroes
    // the borrow view's `cap`. Loops so a per-iteration double-free trips ASAN
    // and any strand accumulates as a leak. Exercises a `String` value across
    // a method receiver / a by-value fn arg / a bound read, and a `Vec` value
    // indexed inline + bound — all through a struct-field map. Only the
    // double-free surfaces at scope exit AFTER the reads.
    assert_clean_asan_run(
        r#"
struct Holder { tag: i64, ms: Map[i64, String], mv: Map[i64, Vec[i64]] }
fn takes(s: String) -> i64 {
    s.len()
}
fn main() {
    let mut ms: Map[i64, String] = Map.new();
    ms.insert(1, "padded aaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    ms.insert(2, "padded bbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let mut mv: Map[i64, Vec[i64]] = Map.new();
    mv.insert(1, [10, 20, 30]);
    let h = Holder { tag: 7, ms: ms, mv: mv };
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 100 {
        acc = acc + h.ms.get(1).unwrap().len();
        acc = acc + takes(h.ms.get(2).unwrap());
        let v = h.ms.get(1).unwrap();
        acc = acc + v.len();
        acc = acc + h.mv.get(1).unwrap()[1];
        let row = h.mv.get(1).unwrap();
        acc = acc + row[2];
        i = i + 1;
    }
    println(acc);
    println(h.ms.get(1).unwrap());
}
"#,
        &["15500", "padded aaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        "field_map_get_unwrap_heap_value_no_double_free",
    );
}

// B-2026-07-23-1 — an early `return` out of a `for (k, v) in map` /
// `for x in set` loop bypassed the loop's exit block, where the sole
// `karac_map_iter_free` lived, leaking the `karac_map_iter_new` handle on
// every early exit. The fix registers the free as a `FreeMapIter` cleanup
// in the ENCLOSING frame (drained on `return`) while the exit block nulls
// the iterator slot so normal exhaustion / `break` stay exactly-once.
// Looped 300× so any per-call iterator leak accumulates into a definite
// LeakSanitizer report; the map surfaced it (kata #211, trie wildcard DFS).
#[test]
fn asan_map_set_iter_early_return_no_leak() {
    assert_clean_asan_run(
        r#"
fn first_ge(m: Map[i64, i64], thresh: i64) -> i64 {
    for (k, v) in m {
        if v >= thresh { return v; }   // early return mid-iteration
    }
    return -1i64;
}

fn has_elem(s: Set[i64], target: i64) -> bool {
    for x in s {
        if x == target { return true; }   // early return out of set iteration
    }
    return false;
}

fn main() {
    let mut hits = 0i64;
    let mut i = 0i64;
    while i < 300i64 {
        let mut m: Map[i64, i64] = Map.new();
        let _ = m.insert(1i64, 10i64);
        let _ = m.insert(2i64, 20i64);
        let _ = m.insert(3i64, 30i64);
        if first_ge(m, 15i64) >= 0i64 { hits = hits + 1i64; }

        let mut s: Set[i64] = Set.new();
        s.insert(4i64);
        s.insert(5i64);
        s.insert(6i64);
        if has_elem(s, 5i64) { hits = hits + 1i64; }
        i = i + 1;
    }
    println(hits);
}
"#,
        // Each iter: first_ge finds a value >= 15 (hit) and has_elem finds 5
        // (hit) → +2; ×300 = 600.
        &["600"],
        "map_set_iter_early_return_no_leak",
    );
}

/// B-2026-07-25-5: `m[k].push(x)` — an indexed-receiver method call whose
/// outer is a Map — lowers through `karac_map_lookup_slot`, so the element
/// pointer ALIASES the value half of the key's bucket rather than a copied
/// out-slot. That aliasing is the whole point (MR3: mutations must
/// propagate), which also makes it the shape most likely to double-free or
/// leak: the pushed element is adopted by a Vec living inside the map's kv
/// buffer, and the map's own teardown must free it exactly once.
///
/// The loop drives repeated lookups on a heap key with growth on the value
/// Vec (200 pushes across 7 keys forces several reallocs of the in-bucket
/// Vec), and the key expression is a live `Vec[String]` element read — the
/// case where freeing the key buffer after lookup would be a
/// use-after-free rather than the fix for a leak.
///
/// LeakSanitizer only runs on Linux, so the leak half of this is
/// authoritative in Linux CI, not on a local macOS run.
#[test]
fn asan_map_indexed_receiver_push_no_leak() {
    let label = "map_indexed_receiver_push";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan_with_full_pipeline(
        r#"
fn main() {
    let mut froms: Vec[String] = Vec.new();
    let mut tos: Vec[String] = Vec.new();
    let mut i = 0i64;
    while i < 200i64 {
        froms.push(f"K{i % 7i64}");
        tos.push(f"V{i}");
        i = i + 1;
    }

    let mut adj: Map[String, Vec[String]] = Map.new();
    let mut j = 0i64;
    while j < froms.len() {
        let k = froms[j].clone();
        let present = match adj.get(k) { Some(_) => true, None => false };
        if not present {
            let empty: Vec[String] = Vec.new();
            let _ = adj.insert(k, empty);
        }
        adj[k].push(tos[j]);
        j = j + 1i64;
    }

    let mut total = 0i64;
    for key in adj.keys() {
        let d: Vec[String] = match adj.get(key) { Some(v) => v, None => Vec.new() };
        total = total + d.len();
    }
    println(f"keys={adj.len()} total={total}");

    // Scalar-keyed sibling: same lowering, no heap key half.
    let mut mi: Map[i64, Vec[i64]] = Map.new();
    let mut s = 0i64;
    while s < 5i64 {
        let empty: Vec[i64] = Vec.new();
        let _ = mi.insert(s, empty);
        s = s + 1i64;
    }
    let mut t = 0i64;
    while t < 100i64 {
        mi[t % 5i64].push(t);
        t = t + 1i64;
    }
    let mut sum = 0i64;
    let mut u = 0i64;
    while u < 5i64 {
        let d: Vec[i64] = match mi.get(u) { Some(v) => v, None => Vec.new() };
        sum = sum + d.len();
        u = u + 1i64;
    }
    println(f"scalar={sum}");
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN/LSAN reported a memory error (exit code {:?}) — the \
             indexed-receiver Map path aliases the in-bucket value, so look for a \
             double-free of a pushed element or a leak of the value Vec's buffer",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["keys=7 total=200", "scalar=100"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

#[test]
fn asan_struct_field_map_heap_value_drop_no_leak() {
    // B-2026-07-27-2: a struct FIELD `Map[_, Vec[String]]` freed the map and
    // each value Vec's `{ptr,len,cap}` buffer but never walked that Vec's
    // ELEMENTS, so every String inside every value leaked — scaling with
    // entry count, so a real adjacency map / multimap index leaks
    // proportionally. The LOCAL-binding path was always clean: it routes
    // through `emit_free_one_map_handle`, which prefers
    // `karac_map_free_with_val_drop_fn` when the value owns heap below its
    // header. The struct-drop `MapOrSet` arm hand-rolled the flag call and
    // skipped that branch; it now takes the same per-value drop fn.
    //
    // The regression risk runs BOTH ways, so this covers both: a missing
    // walk leaks, and a walk that double-counts against the flag free would
    // double-free. Shapes: heap value under a scalar key and under a heap
    // key (the leaking pair); a nested `Map` value; `Map[String, String]`
    // and `Set[String]` (exact one-level overlays that must stay on the
    // flag path); and `Map[_, Vec[i64]]` / `Map[i64, i64]` (Copy values that
    // must not be walked at all). Looped so a leak clears LSan's floor.
    let label = "struct_field_map_heap_value_drop";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan_with_full_pipeline(
        r#"
struct A { m: Map[i64, Vec[String]] }
struct B { m: Map[String, Vec[String]] }
struct C { m: Map[String, String] }
struct D { m: Map[i64, Map[i64, String]] }
struct E { s: Set[String] }
struct F { m: Map[i64, Vec[i64]] }
struct G { m: Map[i64, i64] }

fn main() {
    let mut a = A { m: Map.new() };
    let mut i = 0i64;
    while i < 40i64 {
        let mut v: Vec[String] = Vec.new();
        v.push(f"x{i}");
        v.push(f"yy{i}");
        a.m.insert(i, v);
        i = i + 1;
    }
    println(f"a={a.m.len()}");

    let mut b = B { m: Map.new() };
    let mut j = 0i64;
    while j < 40i64 {
        let mut v: Vec[String] = Vec.new();
        v.push(f"p{j}");
        b.m.insert(f"k{j}", v);
        j = j + 1i64;
    }
    println(f"b={b.m.len()}");

    let mut c = C { m: Map.new() };
    let mut k = 0i64;
    while k < 20i64 {
        c.m.insert(f"ck{k}", f"cv{k}");
        k = k + 1i64;
    }
    println(f"c={c.m.len()}");

    let mut d = D { m: Map.new() };
    let mut n = 0i64;
    while n < 10i64 {
        let mut inner: Map[i64, String] = Map.new();
        inner.insert(n, f"z{n}");
        d.m.insert(n, inner);
        n = n + 1i64;
    }
    println(f"d={d.m.len()}");

    let mut e = E { s: Set.new() };
    let mut q = 0i64;
    while q < 20i64 {
        e.s.insert(f"s{q}");
        q = q + 1i64;
    }
    println(f"e={e.s.len()}");

    let mut f = F { m: Map.new() };
    let mut r = 0i64;
    while r < 20i64 {
        let mut vi: Vec[i64] = Vec.new();
        vi.push(r);
        f.m.insert(r, vi);
        r = r + 1i64;
    }
    println(f"f={f.m.len()}");

    let mut g = G { m: Map.new() };
    let mut t = 0i64;
    while t < 20i64 {
        g.m.insert(t, t * 2i64);
        t = t + 1i64;
    }
    println(f"g={g.m.len()}");
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN/LSAN reported a memory error (exit code {:?}) — the \
             struct-drop Map arm routes a heap value through its own drop fn, so \
             look for a leak of the value Vecs' element buffers (walk missing) or \
             a double-free of those elements (walked AND flag-freed)",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["a=40", "b=40", "c=20", "d=10", "e=20", "f=20", "g=20"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

// B-2026-07-30-11 (Map-values leg) — value bodies for let-bound Maps,
// plus the remove-tombstone skip, the insert-of-binding full disarm, and
// the whole-map rebind disarm. Same unsafe-direction guard as the other
// legs: the bucket walk frees nothing, so what ASAN catches is an
// OVER-fire — the walk reading a tombstoned (removed) slot's stale bytes,
// a moved-from binding's wiped value, or the rebound source's buckets
// after the destination freed them. The `self.buf[0]` read keeps the
// element allocations live (the standing vacuity lesson).
#[test]
fn asan_map_value_user_drop_bodies_fire_once() {
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
    return Full { name: "map-value-payload-string", buf: b };
}

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        // Held to the binding's end — the value body fires via the walk.
        let mut m: Map[i64, Full] = Map.new();
        m.insert(i, mk(i));
        n = n + 1i64;

        // Removed value leaves through the Option; the walk must skip the
        // tombstone and the moved-out binding runs the body instead.
        let mut m2: Map[i64, Full] = Map.new();
        m2.insert(i, mk(i));
        let out = m2.remove(i);
        n = n + 1i64;

        // Insert of a BINDING: the source's own action is fully disarmed;
        // only the map walk runs the body.
        let mut m3: Map[i64, Full] = Map.new();
        let r = mk(i);
        m3.insert(i, r);
        n = n + 1i64;

        // Whole-map REBIND (B-2026-07-31-27): the move nulls m4's slot and
        // `transfer_map_handle_on_rebind` copies the handle free onto m5 —
        // exactly one free of the handle + arrays + values. Before the fix
        // the ENTIRE map leaked here; the loop makes that LSan-visible (each
        // iteration's dead map is unreachable once the allocas are
        // overwritten by the next one — unlike the straight-line shape,
        // whose stale slot pointer LSan's conservative stack scan finds).
        let mut m4: Map[i64, Full] = Map.new();
        m4.insert(i, mk(i));
        let m5 = m4;
        n = n + 1i64;

        i = i + 1;
    }
    println(n);
}
"#,
        // 4 per iteration x 200 = 800.
        &["800"],
        "map_value_user_drop_bodies_fire_once",
    );
}

/// B-2026-08-08-25 leg 1, the CHAIN interaction — one owner, not zero.
///
/// `suppress_inline_option_result_binding_move` disarms a source binding
/// when a consuming combinator has taken its payload, and for
/// `<src>.map(f).unwrap_or(d)` it resolves back through the map to `<src>`
/// (B-2026-08-07-3) on the premise that the map's ARM already owns the
/// buffer. Leg 1 makes that premise conditional: once the arm is classified
/// as a borrow it frees nothing, so disarming the source as well leaves the
/// payload with NOBODY.
///
/// Caught by `asan_option_map_heap_payload_no_leak` rather than by
/// reasoning, and it is worth recording WHY a direct valgrind run on
/// `karac build` output said the program was clean: the payload here is a
/// `Vec[i64]` read only through `.len()`, which at the default opt level is
/// a provably dead allocation LLVM deletes outright. The leak is real at
/// every level — only its observability moves. Pinned at both.
#[test]
fn asan_chained_map_unwrap_or_over_live_option_frees_payload_exactly_once() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let mut v: Vec[i64] = Vec.new();
        v.push(1i64); v.push(2i64); v.push(3i64);
        let opt: Option[Vec[i64]] = Some(v);
        // The shape that leaked: the mapper only reads, so the arm borrows —
        // and `unwrap_or` must then NOT disarm `opt`, which still owns it.
        total = total + opt.map(|xs| xs.len()).unwrap_or(0i64);
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        &["120"],
        "chained_map_unwrap_or_live_option",
    );
}

/// B-2026-08-09-6 — `Result[i64, String].map(f)` DOUBLE-FREED the `Err`
/// payload. `map` picked its lowering from `T` alone, so a scalar `T` took
/// the hand-rolled path whose absent branch is a shallow copy of the
/// receiver's words: the result and the receiver both ended up owning the
/// same `{ptr,len,cap}`, and both freed it (`free(): double free detected in
/// tcache 2`).
///
/// This is the half the original report did NOT name — it recorded only the
/// `Result[String, String]` shape below, where the same root cause is a
/// silent 4-byte leak. Worth keeping both: they are one defect that presents
/// as an abort or as nothing at all depending on which side of the `Result`
/// happens to hold heap, and only the abort is self-announcing.
///
/// DISCRIMINATING LEVEL IS `-O0`, measured both ways. On the broken tree
/// this fixture PASSES at the default level — the standalone program aborts
/// with `free(): double free detected in tcache 2` at `KARAC_OPT_LEVEL=0`
/// but prints its answer cleanly at `-O2`, and making the payload
/// non-constant (`f"boom{i}"`) does not change that. So the CI `-O0` leg is
/// what actually pins this one; at the default level it is a guard against
/// future drift, not a witness. Verified red at `-O0` on the pre-fix tree
/// rather than assumed.
#[test]
fn asan_result_map_scalar_payload_frees_its_heap_err_exactly_once() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let r: Result[i64, String] = Err(f"boom");
        // Scalar `T` + heap `E`: the receiver's Err buffer must reach the result
        // exactly once, not be aliased into it and freed twice.
        match r.map(|x| x + 1i64) {
            Ok(v) => { total = total + v; }
            Err(e) => { total = total + e.len(); }
        }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        &["160"],
        "result_map_scalar_payload_heap_err",
    );
}

/// B-2026-08-09-7 — a chained `Result` combinator evaluated its receiver
/// TWICE. `try_compile_option_result_method` compiled the receiver eagerly,
/// and `compile_map_via_match_synthesis` then compiled the same expression
/// again as the synthesized match's scrutinee.
///
/// The memory consequence is what this fixture is for: the FIRST evaluation
/// moves the heap payload out and zeroes the source's words, so the second
/// reads a moved-from value — the buffer is reachable through two paths that
/// disagree about who owns it. The E2E pin catches the wrong output; this
/// one asserts the buffer is allocated and freed exactly once per iteration
/// with the chain running 40 times.
///
/// The `let`-bound form is the control: it never double-evaluated, because
/// an identifier receiver's extra compile is a dead reload rather than a
/// re-run of the inner combinator.
#[test]
fn asan_chained_result_map_evaluates_its_receiver_once() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let r: Result[String, String] = Ok(f"hi");
        // Chained without an intervening `let` — the shape that re-ran the
        // inner map over a source it had already moved out of.
        match r.map(|x| x.to_uppercase()).map(|x| x.to_uppercase()) {
            Ok(v) => { total = total + v.len(); }
            Err(e) => { total = total + e.len(); }
        }
        let s: Result[String, String] = Err(f"boom");
        match s.map(|x| x.to_uppercase()).map(|x| x.to_uppercase()) {
            Ok(v) => { total = total + v.len(); }
            Err(e) => { total = total + e.len(); }
        }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        &["240"],
        "chained_result_map_receiver_once",
    );
}

/// B-2026-08-12-2 (split out of B-2026-08-11-30's leg A). A `Map` field of
/// an `Ok` payload leaks when the `Result` is LET-BOUND before the match;
/// matching the producing call inline is clean. No by-value parameter
/// boundary is involved, and the identical shape with a `Vec` field is
/// clean — which is why this is its own row rather than a polarity of the
/// param bug above.
///
/// FIXED: the arm's struct-payload binding is now admitted when the struct's
/// only non-duplicable heap is a `Map`/`Set` handle — it needs the FREE, not
/// the entry copy the old gate required.
#[test]
fn asan_let_bound_result_map_field_leaks_on_match() {
    assert_clean_asan_run(
        r#"
enum E { Missing(String) }
struct S { a: Map[String, i64] }
fn mk() -> Result[S, E] {
    let mut m: Map[String, i64] = Map.new();
    m.insert("k", 1);
    return Ok(S { a: m });
}
fn main() {
    let r = mk();
    match r {
        Ok(s) => println(f"{s.a.len()}"),
        Err(_e) => println("err"),
    }
    println("end");
}
"#,
        &["1", "end"],
        "let_bound_result_map_field",
    );
}

/// B-2026-08-15-1 — the leak question the row filed as unmeasured.
///
/// Its detail closed with "Whether the handle is also leaked at scope exit
/// was NOT measured", and the doubt was reasonable: the `let` fallback's
/// existing `Map`/`Set` arm documents itself as DISPATCH-ONLY —
/// `register_var_from_type_expr` queues no `FreeMapHandle`, on the
/// reasoning that a PLACE source is a caller-retains alias whose owner
/// frees it. A CALL RESULT is not that: `mkm()` hands back a fresh handle
/// with no other owner, so if the same arm registered dispatch and nothing
/// else, the handle would leak once per binding.
///
/// It does not — the ownership tracking is armed elsewhere on the let path
/// and is head-agnostic, so it was already correct while the dispatch
/// tables were not. Measured rather than argued, over both spellings and
/// both containers, with `String` keys so a leaked handle drags its whole
/// key storage with it.
#[test]
fn asan_unannotated_sorted_collection_binding_frees_its_handle() {
    assert_clean_asan_run(
        r#"
fn mkm() -> SortedMap[String, i64] {
    let mut m: SortedMap[String, i64] = SortedMap.new();
    let _ = m.insert("zzzzzzzzzzzzzzzzzzzz", 1);
    let _ = m.insert("aaaaaaaaaaaaaaaaaaaa", 2);
    return m;
}
fn mks() -> SortedSet[String] {
    let mut s: SortedSet[String] = SortedSet.new();
    let _ = s.insert("zzzzzzzzzzzzzzzzzzzz");
    let _ = s.insert("aaaaaaaaaaaaaaaaaaaa");
    return s;
}

fn main() {
    let m1: SortedMap[String, i64] = mkm();
    println(f"{m1.len()}");
    let m2 = mkm();
    println(f"{m2.len()}");
    let mut acc = "";
    for k in m2.keys() { acc = acc + k + ","; }
    println(f"{acc.len()}");
    let s1: SortedSet[String] = mks();
    println(f"{s1.len()}");
    let s2 = mks();
    println(f"{s2.len()}");
    println("end");
}
"#,
        &["2", "2", "42", "2", "2", "end"],
        "unannotated_sorted_collection_binding",
    );
}

/// B-2026-08-14-15 leg A — a nested container read into a `let` binding
/// and then probed with a key-taking method.
///
/// `let cur = vms[0]` over `Vec[Map[..]]` deep-clones the element (the
/// binding owns a COPY, which is what both backends observe), but the
/// `let` site read the bare `v[i]` RHS as a caller-retains ALIAS and armed
/// no handle free — so the whole cloned control block leaked. The clone is
/// elided for a read-only binding, which is why `cur.len()` and an unused
/// `cur` were clean and only a key-taking probe (`get` / `contains_key` /
/// `Set.contains`) reached the leak.
#[test]
fn asan_nested_map_bound_out_of_vec_then_probed_is_freed() {
    assert_clean_asan_run(
        r#"
fn main() {
    let k = env.args().len() as i64;
    let mut vms: Vec[Map[String, i64]] = Vec.new();
    let mut inner: Map[String, i64] = Map.new();
    let _ = inner.insert("n", k);
    vms.push(inner);
    let cur = vms[0].clone();
    let mut tot = 0i64;
    match cur.get("n") { Some(x) => { tot = tot + x; }, None => {} }
    if cur.contains_key("n") { tot = tot + 10; }
    println(tot);
}
"#,
        // `k` is a stable 1 (the binary runs with no args); it exists to
        // keep the map value out of reach of constant folding.
        &["11"],
        "nested_map_bound_out_of_vec_then_probed_is_freed",
    );
}

/// B-2026-08-24-19 — a `Map` built fresh on every iteration and broken out
/// on a later one. The breaking path must hand its handle to the receiver;
/// every NON-breaking iteration must still free its own.
///
/// This is the fixture that rules out the tempting fix. Retracting the
/// queued `FreeMapHandle` at compile time — what the tail-return
/// suppressor does, and what looks like the obvious reuse — is
/// flow-insensitive: it would disarm the free on iterations 1 and 2 as
/// well, leaking both maps while the ASAN output stayed silent about the
/// double free it fixed. Only LeakSanitizer catches that, and only on
/// Linux.
#[test]
fn asan_loop_break_map_handle_frees_unbroken_iterations() {
    assert_clean_asan_run_min_allocs(
        "fn pick() -> Map[String, i64] {\n\
             \x20   let mut i = 0;\n\
             \x20   loop {\n\
             \x20       i = i + 1;\n\
             \x20       let mut m: Map[String, i64] = Map.new();\n\
             \x20       m.insert(f\"k{i}\", i * 7);\n\
             \x20       if i == 3 { break m }\n\
             \x20   }\n\
             }\n\
             fn main() { let m = pick(); println(m.get(f\"k3\").unwrap()); }\n",
        &["21"],
        "loop-break-map-handle",
        8,
    );
}

/// The `Map` rvalue — a call RETURN, so the handle is manufactured a frame
/// down and arrives already owned. The per-iteration `scratch` map is the
/// leak half of the same two-sided check: the break path drains it while
/// carrying an unrelated handle out, so a suppressor that reached too far
/// would leak three maps here.
#[test]
fn asan_loop_break_map_rvalue_frees_unbroken_iterations() {
    assert_clean_asan_run_min_allocs(
        "fn make_map(n: i64) -> Map[String, i64] {\n\
             \x20   let mut m: Map[String, i64] = Map.new();\n\
             \x20   m.insert(f\"k{n}\", n * 7);\n\
             \x20   m\n\
             }\n\
             fn pick() -> Map[String, i64] {\n\
             \x20   let mut i = 0;\n\
             \x20   loop {\n\
             \x20       i = i + 1;\n\
             \x20       let mut scratch: Map[String, i64] = Map.new();\n\
             \x20       scratch.insert(f\"s{i}\", i);\n\
             \x20       if i == 3 { break make_map(i) }\n\
             \x20   }\n\
             }\n\
             fn main() { let m = pick(); println(m.get(f\"k3\").unwrap()); }\n",
        &["21"],
        "loop-break-map-rvalue",
        8,
    );
}

/// B-2026-08-25-14 — an owned aggregate param's own scope-exit drop was
/// registered by NAME, so it never walked the elements its entry copy owned.
///
/// `make_aggregate_param_callee_owned_inst` has two branches. The
/// copy-unsupported one registers the drop at the param's declared
/// INSTANTIATION; the copy-supported one — which a `Heap[T] { xs: Vec[T] }`
/// takes — discarded that instantiation and called the name-keyed
/// `track_struct_var`, i.e. `track_struct_var_inst(.., None)`. The resulting
/// `__karac_drop_struct_Heap` resolves the `xs: Vec[T]` field from the erased
/// `T`, classifies it outer-only, and frees only the copied outer buffer.
/// Since B-2026-08-25-10 that entry copy is element-deep inside a monomorph,
/// so every element buffer it allocated was left with no owner at all.
///
/// `count` takes `self` and transfers NOTHING out — that is the trigger, not
/// an incidental detail. The sibling shapes hid this for two prior rows:
/// `let mut h = self` hands ownership to `h`, whose drop comes from the
/// binding's recorded instantiation, and `self`'s drop is then cap/len-zeroed
/// by the move-suppression, so the erased drop is only ever REACHED when
/// nothing moves out of `self`.
///
/// (b) is a CASE, not a control, and the distinction matters: in
/// B-2026-08-25-11 a `String` element was genuinely exempt, because that
/// erasure lived in the instantiation record and `String` is a single name a
/// name-to-name map carries losslessly. Here the drop is unmangled outright,
/// so any heap-owning element leaks. Its Strings are built on the heap —
/// literals live in static rodata with cap 0 and leak nothing, the
/// false-negative that mis-narrowed B-2026-08-25-10.
///
/// Verified RED pre-fix: 63 bytes in 6 allocations — (a)'s three Vec buffers
/// (16 + 8 + 24) plus (b)'s three String bodies (5 each).
#[test]
fn asan_owned_aggregate_param_drop_frees_its_entry_copied_elements() {
    assert_clean_asan_run(
        r#"
struct Heap[T] { xs: Vec[T] }
impl[T] Heap[T] {
    // Takes `self` by value and transfers NOTHING out. The entry deep-copy's
    // element buffers are the ones that leaked.
    fn count(self) -> i64 { self.xs.len() }
}
struct PlainHeap { xs: Vec[Vec[i64]] }
impl PlainHeap {
    fn count(self) -> i64 { self.xs.len() }
}
fn main() {
    // (a) nested-Vec element.
    let a = Heap { xs: [[1, 2], [3], [4, 5, 6]] };
    println(f"a={a.count()}");
    // (b) HEAP-allocated String elements — a CASE here, unlike in -11.
    let mut ss: Vec[String] = Vec.new();
    let mut i = 0;
    while i < 3 { ss.push(f"item{i}"); i = i + 1; }
    let b = Heap { xs: ss };
    println(f"b={b.count()}");
    // (c) control: scalar element, no inner buffer to own.
    let c = Heap { xs: [7, 8, 9] };
    println(f"c={c.count()}");
    // (d) control: the NON-generic twin at the same element type.
    let d = PlainHeap { xs: [[1], [2]] };
    println(f"d={d.count()}");
}
"#,
        &["a=3", "b=3", "c=3", "d=2"],
        "owned-aggregate-param-drop-elements",
    );
}

/// CONTROL for the row's "binding the key first is clean" claim — this is
/// the documented workaround, and it must stay clean so a fix to the
/// inline path cannot be mistaken for having broken the bound path.
#[test]
fn asan_map_get_with_bound_struct_key_is_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct Item { id: i64, name: String }
fn main() {
    let mut m: Map[Item, i64] = Map.new();
    m.insert(Item { id: 1i64, name: f"a-padding-padding-padding" }, 7i64);
    let mut j = 0i64;
    let mut hits = 0i64;
    while j < 5i64 {
        let probe = Item { id: j, name: f"b-{j}-padding-padding-padding" };
        match m.get(probe) {
            Some(v) => { hits = hits + v; }
            None => {}
        }
        j = j + 1i64;
    }
    println(f"{hits}{m.len()}");
}
"#,
        &["01"],
        "map-get-bound-struct-key",
    );
}

/// PROBE (B-2026-08-26-32 blast radius): the key temporary produced by a
/// FUNCTION CALL rather than an inline literal. Same borrowed-then-discarded
/// lifetime, different producing expression.
#[test]
fn asan_map_get_with_call_produced_struct_key_is_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct Item { id: i64, name: String }
fn mk(i: i64) -> Item { return Item { id: i, name: f"m-{i}-padding-padding-padding" }; }
fn main() {
    let mut m: Map[Item, i64] = Map.new();
    m.insert(Item { id: 1i64, name: f"a-padding-padding-padding" }, 7i64);
    let mut j = 0i64;
    let mut hits = 0i64;
    while j < 5i64 {
        match m.get(mk(j)) {
            Some(v) => { hits = hits + v; }
            None => {}
        }
        j = j + 1i64;
    }
    println(f"{hits}{m.len()}");
}
"#,
        &["01"],
        "map-get-call-produced-struct-key",
    );
}

/// CONTROL for the enum leg's shape gate, and the reason it exists.
/// `enum_name_of_expr` resolves a bare `Identifier` too, so a fix that
/// consulted it without first checking the expression shape would free a
/// LET-BOUND enum whose drop belongs to its binding — a double free. This
/// is that program; it must stay clean.
#[test]
fn asan_map_get_with_bound_enum_key_is_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
enum Tag { Named { s: String }, Anon }
fn main() {
    let mut m: Map[Tag, i64] = Map.new();
    m.insert(Tag.Named { s: f"a-padding-padding-padding" }, 7i64);
    let mut j = 0i64;
    let mut hits = 0i64;
    while j < 5i64 {
        let probe = Tag.Named { s: f"b-{j}-padding-padding-padding" };
        match m.get(probe) {
            Some(v) => { hits = hits + v; }
            None => {}
        }
        j = j + 1i64;
    }
    println(f"{hits}{m.len()}");
}
"#,
        &["01"],
        "map-get-bound-enum-key",
    );
}

/// B-2026-08-27-3 — `Map.remove` LEAKED A STRUCT KEY'S HEAP FIELDS, one
/// allocation per removal.
///
/// `remove` tombstones the bucket, and the map's own teardown only walks
/// OCCUPIED slots, so whatever the vacated slot still owned is orphaned.
/// The runtime releases the stored key through the `drop_key` FLAG, which
/// codegen sets from `llvm_ty_is_vec_struct(key_ty)` — true for a key that
/// IS a heap `{ptr,len,cap}` (`Map[String, V]`), false for a STRUCT key
/// that merely OWNS one. The flag was doing what it says; what was missing
/// was a path for the other key shape.
#[test]
fn asan_map_remove_releases_a_struct_keys_heap_fields() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct K { id: i64, name: String }
fn main() {
    let mut m: Map[K, i64] = Map.new();
    let mut i = 0i64;
    while i < 40i64 {
        m.insert(K { id: i, name: f"key-{i}-padding-padding-padding" }, i);
        m.remove(K { id: i, name: f"key-{i}-padding-padding-padding" });
        i = i + 1;
    }
    println(f"{m.len()}");
}
"#,
        &["0"],
        "map-remove-struct-key-heap-fields",
    );
}

/// PROBE (B-2026-08-27-3 blast radius): `Set.remove`. A Set's element IS
/// the key half, so the same flag governs it.
#[test]
fn asan_set_remove_releases_a_struct_elements_heap_fields() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct K { id: i64, name: String }
fn main() {
    let mut s: Set[K] = Set.new();
    let mut i = 0i64;
    while i < 40i64 {
        s.insert(K { id: i, name: f"key-{i}-padding-padding-padding" });
        s.remove(K { id: i, name: f"key-{i}-padding-padding-padding" });
        i = i + 1;
    }
    println(f"{s.len()}");
}
"#,
        &["0"],
        "set-remove-struct-elem-heap-fields",
    );
}

/// PROBE (B-2026-08-27-3 blast radius): `SortedMap.remove`.
#[test]
fn asan_sorted_map_remove_releases_a_struct_keys_heap_fields() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq, Ord, PartialOrd)]
struct K { id: i64, name: String }
fn main() {
    let mut m: SortedMap[K, i64] = SortedMap.new();
    let mut i = 0i64;
    while i < 40i64 {
        m.insert(K { id: i, name: f"key-{i}-padding-padding-padding" }, i);
        m.remove(K { id: i, name: f"key-{i}-padding-padding-padding" });
        i = i + 1;
    }
    println(f"{m.len()}");
}
"#,
        &["0"],
        "sorted-map-remove-struct-key-heap-fields",
    );
}

/// PROBE (B-2026-08-27-3 blast radius): `m.clear()`. It vacates every
/// bucket at once and is governed by the same `drop_key` flag.
#[test]
fn asan_map_clear_releases_struct_keys_heap_fields() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct K { id: i64, name: String }
fn main() {
    let mut m: Map[K, i64] = Map.new();
    let mut i = 0i64;
    while i < 40i64 {
        m.insert(K { id: i, name: f"key-{i}-padding-padding-padding" }, i);
        i = i + 1;
    }
    m.clear();
    println(f"{m.len()}");
}
"#,
        &["0"],
        "map-clear-struct-key-heap-fields",
    );
}

/// PROBE (B-2026-08-27-3 blast radius): `Set.clear` over a STRUCT element.
#[test]
fn asan_set_clear_releases_struct_elements_heap_fields() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct K { id: i64, name: String }
fn main() {
    let mut s: Set[K] = Set.new();
    let mut i = 0i64;
    while i < 40i64 {
        s.insert(K { id: i, name: f"key-{i}-padding-padding-padding" });
        i = i + 1;
    }
    s.clear();
    println(f"{s.len()}");
}
"#,
        &["0"],
        "set-clear-struct-elem-heap-fields",
    );
}

/// PROBE (B-2026-08-27-3 blast radius): `SortedSet.remove`.
#[test]
fn asan_sorted_set_remove_releases_a_struct_elements_heap_fields() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq, Ord, PartialOrd)]
struct K { id: i64, name: String }
fn main() {
    let mut s: SortedSet[K] = SortedSet.new();
    let mut i = 0i64;
    while i < 40i64 {
        s.insert(K { id: i, name: f"key-{i}-padding-padding-padding" });
        s.remove(K { id: i, name: f"key-{i}-padding-padding-padding" });
        i = i + 1;
    }
    println(f"{s.len()}");
}
"#,
        &["0"],
        "sorted-set-remove-struct-elem-heap-fields",
    );
}

/// B-2026-08-27-3 adjacency, held as a CONTROL: a `shared` KEY must stay
/// refcount-balanced across `remove`.
///
/// The per-key drop fn declines for a shared half by construction —
/// teardown gives the count back through the rc_dec WALK instead, and
/// `remove` runs no walk — so this is the one key shape the fix
/// deliberately does NOT route through `map_key_mem_drop_fn_for_var`. It
/// is balanced today and the test exists to keep it that way: a later
/// widening that let the drop fn answer for a shared key would over-release
/// a count the map never took.
///
/// The key is removed through the SAME binding it was inserted with, not a
/// structurally-equal twin. That is not stylistic: on the compiled backends
/// a `shared` key is matched by POINTER IDENTITY, so a twin finds nothing
/// and this would measure an empty `remove`. (The interpreter matches it
/// structurally — a real run-vs-build divergence, filed separately as
/// B-2026-08-27-4; it is out of scope here and must not be smuggled into a
/// leak fixture.)
#[test]
fn asan_map_remove_with_a_shared_key_is_balanced() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
shared struct K { id: i64, name: String }
fn main() {
    let mut m: Map[K, i64] = Map.new();
    let mut i = 0i64;
    while i < 20i64 {
        let k = K { id: i, name: f"key-{i}-padding-padding-padding" };
        m.insert(k, i);
        m.remove(k);
        i = i + 1;
    }
    println(f"{m.len()}");
}
"#,
        &["0"],
        "map-remove-shared-key-balanced",
    );
}

#[test]
fn asan_array_held_as_a_map_value_frees_its_elements() {
    // B-2026-09-12-13. `map_val_drop_fn_for_type_expr` dispatched on
    // `val_te.kind` with arms for `Weak`, `Path` and `Tuple`, so an
    // `Array[T, N]` half fell out of the `_ => return None` tail and the
    // map's value side got NOTHING: no drop fn, and no `val_is_vec`
    // overlay either, because an array OF vec structs is not itself one.
    // 384 B in 16 blocks at `-O0` for `Map[i64, Array[String, 2]]`.
    //
    // The CONTAINER was the axis, not the element: the same array as a
    // plain local, a struct field, an enum payload, an `Option` payload
    // and a `Result` payload is clean, and a `String`, a user struct, a
    // tuple and a `Vec[String]` VALUE in the same map are clean too. So
    // every neighbouring cell answered "fine" and only this one did not.
    //
    // The fix is a PAIR, and cells 2 and 10 are why. The drop fn alone
    // would double-free an array moved in from a source LOCAL, whose own
    // one-level drop is today the only owner -- and that spelling measures
    // CLEAN before the fix, because the local frees the buffers and the
    // map frees nothing. They balance on a DANGLING bucket: outlive the
    // local and the reads come back garbage (measured, 2 invalid reads),
    // against a temp-literal twin that reads correctly and merely leaks.
    // So this row's leak and a latent use-after-free are one defect, and
    // `suppress_array_binding_move_arg` on the insert arguments is the
    // half that makes the map the single owner rather than a second one.
    //
    // 1 -- the reported shape.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, [f\"aaaaaaaa0\", f\"bbbbbbbb0\"]);\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "map-value-array-literal",
    );
    // 2 -- the LOCAL-source spelling, with the map OUTLIVING the local and
    //      reading back. This is the use-after-free face: before the fix
    //      it printed garbage with two invalid reads while reporting no
    //      leak at all. The readback is the assertion -- a clean ASAN run
    //      alone would not have caught it.
    assert_clean_asan_run(
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
             }\n",
            &["s:aaaaaaaa0"],
            "map-value-array-local-source-readback",
        );
    // 3 -- the NESTED element, through the recursive walk
    //      B-2026-09-10-8/-26 built. 704 B in 32 blocks before the fix.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[Array[String, 2], 2]] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, [[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]]);\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
            &["s:ok"],
            "map-value-nested-array",
        );
    // 4 -- a `Vec` ELEMENT inside the array value.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut v0: Vec[String] = Vec.new();\n\
             \x20\x20\x20\x20v0.push(f\"aaaaaaaa0\");\n\
             \x20\x20\x20\x20let mut v1: Vec[String] = Vec.new();\n\
             \x20\x20\x20\x20v1.push(f\"bbbbbbbb0\");\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[Vec[String], 2]] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, [v0, v1]);\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "map-value-array-of-vec",
    );
    // 5 -- `SortedMap`, the ordered sibling. Same KaracMap storage, so it
    //      leaked the same 384 B and takes the same fix.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: SortedMap[i64, Array[String, 2]] = SortedMap.new();\n\
             \x20\x20\x20\x20m.insert(1, [f\"aaaaaaaa0\", f\"bbbbbbbb0\"]);\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "sortedmap-value-array",
    );
    // 6 -- the map held as a STRUCT FIELD, which reaches the value side
    //      through a different walk than a bare local does.
    assert_clean_asan_run(
        "struct H { m: Map[i64, Array[String, 2]] }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, [f\"aaaaaaaa0\", f\"bbbbbbbb0\"]);\n\
             \x20\x20\x20\x20let h: H = H { m: m };\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "map-value-array-in-struct-field",
    );
    // 7 -- the map RETURNED from the fn that built it.
    assert_clean_asan_run(
        "fn mk() -> Map[i64, Array[String, 2]] {\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, [f\"aaaaaaaa0\", f\"bbbbbbbb0\"]);\n\
             \x20\x20\x20\x20return m;\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let m = mk();\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "map-value-array-returned",
    );
    // 8 -- the map passed BY VALUE to a callee, which owns it on arrival.
    assert_clean_asan_run(
        "fn take(m: Map[i64, Array[String, 2]]) -> i64 { return m.len() as i64; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, [f\"aaaaaaaa0\", f\"bbbbbbbb0\"]);\n\
             \x20\x20\x20\x20println(f\"s:{take(m)}\");\n\
             }\n",
        &["s:1"],
        "map-value-array-by-value-param",
    );
    // 9 -- `clear()`, which resolves its per-value drop through the SAME
    //       selector (`karac_map_clear_with_val_drop_fn`) and so was
    //       leaking for the same reason the scope-exit free was.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, [f\"aaaaaaaa0\", f\"bbbbbbbb0\"]);\n\
             \x20\x20\x20\x20m.clear();\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "map-value-array-clear",
    );
    // 10 -- CONTROL: the `Vec[String]` VALUE from a source local. This type
    //       already resolved a `val_drop_fn` before the fix and was clean,
    //       which means `insert` ALREADY retracts a Vec source's cleanup.
    //       It must stay exactly one owner -- if the new array retraction
    //       had been written as a widening of that battery rather than a
    //       new member, this is the cell that would double-free.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut inner: Vec[String] = Vec.new();\n\
             \x20\x20\x20\x20inner.push(f\"aaaaaaaa0\");\n\
             \x20\x20\x20\x20let mut m: Map[i64, Vec[String]] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, inner);\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "map-value-vec-local-control",
    );
    // 11 -- CONTROL: a user STRUCT value from a source local, the other
    //       already-clean neighbour on the same battery.
    assert_clean_asan_run(
        "struct W { a: String, b: String }\n\
             fn main() {\n\
             \x20\x20\x20\x20let w: W = W { a: f\"aaaaaaaa0\", b: f\"bbbbbbbb0\" };\n\
             \x20\x20\x20\x20let mut m: Map[i64, W] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, w);\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "map-value-struct-local-control",
    );
    // 12 -- CONTROL: a TUPLE value, which reaches the selector's own
    //       `TypeKind::Tuple` arm -- the arm the new `Array` route is
    //       modelled on and must not disturb.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, (String, String)] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, (f\"aaaaaaaa0\", f\"bbbbbbbb0\"));\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "map-value-tuple-control",
    );
    // 13 -- CONTROL: a plain `String` value, the one-level `val_is_vec`
    //       overlay. The new arm sits before the name-keyed dispatch, so
    //       this is the cell that proves it did not shadow it.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, String] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, f\"aaaaaaaa0\");\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "map-value-string-control",
    );
    // 14 -- CONTROL: a SCALAR element. `emit_drop_fn_for_array` declines
    //       it, so this map must register nothing at all; a widened
    //       admission would emit a walk over `i64`s and free them.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[i64, 2]] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, [11, 22]);\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "map-value-scalar-array-control",
    );
    // 15 -- CONTROL: the same array as a PLAIN LOCAL, the position the
    //       sweep found already clean. Its own one-level drop owns the
    //       buffers and nothing in this fix may add a second owner.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20println(f\"s:{a[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "plain-local-array-control",
    );
}

#[test]
fn asan_array_used_as_a_map_key_frees_its_elements() {
    // B-2026-09-13-1. The KEY half of B-2026-09-12-13's selector. It leaked
    // the same 384 B in 16 blocks, and the same one-line arm answers it --
    // but a key carries two obligations a value does not, and wiring the
    // arm without them measured STRICTLY WORSE than the leak it replaced.
    //
    // A key has a NO-ADOPT branch. On a duplicate key the bucket keeps the
    // key it already holds, so the caller's key is orphaned rather than
    // adopted; a value is simply replaced and always adopted. And there are
    // TWO insert entry points, `insert` and `try_insert`, the second with
    // its own no-adopt branch plus an OOM branch where nothing is stored at
    // all. Wired with only `insert`'s retraction, `try_insert` aborted with
    // a glibc tcache double free and a duplicate-key `insert` still leaked
    // 13 B in 2 blocks.
    //
    // Cells 3 and 4 are the ones that would have caught that, and neither
    // shape existed in this file before: every cell written for
    // B-2026-09-12-13 used DISTINCT keys and the `insert` entry point. The
    // test that did catch it,
    // `asan_slice_and_array_equality_are_ownership_neutral`, does it only
    // incidentally.
    //
    // 1 -- the reported shape, distinct keys.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[Array[String, 2], i64] = Map.new();\n\
             \x20\x20\x20\x20m.insert([f\"aaaaaaaa0\", f\"bbbbbbbb0\"], 7);\n\
             \x20\x20\x20\x20m.insert([f\"cccccccc0\", f\"dddddddd0\"], 8);\n\
             \x20\x20\x20\x20println(f\"s:{m.len()}\");\n\
             }\n",
        &["s:2"],
        "map-key-array-distinct",
    );
    // 2 -- the source-LOCAL spelling, map OUTLIVING the local, then looked
    //      up. Before the fix this read the stored key out of freed memory
    //      during the hash compare -- one invalid read in `karac_map_get`,
    //      on a block the source local's drop had already released. The
    //      lookup still "worked", which is what made it invisible.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[Array[String, 2], i64] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 2 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20let k: Array[String, 2] = [f\"kaaaaaaa{i}\", f\"kbbbbbbb{i}\"];\n\
             \x20\x20\x20\x20\x20\x20\x20\x20m.insert(k, i);\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20let probe: Array[String, 2] = [f\"kaaaaaaa1\", f\"kbbbbbbb1\"];\n\
             \x20\x20\x20\x20match m.get(probe) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(v) => { println(f\"s:{v}\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"s:missing\"); }\n\
             \x20\x20\x20\x20}\n\
             }\n",
            &["s:1"],
            "map-key-array-local-source-lookup",
        );
    // 3 -- DUPLICATE keys, the no-adopt branch. The bucket keeps its own
    //      key and the incoming one is orphaned; without
    //      `free_half_on_no_adopt` this leaked 13 B in 2 blocks per
    //      duplicate while every distinct-key cell stayed clean.
    assert_clean_asan_run(
        "fn mk() -> Array[String, 2] { return Array[f\"aaaaaaaa0\", f\"bbbbbbbb0\"]; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[Array[String, 2], i64] = Map.new();\n\
             \x20\x20\x20\x20let k1 = mk();\n\
             \x20\x20\x20\x20let k2 = mk();\n\
             \x20\x20\x20\x20m.insert(k1, 1);\n\
             \x20\x20\x20\x20m.insert(k2, 2);\n\
             \x20\x20\x20\x20println(f\"s:{m.len()}\");\n\
             }\n",
        &["s:1"],
        "map-key-array-duplicate-no-adopt",
    );
    // 4 -- `try_insert`, the SECOND entry point. It has its own no-adopt
    //      branch plus an OOM branch, and none of the array retractions
    //      reached it before this row -- including the VALUE-side one that
    //      B-2026-09-12-13 added to `insert` alone, which left
    //      `try_insert(k, <array local>)` aborting on a double free.
    assert_clean_asan_run(
        "fn mk() -> Array[String, 2] { return Array[f\"aaaaaaaa0\", f\"bbbbbbbb0\"]; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[Array[String, 2], i64] = Map.new();\n\
             \x20\x20\x20\x20let k1 = mk();\n\
             \x20\x20\x20\x20let k2 = mk();\n\
             \x20\x20\x20\x20match m.try_insert(k1, 1) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Ok(o) => { println(\"s:ok\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Err(e) => { println(\"s:err\"); }\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20match m.try_insert(k2, 2) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Ok(o) => { println(\"t:ok\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Err(e) => { println(\"t:err\"); }\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"u:{m.len()}\");\n\
             }\n",
        &["s:ok", "t:ok", "u:1"],
        "map-key-array-try-insert-duplicate",
    );
    // 5 -- `try_insert` on the VALUE half from a source local. This is the
    //      B-2026-09-12-13 correction: that fix gave the map's value side a
    //      drop fn and retracted the source at `insert` only, so this
    //      spelling went from a silent use-after-free (3 invalid reads,
    //      garbage read-back) to a hard abort. Two entry points, one
    //      battery.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
             \x20\x20\x20\x20let e: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20match m.try_insert(1, e) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Ok(o) => { println(\"s:ok\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Err(x) => { println(\"s:err\"); }\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20match m.get(1) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { println(f\"t:{a[0]}\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"t:missing\"); }\n\
             \x20\x20\x20\x20}\n\
             }\n",
        &["s:ok", "t:aaaaaaaa0"],
        "map-value-array-try-insert-local",
    );
    // 6 -- the NESTED array key, through the recursive walk.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[Array[Array[String, 2], 2], i64] = Map.new();\n\
             \x20\x20\x20\x20m.insert([[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]], 7);\n\
             \x20\x20\x20\x20println(f\"s:{m.len()}\");\n\
             }\n",
            &["s:1"],
            "map-key-nested-array",
        );
    // 7 -- CONTROL: a `String` KEY, the one-level overlay whose own
    //      no-adopt reclaim (`free_str_vec_buffer_if_heap`) the array one
    //      sits beside. Duplicate keys, so it exercises the same branch.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[String, i64] = Map.new();\n\
             \x20\x20\x20\x20let k1 = f\"aaaaaaaa0\";\n\
             \x20\x20\x20\x20let k2 = f\"aaaaaaaa0\";\n\
             \x20\x20\x20\x20m.insert(k1, 1);\n\
             \x20\x20\x20\x20m.insert(k2, 2);\n\
             \x20\x20\x20\x20println(f\"s:{m.len()}\");\n\
             }\n",
        &["s:1"],
        "map-key-string-duplicate-control",
    );
    // 8 -- duplicate keys from LITERAL temps rather than source locals.
    //      The no-adopt reclaim must fire whether or not a retraction
    //      happened first: with a literal there is no source binding to
    //      retract, so the orphan is reached by a different route to
    //      cell 3's.
    //
    //      The user-STRUCT key this cell deliberately did NOT cover --
    //      the same no-adopt orphan, 252 B in 14 blocks, left open here as
    //      pre-existing -- is B-2026-09-13-6, now fixed by generalizing
    //      this row's array-shaped reclaim into the shape DISPATCHER
    //      `free_half_on_no_adopt`. Its battery is
    //      `asan_a_no_adopt_map_key_orphan_is_reclaimed_for_every_shape`
    //      below.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[Array[String, 2], i64] = Map.new();\n\
             \x20\x20\x20\x20m.insert([f\"aaaaaaaa0\", f\"bbbbbbbb0\"], 1);\n\
             \x20\x20\x20\x20m.insert([f\"aaaaaaaa0\", f\"bbbbbbbb0\"], 2);\n\
             \x20\x20\x20\x20println(f\"s:{m.len()}\");\n\
             }\n",
        &["s:1"],
        "map-key-array-duplicate-literals",
    );
    // 9 -- CONTROL: a SCALAR-element array key. `emit_drop_fn_for_array`
    //      declines it, so both the drop fn and the reclaim must emit
    //      nothing; a widened admission would free `i64`s.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[Array[i64, 2], i64] = Map.new();\n\
             \x20\x20\x20\x20m.insert([11, 22], 7);\n\
             \x20\x20\x20\x20m.insert([11, 22], 8);\n\
             \x20\x20\x20\x20println(f\"s:{m.len()}\");\n\
             }\n",
        &["s:1"],
        "map-key-scalar-array-control",
    );
}

#[test]
fn asan_a_no_adopt_map_key_orphan_is_reclaimed_for_every_shape() {
    // B-2026-09-13-6. B-2026-09-13-1 reclaimed the key a map DECLINES TO
    // ADOPT on a duplicate for one shape -- `Array[T, N]` -- by calling a
    // bespoke array helper alongside `free_str_vec_buffer_if_heap`, whose
    // guard no-ops on anything that is not itself the 24-byte
    // `{ptr,len,cap}` overlay. Every other non-overlay shape was left
    // orphaned, and the reported one was a user struct.
    //
    // MEASURED at `KARAC_OPT_LEVEL=0` under valgrind, eight inserts of ONE
    // duplicate key (so seven orphans), before -> after:
    //
    //     Map[K, i64], K = struct { a: String, b: String }  252 B/14 -> 0
    //     Map[(String, String), i64]                        252 B/14 -> 0
    //     Map[Vec[String], i64]                             252 B/14 -> 0
    //     Map[K, i64], K = struct { a: String, b: i64 }     154 B/ 7 -> 0
    //     Map[String, i64]                                  clean -> clean
    //     Map[i64, i64]                                     clean -> clean
    //     Map[Array[String, 2], i64]                        clean -> clean
    //
    // The TUPLE and `Vec[String]` rows were not in B-2026-09-13-6's title;
    // they were found by sweeping the shape axis before writing the fix,
    // and they are why this is a DISPATCHER rather than a third sibling.
    // A `Vec[String]` key's outer buffer is ALREADY reclaimed by
    // `free_str_vec_buffer_if_heap` at the same site, so a helper running
    // IN ADDITION to it would have freed that buffer twice -- a
    // double-free traded for a leak. `free_half_on_no_adopt` therefore
    // chooses: the half's own full drop fn when one exists, else the
    // shallow cap-guarded free.
    //
    // The retraction half needs nothing new, and the leak measurements are
    // themselves the evidence: had the source local kept its own cleanup,
    // those interiors would have been freed by it rather than lost.
    //
    // 1 -- the reported shape: a two-String user struct key, duplicates
    //      from a source LOCAL.
    assert_clean_asan_run(
        "#[derive(Hash, Eq)]\n\
             struct K6 { a: String, b: String }\n\
             fn mk() -> K6 { return K6 { a: f\"aaaaaaaaaaaa0\", b: f\"bbbbbbbbbbbb0\" }; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[K6, i64] = Map.new();\n\
             \x20\x20\x20\x20let k1 = mk();\n\
             \x20\x20\x20\x20let k2 = mk();\n\
             \x20\x20\x20\x20m.insert(k1, 1);\n\
             \x20\x20\x20\x20m.insert(k2, 2);\n\
             \x20\x20\x20\x20println(f\"s:{m.len()}\");\n\
             }\n",
        &["s:1"],
        "map-key-struct-duplicate-no-adopt",
    );
    // 2 -- a TUPLE key. Not in the row's title; same orphan, same size.
    assert_clean_asan_run(
        "fn mk() -> (String, String) { return (f\"aaaaaaaaaaaa0\", f\"bbbbbbbbbbbb0\"); }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[(String, String), i64] = Map.new();\n\
             \x20\x20\x20\x20let k1 = mk();\n\
             \x20\x20\x20\x20let k2 = mk();\n\
             \x20\x20\x20\x20m.insert(k1, 1);\n\
             \x20\x20\x20\x20m.insert(k2, 2);\n\
             \x20\x20\x20\x20println(f\"s:{m.len()}\");\n\
             }\n",
        &["s:1"],
        "map-key-tuple-duplicate-no-adopt",
    );
    // 3 -- the `Vec[String]` key: the cell that forces the dispatcher
    //      shape. Its OUTER buffer was already freed here before this
    //      change and only the 14 inner Strings leaked, so a reclaim
    //      running alongside the shallow free would double-free it.
    assert_clean_asan_run(
        "fn mk() -> Vec[String] {\n\
             \x20\x20\x20\x20let mut v: Vec[String] = Vec.new();\n\
             \x20\x20\x20\x20v.push(f\"aaaaaaaaaaaa0\");\n\
             \x20\x20\x20\x20v.push(f\"bbbbbbbbbbbb0\");\n\
             \x20\x20\x20\x20return v;\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[Vec[String], i64] = Map.new();\n\
             \x20\x20\x20\x20let k1 = mk();\n\
             \x20\x20\x20\x20let k2 = mk();\n\
             \x20\x20\x20\x20m.insert(k1, 1);\n\
             \x20\x20\x20\x20m.insert(k2, 2);\n\
             \x20\x20\x20\x20println(f\"s:{m.len()}\");\n\
             }\n",
        &["s:1"],
        "map-key-vecstring-duplicate-no-adopt",
    );
    // 4 -- a PARTLY-heap struct key. Its 154 B / 7 says the walk reaches
    //      the String field and steps over the i64 rather than treating
    //      the struct as one opaque block.
    assert_clean_asan_run(
        "#[derive(Hash, Eq)]\n\
             struct K6b { a: String, b: i64 }\n\
             fn mk() -> K6b { return K6b { a: f\"aaaaaaaaaaaaaaaa0\", b: 3 }; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[K6b, i64] = Map.new();\n\
             \x20\x20\x20\x20let k1 = mk();\n\
             \x20\x20\x20\x20let k2 = mk();\n\
             \x20\x20\x20\x20m.insert(k1, 1);\n\
             \x20\x20\x20\x20m.insert(k2, 2);\n\
             \x20\x20\x20\x20println(f\"s:{m.len()}\");\n\
             }\n",
        &["s:1"],
        "map-key-struct-partial-heap-duplicate",
    );
    // 5 -- `try_insert`, the SECOND entry point with its own no-adopt
    //      branch. B-2026-09-13-1 records that wiring only `insert` turned
    //      its array leak into a glibc tcache abort, so the struct shape
    //      gets the same two-entry-point battery rather than one.
    assert_clean_asan_run(
        "#[derive(Hash, Eq)]\n\
             struct K6c { a: String, b: String }\n\
             fn mk() -> K6c { return K6c { a: f\"aaaaaaaaaaaa0\", b: f\"bbbbbbbbbbbb0\" }; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[K6c, i64] = Map.new();\n\
             \x20\x20\x20\x20let k1 = mk();\n\
             \x20\x20\x20\x20let k2 = mk();\n\
             \x20\x20\x20\x20match m.try_insert(k1, 1) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Ok(o) => { println(\"s:ok\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Err(e) => { println(\"s:err\"); }\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20match m.try_insert(k2, 2) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Ok(o) => { println(\"t:ok\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Err(e) => { println(\"t:err\"); }\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"u:{m.len()}\");\n\
             }\n",
        &["s:ok", "t:ok", "u:1"],
        "map-key-struct-try-insert-duplicate",
    );
    // 6 -- the map is still USABLE after a reclaim: the bucket kept its own
    //      key, so a later lookup must hash and compare live memory. This
    //      is the cell that fails if the reclaim ever frees the ADOPTED
    //      half instead of the orphan -- a leak fix's worst failure mode,
    //      and one no leak count would show.
    assert_clean_asan_run(
            "#[derive(Hash, Eq)]\n\
             struct K6d { a: String, b: String }\n\
             fn mk(n: i64) -> K6d { return K6d { a: f\"aaaaaaaaaaaa{n}\", b: f\"bbbbbbbbbbbb{n}\" }; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[K6d, i64] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 6 { m.insert(mk(i % 3), i); i = i + 1; }\n\
             \x20\x20\x20\x20println(f\"len:{m.len()}\");\n\
             \x20\x20\x20\x20match m.get(mk(1)) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(v) => { println(f\"g:{v}\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"g:missing\"); }\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20match m.remove(mk(1)) {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20Some(v) => { println(f\"r:{v}\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"r:nothing\"); }\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"len2:{m.len()}\");\n\
             }\n",
            &["len:3", "g:4", "r:4", "len2:2"],
            "map-key-struct-duplicate-then-lookup",
        );
    // 7 -- the shapes that must NOT gain a walk. A scalar key has no heap,
    //      and a plain `String` key is exactly the overlay the shallow
    //      sibling already owns -- routing it through the drop fn too would
    //      free one buffer twice. Both were clean before and stay clean.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, i64] = Map.new();\n\
             \x20\x20\x20\x20m.insert(7, 1);\n\
             \x20\x20\x20\x20m.insert(7, 2);\n\
             \x20\x20\x20\x20let mut s: Map[String, i64] = Map.new();\n\
             \x20\x20\x20\x20let a = f\"aaaaaaaaaaaa0\";\n\
             \x20\x20\x20\x20let b = f\"aaaaaaaaaaaa0\";\n\
             \x20\x20\x20\x20s.insert(a, 1);\n\
             \x20\x20\x20\x20s.insert(b, 2);\n\
             \x20\x20\x20\x20println(f\"s:{m.len()}:{s.len()}\");\n\
             }\n",
        &["s:1:1"],
        "map-key-scalar-and-string-duplicate-controls",
    );
    // 8 -- THE ONE-OWNER CELL. A for-loop-owned struct element key has a
    //      SECOND reclaim on this very branch (B-2026-08-01-29's
    //      `free_staged_for_loop_agg_copy_on_no_adopt`, which drops the
    //      staged deep copy). That one coexisted with the shallow sibling
    //      only because the sibling no-ops on a struct; it cannot coexist
    //      with a dispatcher that drops a struct properly. The first cut of
    //      this fix ran both and aborted
    //      `asan_dup_key_for_loop_elem_insert_no_leak` -- caught by the
    //      suite, not by any hand probe here, which is why the shape is
    //      pinned in this battery too and not only in that test.
    assert_clean_asan_run(
        "#[derive(Hash, Eq, Ord)]\n\
             struct K6e { a: i64, s: String }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut ks: Vec[K6e] = Vec.new();\n\
             \x20\x20\x20\x20ks.push(K6e { a: 1, s: f\"ssssssssssss1\" });\n\
             \x20\x20\x20\x20ks.push(K6e { a: 1, s: f\"ssssssssssss1\" });\n\
             \x20\x20\x20\x20let mut m: Map[K6e, i64] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20for k in ks { m.insert(k, i); i = i + 1; }\n\
             \x20\x20\x20\x20println(f\"s:{m.len()}\");\n\
             }\n",
        &["s:1"],
        "map-key-struct-for-loop-elem-duplicate-one-owner",
    );
}

#[test]
fn asan_a_nameless_map_key_temporary_is_reclaimed_at_every_lookup() {
    // B-2026-09-13-20. A lookup BORROWS its key and discards it, so a
    // fresh-owned key temporary is the caller's to reclaim. `get`,
    // `contains_key`, `remove` and `Set.contains` each called
    // `free_fresh_owned_str_arg` (the `{ptr,len,cap}` overlay) and
    // `free_fresh_owned_struct_key_arg` (the named-aggregate sibling) --
    // and a key whose type has NO NAME is neither, so both declined and
    // the temporary was lost once per lookup.
    //
    // Filed as a TUPLE key. Sweeping the key-shape axis BEFORE writing the
    // fix -- the lesson B-2026-09-13-6 recorded when its struct-keyed title
    // turned out to cover a tuple and a `Vec[String]` too -- found three
    // more shapes the title does not mention: `Array[String, 2]`, a nested
    // tuple, and the `Set.contains` spelling of all of them.
    //
    // MEASURED at `KARAC_OPT_LEVEL=0` under valgrind, two lookups each:
    //
    //     Map[(String, String), i64]                64 B / 4  -> clean
    //     Map[(String, i64), i64]                   36 B / 2  -> clean
    //     Map[Array[String, 2], i64]                64 B / 4  -> clean
    //     Map[((String, String), i64), i64]         64 B / 4  -> clean
    //     Set[(String, String)]                     64 B / 4  -> clean
    //     m.get((a, b)) from locals                 32 B / 2  -> clean
    //     Map[K, i64] named struct                  clean     -> clean
    //     Map[Vec[String], i64]                     clean     -> clean
    //     let probe = mk(1); m.get(probe)           clean     -> clean
    //     insert-only                               clean     -> clean
    //
    // Reclaim resolved through `map_key_drop_fn_for_type_expr`, the same
    // one-policy resolver the map's storage side uses for both halves, so
    // this adds no third notion of what a key owns.
    //
    // THE METHOD-CALL TEMP left open here (`m.get(g.make(0))`, 32 B / 2)
    // is CLOSED by B-2026-09-13-30 -- see
    // `asan_a_method_call_map_key_temporary_is_reclaimed_at_every_lookup`
    // below. The reason recorded here for leaving it, that "its return type
    // is absent from `fn_return_type_exprs`" and so needs a method-return
    // map, was WRONG: impl methods are minted as `Function`s named
    // `Type.method` and recorded in that very table, so only the lookup KEY
    // was missing and no new map was built. Kept rather than deleted
    // because the wrong inference is the instructive part -- the leg's
    // `let ExprKind::Identifier(fn_name) = &callee.kind else { return }`
    // makes an unlooked-up key and an unrecorded type look identical from
    // the outside.
    // 1 -- THE ROW'S OWN SHAPE. A lookup BORROWS its key, so a fresh
    //      owned temporary is the caller's to reclaim; a tuple has no type
    //      NAME, so both existing legs declined it. 64 B in 4 blocks
    //      before, over two lookups.
    assert_clean_asan_run(
            "fn mk20(n: i64) -> (String, String) { return (f\"aaaaaaaaaaaa{n}\", f\"bbbbbbbbbbbb{n}\"); }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[(String, String), i64] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 3 { m.insert(mk20(i), i); i = i + 1; }\n\
             \x20\x20\x20\x20let mut j: i64 = 0;\n\
             \x20\x20\x20\x20while j < 2 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20match m.get(mk20(j)) { Some(v) => { println(f\"g:{v}\"); } None => { println(\"miss\"); } }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20j = j + 1;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"len:{m.len()}\");\n\
             }",
            &["g:0", "g:1", "len:3"],
            "map-get-tuple-key-fresh-temp",
        );

    // 2 -- `contains_key`, same 64 B / 4. All three lookup entry points
    //      share the helper, so all three are pinned.
    assert_clean_asan_run(
            "fn mk20(n: i64) -> (String, String) { return (f\"aaaaaaaaaaaa{n}\", f\"bbbbbbbbbbbb{n}\"); }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[(String, String), i64] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 3 { m.insert(mk20(i), i); i = i + 1; }\n\
             \x20\x20\x20\x20let mut j: i64 = 0;\n\
             \x20\x20\x20\x20while j < 2 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20if m.contains_key(mk20(j)) { println(\"yes\"); } else { println(\"no\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20j = j + 1;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"len:{m.len()}\");\n\
             }",
            &["yes", "yes", "len:3"],
            "map-contains-key-tuple-key-fresh-temp",
        );

    // 3 -- `remove`, same 64 B / 4.
    assert_clean_asan_run(
            "fn mk20(n: i64) -> (String, String) { return (f\"aaaaaaaaaaaa{n}\", f\"bbbbbbbbbbbb{n}\"); }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[(String, String), i64] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 3 { m.insert(mk20(i), i); i = i + 1; }\n\
             \x20\x20\x20\x20let mut j: i64 = 0;\n\
             \x20\x20\x20\x20while j < 2 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20match m.remove(mk20(j)) { Some(v) => { println(f\"r:{v}\"); } None => { println(\"miss\"); } }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20j = j + 1;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"len:{m.len()}\");\n\
             }",
            &["r:0", "r:1", "len:1"],
            "map-remove-tuple-key-fresh-temp",
        );

    // 4 -- a PARTLY heap tuple, 36 B / 2. It pins that the walk frees
    //      the String element and steps over the scalar.
    assert_clean_asan_run(
            "fn mkx20(n: i64) -> (String, i64) { return (f\"aaaaaaaaaaaa{n}\", n); }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[(String, i64), i64] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 3 { m.insert(mkx20(i), i); i = i + 1; }\n\
             \x20\x20\x20\x20let mut j: i64 = 0;\n\
             \x20\x20\x20\x20while j < 2 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20match m.get(mkx20(j)) { Some(v) => { println(f\"g:{v}\"); } None => { println(\"miss\"); } }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20j = j + 1;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"len:{m.len()}\");\n\
             }",
            &["g:0", "g:1", "len:3"],
            "map-get-mixed-tuple-key-fresh-temp",
        );

    // 5 -- AN `Array[T, N]` KEY, and the cell that survived this row's
    //      first fix. Not in the row's title; found by sweeping the
    //      key-shape axis. An array VALUE is an LLVM array, not a struct,
    //      so the caller's `StructType` shape guard turned it away before
    //      any leg ran -- the tuple cells above went clean while this one
    //      kept leaking its 64 B / 4 unchanged. Two gates, not one.
    assert_clean_asan_run(
            "fn mka20(n: i64) -> Array[String, 2] { return Array[f\"aaaaaaaaaaaa{n}\", f\"bbbbbbbbbbbb{n}\"]; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[Array[String, 2], i64] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 3 { m.insert(mka20(i), i); i = i + 1; }\n\
             \x20\x20\x20\x20let mut j: i64 = 0;\n\
             \x20\x20\x20\x20while j < 2 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20match m.get(mka20(j)) { Some(v) => { println(f\"g:{v}\"); } None => { println(\"miss\"); } }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20j = j + 1;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"len:{m.len()}\");\n\
             }",
            &["g:0", "g:1", "len:3"],
            "map-get-array-key-fresh-temp",
        );

    // 6 -- a NESTED tuple key, 64 B / 4. Also absent from the title.
    assert_clean_asan_run(
            "fn mkn20(n: i64) -> ((String, String), i64) { return ((f\"aaaaaaaaaaaa{n}\", f\"bbbbbbbbbbbb{n}\"), n); }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[((String, String), i64), i64] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 3 { m.insert(mkn20(i), i); i = i + 1; }\n\
             \x20\x20\x20\x20let mut j: i64 = 0;\n\
             \x20\x20\x20\x20while j < 2 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20match m.get(mkn20(j)) { Some(v) => { println(f\"g:{v}\"); } None => { println(\"miss\"); } }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20j = j + 1;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"len:{m.len()}\");\n\
             }",
            &["g:0", "g:1", "len:3"],
            "map-get-nested-tuple-key-fresh-temp",
        );

    // 7 -- `Set.contains`, 64 B / 4. The helper is shared by eight call
    //      sites across maps, sets and `Vec.contains`, so fixing it there
    //      rather than at a site covers the Set spelling for free.
    assert_clean_asan_run(
            "fn mk20(n: i64) -> (String, String) { return (f\"aaaaaaaaaaaa{n}\", f\"bbbbbbbbbbbb{n}\"); }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut s: Set[(String, String)] = Set.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 3 { s.insert(mk20(i)); i = i + 1; }\n\
             \x20\x20\x20\x20let mut j: i64 = 0;\n\
             \x20\x20\x20\x20while j < 2 { if s.contains(mk20(j)) { println(\"yes\"); } else { println(\"no\"); } j = j + 1; }\n\
             \x20\x20\x20\x20println(f\"len:{s.len()}\");\n\
             }",
            &["yes", "yes", "len:3"],
            "set-contains-tuple-key-fresh-temp",
        );

    // 8 -- THE SOUNDNESS CELL, and the only one whose second assertion
    //      matters as much as the leak. A tuple LITERAL built from LOCALS
    //      reads like a reclaim that would double-free them -- and the
    //      locals are demonstrably still alive after the lookup, since
    //      `after:` prints their lengths. It leaked 32 B / 2 anyway, which
    //      is the proof that the literal DEEP-COPIES them and abandons the
    //      copy: an alias would have had nothing extra to lose. So the
    //      copy is the only owner and freeing it is the only reclaim.
    //
    //      Both halves are asserted deliberately: a fix that mistook the
    //      copy for an alias would ABORT here rather than leak, and only
    //      the `after:` half distinguishes the two.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[(String, String), i64] = Map.new();\n\
             \x20\x20\x20\x20m.insert((f\"aaaaaaaaaaaa0\", f\"bbbbbbbbbbbb0\"), 0);\n\
             \x20\x20\x20\x20let a: String = f\"aaaaaaaaaaaa0\";\n\
             \x20\x20\x20\x20let b: String = f\"bbbbbbbbbbbb0\";\n\
             \x20\x20\x20\x20match m.get((a, b)) { Some(v) => { println(f\"g:{v}\"); } None => { println(\"miss\"); } }\n\
             \x20\x20\x20\x20println(f\"after:{a.len()}:{b.len()}\");\n\
             }",
            &["g:0", "after:13:13"],
            "map-get-tuple-literal-key-from-locals-still-valid",
        );

    // 9 -- THE NAMED CONTROL, clean before and after. It is the cell
    //      that named the missing owner in the first place: the same shape
    //      to the program, differing only in whether the key type has a
    //      name for `emit_struct_drop_synthesis` to key on. A fix that
    //      routed a named struct through the new leg as well would
    //      double-free here.
    assert_clean_asan_run(
            "#[derive(Hash, Eq)]\n\
             struct K20 { a: String, b: String }\n\
             fn mks20(n: i64) -> K20 { return K20 { a: f\"aaaaaaaaaaaa{n}\", b: f\"bbbbbbbbbbbb{n}\" }; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[K20, i64] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 3 { m.insert(mks20(i), i); i = i + 1; }\n\
             \x20\x20\x20\x20let mut j: i64 = 0;\n\
             \x20\x20\x20\x20while j < 2 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20match m.get(mks20(j)) { Some(v) => { println(f\"g:{v}\"); } None => { println(\"miss\"); } }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20j = j + 1;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"len:{m.len()}\");\n\
             }",
            &["g:0", "g:1", "len:3"],
            "map-get-named-struct-key-control",
        );

    // 10 -- THE OVERLAY CONTROL. A `Vec` key is already fully reclaimed
    //      by `free_fresh_owned_str_arg` at these same sites, and the
    //      TypeExpr resolver would hand back a FULL drop for
    //      `Vec[String]`. The two together are a double free of one
    //      buffer, which is why the new leg declines a `String`/`Vec`-
    //      headed key explicitly as well as behind the caller's shape
    //      guard. This cell aborts if that decline is ever dropped.
    assert_clean_asan_run(
            "fn mkv20(n: i64) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(f\"aaaaaaaaaaaa{n}\"); return v; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[Vec[String], i64] = Map.new();\n\
             \x20\x20\x20\x20m.insert(mkv20(0), 0);\n\
             \x20\x20\x20\x20match m.get(mkv20(0)) { Some(v) => { println(f\"g:{v}\"); } None => { println(\"miss\"); } }\n\
             \x20\x20\x20\x20println(f\"len:{m.len()}\");\n\
             }",
            &["g:0", "len:1"],
            "map-get-vec-key-overlay-control",
        );

    // 11 -- THE OTHER-OWNER CONTROL. A let-bound key is owned by its
    //      BINDING and freed at that binding's scope exit, so the reclaim
    //      must not fire. `Identifier` keys are excluded in every leg for
    //      this reason, and this is the cell that proves the exclusion
    //      still holds.
    assert_clean_asan_run(
            "fn mk20(n: i64) -> (String, String) { return (f\"aaaaaaaaaaaa{n}\", f\"bbbbbbbbbbbb{n}\"); }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[(String, String), i64] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 3 { m.insert(mk20(i), i); i = i + 1; }\n\
             \x20\x20\x20\x20let probe: (String, String) = mk20(1);\n\
             \x20\x20\x20\x20match m.get(probe) { Some(v) => { println(f\"g:{v}\"); } None => { println(\"miss\"); } }\n\
             \x20\x20\x20\x20println(f\"len:{m.len()}\");\n\
             }",
            &["g:1", "len:3"],
            "map-get-tuple-key-place-expr-control",
        );

    // 12 -- THE INSERT CONTROL. An insert MOVES its key into the
    //      collection and the map's own drop reclaims it, so the insert
    //      path must stay untouched -- the row said to check this
    //      separately, and B-2026-09-13-6 had just made the tuple key's
    //      no-adopt branch sound. A lookup-site reclaim wired into the
    //      insert path double-frees every stored key.
    assert_clean_asan_run(
            "fn mk20(n: i64) -> (String, String) { return (f\"aaaaaaaaaaaa{n}\", f\"bbbbbbbbbbbb{n}\"); }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[(String, String), i64] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 3 { m.insert(mk20(i), i); i = i + 1; }\n\
             \x20\x20\x20\x20println(f\"len:{m.len()}\");\n\
             }",
            &["len:3"],
            "map-insert-tuple-key-only-control",
        );

    // 13 -- A HEAPLESS array key. The resolver's `emit_drop_fn_for_array`
    //      declines a heapless element, so this emits nothing at all and
    //      keeps exactly the no-op it had. Pinned so that a future widening
    //      of the resolver does not silently start walking it.
    assert_clean_asan_run(
            "fn mki20(n: i64) -> Array[i64, 2] { return Array[n, n + 1]; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[Array[i64, 2], i64] = Map.new();\n\
             \x20\x20\x20\x20m.insert(mki20(0), 7);\n\
             \x20\x20\x20\x20match m.get(mki20(0)) { Some(v) => { println(f\"g:{v}\"); } None => { println(\"miss\"); } }\n\
             \x20\x20\x20\x20println(f\"len:{m.len()}\");\n\
             }",
            &["g:7", "len:1"],
            "map-get-heapless-array-key-control",
        );
}

#[test]
fn asan_a_method_call_map_key_temporary_is_reclaimed_at_every_lookup() {
    // B-2026-09-13-30. The remainder B-2026-09-13-20 measured and left
    // open: a lookup key produced by a METHOD call was lost once per
    // lookup. Sweeping the CALL-SPELLING axis before writing the fix --
    // the same discipline that found the array and nested-tuple shapes on
    // the parent row -- turned a one-leg gap into a three-leg one, because
    // all three legs of `free_fresh_owned_struct_key_arg` ask the same
    // question ("what does this callee hand back?") and all three keyed it
    // on a BARE `Identifier` callee.
    //
    // MEASURED at `KARAC_OPT_LEVEL=0` under valgrind, one lookup each:
    //
    //     m.get(g.make(0))      (String, String)   32 B / 2  -> clean
    //     m.get(g.make(0))      Array[String, 2]   32 B / 2  -> clean
    //     m.get(g.make(0))      (String, i64)      18 B / 1  -> clean
    //     m.get(g.mks(0))       struct Kk          32 B / 2  -> clean
    //     m.get(g.mke(0))       enum Tg            18 B / 1  -> clean
    //     m.get(Mk.build(0))    assoc fn           32 B / 2  -> clean
    //     m.get(g.make(0))      generic receiver   32 B / 2  -> clean
    //     s.contains(g.make(0))                    32 B / 2  -> clean
    //     v.contains(g.make(0))                    32 B / 2  -> clean
    //     m.contains_key(..) + m.remove(..)        64 B / 4  -> clean
    //     5x m.get(g.make(0)) in a loop           160 B / 10 -> clean
    //     m.get(h.pair())       -> self.pr copy    34 B / 2  -> clean
    //     m.get(mkk(0))         free fn            clean     -> clean
    //     let k = g.make(0); m.get(k)              clean     -> clean
    //     m.get(g.make(0))      Array[i64, 2]      clean     -> clean
    //     m.get(g.make(0))      Vec[String]        clean     -> clean
    //
    // THE PARENT ROW'S STATED ROOT CAUSE WAS WRONG, and correcting it IS
    // the fix. It read "a method's return type is absent from
    // `fn_return_type_exprs`" and proposed a new method-return map keyed on
    // (receiver type, method name). No such map was needed:
    // `make_impl_method_function` mints every impl method as a `Function`
    // named `Type.method`, `declare_function` records THAT name in the same
    // `fn_return_type_exprs` / `fn_return_type_names` pair free functions
    // use, and an associated fn's two-segment path is the identical symbol.
    // The answer was always in the table under a key nobody looked up --
    // which is why the remedy is one shared key resolver
    // (`fresh_owned_key_callee_key`) and not a new source of truth.
    //
    // THE ASSOC-FN SPELLING WAS NOT ON THE ROW AT ALL. `m.get(Mk.build(0))`
    // is an `ExprKind::Call` whose callee is a `Path`, so the leg's
    // `let ExprKind::Identifier(fn_name) = &callee.kind else { return }`
    // dropped it on the floor exactly as it dropped the method call. Found
    // by enumerating what can spell a call rather than by reading the row.
    //
    // A DOUBLE FREE WAS INTRODUCED AND CAUGHT IN THE SAME CHANGE, and cell
    // 6 is its guard. The dispatcher runs the enum leg and the nameless leg
    // back to back, relying on the nameless leg's opening
    // `enum_name_of_expr` check to keep them exclusive -- but that resolver
    // reads CONSTRUCTOR spellings, so for `g.mke(0)` it answers `None` in
    // both places and both legs claimed the payload. The exclusion is now
    // stated on the RESOLVED return type, the one thing the two legs agree
    // on.

    // 1 -- THE ROW'S OWN SHAPE: a method-returned tuple key.
    assert_clean_asan_run(
        r#"
struct Mk { p: String }
impl Mk { fn make(ref self, n: i64) -> (String, String) {
    return (f"aaaaaaaaaaaaaaaa-{n}", f"bbbbbbbbbbbb-{n}");
} }
fn main() {
    let mut m: Map[(String, String), i64] = Map.new();
    let g: Mk = Mk { p: f"x" };
    m.insert(g.make(0i64), 0i64);
    match m.get(g.make(0i64)) {
        Some(v) => { println(f"g:{v}"); } None => { println("missing"); }
    }
    println(f"len:{m.len()}");
}
"#,
        &["g:0", "len:1"],
        "map-get-method-call-tuple-key",
    );

    // 2 -- the `Array[String, N]` key, the shape the parent row had to add
    //      a second measurement round for on the free-function side.
    assert_clean_asan_run(
        r#"
struct Mk { p: String }
impl Mk { fn make(ref self, n: i64) -> Array[String, 2] {
    return [f"aaaaaaaaaaaaaaaa-{n}", f"bbbbbbbbbbbb-{n}"];
} }
fn main() {
    let mut m: Map[Array[String, 2], i64] = Map.new();
    let g: Mk = Mk { p: f"x" };
    m.insert(g.make(0i64), 0i64);
    match m.get(g.make(0i64)) {
        Some(v) => { println(f"g:{v}"); } None => { println("missing"); }
    }
    println(f"len:{m.len()}");
}
"#,
        &["g:0", "len:1"],
        "map-get-method-call-array-key",
    );

    // 3 -- THE ASSOCIATED-FN SPELLING. A `Path` callee, not a method, and
    //      not mentioned on the row; same 32 B / 2.
    assert_clean_asan_run(
        r#"
struct Mk { p: String }
impl Mk { fn build(n: i64) -> (String, String) {
    return (f"aaaaaaaaaaaaaaaa-{n}", f"bbbbbbbbbbbb-{n}");
} }
fn main() {
    let mut m: Map[(String, String), i64] = Map.new();
    m.insert(Mk.build(0i64), 0i64);
    match m.get(Mk.build(0i64)) {
        Some(v) => { println(f"g:{v}"); } None => { println("missing"); }
    }
    println(f"len:{m.len()}");
}
"#,
        &["g:0", "len:1"],
        "map-get-assoc-fn-tuple-key",
    );

    // 4 -- a GENERIC receiver. The declared return is concrete here, so
    //      this pins the `Type.method` key resolving for a monomorphized
    //      impl; a generic RETURN goes through
    //      `callee_param_te_for_call` and fails closed if unresolved.
    assert_clean_asan_run(
        r#"
struct Gk[T] { p: T }
impl[T] Gk[T] { fn make(ref self, n: i64) -> (String, String) {
    return (f"aaaaaaaaaaaaaaaa-{n}", f"bbbbbbbbbbbb-{n}");
} }
fn main() {
    let mut m: Map[(String, String), i64] = Map.new();
    let g: Gk[i64] = Gk { p: 7i64 };
    m.insert(g.make(0i64), 0i64);
    match m.get(g.make(0i64)) {
        Some(v) => { println(f"g:{v}"); } None => { println("missing"); }
    }
    println(f"len:{m.len()}");
}
"#,
        &["g:0", "len:1"],
        "map-get-generic-receiver-method-key",
    );

    // 5 -- THE STRUCT LEG. Not on the row, which named the nameless leg
    //      only: `fresh_owned_struct_key_type_name` reads
    //      `fn_return_type_names` under the same bare-`Identifier` key, so
    //      a method returning a named struct leaked identically.
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct Kk { a: String, b: String }
struct Mk { p: String }
impl Mk { fn mks(ref self, n: i64) -> Kk {
    return Kk { a: f"aaaaaaaaaaaaaaaa-{n}", b: f"bbbbbbbbbbbb-{n}" };
} }
fn main() {
    let mut m: Map[Kk, i64] = Map.new();
    let g: Mk = Mk { p: f"x" };
    m.insert(g.mks(0i64), 0i64);
    match m.get(g.mks(0i64)) {
        Some(v) => { println(f"g:{v}"); } None => { println("missing"); }
    }
    println(f"len:{m.len()}");
}
"#,
        &["g:0", "len:1"],
        "map-get-method-call-struct-key",
    );

    // 6 -- THE ENUM LEG, and the double-free guard described above. This
    //      is the cell that fails loudly if the two legs ever both claim
    //      the payload again.
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
enum Tg { Named { s: String }, Num { n: i64 } }
struct Mk { p: String }
impl Mk { fn mke(ref self, n: i64) -> Tg {
    return Tg.Named { s: f"aaaaaaaaaaaaaaaa-{n}" };
} }
fn main() {
    let mut m: Map[Tg, i64] = Map.new();
    let g: Mk = Mk { p: f"x" };
    m.insert(g.mke(0i64), 0i64);
    match m.get(g.mke(0i64)) {
        Some(v) => { println(f"g:{v}"); } None => { println("missing"); }
    }
    println(f"len:{m.len()}");
}
"#,
        &["g:0", "len:1"],
        "map-get-method-call-enum-key",
    );

    // 7 -- `Set.contains` and `Vec.contains`. All the lookup entry points
    //      share the helper, so one resolution covers them; pinned because
    //      that sharing is the fix's whole economy.
    assert_clean_asan_run(
        r#"
struct Mk { p: String }
impl Mk { fn make(ref self, n: i64) -> (String, String) {
    return (f"aaaaaaaaaaaaaaaa-{n}", f"bbbbbbbbbbbb-{n}");
} }
fn main() {
    let mut s: Set[(String, String)] = Set.new();
    let mut v: Vec[(String, String)] = Vec.new();
    let g: Mk = Mk { p: f"x" };
    s.insert(g.make(0i64));
    v.push(g.make(0i64));
    if s.contains(g.make(0i64)) { println("s:hit"); } else { println("s:miss"); }
    if v.contains(g.make(0i64)) { println("v:hit"); } else { println("v:miss"); }
    println(f"len:{s.len()}");
}
"#,
        &["s:hit", "v:hit", "len:1"],
        "set-vec-contains-method-call-key",
    );

    // 8 -- `contains_key` + `remove`, 64 B / 4 over the two lookups.
    assert_clean_asan_run(
        r#"
struct Mk { p: String }
impl Mk { fn make(ref self, n: i64) -> (String, String) {
    return (f"aaaaaaaaaaaaaaaa-{n}", f"bbbbbbbbbbbb-{n}");
} }
fn main() {
    let mut m: Map[(String, String), i64] = Map.new();
    let g: Mk = Mk { p: f"x" };
    m.insert(g.make(0i64), 0i64);
    if m.contains_key(g.make(0i64)) { println("has"); } else { println("no"); }
    m.remove(g.make(0i64));
    println(f"len:{m.len()}");
}
"#,
        &["has", "len:0"],
        "map-contains-key-remove-method-call-key",
    );

    // 9 -- THE LOOP, which is what makes a bounded-per-lookup leak matter:
    //      160 B in 10 blocks over five lookups, linear in lookup count.
    //      An immediate drop is what gets this right; a scope-exit
    //      registration would free the LAST key and leak the other four.
    assert_clean_asan_run(
        r#"
struct Mk { p: String }
impl Mk { fn make(ref self, n: i64) -> (String, String) {
    return (f"aaaaaaaaaaaaaaaa-{n}", f"bbbbbbbbbbbb-{n}");
} }
fn main() {
    let mut m: Map[(String, String), i64] = Map.new();
    let g: Mk = Mk { p: f"x" };
    m.insert(g.make(0i64), 0i64);
    let mut hits: i64 = 0i64;
    for i in 0i64..5i64 {
        match m.get(g.make(0i64)) {
            Some(v) => { hits = hits + 1i64; } None => { }
        }
    }
    println(f"hits:{hits}");
    println(f"len:{m.len()}");
}
"#,
        &["hits:5", "len:1"],
        "map-get-method-call-key-in-a-loop",
    );

    // 10 -- A METHOD RETURNING A PROJECTION OF `self`, which looked like
    //       the fix's soundness boundary and is not. `fn pair(ref self) ->
    //       (String, String) { return self.pr; }` plainly does not take
    //       `self`'s buffers -- `h.pr.0` is still readable afterwards, and
    //       this cell asserts that it is -- yet it LEAKED 34 B / 2 before
    //       the fix. That leak is the proof the return DEEP-COPIES, so
    //       freeing the copy is the only reclaim rather than a second one;
    //       an alias would have had nothing extra to lose. The
    //       free-function twin was already clean under the parent row's
    //       fix, which is the precedent this follows.
    assert_clean_asan_run(
        r#"
struct Hold { pr: (String, String) }
impl Hold { fn pair(ref self) -> (String, String) { return self.pr; } }
fn main() {
    let mut m: Map[(String, String), i64] = Map.new();
    let h: Hold = Hold { pr: (f"aaaaaaaaaaaaaaaa-0", f"bbbbbbbbbbbb-0") };
    m.insert(h.pair(), 0i64);
    match m.get(h.pair()) {
        Some(v) => { println(f"g:{v}"); } None => { println("missing"); }
    }
    println(f"h0:{h.pr.0}");
    println(f"len:{m.len()}");
}
"#,
        &["g:0", "h0:aaaaaaaaaaaaaaaa-0", "len:1"],
        "map-get-method-self-projection-key",
    );

    // 11 -- NEGATIVE CONTROLS, one program per boundary the fix must not
    //       cross. A BOUND key is owned by its binding (the row's
    //       documented workaround); a `Vec[String]` key belongs to the
    //       `free_fresh_owned_str_arg` overlay and freeing it here would
    //       double-free; an `Array[i64, N]` key has no heap at all and the
    //       resolver must keep declining it.
    assert_clean_asan_run(
        r#"
struct Mk { p: String }
impl Mk { fn make(ref self, n: i64) -> (String, String) {
    return (f"aaaaaaaaaaaaaaaa-{n}", f"bbbbbbbbbbbb-{n}");
} }
fn main() {
    let mut m: Map[(String, String), i64] = Map.new();
    let g: Mk = Mk { p: f"x" };
    m.insert(g.make(0i64), 0i64);
    let probe: (String, String) = g.make(0i64);
    match m.get(probe) {
        Some(v) => { println(f"g:{v}"); } None => { println("missing"); }
    }
    println(f"len:{m.len()}");
}
"#,
        &["g:0", "len:1"],
        "map-get-bound-method-call-key-control",
    );
    assert_clean_asan_run(
        r#"
struct Mk { p: String }
impl Mk { fn make(ref self, n: i64) -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push(f"aaaaaaaaaaaaaaaa-{n}");
    v.push(f"bbbbbbbbbbbb-{n}");
    return v;
} }
fn main() {
    let mut m: Map[Vec[String], i64] = Map.new();
    let g: Mk = Mk { p: f"x" };
    m.insert(g.make(0i64), 0i64);
    match m.get(g.make(0i64)) {
        Some(v) => { println(f"g:{v}"); } None => { println("missing"); }
    }
    println(f"len:{m.len()}");
}
"#,
        &["g:0", "len:1"],
        "map-get-vec-key-method-call-control",
    );
    assert_clean_asan_run(
        r#"
struct Mk { p: String }
impl Mk { fn make(ref self, n: i64) -> Array[i64, 2] {
    return [n, n + 1i64];
} }
fn main() {
    let mut m: Map[Array[i64, 2], i64] = Map.new();
    let g: Mk = Mk { p: f"x" };
    m.insert(g.make(0i64), 0i64);
    match m.get(g.make(0i64)) {
        Some(v) => { println(f"g:{v}"); } None => { println("missing"); }
    }
    println(f"len:{m.len()}");
}
"#,
        &["g:0", "len:1"],
        "map-get-heapless-array-key-method-call-control",
    );

    // 12 -- THE FREE-FUNCTION TWIN as the oracle. Already clean under the
    //       parent row's fix, and re-asserted here because this change
    //       rewrites the key the covered spelling resolves through: if
    //       `fresh_owned_key_callee_key` ever stops answering for a bare
    //       `Identifier` callee, this is the cell that says so.
    assert_clean_asan_run(
        r#"
fn mkk(n: i64) -> (String, String) {
    return (f"aaaaaaaaaaaaaaaa-{n}", f"bbbbbbbbbbbb-{n}");
}
fn main() {
    let mut m: Map[(String, String), i64] = Map.new();
    m.insert(mkk(0i64), 0i64);
    match m.get(mkk(0i64)) {
        Some(v) => { println(f"g:{v}"); } None => { println("missing"); }
    }
    println(f"len:{m.len()}");
}
"#,
        &["g:0", "len:1"],
        "map-get-free-fn-tuple-key-oracle",
    );

    // 13 -- TWO MORE NEGATIVE CONTROLS, each guarding a boundary the
    //       shared resolver could have crossed but must not. A method
    //       returning `String` is the `free_fresh_owned_str_arg` overlay's
    //       and the dispatcher's `vec_struct_type()` gate is what turns it
    //       away BEFORE any leg keys a name -- so this cell fails as a
    //       double free, not a leak, if that ordering is ever lost. A
    //       NESTED tuple is the shape the parent row added after its first
    //       pass, re-asserted through the method spelling.
    assert_clean_asan_run(
        r#"
struct Mk { p: String }
impl Mk { fn mkstr(ref self, n: i64) -> String {
    return f"aaaaaaaaaaaaaaaa-{n}";
} }
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let g: Mk = Mk { p: f"x" };
    m.insert(g.mkstr(0i64), 0i64);
    match m.get(g.mkstr(0i64)) {
        Some(v) => { println(f"g:{v}"); } None => { println("missing"); }
    }
    println(f"len:{m.len()}");
}
"#,
        &["g:0", "len:1"],
        "map-get-string-key-method-call-control",
    );
    assert_clean_asan_run(
        r#"
struct Mk { p: String }
impl Mk { fn make(ref self, n: i64) -> ((String, String), i64) {
    return ((f"aaaaaaaaaaaaaaaa-{n}", f"bbbbbbbbbbbb-{n}"), n);
} }
fn main() {
    let mut m: Map[((String, String), i64), i64] = Map.new();
    let g: Mk = Mk { p: f"x" };
    m.insert(g.make(0i64), 0i64);
    match m.get(g.make(0i64)) {
        Some(v) => { println(f"g:{v}"); } None => { println("missing"); }
    }
    println(f"len:{m.len()}");
}
"#,
        &["g:0", "len:1"],
        "map-get-method-call-nested-tuple-key",
    );
}
