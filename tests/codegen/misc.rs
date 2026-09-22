//! everything the area rules do not claim -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen misc::
//!
//! New fixtures about everything the area rules do not claim belong in this file.

use super::*;

/// `StableHash.siphash24` — the compiled backend must agree with the
/// interpreter, byte for byte (B-2026-08-25-22).
///
/// This is the load-bearing test for the whole feature, and the reason it
/// is written as a TWIN comparison rather than a pinned constant. The
/// function's contract is that the same bytes under the same key give the
/// same `u64` "across runs, across machines, across targets" — a promise
/// that is already broken if `karac run` and `karac build` disagree, and a
/// user storing a digest to disk would have no way to notice. Pinning both
/// sides to the same literal would still pass if both drifted together;
/// running the interpreter as an in-process oracle over the same source
/// cannot.
///
/// The cases cover the four byte sources that reach different codegen
/// paths — a `String`'s bytes, a named `Vec[u8]`, a bare array literal
/// (no alloca), and the empty input (`(null, 0)` at the FFI boundary) —
/// plus a non-zero 128-bit key, since a lowering that dropped `k1` would
/// agree with a `k1`-dropping interpreter and disagree with the world.
#[test]
fn e2e_stable_hash_siphash24_agrees_with_the_interpreter() {
    let cases: &[(&str, &str)] = &[
        (
            "string bytes",
            "let s: String = \"kara\";\n\
                 println(StableHash.siphash24(s.bytes(), 0u64, 0u64));",
        ),
        (
            "named Vec[u8]",
            "let v: Vec[u8] = [1u8, 2u8, 3u8];\n\
                 println(StableHash.siphash24(v, 0u64, 0u64));",
        ),
        (
            "bare array literal",
            "println(StableHash.siphash24([1u8, 2u8, 3u8], 0u64, 0u64));",
        ),
        (
            "empty input",
            "let v: Vec[u8] = [];\n\
                 println(StableHash.siphash24(v, 0u64, 0u64));",
        ),
        (
            "non-zero 128-bit key",
            "let s: String = \"kara\";\n\
                 println(StableHash.siphash24(s.bytes(), 506097522914230528u64, \
                 1084818905618843912u64));",
        ),
    ];
    for (label, body) in cases {
        let src = format!("fn main() {{\n{body}\n}}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errors: {interp_errs:?}"
        );
        let expected = interp_out.join("");
        // A digest of 0 would make the comparison vacuous on both sides —
        // it is exactly what a stubbed-out lowering returns.
        assert!(
            expected.trim() != "0" && !expected.trim().is_empty(),
            "{label}: interpreter produced a vacuous digest: {expected:?}"
        );
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, expected,
                "{label}: AOT `StableHash.siphash24` must compute the same \
                     digest as the interpreter — a stable digest that differs \
                     between `karac run` and `karac build` is not stable",
            );
        }
    }
}

/// Ad-hoc IR probe — prints the LLVM IR for an ARBITRARY `.kara` file, or
/// just the functions matching a filter. Ignored by default; it asserts
/// nothing, because the program under inspection changes every iteration.
///
/// The `memory_sanitizer` sibling (`asan_probe_from_env`) answers "does this
/// shape leak"; this one answers "what did codegen actually emit", which is
/// the question every leak reduction ends on — B-2026-08-06-10's next step
/// is literally "does the callee write through the box pointer, or retract
/// an action", and those have opposite fixes. Reading the source from the
/// environment rebuilds this ~200 MB test binary once instead of per edit.
///
///   KARAC_PROBE_SRC=/path/to/probe.kara KARAC_PROBE_FN=hname \
///     cargo test --features llvm --test codegen -- \
///     --ignored --exact codegen_tests::ir_probe_from_env --nocapture
///
/// `KARAC_PROBE_FN` is optional: unset prints the whole module; set, it
/// prints only `define`s whose line contains that substring (plus their
/// bodies), which is usually the one callee under investigation.
///
/// IT RUNS THE FULL PIPELINE, NOT [`ir_for`], and that distinction is the
/// whole point of the helper existing separately. `ir_for` calls
/// `compile_to_ir(program, None, None)` — ownership is `None` — so every
/// ownership-derived decision (parameter modes, RC elision, borrow
/// classification, and therefore most drop/move emission) differs from what
/// `karac build` and the ASAN harness actually compile. Reading a
/// drop-ownership question out of `ir_for`'s output is reading a different
/// program: it cost one wrong mechanism on B-2026-08-06-10, where the
/// dumped IR showed the caller zeroing a tag and nothing freeing a box in
/// BOTH variants, while the built binaries measurably differed. This probe
/// mirrors `memory_sanitizer`'s `run_under_asan_opts` sequence exactly —
/// desugar, gated-stdlib expansion, resolve, typecheck, lower,
/// ownershipcheck — so what it prints is what runs.
#[test]
#[ignore = "inspection aid: needs KARAC_PROBE_SRC"]
fn ir_probe_from_env() {
    let path =
        std::env::var("KARAC_PROBE_SRC").expect("set KARAC_PROBE_SRC to the .kara file to inspect");
    let src = std::fs::read_to_string(&path).expect("KARAC_PROBE_SRC unreadable");
    let mut parsed = karac::parse(&src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    let ir = compile_to_ir(&parsed.program, Some(&ownership), None).expect("codegen failed");
    match std::env::var("KARAC_PROBE_FN") {
        Ok(filter) if !filter.is_empty() => {
            let mut printing = false;
            for line in ir.lines() {
                if line.starts_with("define") {
                    printing = line.contains(&filter);
                }
                if printing {
                    println!("{line}");
                }
                if line == "}" {
                    printing = false;
                }
            }
        }
        _ => println!("{ir}"),
    }
}

#[test]
/// B-2026-09-06-52 — a whole rebind of a by-value param whose struct the
/// PROLOGUE DECLINED TO OWN must not register a second owner for the
/// caller's buffers.
///
/// A struct with a `shared` field (`inner`, direct in `R`, one level down in
/// `S`) or a self-referential one (`Node`) fails
/// `aggregate_param_copy_supported_struct`, so it is never entry-copied, and
/// `make_aggregate_param_callee_owned_transfer` then refuses the
/// own-by-transfer bargain as well — the param FORWARDS the caller's object.
/// `compile_let`'s param-view arm registered its memory-only `StructDrop`
/// anyway, on the strength of "the deep copy the rebind received", and there
/// was no deep copy: measured before the fix as `free(): double free
/// detected in tcache 2` under `karac run` and at `KARAC_OPT_LEVEL=0`, and
/// at the DEFAULT `-O2` as a surviving use-after-free of the `shared`
/// handle's 16-byte refcount block that still printed the right answer.
///
/// Eight red spellings, one per cell: the plain `top`, the branch-nested
/// `br`, the chained `two` (whose second `let` sees a LOCAL, which is why
/// the caller-retains fact has to propagate), the call form `call`, the
/// second-param `pair`, the loop `loop`, the indirect-`shared` `deep`, the
/// no-`impl Drop` `nod` and the self-referential `self`. `ctl` is the
/// control the fix must not disturb — a copy-supported `P` IS entry-copied,
/// so its rebind keeps the owned drop it has always had. `rd` pins that the
/// source's bytes survive: Kara does not reject a read after a move, and
/// `h27/h27` would read empty if the decline had been spelled as a
/// cap-zeroing.
fn test_e2e_declined_copy_param_rebind_keeps_the_callers_ownership() {
    let out = run_program(
        r#"
shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }

shared struct Deep { v: i64 }
struct Mid { d: Deep }
struct S { id: i64, name: String, mid: Mid }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, name: f"h{i}", mid: Mid { d: Deep { v: i } } }; }

struct N { id: i64, name: String, inner: Inner }
fn mkn(i: i64) -> N { return N { id: i, name: f"h{i}", inner: Inner { v: i } }; }

struct Node { id: i64, name: String, kids: Vec[Node] }
impl Drop for Node { fn drop(mut ref self) { println(f"dNd{self.id}") } }
fn mknode(i: i64) -> Node { return Node { id: i, name: f"h{i}", kids: Vec[Node].new() }; }

struct P { id: i64, name: String }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
fn mkp(i: i64) -> P { return P { id: i, name: f"h{i}" }; }

fn top(r: R) -> i64 { let m = r; return m.inner.v; }
fn br(r: R, keep: bool) -> i64 { if keep { let m = r; return m.inner.v; } return 0; }
fn two(r: R) -> i64 { let m = r; let n = m; return n.inner.v; }
fn keeps(x: R) -> R { return x; }
fn call(r: R) -> i64 { let w = keeps(r); return w.inner.v; }
fn pair(a: R, b: R) -> i64 { let m = b; return m.inner.v + a.id; }
fn rdmove(r: R) -> String { let m = r; return f"{m.name}/{r.name}"; }
fn loopreb(r: R, n: i64) -> i64 {
    let mut t = 0;
    for i in 0..n { if i == 0 { let m = r; t = t + m.inner.v; } }
    return t;
}
fn deep(s: S) -> i64 { let m = s; return m.mid.d.v; }
fn nodrop(n: N) -> i64 { let m = n; return m.inner.v; }
fn selfref(nd: Node) -> i64 { let m = nd; return m.id; }
fn ctl(p: P) -> i64 { let m = p; return m.id; }

fn main() {
    println(f"top={top(mk(21))}");
    println(f"brT={br(mk(22), true)}");
    println(f"brF={br(mk(23), false)}");
    println(f"two={two(mk(24))}");
    println(f"call={call(mk(25))}");
    println(f"pair={pair(mk(1), mk(26))}");
    println(f"rd={rdmove(mk(27))}");
    println(f"loop={loopreb(mk(28), 3)}");
    println(f"deep={deep(mks(29))}");
    println(f"nod={nodrop(mkn(30))}");
    println(f"self={selfref(mknode(31))}");
    println(f"ctl={ctl(mkp(32))}");
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out, "dR21\ntop=21\ndR22\nbrT=22\ndR23\nbrF=0\ndR24\ntwo=24\ndR25\ncall=25\ndR26\ndR1\npair=27\ndR27\nrd=h27/h27\ndR28\nloop=28\ndS29\ndeep=29\nnod=30\ndNd31\nself=31\ndP32\nctl=32\nend\n",
                "a param the prologue declined to own is a VIEW: the rebind runs \
                 one body and frees nothing the caller still owns; got {out:?}"
            );
    }
}

#[test]
/// B-2026-09-06-61 — a callee that hands a by-value param back THROUGH A
/// REBIND leaves the caller owning nothing.
///
/// `fn top(r: R) -> R { let m = r; return m; }` over a struct with a
/// `shared` field: the param is not copy-supported, so it FORWARDS the
/// caller's object, and the value the caller's result binding receives is
/// the very buffer its fresh-temp argument holds. The caller's admission
/// gate (`call_arg_flows_into_return`) asked `fn_returns_param`, whose
/// return-site test matches the param's own NAME, so `return m` read as
/// "does not hand it back" and the temp was registered as an owner beside
/// the result binding.
///
/// The tell is that the `Drop` BODY was already right: `escapes_frame`, in
/// the same call-site block, ORs in `callee_hands_arg_off` — which IS
/// `fn_always_returns_param` and DOES follow the rebind — so the registrar
/// ran in its memory-only mode. One body, two frees. Measured on the parent
/// as `free(): double free detected in tcache 2` under `karac run` and at
/// `KARAC_OPT_LEVEL=0`, and at the default `-O2` as a surviving
/// use-after-free that still printed every line correctly (76 valgrind
/// errors at -O2, 79 at -O0 for this fixture; 0 after).
///
/// The cells are the spellings that were red: the plain `top`, the chained
/// `two` (whose second `let` sees a LOCAL), the tail-expression `tail` with
/// no `return` keyword at all, the second-param `pair`, the
/// aggregate-literal `bx` and tuple `tp`, the `Option` ctor `op`, the
/// read-then-return `rd`, the DISCARDED result (`top(mk(49))`, which has no
/// result binding and still double-freed), the loop, and `qq` — a `Map`
/// field, which on the parent did not merely abort but SEGFAULTED silently.
///
/// `ctl` and `ret` are the controls the fix must not disturb. `ctl` is
/// copy-supported, so the callee entry-copies and hands back an INDEPENDENT
/// object whose original the caller must still drop — that case is re-admitted
/// by `arg_is_entry_copied_heap_struct`, and it is the reason this widening
/// is safe at all. `ret` is the same function without the rebind, which was
/// always clean; one binding was the whole difference.
fn test_e2e_param_handed_back_through_a_rebind_leaves_one_owner() {
    let out = run_program(
        r#"
shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
struct Box2 { r: R }
struct P { id: i64, name: String }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
fn mkP(i: i64) -> P { return P { id: i, name: f"p{i}" }; }
struct Q { id: i64, t: Map[String, i64] }
impl Drop for Q { fn drop(mut ref self) { println(f"dQ{self.id}") } }
fn mkQ(i: i64) -> Q { let mut t = Map.new(); t.insert(f"k{i}", i); return Q { id: i, t: t }; }

fn top(r: R) -> R { let m = r; return m; }
fn two(r: R) -> R { let m = r; let n = m; return n; }
fn tail(r: R) -> R { let m = r; m }
fn pair(a: i64, r: R) -> R { let m = r; return m; }
fn bx(r: R) -> Box2 { let m = r; return Box2 { r: m }; }
fn tp(r: R) -> (R, i64) { let m = r; return (m, 9); }
fn op(r: R) -> Option[R] { let m = r; return Option.Some(m); }
fn rd(r: R) -> R { let m = r; println(f"in={m.name}"); return m; }
fn qq(q: Q) -> Q { let m = q; return m; }
fn ctl(p: P) -> P { let m = p; return m; }
fn ret(r: R) -> R { return r; }

fn main() {
  let a = top(mk(41)); println(f"top={a.inner.v}");
  let b = two(mk(42)); println(f"two={b.inner.v}");
  let c = tail(mk(43)); println(f"tail={c.inner.v}");
  let d = pair(7, mk(44)); println(f"pair={d.inner.v}");
  let e = bx(mk(45)); println(f"bx={e.r.inner.v}");
  let g = tp(mk(46)); println(f"tp={g.0.inner.v}");
  match op(mk(47)) { Option.Some(v) => println(f"op={v.inner.v}"), Option.None => println("none") }
  let h = rd(mk(48)); println(f"rd={h.name}");
  top(mk(49));
  let mut i = 0;
  while i < 2 { let z = top(mk(50)); println(f"lp={z.inner.v}"); i = i + 1; }
  let q = qq(mkQ(51)); println(f"qq={q.t.len()}");
  let p = ctl(mkP(52)); println(f"ctl={p.name}");
  let r = ret(mk(53)); println(f"ret={r.inner.v}");
  println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out, "top=41\ndR41\ntwo=42\ndR42\ntail=43\ndR43\npair=44\ndR44\nbx=45\ndR45\ntp=46\ndR46\nop=47\ndR47\nin=h48\nrd=h48\ndR48\ndR49\nlp=50\ndR50\nlp=50\ndR50\nqq=1\ndQ51\nctl=p52\ndP52\nret=53\ndR53\nend\n",
                "a callee that returns a REBIND of its by-value param hands the \
                 caller's object back, so the argument temp must not stay an \
                 owner beside the result binding; got {out:?}"
            );
    }
}

#[test]
fn e2e_user_method_builtin_name_on_literal_receiver() {
    // B-2026-07-18-48: a user method whose name collides with a builtin
    // Vec/String method (`get`/`take`/…), called on a NON-identifier receiver
    // (a struct/enum literal or call result) with a heap-shaped receiver,
    // was hijacked by the builtin shape-based dispatch — a single-heap-field
    // struct (`R { v: String }`) shares String's `{ptr,len,cap}` LLVM layout,
    // and a literal receiver's user type isn't in `var_type_names`, so
    // `R { v: "x" }.get()` misrouted to `Vec.get` ("requires an index
    // argument"). The fix dispatches a typechecker-resolved user
    // `Type.method` before the builtin routing. Covers a struct literal, an
    // enum literal, and a chained (call-result) receiver.
    if let Some(out) = run_program(
        "struct R { v: String }\n\
             impl R { fn get(self) -> String { self.v } }\n\
             enum E { A(String) }\n\
             impl E { fn take(self) -> String { match self { E.A(s) => s } } }\n\
             fn mk() -> R { R { v: \"chained\".to_string() } }\n\
             fn main() {\n\
                 println(R { v: \"structlit\".to_string() }.get());\n\
                 println(E.A(\"enumlit\".to_string()).take());\n\
                 println(mk().get());\n\
             }",
    ) {
        assert_eq!(out, "structlit\nenumlit\nchained\n");
    }
}

// ── Basic arithmetic ─────────────────────────────────────────

#[test]
fn test_ir_add_function() {
    let ir = ir_for("fn add(a: i64, b: i64) -> i64 { a + b }");
    assert!(
        ir.contains("define"),
        "should contain a function definition"
    );
    assert!(ir.contains("@add"), "should contain the function name");
    // Checked arithmetic (design.md § Arithmetic Overflow): integer
    // `+` lowers through llvm.sadd.with.overflow since 2026-06-07.
    assert!(
        ir.contains("sadd.with.overflow"),
        "should contain checked integer add"
    );
}

#[test]
fn test_ir_sub_mul_div() {
    let ir = ir_for("fn calc(a: i64, b: i64) -> i64 { (a - b) * (a + b) }");
    // Checked arithmetic: - and * lower through the with.overflow family.
    assert!(ir.contains("ssub.with.overflow"));
    assert!(ir.contains("smul.with.overflow"));
}

/// IR pin (phase-10 line 284): the opaque allocator wrappers
/// (`karac_alloc_fallible` / `karac_alloc_or_panic` / `karac_realloc_or_panic`)
/// carry the SAFE malloc-family modeling attributes (memory-effects +
/// allockind + alloc-family) so LLVM stops treating them as
/// clobber-everything barriers — the alloc-side twin of the free-family set
/// on `karac_free_buf`. The `Zeroed` allockind bit must stay ABSENT
/// (malloc-backed memory is uninitialized). Crucially, `noalias`-return must
/// NOT be present: it is what LLVM needs to REMOVE a dead allocation, but it
/// is unsound under Kāra's recycling cache + move-aliasing (it miscompiled 15
/// E2E programs) — see the helper doc + the phase-10 entry.
#[test]
fn ir_alloc_wrappers_carry_malloc_family_attrs() {
    // Vec.new + push exercises alloc_or_panic + realloc_or_panic; the
    // fallible wrapper is declared unconditionally in Codegen::new.
    let ir = ir_for(
        "fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             let mut i: i64 = 0i64;\n\
             while i < 64i64 { v.push(i); i = i + 1i64; }\n\
             println(v.len().to_string());\n\
             }\n",
    );
    // Collect the wrapper declares + every attribute group so a shared
    // group is visible regardless of which `#N` a declare points at.
    let relevant: String = ir
        .lines()
        .filter(|l| {
            l.contains("@karac_alloc")
                || l.contains("@karac_realloc")
                || l.starts_with("attributes #")
        })
        .collect::<Vec<_>>()
        .join("\n");
    for f in [
        "karac_alloc_fallible",
        "karac_alloc_or_panic",
        "karac_realloc_or_panic",
    ] {
        assert!(
            relevant.contains(&format!("@{f}(")),
            "missing decl {f}\n{relevant}"
        );
    }
    assert!(
        relevant.contains("\"alloc-family\"=\"malloc\""),
        "alloc-family=malloc missing (pairs alloc with karac_free_buf)\n{relevant}"
    );
    assert!(
        relevant.contains("allockind(\"alloc,uninitialized\")"),
        "alloc allockind missing / Zeroed bit leaked\n{relevant}"
    );
    assert!(
        relevant.contains("allockind(\"realloc,uninitialized\")"),
        "realloc allockind missing\n{relevant}"
    );
    // The deliberate exclusion: no `noalias` on any wrapper's return (unsound
    // under the recycling cache). Return attributes render inline on the
    // `declare` line, so check those specifically.
    for f in [
        "karac_alloc_fallible",
        "karac_alloc_or_panic",
        "karac_realloc_or_panic",
    ] {
        let decl = ir
            .lines()
            .find(|l| l.contains("declare") && l.contains(&format!("@{f}(")))
            .unwrap_or_else(|| panic!("no declare line for {f}"));
        assert!(
            !decl.contains("noalias"),
            "{f} must NOT carry noalias-return (unsound under the recycling cache): {decl}"
        );
    }
}

// ── Short-circuit `and` / `or` (roadmap.md:425, 429) ─────────

#[test]
fn test_ir_and_short_circuits_rhs_call() {
    // `false and boom()` must NOT emit `@boom` on the unconditional
    // path; it must live inside an `sc.rhs` block reached only when
    // the LHS is true.
    let ir = ir_for(
        r#"
fn boom() -> bool { true }
fn use_and(x: bool) -> bool { x and boom() }
"#,
    );
    assert!(
        ir.contains("sc.rhs"),
        "expected sc.rhs basic block; IR:\n{ir}"
    );
    assert!(
        ir.contains("sc.merge"),
        "expected sc.merge basic block; IR:\n{ir}"
    );
    assert!(
        ir.contains("phi i1"),
        "expected i1 phi for short-circuit result; IR:\n{ir}"
    );
    // The result must come from a phi with a constant `false` (i1 0)
    // for the short-circuit edge — the LHS-false case never reaches
    // the boom() call.
    assert!(
        ir.contains("phi i1 [ false") || ir.contains("[ false,") || ir.contains("i1 false"),
        "expected short-circuit constant in phi; IR:\n{ir}"
    );
}

#[test]
fn test_ir_or_short_circuits_rhs_call() {
    // `true or boom()` must keep `@boom` behind a conditional branch.
    let ir = ir_for(
        r#"
fn boom() -> bool { false }
fn use_or(x: bool) -> bool { x or boom() }
"#,
    );
    assert!(
        ir.contains("sc.rhs"),
        "expected sc.rhs basic block; IR:\n{ir}"
    );
    assert!(
        ir.contains("sc.merge"),
        "expected sc.merge basic block; IR:\n{ir}"
    );
    assert!(
        ir.contains("phi i1"),
        "expected i1 phi for short-circuit result; IR:\n{ir}"
    );
    // Result phi must carry a constant `true` for the short-circuit edge.
    assert!(
        ir.contains("phi i1 [ true") || ir.contains("[ true,") || ir.contains("i1 true"),
        "expected short-circuit constant in phi; IR:\n{ir}"
    );
}

// ── Recursive functions ───────────────────────────────────────

#[test]
fn test_ir_fibonacci_recursive() {
    let ir = ir_for(
        r#"
fn fib(n: i64) -> i64 {
    if n <= 1 { n } else { fib(n - 1) + fib(n - 2) }
}
"#,
    );
    // Should have a direct recursive call
    assert!(ir.contains("call") && ir.contains("fib"));
}

// ── Multiple functions ────────────────────────────────────────

#[test]
fn test_ir_multiple_functions() {
    let ir = ir_for(
        r#"
fn square(x: i64) -> i64 { x * x }
fn sum_of_squares(a: i64, b: i64) -> i64 { square(a) + square(b) }
"#,
    );
    assert!(ir.contains("define") && ir.chars().filter(|&c| c == '\n').count() > 5);
    // Both functions should be in the IR
    assert!(ir.contains("square"));
    assert!(ir.contains("sum_of_squares"));
}

// ── Compile-to-object (requires linker) ───────────────────────

#[test]
fn test_compile_to_object_hello_world() {
    use karac::codegen::compile_to_object;
    use std::path::Path;

    let src = r#"
fn main() {
    println(42);
}
"#;
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty());
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);

    let obj_path = "/tmp/karac_test_hello.o";
    let result = compile_to_object(&parsed.program, obj_path, None, None);
    assert!(result.is_ok(), "compile_to_object failed: {:?}", result);
    assert!(Path::new(obj_path).exists(), "object file should exist");

    // Clean up
    let _ = std::fs::remove_file(obj_path);
}

/// B-2026-08-01-15 — a whole-move rebind of an owned param
/// (`let h2 = h;`) is a param VIEW, transitively: a destructure of h2
/// hits the same param gates as the direct case, and h2's own let-site
/// registration is memory-only. Pre-fix `karac build` fired a wrapper
/// over h2's cap-zeroed slot (an empty-name body before "got 5") and
/// `karac run` doubled the field body — divergently. Twin of
/// `tests/interpreter.rs`'s `test_param_view_rebind_single_caller_fire`.
#[test]
fn e2e_param_view_rebind_single_caller_fire() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             struct Holder { r: Res }\n\
             fn take(h: Holder) {\n\
             \x20   let h2 = h;\n\
             \x20   let Holder { r } = h2;\n\
             \x20   println(f\"got {r.id}\");\n\
             \x20   println(\"take done\");\n\
             }\n\
             fn take2(h: Holder) {\n\
             \x20   let h2 = h;\n\
             \x20   println(f\"held {h2.r.id}\");\n\
             \x20   println(\"take2 done\");\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let x = Holder { r: Res { id: 5, name: f\"y{5}\" } };\n\
             \x20   take(x);\n\
             \x20   println(\"b\");\n\
             \x20   take2(Holder { r: Res { id: 7, name: f\"y{7}\" } });\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a\ngot 5\ntake done\ndrop 5 y5\nb\nheld 7\ntake2 done\ndrop 7 y7\nend\n"
    );
}

/// B-2026-09-02-2 — AN AGGREGATE LITERAL IN AN ESCAPING POSITION CONSUMES ITS
/// SOURCES. `return W { r: r }` hands `r` to the caller exactly as `return r`
/// does, so ONE `Drop` body is due, at the caller's binding death. The
/// interpreter ran it twice -- `mid dR14 v14 dR14 post` against
/// `mid v14 dR14 post` from all three compiled backends -- because
/// `suppress_tail_expr_user_drop`, the hook both escaping positions drive, read
/// a bare `Identifier` and nothing else, so a source one aggregate deeper was
/// never retracted from the frame's cleanup.
///
/// The TUPLE spelling of the same move was already correct, and that asymmetry
/// is what located the cause. Both escaping positions also call
/// `record_container_bodies_move_sources`, whose `Tuple`/`ArrayLiteral` arms
/// route through `record_container_move_sources_in_aggregate_arg` and put a
/// source carrying its own `impl Drop` on the whole-value channel
/// (B-2026-08-02-27 / B-2026-08-29-45), while its `StructLiteral` arm keeps the
/// container-only recording on purpose: the whole-value channel is wrong for the
/// DISCARD position, where no struct-literal discard walk takes over the
/// retracted body. `discard-guard` is that line, and both backends draw it in
/// the same place. So the struct literal's escaping half went into the hook only
/// the escaping positions reach.
///
/// `enclosing-block-guard` is the shape that was ALREADY correct and says why:
/// a `return` nested one block deeper leaves `r` out of THIS frame's cleanup, so
/// the retraction cannot see it and `record_conditional_move_tail` -- widened to
/// aggregates by B-2026-08-31-35 -- carries it instead. The fix here is the same
/// widening, one hook over.
///
/// `enum-field` and `two-fields` are shapes the row never named and the same
/// change fixes; `sibling-not-moved` and `container-field-control` are the
/// guards in the other direction, where a too-eager retraction would LOSE a body
/// rather than duplicate one.
///
/// Twin of
/// `interpreter::an_escaping_aggregate_literal_consumes_its_sources`, asserting
/// the same strings so a later one-sided edit cannot re-negotiate them.
#[test]
fn e2e_an_escaping_aggregate_literal_consumes_its_sources() {
    const H: &str = "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             enum E { A(i64), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
             struct W { r: R }\n\
             struct Outer { w: W }\n\
             struct Two { a: R, b: R }\n\
             struct Box3 { xs: Vec[R] }\n\
             struct Wrap { e: E }\n\
             fn t_lit(k: i64) -> W { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); return W { r: r } }\n\
             fn s_lit(k: i64) -> W { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); return W { r: r }; }\n\
             fn b_lit(k: i64) -> W { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); W { r: r } }\n\
             fn n_lit(k: i64) -> Outer { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); return Outer { w: W { r: r } } }\n\
             fn two_lit(k: i64) -> Two { let x: R = R { id: k, tag: f\"x\" }; let y: R = R { id: k + 1, tag: f\"y\" }; println(\"mid\"); return Two { a: x, b: y } }\n\
             fn sib_lit(k: i64) -> W { let x: R = R { id: k, tag: f\"x\" }; let y: R = R { id: k + 50, tag: f\"y\" }; println(f\"mid{y.id}\"); return W { r: x } }\n\
             fn enum_lit(k: i64) -> Wrap { let e: E = E.A(k); println(\"mid\"); return Wrap { e: e } }\n\
             fn tup_lit(k: i64) -> (R, i64) { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); return (r, 3) }\n\
             fn named_lit(k: i64) -> W { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); let w: W = W { r: r }; return w }\n\
             fn box_lit(k: i64) -> Box3 { let xs: Vec[R] = [R { id: k, tag: f\"t\" }]; println(\"mid\"); return Box3 { xs: xs } }\n\
             fn cond_lit(k: i64) -> W { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); if k > 0 { return W { r: r } } return W { r: R { id: 0, tag: f\"z\" } } }\n\
             fn fresh_lit(k: i64) -> W { println(\"mid\"); return W { r: R { id: k, tag: f\"t\" } } }\n";
    for (label, body, want) in [
        // THE ROW, in its three escaping spellings: a `return` statement, a
        // tail `return` with no semicolon, and a bare tail. All three hand the
        // literal out, so all three print one body.
        (
            "lit-return-stmt",
            "let v: W = s_lit(1); println(f\"v{v.r.id}\");\n",
            "mid\nv1\ndR1\npost\n",
        ),
        (
            "lit-tail-return",
            "let v: W = t_lit(2); println(f\"v{v.r.id}\");\n",
            "mid\nv2\ndR2\npost\n",
        ),
        (
            "lit-bare-tail",
            "let v: W = b_lit(3); println(f\"v{v.r.id}\");\n",
            "mid\nv3\ndR3\npost\n",
        ),
        // NESTED literals -- the source walk has to recurse, exactly as the
        // container-move and conditional-move walks already do.
        (
            "nested-literal",
            "let v: Outer = n_lit(4); println(f\"v{v.w.r.id}\");\n",
            "mid\nv4\ndR4\npost\n",
        ),
        // TWO consumed sources in one literal, and an ENUM-typed source: both
        // shapes the row never named, both fixed by the same widening. Fields
        // die in reverse declaration order (design.md § Drop ordering).
        (
            "two-fields",
            "let v: Two = two_lit(10); println(f\"v{v.a.id}-{v.b.id}\");\n",
            "mid\nv10-11\ndR11\ndR10\npost\n",
        ),
        (
            "enum-field",
            "let v: Wrap = enum_lit(30); println(\"v\");\n",
            "mid\ndE\nv\npost\n",
        ),
        // GUARD: only the CONSUMED source is retracted. `y` is never moved
        // into the literal, so it still dies inside `sib_lit`.
        (
            "sibling-not-moved",
            "let v: W = sib_lit(20); println(f\"v{v.r.id}\");\n",
            "mid70\ndR70\nv20\ndR20\npost\n",
        ),
        // The three spellings that were ALREADY correct, kept as controls so a
        // later edit cannot regress them into the fixed one's shape. The tuple
        // is the one that located the cause (a second channel, see the doc
        // comment); the named binding records the move at its `let`; the
        // container source resolves to a `Value::Array` and is left alone.
        (
            "tuple-control",
            "let v: (R, i64) = tup_lit(40); println(f\"v{v.0.id}\");\n",
            "mid\nv40\ndR40\npost\n",
        ),
        (
            "named-binding-control",
            "let v: W = named_lit(50); println(f\"v{v.r.id}\");\n",
            "mid\nv50\ndR50\npost\n",
        ),
        (
            "container-field-control",
            "let v: Box3 = box_lit(60); println(f\"v{v.xs.len()}\");\n",
            "mid\nv1\ndR60\npost\n",
        ),
        // GUARD: a `return` one block deeper. `r` is not in the `if`-block's
        // cleanup, so the retraction is a no-op and the conditional-move set
        // has to carry it -- the path that was already right.
        (
            "enclosing-block-guard",
            "let v: W = cond_lit(70); println(f\"v{v.r.id}\");\n",
            "mid\nv70\ndR70\npost\n",
        ),
        // GUARD: no local is consumed at all, so nothing is retracted.
        (
            "fresh-temp-guard",
            "let v: W = fresh_lit(80); println(f\"v{v.r.id}\");\n",
            "mid\nv80\ndR80\npost\n",
        ),
        // GUARD: the DISCARD position, which is NOT escaping and keeps its own
        // channel. Widening the shared dispatcher instead of this hook would
        // have retracted `r0` here with no discard walk to take the body over,
        // losing it entirely.
        (
            "discard-guard",
            "let r0: R = R { id: 90, tag: f\"t\" }; println(\"mid\"); let _ = W { r: r0 };\n",
            "mid\ndR90\npost\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-25-32 — `PriorityQueue.peek`: read the root WITHOUT removing it.
///
/// `peek` returns `Option[T]`, not the `Option[ref T]` Rust's
/// `BinaryHeap::peek` hands back, because a Kāra body cannot construct a
/// borrow-carrying `Option` — see the method's own note in
/// `runtime/stdlib/priority_queue.kara`. The root is therefore COPIED, and
/// the properties worth pinning are that the copy is faithful and that the
/// queue is left alone.
///
/// Both element classes appear for the reason the sibling
/// `..._min_and_max_at_scalar_and_heap_t` states: a scalar `T` rides the
/// all-`i64` base layout, so a `String` failure hides completely behind an
/// `i64`-only fixture (B-2026-08-25-7 looked correct at `T = i64` while every
/// `String` came back empty). Both DIRECTIONS appear because `outranks` is the
/// one branch that differs, and a `peek` that read index 0 without the heap
/// property holding would still look right on a min-first queue.
///
/// The non-disturbance assertions are the load-bearing ones: peek twice and
/// the same element must come back, `len` must not move, and the following
/// `pop` must return exactly what `peek` promised. A `peek` that moved the
/// root out would satisfy the first read and fail all three.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_priority_queue_peek_reads_the_root_without_removing_it`. Sized
/// worker for the same reason the sibling E2E states: on-demand
/// monomorphization recurses through the callee chain at two
/// instantiations, and libtest's default thread is smaller than the 16 MB
/// `src/main.rs` gives the real CLI.
#[test]
fn e2e_stdlib_priority_queue_peek_reads_the_root() {
    let src = r#"
fn main() {
    let e: PriorityQueue[i64] = PriorityQueue.new();
    match e.peek() { Some(v) => { println(v); } None => { println("none"); } }
    let mut q: PriorityQueue[i64] = PriorityQueue.new();
    q.push(5); q.push(1); q.push(4); q.push(9);
    match q.peek() { Some(v) => { println(v); } None => {} }
    match q.peek() { Some(v) => { println(v); } None => {} }
    println(q.len());
    match q.pop() { Some(v) => { println(v); } None => {} }
    match q.peek() { Some(v) => { println(v); } None => {} }
    println(q.len());
    let mut m: PriorityQueue[i64] = PriorityQueue.max_first();
    m.push(5); m.push(1); m.push(4); m.push(9);
    match m.peek() { Some(v) => { println(v); } None => {} }
    let h = PriorityQueue.from([9, 7, 8, 1, 3]);
    match h.peek() { Some(v) => { println(v); } None => {} }
    let hm = PriorityQueue.max_first_from([9, 7, 8, 1, 3]);
    match hm.peek() { Some(v) => { println(v); } None => {} }
    let mut s: PriorityQueue[String] = PriorityQueue.new();
    s.push("pear"); s.push("apple"); s.push("fig");
    match s.peek() { Some(v) => { println(v); } None => {} }
    match s.peek() { Some(v) => { println(v); } None => {} }
    println(s.len());
    match s.pop() { Some(v) => { println(v); } None => {} }
    match s.peek() { Some(v) => { println(v); } None => {} }
}
"#;
    let out = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run_program(src))
        .expect("failed to spawn sized worker")
        .join()
        .expect("compile worker panicked");
    assert_eq!(
        out.as_deref(),
        Some("none\n1\n1\n4\n1\n4\n3\n9\n1\n9\napple\napple\n3\napple\nfig\n")
    );
}

/// B-2026-08-25-32 — the streaming median over two queues, which is the use
/// case the row was filed from (LeetCode 295, kata 295) and the reason `peek`
/// is worth having at all.
///
/// Hold a max-first queue over the lower half and a min-first queue over the
/// upper half; the median is THE TWO ROOTS. That query is O(1) only because
/// reading a root is O(1) — without `peek` the textbook algorithm has to
/// `pop` and `push` back (O(log n) twice, and mutating where a reader wants
/// `ref`), so it cannot be written at its textbook cost.
///
/// The medians are cross-checked against an independent oracle (Python
/// `bisect.insort` + midpoint over the same feed): 5, 10, 5, 4, 5, 6, 7, 7,
/// 8, 7. The trailing `lo.len() + hi.len()` is the non-disturbance assertion
/// that matters most here — after ten iterations each calling `peek` up to
/// twice, all ten elements must still be in the queues. A `peek` that removed
/// what it read would print a number below 10 while every median above it
/// stayed plausible.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_priority_queue_peek_drives_a_streaming_median`.
#[test]
fn e2e_stdlib_priority_queue_peek_drives_a_streaming_median() {
    let src = r#"
fn main() {
    let mut lo: PriorityQueue[i64] = PriorityQueue.max_first();
    let mut hi: PriorityQueue[i64] = PriorityQueue.new();
    let feed: Vec[i64] = [5, 15, 1, 3, 8, 7, 9, 10, 20, 2];
    let mut k = 0;
    while k < feed.len() {
        lo.push(feed[k]);
        match lo.pop() { Some(t) => { hi.push(t); } None => {} }
        if hi.len() > lo.len() {
            match hi.pop() { Some(t) => { lo.push(t); } None => {} }
        }
        if lo.len() == hi.len() {
            match lo.peek() {
                Some(a) => {
                    match hi.peek() { Some(b) => { println((a + b) / 2); } None => {} }
                }
                None => {}
            }
        } else {
            match lo.peek() { Some(a) => { println(a); } None => {} }
        }
        k = k + 1;
    }
    println(lo.len() + hi.len());
}
"#;
    let out = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run_program(src))
        .expect("failed to spawn sized worker")
        .join()
        .expect("compile worker panicked");
    assert_eq!(out.as_deref(), Some("5\n10\n5\n4\n5\n6\n7\n7\n8\n7\n10\n"));
}

/// `PriorityQueue` needs no usage gate, and this pins the reason.
///
/// `std.cli`'s bodies had to be gated (see the sibling test below) because
/// they are non-generic: registering the module emitted them into every
/// program, leaking clone calls and overflow intrinsics that two IR-shape
/// tests assert the absence of. Every `PriorityQueue` method instead lives
/// on a generic impl, so `declare_stdlib_program_inner` seeds it into
/// `mono_state.generic_fns` for on-demand monomorphization — a program that
/// never names the type instantiates nothing. The genericity IS the gate,
/// which is a structural property worth a test rather than a comment: if a
/// future non-generic helper is added to the impl, this fails and whoever
/// added it learns the module now needs cli's treatment.
#[test]
fn stdlib_priority_queue_bodies_stay_out_of_a_queue_free_program() {
    let mut parsed = karac::parse("fn main() { let x = 1 + 2; println(f\"{x}\"); }\n");
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ir = compile_to_ir(&parsed.program, None, None).expect("queue-free program must compile");
    assert!(
        !ir.contains("PriorityQueue"),
        "a PriorityQueue body leaked into a program that never mentions the type"
    );
}

/// B-2026-08-29-43 — a MIXED struct literal over a struct with its OWN
/// `impl Drop`: some Drop-bearing fields moved in from a param VIEW, some
/// minted fresh.
///
/// The view's body belongs to the caller under caller-retains, so this
/// binding must not run it; the fresh field's body belongs to nobody else,
/// so it must. Before the fix the whole literal kept its walk and the view
/// doubled — agreed across all three backends, which is why no A/B gate
/// caught it.
///
/// The danger in fixing it is the OPPOSITE failure: the only wrapper
/// surgery available for an own-`Drop` struct used to be all-or-nothing,
/// and applying that here would have silenced the fresh field too. So every
/// row below pins BOTH halves — the view fires once, the fresh field fires
/// once — and the boundary rows (all-views, all-fresh, no-own-`Drop`) pin
/// that the neighbouring paths did not move.
/// B-2026-08-29-44 — a WHOLE-VALUE REBIND (`let s2 = s;`) after a MIXED
/// wrap re-armed the walk the wrap's mask had just withheld.
///
/// Every mask in this family is keyed on the BINDING, and a rebind
/// registers the destination afresh — so the param view's body ran a
/// second time. The ALL-VIEWS case has no such hole: it marks the binding
/// a param view and view-ness already propagates (B-2026-08-01-15), which
/// is why only the MIXED spellings lost their mask.
///
/// Registering full and disarming afterwards does NOT work at a `let` —
/// the disarm helpers re-register rather than replace, leaving the
/// destination with two walkers and MORE bodies. The fix inherits the
/// source's masks first and builds the walker already masked.
#[test]
fn e2e_whole_value_rebind_inherits_the_wrap_mask() {
    let hdr = "struct R { id: i64, name: String }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   fn mk(i: i64) -> R { return R { id: i, name: f\"heap-{i}\" }; }\n\
                   enum W2 { Two(R, R), None2 }\n\
                   struct S3 { a: R, b: R }\n\
                   struct S1 { r: R }\n";
    for (label, fns, main, want) in [
            (
                "struct literal MIXED, then rebind",
                "fn take(r: R) -> i64 { let s = S3 { a: r, b: mk(2) }; let s2 = s; return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "dR2\ndR1\nv=7\n",
            ),
            (
                "tuple MIXED, then rebind",
                "fn take(r: R) -> i64 { let t = (r, 5); let t2 = t; return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "dR1\nv=7\n",
            ),
            (
                "rebound twice — the mask survives the chain",
                "fn take(r: R) -> i64 { let s = S3 { a: r, b: mk(2) }; let s2 = s; let s3 = s2; return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "dR2\ndR1\nv=7\n",
            ),
            // CONTROLS: the same wraps WITHOUT a rebind were always correct,
            // which is what isolates the rebind as the trigger; and the
            // all-views wrap WITH one was correct through view-ness
            // propagation, the mechanism the mixed case lacks.
            (
                "control: struct MIXED, no rebind",
                "fn take(r: R) -> i64 { let s = S3 { a: r, b: mk(2) }; return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "dR2\ndR1\nv=7\n",
            ),
            (
                "control: tuple MIXED, no rebind",
                "fn take(r: R) -> i64 { let t = (r, 5); return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "dR1\nv=7\n",
            ),
            (
                "control: ALL-VIEWS struct, then rebind",
                "fn take(r: R) -> i64 { let s = S1 { r: r }; let s2 = s; return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "dR1\nv=7\n",
            ),
            (
                "control: enum ctor MIXED, no rebind",
                "fn take(r: R) -> i64 { let w = W2.Two(r, mk(2)); return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "dR2\ndR1\nv=7\n",
            ),
        ] {
            let src = format!("{hdr}{fns}\nfn main() {{ {main} }}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
        }
    // B-2026-08-31-50 — the ENUM-CTOR spelling, pinned at the DEFECT
    // until this row: it doubled on both backends, because codegen derived
    // a constructor's view slots from the ctor EXPRESSION at the `let` and
    // stored nothing per variable, so a rebind had no mask to inherit —
    // and the interpreter's ready transfer was withheld so the two would
    // land together. Codegen now keeps `enum_ctor_moved_payload_slots`
    // per binding and inherits it under the rebind gate, the interpreter
    // transfers `moved_out_enum_payload_slots`, and both print the due
    // `dR2 dR1`.
    let enum_rebind = format!(
        "{hdr}fn take(r: R) -> i64 {{ let w = W2.Two(r, mk(2)); let w2 = w; return 7; }}\n\
             fn main() {{ let v = take(mk(1)); println(f\"v={{v}}\"); }}\n"
    );
    assert_eq!(
        run_program(&enum_rebind).as_deref(),
        Some("dR2\ndR1\nv=7\n"),
        "[enum ctor MIXED then rebind — the mask is inherited (B-2026-08-31-50)]"
    );
}

/// B-2026-09-06-19 — a by-value `Drop` param WRAPPED through a local on
/// its way out (`let p = P2 { r: r, n: 1 }; return Box2 { r: p.r }`) ran
/// its body twice on the hand-back path on every surface: the hand-back
/// predicates followed whole rebinds but not a wrap, so the caller kept
/// firing beside the result binding. `param_wrap_aliases` now carries the
/// param through the wrap (nested wraps, rebinds and projection rebinds of
/// the wrapper included) and all three predicates consult it. The same
/// row's method spelling exposed a second hole: a `Drop` field projected
/// out of a PLAIN local into the returned value (`lproj` .. `mlocal`) ran
/// its body at the local's death and at the result's, on every backend —
/// the return site never masked a projected field the way `let q = p.r`
/// does; it does now, one hop or nested. Every cell is one body per path:
/// the dies-inside path keeps its body inside the callee (`d1 C82`), the
/// hand-back path's body is the result binding's (`C2 d2`).
///
/// Twin of `tests/interpreter.rs`'s `test_param_wrapped_through_local_hands_back_once`, pinned to the same string.
#[test]
fn e2e_param_wrapped_through_local_hands_back_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mr(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct P2 { r: R, n: i64 }
struct Box2 { r: R }
struct W { p: P2 }
struct K { n: i64 }

fn ftwo(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(82) }; } let p = P2 { r: r, n: 1 }; return Box2 { r: p.r }; }
fn fone(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(83) }; } let p = Box2 { r: r }; return p; }
fn fproj(r: R, k: bool) -> R { if k { return mr(84); } let p = P2 { r: r, n: 1 }; return p.r; }
fn flet(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(85) }; } let p = P2 { r: r, n: 1 }; let q = p.r; return Box2 { r: q }; }
fn fnest(r: R, k: bool) -> W { if k { return W { p: P2 { r: mr(86), n: 0 } }; } let p = P2 { r: r, n: 1 }; let w = W { p: p }; return w; }
fn ftup(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(87) }; } let t = (r, 1); return Box2 { r: t.0 }; }
fn frebind(r: R, k: bool) -> P2 { if k { return P2 { r: mr(88), n: 0 }; } let p = P2 { r: r, n: 1 }; let q = p; return q; }
fn fearly(r: R, k: bool) -> Box2 { let p = P2 { r: r, n: 1 }; if k { return Box2 { r: mr(89) }; } return Box2 { r: p.r }; }
fn funcond(r: R) -> Box2 { let p = P2 { r: r, n: 1 }; return Box2 { r: p.r }; }
fn fpart(r: R, k: bool) -> i64 { let p = P2 { r: r, n: 1 }; if k { return 0; } return p.r.id; }
fn lproj() -> Box2 { let p = P2 { r: mr(19), n: 1 }; return Box2 { r: p.r }; }
fn lbare() -> R { let p = P2 { r: mr(20), n: 1 }; return p.r; }
fn ltail() -> Box2 { let p = P2 { r: mr(21), n: 1 }; Box2 { r: p.r } }
fn lopt() -> Option[R] { let p = P2 { r: mr(22), n: 1 }; return Option.Some(p.r); }
fn lnest() -> R { let w = W { p: P2 { r: mr(23), n: 1 } }; return w.p.r; }
impl K {
    fn mlocal(self) -> Box2 { let p = P2 { r: mr(24), n: self.n }; return Box2 { r: p.r }; }
    fn m(self, r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(90) }; } let p = P2 { r: r, n: self.n }; return Box2 { r: p.r }; }
    fn mref(ref self, r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(91) }; } let p = P2 { r: r, n: self.n }; return Box2 { r: p.r }; }
    fn muncond(self, r: R) -> Box2 { let p = P2 { r: r, n: self.n }; return Box2 { r: p.r }; }
}

fn main() {
    println("ftwo/t"); let a1 = ftwo(mr(1), true); println(f"  C{a1.r.id}");
    println("ftwo/f"); let a2 = ftwo(mr(2), false); println(f"  C{a2.r.id}");
    println("fone/f"); let a3 = fone(mr(3), false); println(f"  C{a3.r.id}");
    println("fproj/f"); let a4 = fproj(mr(4), false); println(f"  C{a4.id}");
    println("flet/f"); let a5 = flet(mr(5), false); println(f"  C{a5.r.id}");
    println("fnest/f"); let a6 = fnest(mr(6), false); println(f"  C{a6.p.r.id}");
    println("ftup/f"); let a7 = ftup(mr(7), false); println(f"  C{a7.r.id}");
    println("frebind/f"); let a8 = frebind(mr(8), false); println(f"  C{a8.r.id}");
    println("fearly/t"); let a9 = fearly(mr(9), true); println(f"  C{a9.r.id}");
    println("fearly/f"); let a10 = fearly(mr(10), false); println(f"  C{a10.r.id}");
    println("funcond"); let a11 = funcond(mr(11)); println(f"  C{a11.r.id}");
    println("funcond/named"); let x12 = mr(12); let a12 = funcond(x12); println(f"  C{a12.r.id}");
    println("fpart/t"); let a13 = fpart(mr(13), true); println(f"  C{a13}");
    println("fpart/f"); let a14 = fpart(mr(14), false); println(f"  C{a14}");
    println("m/t"); let a15 = K { n: 1 }.m(mr(15), true); println(f"  C{a15.r.id}");
    println("m/f"); let a16 = K { n: 1 }.m(mr(16), false); println(f"  C{a16.r.id}");
    println("mref/f"); let kk = K { n: 2 }; let a17 = kk.mref(mr(17), false); println(f"  C{a17.r.id}");
    println("muncond"); let a18 = K { n: 3 }.muncond(mr(18)); println(f"  C{a18.r.id}");
    println("lproj"); let a19 = lproj(); println(f"  C{a19.r.id}");
    println("lbare"); let a20 = lbare(); println(f"  C{a20.id}");
    println("ltail"); let a21 = ltail(); println(f"  C{a21.r.id}");
    println("lopt"); if let Some(a22) = lopt() { println(f"  C{a22.id}"); }
    println("lnest"); let a23 = lnest(); println(f"  C{a23.id}");
    println("mlocal"); let a24 = K { n: 4 }.mlocal(); println(f"  C{a24.r.id}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"ftwo/t
  d1
  C82
  d82
ftwo/f
  C2
  d2
fone/f
  C3
  d3
fproj/f
  C4
  d4
flet/f
  C5
  d5
fnest/f
  C6
  d6
ftup/f
  C7
  d7
frebind/f
  C8
  d8
fearly/t
  C89
  d89
fearly/f
  C10
  d10
funcond
  C11
  d11
funcond/named
  C12
  d12
fpart/t
  d13
  C0
fpart/f
  d14
  C14
m/t
  d15
  C90
  d90
m/f
  C16
  d16
mref/f
  C17
  d17
muncond
  C18
  d18
lproj
  C19
  d19
lbare
  C20
  d20
ltail
  C21
  d21
lopt
  C22
  d22
lnest
  C23
  d23
mlocal
  C24
  d24
end
"#
    );
}

/// B-2026-09-07-4, the via-call half — an argument handed back THROUGH one
/// further call double-freed on the METHOD and ASSOC-FN legs, where the free
/// leg was clean: `R.passb(mk(1))` over
/// `impl R { fn passb(r: R) -> R { return fwd(r); } }` aborted `free(): double
/// free detected in tcache 2` on all four compiled surfaces while the DIRECT
/// `return r;` spelling beside it (`R.passa`) was fine.
///
/// Three legs of one rule, each missing a piece its sibling had. The two
/// registrars' ADMISSION gates asked `fn_always_returns_param`, whose `yields`
/// walker admits an identifier, an aggregate literal and an optres ctor and
/// lets a `Call` fall through, so a hop was invisible; they now also ask
/// `fn_always_returns_param_via_call` (B-2026-09-07-10's predicate). The ASSOC
/// leg's named-argument stand-down kept the binding's MEMORY, which is the
/// entry-copy contract and wrong for a forwarded param — B-2026-09-06-71's
/// split, applied here as it was to the method leg in B-2026-09-07-11. And the
/// interpreter's `record_method_arg_moves` did not count a hop as an escape at
/// all, so it ran the body twice for a named local.
///
/// Cells: the assoc hop, the assoc direct control, the method hop, a
/// copy-supported assoc hop (whose caller slot keeps its own copy), a method
/// whose value dies one hop down, and a named local into the assoc hop.
///
/// Twin of `tests/interpreter.rs`'s `test_argument_handed_back_through_a_hop_by_a_method`, pinned to the same string.
#[test]
fn e2e_argument_handed_back_through_a_hop_by_a_method() {
    let Some(out) = run_program(
        r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { id: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"  dP{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn mkp(i: i64) -> P { return P { id: i, name: f"p{i}", xs: [i] }; }
fn fwd(r: R) -> R { return r; }
fn pfwd(p: P) -> P { return p; }
fn dies(r: R) -> i64 { return r.id; }
struct Hold { n: i64 }
impl R { fn passb(r: R) -> R { return fwd(r); } }
impl R { fn passa(r: R) -> R { return r; } }
impl P { fn ppassb(p: P) -> P { return pfwd(p); } }
impl Hold { fn thruv(ref self, r: R) -> R { return fwd(r); } }
impl Hold { fn eats(ref self, r: R) -> i64 { return dies(r); } }
fn main() {
  let h = Hold { n: 0 };
  println("assoc_hop"); let z = R.passb(mk(1)); println(f"  v={z.inner.v}");
  println("assoc_direct"); let y = R.passa(mk(2)); println(f"  v={y.inner.v}");
  println("method_hop"); let w = h.thruv(mk(3)); println(f"  v={w.inner.v}");
  println("assoc_hop_copyable"); let c = P.ppassb(mkp(4)); println(f"  v={c.id}");
  println("method_hop_dies"); println(f"  v={h.eats(mk(5))}");
  println("named_into_assoc_hop"); let a = mk(6); let b = R.passb(a); println(f"  v={b.inner.v}");
  println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"assoc_hop
  v=1
  dR1
assoc_direct
  v=2
  dR2
method_hop
  v=3
  dR3
assoc_hop_copyable
  v=4
  dP4
method_hop_dies
  dR5
  v=5
named_into_assoc_hop
  v=6
  dR6
end
"#
    );
}

/// B-2026-09-07-11 (codegen) and B-2026-09-07-12 (interpreter) — one program,
/// two defects, one on each backend.
///
/// `let a = mk(1); b.push(a);` over
/// `impl Box2 { fn push(mut ref self, r: R) { self.xs.push(r); } }` aborted
/// `free(): double free detected in tcache 2` on every compiled surface for a
/// struct with a `shared` field, while the FREE-FUNCTION twin `take(mut d, e)`
/// and the FRESH-TEMP spelling of the method were both clean: the method arm of
/// the named-argument stand-down kept the binding's MEMORY action, which is the
/// entry-copy contract and wrong for a forwarded (declined-copy) param.
/// B-2026-09-06-71 made that split in the free-function arm and the method arm
/// did not inherit it.
///
/// The same program ran the `Drop` body TWICE under `--interp`, in BOTH copy
/// classes: `record_method_arg_moves` excluded the STORE route from the set of
/// escapes that disarm the binding's own body, while its free-function twin
/// `record_passthrough_arg_moves` includes it (B-2026-08-29-49) — and that leg
/// is the one that measures correct, because the value's new home runs the body.
///
/// Cells: the named local into a storing method (both classes), the fresh-temp
/// control, the free-function control, and two named locals in a row.
///
/// Twin of `tests/interpreter.rs`'s `test_named_local_into_a_storing_method`, pinned to the same string.
#[test]
fn e2e_named_local_into_a_storing_method() {
    let Some(out) = run_program(
        r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct S { id: i64, name: String }
impl Drop for S { fn drop(mut ref self) { println(f"  dS{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn mks(i: i64) -> S { return S { id: i, name: f"s{i}" }; }
struct Box2 { mut xs: Vec[R] }
impl Box2 { fn push(mut ref self, r: R) { self.xs.push(r); } }
struct BoxS { mut ys: Vec[S] }
impl BoxS { fn add(mut ref self, s: S) { self.ys.push(s); } }
fn take(b: mut ref Box2, r: R) { b.xs.push(r); }
fn main() {
  println("named_method");
  let mut b = Box2 { xs: Vec.new() };
  let a = mk(1); b.push(a); println(f"  len={b.xs.len()}");
  println("fresh_method");
  let mut c = Box2 { xs: Vec.new() };
  c.push(mk(2)); println(f"  len={c.xs.len()}");
  println("named_free_fn");
  let mut d = Box2 { xs: Vec.new() };
  let e = mk(3); take(mut d, e); println(f"  len={d.xs.len()}");
  println("copy_supported_method");
  let mut g = BoxS { ys: Vec.new() };
  let h = mks(4); g.add(h); println(f"  len={g.ys.len()}");
  println("two_named");
  let mut i = Box2 { xs: Vec.new() };
  let j = mk(5); i.push(j); let k = mk(6); i.push(k); println(f"  len={i.xs.len()}");
  println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"named_method
  len=1
  dR1
fresh_method
  len=1
  dR2
named_free_fn
  len=1
  dR3
copy_supported_method
  len=1
  dS4
two_named
  len=2
  dR5
  dR6
end
"#
    );
}

/// B-2026-09-07-10 — `let a = mk(1); let z = via(a);` over
/// `fn via(r: R) -> R { return f(r); }` and `fn f(r: R) -> R { return r; }`
/// aborted `free(): double free detected in tcache 2` on every compiled backend
/// while the FRESH-TEMP spelling of the same call was clean, and `--interp` ran
/// the `Drop` body TWICE for one object — once on the moved-from binding before
/// the result was read, once as the result's own.
///
/// One hop was the whole difference. The caller's ADMISSION gate
/// `call_arg_flows_into_return` has known the forwarding route since
/// B-2026-08-28-62; the STAND-DOWN beside it asks
/// `callee_takes_over_arg_drop_body`, which had a via-call disjunct for the
/// STORE route and none for the RETURN route. Both now ask
/// `fn_always_returns_param_via_call` — the ALL-paths form, because this is the
/// suppressing direction and a mixed-path callee's dies-inside leg is
/// registered bodies-only.
///
/// Cells: the named local through one hop, the fresh-temp control, a hop with a
/// REBIND in front of it (`let m = r; return f(m);`, which reaches neither the
/// admission gate's any-path via test nor `callee_hands_arg_off`), a
/// copy-supported struct, a callee where the value dies one hop down, and a
/// MIXED-path hop on its dies-inside leg — the leg that must stay clean, and
/// the reason the predicate is all-paths.
///
/// Twin of `tests/interpreter.rs`'s `test_named_local_through_a_forwarding_hop`, pinned to the same string.
#[test]
fn e2e_named_local_through_a_forwarding_hop() {
    let Some(out) = run_program(
        r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { id: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"  dP{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn mkp(i: i64) -> P { return P { id: i, name: f"p{i}", xs: [i] }; }
fn f(r: R) -> R { return r; }
fn ppass(p: P) -> P { return p; }
fn dies(r: R) -> i64 { return r.id; }
fn via(r: R) -> R { return f(r); }
fn via2(r: R) -> R { let m = r; return f(m); }
fn pvia(p: P) -> P { return ppass(p); }
fn dvia(r: R) -> i64 { return dies(r); }
fn mvia(r: R, c: bool) -> R { if c { return f(r); } return mk(99); }
fn main() {
  println("named_hop"); let a = mk(1); let z = via(a); println(f"  v={z.inner.v}");
  println("fresh_hop"); let w = via(mk(2)); println(f"  v={w.inner.v}");
  println("rebind_hop"); let b = mk(3); let y = via2(b); println(f"  v={y.inner.v}");
  println("copyable_hop"); let c = mkp(4); let d = pvia(c); println(f"  v={d.id}");
  println("dies_in_hop"); let e = mk(5); println(f"  v={dvia(e)}");
  println("mixed_dies_inside"); let g = mk(6); let h = mvia(g, false); println(f"  v={h.id}");
  println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"named_hop
  v=1
  dR1
fresh_hop
  v=2
  dR2
rebind_hop
  v=3
  dR3
copyable_hop
  v=4
  dP4
dies_in_hop
  v=5
  dR5
mixed_dies_inside
  dR6
  v=99
  dR99
end
"#
    );
}

/// B-2026-09-06-71 — `let a = mk(1); let z = pass(a);` over
/// `fn pass(r: R) -> R { return r; }` and a struct with a `shared` field aborted
/// `free(): double free detected in tcache 2` under `karac run` and at both opt
/// levels, while the FRESH-TEMP spelling of the same call (`pass(mk(3))`) was
/// clean on every surface — the argument's spelling, not the callee, was the
/// whole difference.
///
/// The named-argument stand-down retracted the binding's Drop BODY and kept its
/// MEMORY. That split is the entry-copy contract: the callee deep-copies at
/// entry, so the caller's slot still owns an object of its own. A struct that
/// DECLINES copy support is forwarded rather than copied, so the object the
/// callee hands back IS this binding's, and the result binding became its second
/// owner. `aggregate_param_copy_supported_struct` is now the split, which is
/// what keeps the copy-supported cell's memory where it belongs.
///
/// Cells: a named local into a direct passthrough, into a rebinding passthrough,
/// the fresh-temp control, a copy-supported struct (whose caller slot must KEEP
/// its memory), a callee where the value dies inside, and two named locals in a
/// row.
///
/// Twin of `tests/interpreter.rs`'s `test_named_local_argument_to_a_passthrough_callee`, pinned to the same string.
#[test]
fn e2e_named_local_argument_to_a_passthrough_callee() {
    let Some(out) = run_program(
        r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { id: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"  dP{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn mkp(i: i64) -> P { return P { id: i, name: f"p{i}", xs: [i] }; }
fn pass(r: R) -> R { return r; }
fn rebpass(r: R) -> R { let m = r; return m; }
fn ppass(p: P) -> P { return p; }
fn dies(r: R) -> i64 { return r.id; }
fn main() {
  println("named"); let a = mk(1); let z = pass(a); println(f"  v={z.inner.v}");
  println("named_rebind"); let b = mk(2); let y = rebpass(b); println(f"  v={y.inner.v}");
  println("fresh_temp"); let w = pass(mk(3)); println(f"  v={w.inner.v}");
  println("copyable"); let c = mkp(4); let d = ppass(c); println(f"  v={d.id}");
  println("dies_inside"); let e = mk(5); println(f"  v={dies(e)}");
  println("two_in_a_row"); let g = mk(6); let h = pass(g); let n = mk(7); let q = rebpass(n); println(f"  v={h.inner.v}{q.inner.v}");
  println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"named
  v=1
  dR1
named_rebind
  v=2
  dR2
fresh_temp
  v=3
  dR3
copyable
  v=4
  dP4
dies_inside
  v=5
  dR5
two_in_a_row
  v=67
  dR7
  dR6
end
"#
    );
}

/// B-2026-07-30-5 — the VecDeque head-index lowering preserves FIFO
/// semantics across every shape it rewrites.
///
/// An eligible local deque (see `crate::deque_head`) reinterprets the
/// header's `len` as the END INDEX of the live range `data[head..len]`:
/// `pop_front` reads at `head` and bumps it (no tail memmove — the O(n)
/// per pop that made drains O(n²)), with amortized compaction sliding
/// the live range back once the dead prefix dominates. Shapes pinned
/// here: a full 1000-element drain past several growths (sum 499500), a
/// PARTIAL drain whose binding dies with `head > 0` (scope-exit free
/// must still see the untouched malloc base), a push after partial
/// drain, and pop-to-empty-then-push.
///
/// Twin of `tests/interpreter.rs`'s `test_deque_head_fifo_semantics`
/// on identical source and expected output.
#[test]
fn e2e_deque_head_fifo_semantics() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   let mut q: VecDeque[i64] = VecDeque.new();\n\
             \x20   let mut i = 0;\n\
             \x20   while i < 1000 {\n\
             \x20       q.push_back(i);\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   let mut acc = 0;\n\
             \x20   while not q.is_empty() {\n\
             \x20       match q.pop_front() { Some(x) => { acc = acc + x; } None => {} }\n\
             \x20   }\n\
             \x20   println(acc);\n\
             \x20   let mut p: VecDeque[i64] = VecDeque.new();\n\
             \x20   let mut j = 0;\n\
             \x20   while j < 10 {\n\
             \x20       p.push_back(j * 10);\n\
             \x20       j = j + 1;\n\
             \x20   }\n\
             \x20   match p.pop_front() { Some(x) => { println(x); } None => {} }\n\
             \x20   match p.pop_front() { Some(x) => { println(x); } None => {} }\n\
             \x20   p.push_back(999);\n\
             \x20   println(p.len());\n\
             \x20   let mut d: VecDeque[i64] = VecDeque.new();\n\
             \x20   let mut k = 0;\n\
             \x20   while k < 4 {\n\
             \x20       d.push_back(k);\n\
             \x20       k = k + 1;\n\
             \x20   }\n\
             \x20   while not d.is_empty() {\n\
             \x20       match d.pop_front() { Some(_) => {} None => {} }\n\
             \x20   }\n\
             \x20   d.push_back(77);\n\
             \x20   match d.pop_front() { Some(x) => { println(x); } None => {} }\n\
             \x20   println(d.len());\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "499500\n0\n10\n9\n77\n0\n");
}

/// An integer key takes a different arm of `emit_hash_fn_for_type` from a
/// `String` one (the key's own byte width in place, no header load), and a
/// `Set` takes the selector in its own trailing position. Both reach the
/// same user hasher.
#[test]
fn a_user_hasher_serves_integer_keys_and_sets_too() {
    let out = run_program(&format!(
        "{USER_HASHERS}\
             fn main() {{\n\
                 let mut m: Map[i64, String, FnvBuild] = Map.new();\n\
                 m.insert(10, \"ten\");\n\
                 m.insert(20, \"twenty\");\n\
                 println(m.get(10).unwrap());\n\
                 println(m.get(20).unwrap());\n\
                 if m.contains_key(30) {{ println(1); }} else {{ println(0); }}\n\
                 let mut s: Set[String, SumBuild] = Set.new();\n\
                 s.insert(\"alpha\");\n\
                 s.insert(\"bravo\");\n\
                 s.insert(\"alpha\");\n\
                 println(s.len());\n\
                 if s.contains(\"bravo\") {{ println(1); }} else {{ println(0); }}\n\
             }}"
    ));
    assert_eq!(out, Some("ten\ntwenty\n0\n2\n1\n".to_string()));
}

/// B-2026-08-17-13 — deferred initialization (`let x: T;` + later
/// assignment) of SCALAR locals lowers for real. The old "arm" was a
/// no-op whose comment claimed lazy materialization; in fact the first
/// assignment was silently dropped (the generic identifier store has no
/// else) and the first read died with `Undefined variable`. Straight-line,
/// branch-init, and read-back-through-f-string shapes, each an
/// interpreter-parity check.
#[test]
fn scalar_deferred_initialization_lowers() {
    assert_eq!(
        run_program("fn main() { let x: i64; x = 7; println(x); }"),
        Some("7\n".to_string()),
        "straight-line deferred init"
    );
    assert_eq!(
        run_program(
            "fn main() { let c = true; let x: i64; if c { x = 1; } else { x = 2; } println(x); }"
        ),
        Some("1\n".to_string()),
        "branch-init deferred init (the spec's canonical shape)"
    );
    assert_eq!(
        run_program(
            "fn main() { let f: f64; f = 2.5; let b: bool; b = f > 2.0; println(f\"{f} {b}\"); }"
        ),
        Some("2.5 true\n".to_string()),
        "f64/bool deferred init read through f-string holes"
    );
}

/// B-2026-08-13-15 — an IMPLICIT WIDENING COERCION must widen the way the
/// SOURCE type says, and must actually reach the slot it is widening into.
///
/// The typechecker deliberately admits `u8` where `i64` is declared:
/// `check_int_widening_coercion` rejects only narrowing, and its own
/// diagnostic advertises the rule ("widening coercions such as i32 -> i64
/// remain implicit"). Codegen did not implement what that authorizes, in
/// two separate ways, and the interpreter implemented it correctly — so
/// every case below was a silent wrong ANSWER, not a crash:
///
///   * the boundary coercion sign-extended unconditionally, so any
///     unsigned source with its high bit set went negative (`200u8` into
///     an `i64` param arrived as -56);
///   * the container element/key paths did not coerce AT ALL — they stored
///     the narrow value into an `elem_ty`-sized slot, leaving the high
///     bytes undefined for the erased runtime to hash. That one is the
///     nastiest shape a miscompile takes: the answer varied by BACKEND and
///     by OPTIMIZATION LEVEL (`Set[i64].contains(u8)` returned true under
///     the JIT, false at -O0, false at -O2).
///
/// The interpreter is the oracle: every line must match it exactly. The
/// values are chosen with the high bit SET (200, 60000, 4000000000)
/// because that is the only region where sext and zext differ — the
/// original report probed with 97, where they agree, which is why its
/// scope table recorded several of these lines as working.
#[test]
fn test_e2e_implicit_unsigned_widening_matches_interpreter() {
    let src = r#"
struct W { mut a: i64 }

fn t64(v: i64) -> i64 { v }
fn t32(v: i32) -> i32 { v }
fn ret64(b: u8) -> i64 { b }

fn main() {
    let b: u8 = 200u8;
    let w: u16 = 60000u16;
    let d: u32 = 4000000000u32;

    println(f"01 {t64(b)}");
    println(f"02 {t64(w)}");
    println(f"03 {t64(d)}");
    println(f"04 {t32(b)}");
    println(f"05 {ret64(b)}");

    let x: i64 = b;
    println(f"06 {x}");

    let s = W { a: b };
    println(f"07 {s.a}");

    let mut v: Vec[i64] = vec![];
    v.push(b);
    println(f"08 {v[0]}");

    let mut st: Set[i64] = Set.new();
    st.insert(b);
    println(f"09 {st.contains(200i64)}");
    println(f"10 {st.contains(b)}");
    st.remove(b);
    println(f"11 {st.len()}");

    let mut m: Map[i64, i64] = Map.new();
    let _ = m.insert(b, 7i64);
    println(f"12 {m.get_or(200i64, 0i64)}");
    println(f"13 {m.contains_key(b)}");

    let arr: Vec[i64] = vec![b, w];
    println(f"14 {arr[0]} {arr[1]}");

    // Place-expression writes: the same coercion, reached through an
    // assignment rather than a constructor.
    let mut ws = W { a: 0i64 };
    ws.a = b;
    println(f"15 {ws.a}");
    v[0] = d;
    println(f"16 {v[0]}");
    m[b] = 5i64;
    println(f"17 {m[200i64]} {m[b]}");

    // int -> float is a widening too, and `sitofp` on a u8 is just as wrong.
    let fl: f64 = b;
    println(f"18 {fl}");

    // A tuple's LLVM type comes from its element VALUES, so the bytes are
    // right and the READ is what has to know the element is unsigned.
    let t = (b, d);
    println(f"19 {t.0} {t.1}");

    // Ordered container, separate lowering from the hashed one.
    let mut ss: SortedSet[i64] = SortedSet.new();
    ss.insert(b);
    println(f"20 {ss.contains(b)} {ss.contains(200i64)}");
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // Anti-vacuity: the oracle itself must carry the widened values. If the
    // interpreter ever regressed to sext, this test would happily assert
    // agreement on two wrong answers.
    assert!(
        expected.contains("01 200") && expected.contains("03 4000000000"),
        "interpreter oracle is not producing widened values: {expected:?}"
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "compiled output must match the interpreter on every implicit \
             unsigned widening",
    );
}

#[test]
fn test_ir_fence_relaxed_rejected() {
    // A `Relaxed` fence orders nothing and LLVM forbids `fence monotonic`;
    // `compile_atomic_fence` rejects it with an actionable diagnostic.
    let mut parsed = karac::parse(
        r#"
fn barrier() {
    // Safety: rejected before this matters.
    unsafe { fence(MemoryOrdering.Relaxed); }
}
fn main() { barrier(); }
"#,
    );
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let err = compile_to_ir(&parsed.program, None, None)
        .expect_err("a Relaxed fence must be rejected at codegen")
        .message;
    assert!(
        err.contains("fence ordering must be") && err.contains("Relaxed"),
        "expected a Relaxed-fence rejection, got: {err}"
    );
}

#[test]
fn test_e2e_fence_and_compiler_fence_run() {
    // Both fences are valid barriers around ordinary code; a
    // single-threaded program observes no reordering, so the program
    // simply runs and prints. Witnesses end-to-end lowering + link.
    let out = run_program(
        r#"
fn main() {
    // Safety: no paired cross-thread accesses in this single-threaded demo.
    unsafe { fence(MemoryOrdering.SeqCst); }
    compiler_fence(MemoryOrdering.Acquire);
    println(1);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1");
    }
}

#[test]
fn test_e2e_critical_section_run() {
    // End-to-end: acquire a critical section, do work, drop the guard at
    // scope exit (re-enabling interrupts via the runtime). Single-threaded
    // hosted target, so it simply runs and prints — witnesses lowering +
    // link of the acquire/release runtime pair.
    let out = run_program(
        r#"
fn work() with writes(Hardware) {
    let _guard = critical_section.acquire();
    println(42);
}
fn main() {
    work();
    println(99);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42\n99");
    }
}

#[test]
fn test_ir_f16_bf16_arithmetic_lowers_to_half_and_bfloat() {
    // `f16` lowers to LLVM `half`, `bf16` to `bfloat` (phase-11).
    // f16 arithmetic stays native `half` (softPromoteHalf is mature on
    // every backend); bf16 arithmetic must NOT emit `fmul bfloat` —
    // LLVM 18's AArch64 ISel is gapped on bfloat arithmetic/conversion
    // nodes (B-2026-07-22-1), so it computes in f32 (the widen sequence
    // + `fmul float`) and rounds back to bf16 with the RNE bit trick.
    let ir = ir_for(
            "fn f(a: f16, b: f16) -> f16 { a + b }\nfn g(a: bf16, b: bf16) -> bf16 { a * b }\nfn main() { let _ = f(1.0f16, 2.0f16); let _ = g(1.0bf16, 2.0bf16); }",
        );
    assert!(
        ir.contains("fadd half"),
        "expected an `fadd half` for f16 add; IR:\n{ir}"
    );
    assert!(
        !ir.contains("fmul bfloat"),
        "bf16 mul must compute in f32, not `fmul bfloat` \
             (B-2026-07-22-1); IR:\n{ir}"
    );
    assert!(
        ir.contains("fop.l.bf2f") && ir.contains("fop.res"),
        "expected the bf16 mul to widen via the integer sequence and \
             round back (`fop.l.bf2f` / `fop.res`); IR:\n{ir}"
    );
}

/// B-2026-08-31-13's compiled half. The defect was a TYPECHECK rejection —
/// `x.partial_cmp(y)` on a concrete receiver failed with "expects 2
/// argument(s), found 1" — so both backends agreed before the fix by
/// refusing to build at all. This pins that they now agree on RUNNING it,
/// beside `interp_partial_cmp_value_receiver_resolves_on_a_concrete_type`
/// in tests/interpreter.rs.
///
/// `f64-nan` is the case that makes the method worth having: `PartialOrd`
/// is separate from `Ord` so an incomparable pair answers `None`, and a
/// bare float is the population that needs it. `f.cmp(g)` is and stays
/// rejected — floats have no baked `Ord` — which the typechecker test
/// asserts.
#[test]
fn e2e_partial_cmp_value_receiver_runs_on_a_concrete_type() {
    let arms = "Some(Less) => println(\"L\"), Some(Equal) => println(\"E\"), \
                    Some(Greater) => println(\"G\"), None => println(\"N\")";
    for (label, body, want) in [
        (
            "i64",
            format!("let n: i64 = 3; let o: i64 = 4; match n.partial_cmp(o) {{ {arms} }}"),
            "L\n",
        ),
        (
            "u64",
            format!(
                "let n: u64 = 18446744073709551615u64; let o: u64 = 2u64;\n\
                     match n.partial_cmp(o) {{ {arms} }}"
            ),
            "G\n",
        ),
        (
            "f64",
            format!("let x: f64 = 1.5; let y: f64 = 2.5; match x.partial_cmp(y) {{ {arms} }}"),
            "L\n",
        ),
        (
            "f64-nan",
            format!(
                "let x: f64 = 0.0 / 0.0; let y: f64 = 2.5;\n\
                     match x.partial_cmp(y) {{ {arms} }}"
            ),
            "N\n",
        ),
        (
            "String",
            format!(
                "let a: String = \"x\"; let b: String = \"y\";\n\
                     match a.partial_cmp(b) {{ {arms} }}"
            ),
            "L\n",
        ),
        (
            "char",
            format!("let a: char = 'x'; let b: char = 'y'; match a.partial_cmp(b) {{ {arms} }}"),
            "L\n",
        ),
        (
            "parenthesized-receiver",
            format!("let n: i64 = 3; match (n).partial_cmp(n + 1) {{ {arms} }}"),
            "L\n",
        ),
        // The sibling that always worked, and the bound path the row
        // records as unaffected (a type-param receiver resolves by a
        // different route than a concrete one).
        (
            "control-cmp",
            "let n: i64 = 3; let o: i64 = 4;\n\
                 match n.cmp(o) { Less => println(\"L\"), Equal => println(\"E\"), \
                 Greater => println(\"G\") }"
                .to_string(),
            "L\n",
        ),
        (
            "control-operator",
            "let n: i64 = 3; let o: i64 = 4; println(n < o);".to_string(),
            "true\n",
        ),
    ] {
        let src = format!("fn main() {{\n    {body}\n}}");
        assert_eq!(run_program(&src).as_deref(), Some(want), "case {label}");
    }
    // The `T: PartialOrd` BOUND path, which B-2026-08-30-41 made runnable
    // and this row notes is unaffected: lowering emits the same
    // value-receiver shape, and it always worked.
    let src = "fn g[T: PartialOrd](a: T, b: T) -> bool { a < b }\n\
                   fn main() { let n: i64 = 3; let o: i64 = 4; println(g(n, o)); }";
    assert_eq!(run_program(src).as_deref(), Some("true\n"), "control-bound");
}

#[test]
fn test_e2e_total_order_wrapper_nan_canonicalized() {
    // B-2026-08-11-13 — codegen twin of
    // `test_total_order_wrapper_nan_canonicalized_interp_parity`; the two
    // assert the SAME string because run == build is the property at
    // stake. The wrapper's order is bit-level (IEEE totalOrder), so the
    // sign of a NaN used to decide the answer while nothing in the source
    // controlled it: x86 runtime division produces a negative NaN, LLVM's
    // constant folder a positive one, and totalOrder sorts -NaN before
    // -Infinity but +NaN after +Infinity. One program gave four different
    // results (interp / JIT / AOT -O2 / AOT -O0) -- a sort with NaN at
    // both ends, and a two-NaN `Map` whose len was 1 or 2 by backend.
    //
    // Both producers -- `F64.from(x)` and the `F64 { value: x }` literal,
    // which now share one lowering -- canonicalize NaN at construction, so
    // `cmp`/`eq`/`hash`/`sort` inherit the invariant rather than each
    // having to normalize (a site that forgot would fail SILENTLY, with
    // equal keys hashing apart).
    //
    // B-2026-08-14-12: the `F32.from` line carries an explicit `as f32`.
    // `z` is `f64`, so `z / z` is a runtime f64 NaN and feeding it to an
    // f32 slot is a genuine narrowing -- the one site in the whole suite
    // where the float gate found a real one rather than a literal. The
    // cast preserves what the line always did (NaN narrows to NaN) and
    // states it.
    let out = run_program(
        "fn main() {\n\
                 let n = env.args().len();\n\
                 let z = (n as f64) - (n as f64);\n\
                 let c: F64 = F64 { value: 0.0 / 0.0 };\n\
                 let r: F64 = F64.from(z / z);\n\
                 let one: F64 = F64 { value: 1.0 };\n\
                 let inf: F64 = F64 { value: 1.0 / 0.0 };\n\
                 println(c == r);\n\
                 println(c < one);\n\
                 println(r < one);\n\
                 println(r > inf);\n\
                 let mut v: Vec[F64] = Vec.new();\n\
                 v.push(r);\n\
                 v.push(one);\n\
                 v.push(c);\n\
                 v.push(F64 { value: 0.0 - 1.0 });\n\
                 v.sort();\n\
                 println(v[0].value);\n\
                 println(v[1].value);\n\
                 println(v[3].value);\n\
                 let mut m: Map[F64, i64] = Map.new();\n\
                 let _ = m.insert(c, 1);\n\
                 let _ = m.insert(r, 2);\n\
                 println(m.len());\n\
                 match m.get(r) { Some(x) => println(x), None => println(0 - 1) }\n\
                 let n0: F64 = F64 { value: 0.0 * (0.0 - 1.0) };\n\
                 let p0: F64 = F64 { value: 0.0 };\n\
                 println(n0 < p0);\n\
                 println(n0 == p0);\n\
                 let c32: F32 = F32 { value: 0.0 / 0.0 };\n\
                 let r32: F32 = F32.from((z / z) as f32);\n\
                 println(c32 == r32);\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "true\nfalse\nfalse\ntrue\n-1\n1\nNaN\n1\n2\ntrue\nfalse\ntrue\n"
        );
    }
}

#[test]
fn test_e2e_total_order_f16_bf16_wrappers() {
    // The 16-bit siblings of the F32/F64 total-order wrappers
    // (`struct F16 { value: f16 }` / `struct Bf16 { value: bf16 }`,
    // `#[derive(Eq, Ord, Hash)]`). Same codegen path as F32/F64: the
    // struct is seeded (`{ half }` / `{ bfloat }`), and `<`/`>`/`==`
    // emit a TOTAL order on the 16-bit bit pattern (NaN last, -0 < +0,
    // bit-equality) — construction, `.value`, comparison, `Map` keys, and
    // `sort` all agree with `karac run`.
    let out = run_program(
            "fn main() {\n\
                 let a: F16 = F16 { value: 2.5 };\n\
                 let b: F16 = F16 { value: 1.5 };\n\
                 println(a > b);\n\
                 println(a < b);\n\
                 println(a == a);\n\
                 println(a == b);\n\
                 let mut m: Map[F16, i64] = Map.new();\n\
                 let _ = m.insert(F16 { value: 2.0 }, 20);\n\
                 let _ = m.insert(F16 { value: 3.0 }, 30);\n\
                 match m.get(F16 { value: 2.0 }) { Some(v) => println(v), None => println(0 - 1) }\n\
                 let mut v: Vec[F16] = Vec.new();\n\
                 v.push(F16 { value: 3.0 });\n\
                 v.push(F16 { value: 1.0 });\n\
                 v.push(F16 { value: 2.0 });\n\
                 v.sort();\n\
                 println(v[0].value);\n\
                 println(v[2].value);\n\
                 let c: Bf16 = Bf16 { value: 3.25 };\n\
                 let d: Bf16 = Bf16 { value: 1.25 };\n\
                 println(c > d);\n\
                 println(c == c);\n\
                 let neg0: Bf16 = Bf16 { value: 0.0 * (0.0 - 1.0) };\n\
                 let pos0: Bf16 = Bf16 { value: 0.0 };\n\
                 println(neg0 < pos0);\n\
                 println(neg0 == pos0);\n\
             }",
        );
    if let Some(out) = out {
        // a>b, a<b, a==a, a==b, map get, sort[0], sort[2],
        // c>d, c==c, -0<+0 (total order), -0==+0 (bit-eq).
        assert_eq!(
            out,
            "true\nfalse\ntrue\nfalse\n20\n1\n3\ntrue\ntrue\ntrue\nfalse\n"
        );
    }
}

#[test]
fn test_e2e_f16_widens_to_f32_in_mixed_add() {
    // `f16` widened to f32 (fpext) before the add — the module-verifier
    // regression this guards (was `fadd half, float`).
    //
    // B-2026-08-14-13: the widen is now spelled `as f32` in the source. The
    // bare `a + b` this test used to carry is rejected, because mixing
    // float widths in arithmetic let an operand's POSITION pick the result
    // type. The codegen path under test is unchanged and is still the only
    // one that can produce the bad IR — the cast is what feeds `fadd` its
    // widened operand — so the guard survives the language change intact;
    // only the spelling that reaches it moved.
    let out = run_program(
        "fn main() {\n\
                 let a: f16 = 2.0f16;\n\
                 let b: f32 = 3.0f32;\n\
                 println((a as f32) + b);\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5");
    }
}

#[test]
fn test_e2e_cpu_supports_intrinsic() {
    // Phase-11 `cpu.supports("...") -> bool` — the runtime CPU-feature probe
    // (design.md § Multiversioning; the `#[multiversion]` dispatch primitive),
    // lowered to a `karac_cpu_supports` runtime call. Assert only the
    // host-independent facts: an unknown feature is always `false`, and a real
    // feature name yields a usable bool (`a == a` is `true`). The exact
    // support of a real feature is host-dependent, so it is not asserted.
    if let Some(out) = run_program(
        r#"
fn main() {
    println(cpu.supports("not-a-real-feature-xyz"));
    let a = cpu.supports("avx2");
    println(a == a);
    let b = cpu.supports("neon");
    println(b or not b);
}
"#,
    ) {
        assert_eq!(out, "false\ntrue\ntrue\n");
    }
}

#[test]
fn test_e2e_interner_dedup_and_len() {
    // Phase-8 Interner codegen: interning equal strings returns the SAME
    // `Symbol` (integer equality), a distinct string mints a fresh one,
    // and `len()` counts distinct entries. Build==run parity with the
    // interpreter (`test_interner_dedups_equal_strings`).
    let out = run_program(
        r#"
fn main() {
    let mut tab: Interner = Interner.new();
    let a = tab.intern("hello");
    let b = tab.intern("hello");
    let c = tab.intern("world");
    println(a == b);
    println(a == c);
    println(tab.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "true\nfalse\n2");
    }
}

#[test]
fn test_e2e_interner_annotation_free_binding() {
    // The annotation-free form `let mut tab = Interner.new()` — the bind
    // site detects the `Interner.new()` RHS (no `: Interner` needed) and
    // registers dispatch + cleanup identically.
    let out = run_program(
        r#"
fn main() {
    let mut tab = Interner.new();
    let a = tab.intern("x");
    println(tab.resolve(a));
    println(tab.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "x\n1");
    }
}

#[test]
fn test_ir_chars_bailout_fails_closed_on_unduplicable_bodies() {
    // B-2026-07-28-2 fail-closed guard. The bailout emits the user body
    // TWICE, which is only safe when the body's codegen has no side effect
    // keyed by identity rather than position. Each of these must keep the
    // single-copy loop (`for.s.*`, no `for.sb.*`):
    //   - a NESTED LOOP, which is where auto-par / `par` reduction
    //     lowering mints spawn-site rows, and which would also make
    //     duplication compound to 2^depth through nested chars loops;
    //   - a CLOSURE, which emits a module-level function per occurrence;
    //   - a body over the node budget, so even an allowlisted body cannot
    //     silently double an unbounded amount of IR.
    let long_body = "out = out + 1i64; ".repeat(120);
    for (name, body) in [
        (
            "nested-loop",
            "let mut k = 0i64; while k < 3i64 { out = out + 1i64; k = k + 1i64; }".to_string(),
        ),
        (
            "closure",
            "let f = |x: i64| x + 1i64; out = f(out);".to_string(),
        ),
        ("over-budget", long_body),
    ] {
        let ir = ir_for(&format!(
            "fn walk(word: ref String) -> i64 {{\n\
                 \x20   let mut out = 0i64;\n\
                 \x20   for ch in word.chars() {{ {body} }}\n\
                 \x20   return out;\n\
                 }}\n\
                 fn main() {{ println(walk(\"ab\".to_string())); }}"
        ));
        let w = ir
            .split("define")
            .find(|f| f.contains("@walk("))
            .unwrap_or_else(|| panic!("@walk not found in IR for {}", name));
        assert!(
            !w.contains("for.sb.peek"),
            "{}: must fail CLOSED to the single-copy chars loop rather \
                 than duplicate this body; got:\n{}",
            name,
            w
        );
    }
}

#[test]
fn test_e2e_chars_bailout_matches_the_single_copy_loop() {
    // B-2026-07-28-2 behavioural oracle. The expectation below was
    // captured from the SINGLE-COPY lowering
    // (`KARAC_CHARS_ASCII_BAILOUT=0`), so this pins the dual-region shape
    // to pre-existing semantics rather than to itself.
    //
    // The failure mode being policed is a silent miscompile — a bailout
    // that hands off wrongly binds bytes as chars, or resumes at a stale
    // offset, with no crash and no diagnostic. So the cases are chosen to
    // make a wrong offset observable: MIXED strings (the fast region must
    // hand off mid-string and resume past the decode), `é`/`€`/`ß`/`𝄞`
    // covering 2-, 3- and 4-byte scalars, `continue` and `break` from BOTH
    // body copies, a labeled jump out of the loop from both copies, and an
    // index-sensitive `at()` whose output moves if any advance is wrong
    // (a byte-wise walk would put the `Q` in a different place).
    if let Some(out) = run_program(
        "fn echo(s: ref String) -> String {\n\
             \x20   let mut out: String = \"\";\n\
             \x20   for ch in s.chars() { out.push(ch); }\n\
             \x20   return out;\n\
             }\n\
             fn cps(s: ref String) -> i64 {\n\
             \x20   let mut a = 0i64;\n\
             \x20   for ch in s.chars() { a = a + (ch as i64); }\n\
             \x20   return a;\n\
             }\n\
             fn skip(s: ref String) -> i64 {\n\
             \x20   let mut a = 0i64;\n\
             \x20   for ch in s.chars() {\n\
             \x20       if ch == 'a' { continue; }\n\
             \x20       if ch == '\u{e9}' { continue; }\n\
             \x20       a = a + (ch as i64);\n\
             \x20   }\n\
             \x20   return a;\n\
             }\n\
             fn stop_at(s: ref String, k: char) -> i64 {\n\
             \x20   let mut a = 0i64;\n\
             \x20   for ch in s.chars() {\n\
             \x20       if ch == k { break; }\n\
             \x20       a = a + (ch as i64);\n\
             \x20   }\n\
             \x20   return a;\n\
             }\n\
             fn at(s: ref String, pos: i64) -> String {\n\
             \x20   let mut out: String = \"\";\n\
             \x20   let mut i = 0i64;\n\
             \x20   for ch in s.chars() {\n\
             \x20       if i == pos { out.push('Q'); } else { out.push(ch); }\n\
             \x20       i = i + 1i64;\n\
             \x20   }\n\
             \x20   return out;\n\
             }\n\
             fn outer_jump(s: ref String) -> i64 {\n\
             \x20   let mut a = 0i64;\n\
             \x20   let mut r = 0i64;\n\
             \x20   rounds: while r < 2i64 {\n\
             \x20       r = r + 1i64;\n\
             \x20       for ch in s.chars() {\n\
             \x20           if ch == 'z' { continue rounds; }\n\
             \x20           if ch == '\u{df}' { break rounds; }\n\
             \x20           a = a + (ch as i64);\n\
             \x20       }\n\
             \x20       a = a + 1000i64;\n\
             \x20   }\n\
             \x20   return a;\n\
             }\n\
             fn probe(s: ref String) {\n\
             \x20   println(f\"{echo(s)}|{cps(s)}|{skip(s)}|{stop_at(s, 'c')}|\
             {at(s, 2i64)}|{outer_jump(s)}\");\n\
             }\n\
             fn main() {\n\
             \x20   probe(\"\");\n\
             \x20   probe(\"a\");\n\
             \x20   probe(\"\u{e9}\");\n\
             \x20   probe(\"ab\u{e9}cd\");\n\
             \x20   probe(\"\u{e9}abcd\");\n\
             \x20   probe(\"abcd\u{e9}\");\n\
             \x20   probe(\"a\u{e9}\u{e9}b\");\n\
             \x20   probe(\"a\u{20ac}b\u{1d11e}c\u{df}d\");\n\
             \x20   probe(\"azb\u{df}c\");\n\
             }",
    ) {
        assert_eq!(
            out,
            // echo|cps|skip|stop_at('c')|at(2)|outer_jump
            "|0|0|0||2000\n\
                 a|97|0|97|a|2194\n\
                 \u{e9}|233|0|233|\u{e9}|2466\n\
                 ab\u{e9}cd|627|297|428|abQcd|3254\n\
                 \u{e9}abcd|627|297|428|\u{e9}aQcd|3254\n\
                 abcd\u{e9}|627|297|195|abQd\u{e9}|3254\n\
                 a\u{e9}\u{e9}b|661|98|661|a\u{e9}Qb|3322\n\
                 a\u{20ac}b\u{1d11e}c\u{df}d|128051|127954|127629|\
                 a\u{20ac}Q\u{1d11e}c\u{df}d|127728\n\
                 azb\u{df}c|639|542|540|azQ\u{df}c|194\n"
        );
    }
}

/// The self-hosting-relevant `char*` walk: a `mut` pointer reassigned via
/// `p = p.offset(1)` each iteration (un-annotated), reading each byte. This is
/// the exact shape a strlen-style / buffer-scan loop needs, and it only works
/// once `p.offset(..)` returns a proper `*const u8` (else the reassignment
/// re-types `p` to `Error` and the next `p.read()` fails).
#[test]
fn raw_pointer_buffer_walk_loop() {
    let src = "fn main() {\n\
                   \x20   let s = c\"ABCD\";\n\
                   \x20   let mut p = s.as_ptr();\n\
                   \x20   let mut i = 0i64;\n\
                   \x20   let mut sum = 0i64;\n\
                   \x20   unsafe {\n\
                   \x20       while i < 4i64 {\n\
                   \x20           sum = sum + (p.read() as i64);\n\
                   \x20           p = p.offset(1i64);\n\
                   \x20           i = i + 1i64;\n\
                   \x20       }\n\
                   \x20   }\n\
                   \x20   println(sum);\n\
                   }\n";
    if let Some(out) = run_program(src) {
        assert_eq!(out.trim(), "266", "65+66+67+68 = 266 (ABCD)");
    }
}

/// `p.is_null()` method-form (design.md § raw pointers) — the safe null-bits
/// check. Live pointer → false, `ptr.null()` → true. No `unsafe { }` needed.
#[test]
fn raw_pointer_is_null_method() {
    let src = "fn main() {\n\
                   \x20   let s = c\"AB\";\n\
                   \x20   let p = s.as_ptr();\n\
                   \x20   let n: *const u8 = ptr.null();\n\
                   \x20   if p.is_null() { println(\"p-null\"); } else { println(\"p-live\"); }\n\
                   \x20   if n.is_null() { println(\"n-null\"); } else { println(\"n-live\"); }\n\
                   }\n";
    if let Some(out) = run_program(src) {
        assert_eq!(out, "p-live\nn-null\n");
    }
}

/// B-2026-07-11-40: a TURBOFISH-inferred raw-pointer binding
/// (`let p = ptr.null[u8]()`) — no annotation — must lower to a real `ptr`
/// and register the pointee so a later pointer method compiles. Before the
/// fix the turbofish `ptr.null[u8]()` parsed as an `Index`-callee call that
/// neither the typechecker's explicit-generic-args route nor codegen's
/// pointer-intrinsic route recognized: `T` stayed unresolved (binding →
/// `Type::Error`, no recorded pointee) and codegen fell through / stored the
/// value as `i64`, so `p.read()`/`p.is_null()` failed with "no handler for
/// method" or an `expected PointerValue` panic. Uses the SAFE `is_null()`
/// (no deref — `read()` on these constructed pointers would be UB); a null
/// pointer is null, a dangling one is not.
#[test]
fn raw_pointer_turbofish_binding_is_null() {
    let src = "fn main() {\n\
                   \x20   let p = ptr.null[u8]();\n\
                   \x20   let d = ptr.dangling[i64]();\n\
                   \x20   unsafe {\n\
                   \x20       if p.is_null() { println(\"p-null\"); } else { println(\"p-live\"); }\n\
                   \x20       if d.is_null() { println(\"d-null\"); } else { println(\"d-live\"); }\n\
                   \x20   }\n\
                   }\n";
    if let Some(out) = run_program(src) {
        assert_eq!(out, "p-null\nd-live\n");
    }
}

#[test]
fn e2e_user_fn_named_like_compiler_builtin_is_typechecked() {
    // B-2026-07-23-5: a USER function whose NAME collides with a stdlib
    // `#[compiler_builtin]` (`mem::swap`) was silently SKIPPED by the
    // typechecker (`env.compiler_builtins.contains(name)` — a name-keyed set
    // populated across user+stdlib). Its body's `expr_types` were never
    // recorded, so codegen's per-literal instantiation record was empty and
    // a generic `swap` returning a PERMUTED struct (`Pair[A,B] -> Pair[B,A]`)
    // built the returned literal with the INPUT layout, failing LLVM module
    // verification (A and B different sizes). Now the skip gates on the
    // function's OWN `#[compiler_builtin]` attribute, so the user `swap` is
    // type-checked and its literal instantiation recorded. Verifies interp
    // == JIT == AOT.
    if let Some(out) = run_program(
        "struct Pair[A, B] { first: A, second: B }\n\
             fn swap[A, B](p: Pair[A, B]) -> Pair[B, A] {\n\
             \x20   Pair { first: p.second, second: p.first }\n\
             }\n\
             fn main() {\n\
             \x20   let q = swap(Pair { first: 42, second: \"x\".to_string() });\n\
             \x20   println(q.first);\n\
             \x20   println(f\"{q.second}\");\n\
             \x20   let r = swap(Pair { first: true, second: 99 });\n\
             \x20   println(f\"{r.first}\");\n\
             \x20   println(f\"{r.second}\");\n\
             }",
    ) {
        assert_eq!(out, "x\n42\n99\ntrue\n");
    }
}

/// `#[track_caller]` slice 6: the STDLIB panic-emitters honour the caller
/// redirection. Each emitter (`unwrap`, `[i]` bounds check, `/`-by-zero,
/// `assert`) is a compiler intrinsic lowered *inline*, so when it fires
/// inside a `#[track_caller]` fn `emit_panic` (and the assert record path)
/// reports the caller's site, not the emitter's own line. The redirected
/// location is a runtime value forwarded from the call site, so it survives
/// the string-source harness (which does not thread `source_filename`) even
/// though the compile-time-span path does not — same property the slice-5
/// test relies on. The emitter-line marker must be ABSENT to prove the
/// panic was redirected rather than reported in place.
#[test]
fn e2e_track_caller_redirects_stdlib_emitters() {
    // unwrap() on None inside a `#[track_caller]` fn: unwrap is on line 6,
    // main's call is on line 10 → report 10, frame `force`.
    let unwrap = "fn none_val() -> Option[i32] {\n\
                      None\n\
                      }\n\
                      #[track_caller]\n\
                      fn force(o: Option[i32]) -> i32 {\n\
                      o.unwrap()\n\
                      }\n\
                      fn main() {\n\
                      let n = none_val();\n\
                      force(n);\n\
                      }\n";
    if let Some(cap) = run_program_capturing(unwrap) {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        assert!(
            cap.stderr.contains(":10:")
                && cap.stderr.contains("in force")
                && !cap.stderr.contains(":6:"),
            "unwrap must report the caller's line (10), not its own (6); stdout={:?}",
            cap.stdout
        );
    }

    // Vec index out of bounds inside a `#[track_caller]` fn: `v[i]` is on
    // line 3, the call is on line 8 → report 8, frame `at`.
    let index = "#[track_caller]\n\
                     fn at(v: Vec[i64], i: i64) -> i64 {\n\
                     v[i]\n\
                     }\n\
                     fn main() {\n\
                     let mut v: Vec[i64] = Vec.new();\n\
                     v.push(1i64);\n\
                     at(v, 99i64);\n\
                     }\n";
    if let Some(cap) = run_program_capturing(index) {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        assert!(
            cap.stderr.contains(":8:")
                && cap.stderr.contains("in at")
                && !cap.stderr.contains(":3:"),
            "index-OOB must report the caller's line (8), not its own (3); stderr={:?}",
            cap.stderr
        );
    }

    // Division by zero inside a `#[track_caller]` fn: `a / b` is on line 3,
    // the call is on line 7 → report 7, frame `divide`. The divisor comes
    // from a fn return so the guard is not const-folded away.
    let div = "#[track_caller]\n\
                   fn divide(a: i64, b: i64) -> i64 {\n\
                   a / b\n\
                   }\n\
                   fn main() {\n\
                   let z = zero();\n\
                   divide(10i64, z);\n\
                   }\n\
                   fn zero() -> i64 {\n\
                   0i64\n\
                   }\n";
    if let Some(cap) = run_program_capturing(div) {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        assert!(
            cap.stderr.contains(":7:")
                && cap.stderr.contains("in divide")
                && !cap.stderr.contains(":3:"),
            "div-by-zero must report the caller's line (7), not its own (3); stderr={:?}",
            cap.stderr
        );
    }

    // assert() inside a `#[track_caller]` fn goes through the test-record
    // path (`KARAC_TEST_FAILURE` JSON on stderr), which slice 6 also taught
    // to redirect: assert is on line 3, the call is on line 7 → report 7.
    let assert = "#[track_caller]\n\
                      fn check(x: i64) {\n\
                      assert(x > 0);\n\
                      }\n\
                      fn main() {\n\
                      let n = neg();\n\
                      check(n);\n\
                      }\n\
                      fn neg() -> i64 {\n\
                      -5i64\n\
                      }\n";
    if let Some(cap) = run_program_capturing(assert) {
        assert_eq!(cap.status.code(), Some(1), "stdout={:?}", cap.stdout);
        assert!(
            cap.stderr.contains("\"line\":7") && !cap.stderr.contains("\"line\":3"),
            "assert must report the caller's line (7), not its own (3); stderr={:?}",
            cap.stderr
        );
    }

    // Baseline contrast: the SAME assert with NO `#[track_caller]` reports
    // its own line (2 here), proving the redirect is opt-in via the
    // attribute, not unconditional.
    let assert_base = "fn check(x: i64) {\n\
                           assert(x > 0);\n\
                           }\n\
                           fn main() {\n\
                           let n = neg();\n\
                           check(n);\n\
                           }\n\
                           fn neg() -> i64 {\n\
                           -5i64\n\
                           }\n";
    if let Some(cap) = run_program_capturing(assert_base) {
        assert_eq!(cap.status.code(), Some(1), "stdout={:?}", cap.stdout);
        assert!(
            cap.stderr.contains("\"line\":2") && !cap.stderr.contains("\"line\":6"),
            "baseline assert must report its own line (2), not the caller's (6); stderr={:?}",
            cap.stderr
        );
    }
}

/// Bit intrinsics codegen (`llvm.ctpop` / `llvm.ctlz` / `llvm.cttz`),
/// width-correct: narrow receivers are masked to their declared width, so a
/// signed `i8 -1` has 8 set bits and a zero `u8` has 8 leading/trailing
/// zeros — not the i64-widened 64. Mirrors the interpreter
/// (`tests/interpreter.rs::test_bit_intrinsics_width_correct`).
#[test]
fn e2e_bit_intrinsics_width_correct() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let b: u8 = 200;\n\
                 println(b.count_ones());\n\
                 let n: u64 = 1024;\n\
                 println(n.leading_zeros());\n\
                 println(n.trailing_zeros());\n\
                 let z: u8 = 0;\n\
                 println(z.leading_zeros());\n\
                 println(z.trailing_zeros());\n\
                 let neg: i8 = -1;\n\
                 println(neg.count_ones());\n\
                 let m: i32 = 1;\n\
                 println(m.leading_zeros());\n\
             }",
    ) {
        assert_eq!(out, "3\n53\n10\n8\n8\n8\n31\n");
    }
}

/// `count_zeros` / `reverse_bits` / `swap_bytes` lowered on the receiver's
/// declared iN width (`llvm.ctpop`-complement / `llvm.bitreverse` /
/// `llvm.bswap`, with `swap_bytes` identity on 8-bit). Must match the
/// interpreter oracle (`test_bit_permute_and_count_zeros_width_correct`),
/// including signed-narrow sign extension (`1i8.reverse_bits() == -128`).
#[test]
fn e2e_bit_permute_and_count_zeros_width_correct() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println((200u8).count_zeros());\n\
                 println((255u8).count_zeros());\n\
                 println((0i64).count_zeros());\n\
                 println((1u8).reverse_bits());\n\
                 println((258u16).swap_bytes());\n\
                 println((1u32).swap_bytes());\n\
                 println((5u8).swap_bytes());\n\
                 println((4278255360u32).reverse_bits());\n\
                 println((1i8).reverse_bits());\n\
                 println((1i32).reverse_bits());\n\
                 let big: u64 = 1;\n\
                 println(big.reverse_bits());\n\
             }",
    ) {
        // Last line pins the u64-high-bit print: `1u64.reverse_bits()` is
        // 2^63, which must render unsigned (the `expr_is_unsigned_int`
        // builtin-Self-method arm) — pre-fix it printed the signed
        // -9223372036854775808.
        assert_eq!(
            out,
            "5\n0\n64\n128\n513\n16777216\n5\n16711935\n-128\n-2147483648\n9223372036854775808\n"
        );
    }
}

/// `rotate_left(n)` / `rotate_right(n)` lowered to `llvm.fshl` / `llvm.fshr`
/// on the receiver's iN. Must match the interpreter oracle
/// (`test_bit_rotate_width_correct`), incl. the signed-narrow sign
/// extension and the u64-high-bit unsigned print.
#[test]
fn e2e_bit_rotate_width_correct() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println((1u8).rotate_left(1));\n\
                 println((128u8).rotate_left(1));\n\
                 println((1u8).rotate_right(1));\n\
                 println((16u32).rotate_right(4));\n\
                 println((5u8).rotate_left(8));\n\
                 println((1i8).rotate_right(1));\n\
                 let big: u64 = 1;\n\
                 println(big.rotate_left(63));\n\
             }",
    ) {
        assert_eq!(out, "2\n1\n128\n1\n5\n-128\n9223372036854775808\n");
    }
}

/// `uN::is_power_of_two` -> bool lowered to the inline
/// `(x != 0) & ((x & (x-1)) == 0)` on the receiver's iN. Must match the
/// interpreter oracle (`test_is_power_of_two_unsigned`), including the 2^63
/// `u64` case (built via `rotate_left(63)` so no >i64 literal is needed) and
/// the narrow u8/u16 widths.
#[test]
fn e2e_is_power_of_two_unsigned() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println((1u32).is_power_of_two());\n\
                 println((2u32).is_power_of_two());\n\
                 println((3u32).is_power_of_two());\n\
                 println((0u32).is_power_of_two());\n\
                 let big: u64 = 1;\n\
                 println(big.rotate_left(63).is_power_of_two());\n\
                 println((128u8).is_power_of_two());\n\
                 println((129u8).is_power_of_two());\n\
                 println((1024u16).is_power_of_two());\n\
             }",
    ) {
        assert_eq!(out, "true\ntrue\nfalse\nfalse\ntrue\ntrue\nfalse\ntrue\n");
    }
}

/// `iN/uN::abs_diff(other) -> uN` lowered to `select(a≥b, a-b, b-a)` on the
/// receiver's iN then zero-extended. Must match the interpreter oracle
/// (`test_abs_diff_unsigned_result`), including i8 MIN/MAX → 255 and the
/// near-full-i64-range diff that exceeds i64::MAX and MUST print unsigned.
#[test]
fn e2e_abs_diff_unsigned_result() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let a: i32 = 5;\n\
                 let b: i32 = 3;\n\
                 println(a.abs_diff(b));\n\
                 println(b.abs_diff(a));\n\
                 let n: i32 = -5;\n\
                 println(n.abs_diff(b));\n\
                 let u: u32 = 3;\n\
                 let v: u32 = 10;\n\
                 println(u.abs_diff(v));\n\
                 let x: u8 = 200;\n\
                 let y: u8 = 10;\n\
                 println(x.abs_diff(y));\n\
                 let big: i64 = -9223372036854775807;\n\
                 let top: i64 = 9223372036854775807;\n\
                 println(big.abs_diff(top));\n\
                 let s1: i8 = -128;\n\
                 let s2: i8 = 127;\n\
                 println(s1.abs_diff(s2));\n\
             }",
    ) {
        assert_eq!(out, "2\n2\n8\n7\n190\n18446744073709551614\n255\n");
    }
}

/// `uN::next_power_of_two` lowered via `llvm.ctlz` + shift. Must match the
/// interpreter oracle (`test_next_power_of_two_unsigned`), including the u64
/// 2^63 case that MUST print unsigned.
#[test]
fn e2e_next_power_of_two_unsigned() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println((0u32).next_power_of_two());\n\
                 println((1u32).next_power_of_two());\n\
                 println((2u32).next_power_of_two());\n\
                 println((3u32).next_power_of_two());\n\
                 println((5u32).next_power_of_two());\n\
                 println((100u32).next_power_of_two());\n\
                 println((128u8).next_power_of_two());\n\
                 let h: u32 = 1u32 << 31;\n\
                 println(h.next_power_of_two());\n\
                 let i: u64 = 1u64 << 63;\n\
                 println(i.next_power_of_two());\n\
                 let j: u64 = (1u64 << 63) - 1;\n\
                 println(j.next_power_of_two());\n\
             }",
    ) {
        assert_eq!(
            out,
            "1\n1\n2\n4\n8\n128\n128\n2147483648\n9223372036854775808\n9223372036854775808\n"
        );
    }
}

#[test]
fn e2e_128bit_integers_end_to_end() {
    // 128-bit works from source (B-2026-08-19-8 stage 5) — the type is no
    // longer rejected, so this is the first test that can exercise it
    // through the ordinary pipeline rather than a probe build.
    //
    // Every value here is chosen so a 64-bit answer differs: 2^100 cannot
    // be represented at all, and its LOW WORD IS ZERO, which is what made
    // the pre-stage-4 truncation print `0` rather than something obviously
    // wrong. `count_ones` on 2^100 is 1 and on `-1i128` is 128 — the figure
    // from B-2026-08-19-6's original report, which returned 64.
    if let Some(run) = run_program_capturing(
        "fn main() {\n\
             let a: i128 = 1267650600228229401496703205376i128;\n\
             println(a);\n\
             println(a * 2i128);\n\
             println(a / 7i128);\n\
             println(a.wrapping_add(1i128));\n\
             println(a.saturating_add(1i128));\n\
             println(a.count_ones());\n\
             let n: i128 = 0i128 - 1i128;\n\
             println(n.count_ones());\n\
             let p: i128 = 2i128;\n\
             println(p.pow(100u32));\n\
             println(f\"{a}\");\n\
             println(a.to_string());\n\
             let mx: i128 = 170141183460469231731687303715884105727i128;\n\
             println(mx);\n\
             println(a.swap_bytes().swap_bytes());\n\
             let u: u128 = 18446744073709551616u128;\n\
             println(u);\n\
             }",
    ) {
        // B-2026-08-20-5: the macOS arm64 lane fails here with an EMPTY
        // stdout — the binary links and runs but dies before the first
        // `println`. `run_program` discards stderr and the exit status,
        // so the failure reads as a bare `left: ""` and says nothing about
        // whether the process faulted or exited cleanly. Capture both into
        // the message so one CI run diagnoses it instead of confirming it.
        assert_eq!(
            run.stdout,
            "1267650600228229401496703205376\n\
                 2535301200456458802993406410752\n\
                 181092942889747057356671886482\n\
                 1267650600228229401496703205377\n\
                 1267650600228229401496703205377\n\
                 1\n\
                 128\n\
                 1267650600228229401496703205376\n\
                 1267650600228229401496703205376\n\
                 1267650600228229401496703205376\n\
                 170141183460469231731687303715884105727\n\
                 1267650600228229401496703205376\n\
                 18446744073709551616\n",
            "128-bit end-to-end: exit status {:?}, stderr {:?}",
            run.status,
            run.stderr
        );
    }
}

#[test]
fn test_e2e_partition_scalar() {
    // B-2026-07-19-14: `partition(pred: Fn(T) -> bool) -> (Vec[T], Vec[T])`
    // — the eager two-collection terminal. Desugars to two `Vec[T]` push
    // accumulators over the fused for-loop, returning a `(__pt, __pf)` tuple
    // (the verified block-returns-tuple-of-owned-Vecs path). Covers a scalar
    // split, composition with a leading `map`, a float split, and an empty
    // (all-to-one-side) partition. Must match the interpreter. (Heap elements:
    // `test_e2e_partition_heap_elements`.)
    if let Some(out) = run_program(
            "fn main() {\n\
                 let v = [1, 2, 3, 4, 5, 6];\n\
                 let (evens, odds): (Vec[i64], Vec[i64]) = v.iter().partition(|x| x % 2 == 0);\n\
                 println(f\"{evens.len()}:{odds.len()}\");\n\
                 let mut es = 0;\n\
                 for e in evens { es = es + e; }\n\
                 let mut os = 0;\n\
                 for o in odds { os = os + o; }\n\
                 println(f\"{es}:{os}\");\n\
                 let (big, small): (Vec[i64], Vec[i64]) = v.iter().map(|x| x * 2).partition(|y| y > 6);\n\
                 let mut bs = 0;\n\
                 for b in big { bs = bs + b; }\n\
                 let mut ss = 0;\n\
                 for s in small { ss = ss + s; }\n\
                 println(f\"{bs}:{ss}\");\n\
                 let f: Vec[f64] = [1.5, 2.5, 3.5];\n\
                 let (hi, lo): (Vec[f64], Vec[f64]) = f.iter().partition(|x| x > 2.0);\n\
                 println(f\"{hi.len()}:{lo.len()}\");\n\
                 let (none_big, all_small): (Vec[i64], Vec[i64]) = v.iter().partition(|x| x > 100);\n\
                 println(f\"{none_big.len()}:{all_small.len()}\");\n\
             }",
        ) {
            assert_eq!(out, "3:3\n12:9\n30:12\n2:1\n0:6\n");
        }
}

/// B-2026-08-14-14 — the compiled twin of
/// `tests/interpreter.rs::test_element_wise_scalar_as_cast_agrees_across_backends`,
/// same source and same expected string.
///
/// The first line is the row's repro made explicit. Written as
/// `tu + x` with `x: i64 = 300`, it typechecked, TRAPPED under `--interp`
/// with "runtime error: integer overflow", and printed 45 from the binary —
/// three different behaviours from one accepted program, and the compiled
/// one a silent two's-complement wrap that design.md's trapping default
/// exists to prevent. With the `as u8` written the truncation is the
/// author's, both backends do it, and 45 is the honest answer.
///
/// The last line pins the constant-expression promotion: `-1.0` is a unary
/// minus on a literal, so it takes the element type and needs no cast. Two
/// `std.autograd` lines are written that way.
#[test]
fn test_e2e_element_wise_scalar_as_cast_agrees_across_backends() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let tu: Tensor[u8, [2]] = Tensor.from([1, 2]);\n\
                     let x: i64 = 300;\n\
                     let r = tu + (x as u8);\n\
                     println(r[0]);\n\
                     let t32: Tensor[f32, [2]] = Tensor.from([1.0, 2.0]);\n\
                     let d: f64 = 0.1;\n\
                     let s = t32 * (d as f32);\n\
                     println(s[0]);\n\
                     let n = t32 * -1.0;\n\
                     println(n[0]);\n\
                 }"
        )
        .as_deref(),
        Some("45\n0.10000000149011612\n-1\n"),
    );
}

#[test]
fn test_e2e_bytecode_vm_example() {
    // examples/vm.kara — a stack-based bytecode VM. Validates the enum +
    // `match` dispatch HOT PATH (every instruction is an `Op` variant decoded
    // in the fetch-execute loop), `Vec[Op]` indexing (`program[pc]`),
    // `Vec[i64]` stack push/pop via `Option`, indexed-local store, and
    // Call/Ret control flow, all through codegen. Programs compute
    // (2+3)*4=20, sum(1..=5)=15, 5!=120, and a ×2 subroutine on 21=42.
    if let Some(out) = run_program(include_str!("../../examples/vm.kara")) {
        assert_eq!(out, "20\n15\n120\n42\n");
    }
}

#[test]
fn test_e2e_pipeline_example() {
    // examples/pipeline.kara — a functional log-analytics pipeline. Validates
    // the sequential ITERATOR + CLOSURE surface: `iter()` chains of `map` /
    // `filter` closures terminating in `fold` (aggregate) and `collect`
    // (materialize), over heap-bearing (`String`-field) records. The `fold`
    // terminal on a fused chain (B-2026-07-11-17) is exercised as counts
    // (`fold(0, |acc, r| acc + 1)`), sums (`fold(0, |acc, x| acc + x)`), and a
    // max (`fold` with an `if` body), across bare / filtered / mapped chains.
    if let Some(out) = run_program(include_str!("../../examples/pipeline.kara")) {
        assert_eq!(
            out,
            "requests: 10\nok: 7\nserver_err: 2\nclient_err: 1\n\
                 bytes_served: 18432\nmax_latency_ms: 210\navg_ok_latency_ms: 50\n\
                 slow_paths: 3\n  /api/orders\n  /api/users\n  /assets/app\n"
        );
    }
}

#[test]
fn test_e2e_sha256_example() {
    // examples/sha256.kara — SHA-256 + HMAC-SHA256 in pure Kāra, checked
    // against the published NIST FIPS 180-4 and RFC 4231 vectors. This is
    // the strongest oracle in the example set: the digests are bit-exact
    // published constants, so any divergence in 32-bit modular addition,
    // rotate/shift mixing, big-endian packing, or the 64-round state
    // rotation shows up as a mismatched hash rather than a plausible wrong
    // answer. Covers the empty-input padding path, a single block, the
    // two-block 56-byte case where the length field no longer fits, a
    // 16-block message, and HMAC's short-key zero-pad and
    // longer-than-block key-is-hashed-first paths.
    //
    // Words are carried in u64 with an explicit 32-bit mask after every
    // add, because `wrapping_add` is deferred on narrow widths — so this
    // also pins that the u64 bit surface (`&` `|` `^` `<<` `>>`, hex
    // literals, `as u8` truncation) agrees between codegen and the
    // interpreter across 64 rounds per block.
    if let Some(out) = run_program(include_str!("../../examples/sha256.kara")) {
        assert_eq!(out, include_str!("../../examples/sha256.expected"));
    }
}

#[test]
fn test_e2e_sigv4_example() {
    // examples/sigv4.kara — AWS Signature Version 4 request signing, built
    // on sha256.kara's primitives. Two vectors: AWS's own documented
    // `get-vanilla` case (the AKIDEXAMPLE credentials and 20150830T123600Z
    // timestamp), and a second constructed to exercise what the first
    // cannot — query parameters supplied out of order, values needing
    // percent-encoding including a `/` that must encode in a VALUE but not
    // in the PATH, headers in mixed case and out of order, and a header
    // value with runs of internal whitespace the spec collapses.
    //
    // The expected values were cross-checked against an independent Python
    // stdlib implementation (hashlib + hmac) rather than transcribed, so a
    // divergence here means the two implementations disagree on a byte-exact
    // signature — not that a constant was copied wrong.
    //
    // This example found B-2026-08-01-24: moving elements out of a by-value
    // `Vec[S]` parameter (what `sort_headers` does) double-freed each
    // element's heap field, aborting the compiled binary while the
    // interpreter was correct. Keeping the run-vs-build equality asserted
    // here is what would catch its return.
    if let Some(out) = run_program(include_str!("../../examples/sigv4.kara")) {
        assert_eq!(out, include_str!("../../examples/sigv4.expected"));
    }
}

#[test]
fn test_ir_bf16_widen_to_f64_routes_through_f32() {
    // B-2026-07-22-1 hardening: LLVM 18's AArch64 backend cannot select
    // standalone `fpext`/`fptrunc` nodes touching bfloat (verified with
    // llc-18 on both `apple-m1` and `generic` v8a: bf16 → f32,
    // bf16 → f64, and f32 → bf16 all die at ISel with "LLVM ERROR:
    // Cannot select", even load-fed), and Darwin's homebrew LLVM 18
    // additionally fails to select `fadd bfloat` in common DAG shapes
    // (the macOS LLJIT parity lane runs ISel on raw un-optimized IR —
    // its jit-runner child dies with SIGABRT "Cannot select: bf16 =
    // fadd"). So NO bf16 conversion or arithmetic node may survive
    // codegen: conversions emit as integer-level sequences (zext+shl
    // widening / RNE-bias narrowing — `build_float_cast_bf16_safe`),
    // arithmetic computes in f32 and rounds back once, negation is an
    // integer sign-bit flip. Pins every emission surface on bf16 values
    // that arrive through a fn boundary so no fold can hide a node.
    let src = r#"
fn show(x: bf16) -> f64 {
    println(x);
    let y = x + 0.5bf16;
    let n = -y;
    if n < 0.0bf16 {
        println(y * 2.0bf16);
    }
    println(n);
    x as f64
}

fn main() {
    println(show(1.25bf16));
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ir = compile_to_ir(&parsed.program, None, None).expect("codegen failed");
    for line in ir.lines() {
        assert!(
            !line.contains("fpext bfloat"),
            "standalone `fpext bfloat` emitted (unselectable on LLVM 18 \
                 aarch64): {}",
            line
        );
        assert!(
            !(line.contains("fptrunc") && line.contains("bfloat")),
            "standalone `fptrunc … bfloat` emitted (unselectable on \
                 LLVM 18 aarch64): {}",
            line
        );
        // Match the OPCODE position (`= fadd bfloat …`), not a bare
        // substring — our own replacement sequences carry names like
        // `%fneg.bf.bits = bitcast bfloat …` that would false-positive.
        for node in ["fadd", "fsub", "fmul", "fdiv", "frem", "fneg"] {
            assert!(
                !(line.contains(&format!("= {} bfloat", node))),
                "bf16 `{}` emitted (ISel-fragile on LLVM 18 aarch64 — \
                     must compute in f32): {}",
                node,
                line
            );
        }
        assert!(
            !(line.contains("= fcmp") && line.contains(" bfloat ")),
            "bf16 `fcmp` emitted (ISel-fragile on LLVM 18 aarch64 — \
                 must compare in f32): {}",
            line
        );
    }
    let widen_seqs = ir.lines().filter(|l| l.contains(".bf2f.shl")).count();
    assert!(
        widen_seqs >= 2,
        "expected the print path AND the `as f64` cast to widen bf16 \
             via the integer zext+shl sequence (≥2 `.bf2f.shl` legs); \
             found {} in:\n{}",
        widen_seqs,
        ir
    );
}

#[test]
fn test_e2e_reduced_precision_receiver_multiplies_its_constant_at_f32() {
    // B-2026-08-30-5 — `to_degrees` / `to_radians` on an `f16` receiver
    // disagreed with the interpreter on ~25% of inputs. Codegen handles
    // `recip` / `to_degrees` / `to_radians` / `fract` in one block that
    // opened by widening bf16 ONLY (bf16 arithmetic is unselectable off
    // x86, B-2026-08-29-34); f16 selects natively and so was left alone,
    // which meant `fty.const_float(57.29577951308232)` rounded 180/pi
    // INTO f16 before the multiply. f16's ULP near 57.3 is 0.03125, so
    // the constant arrived carrying 2.5e-4 of relative error against the
    // ~1e-8 the interpreter multiplies by, and no intermediate width
    // recovers a multiplicand that was already wrong.
    //
    // The ten f16 receivers below are not a sample of convenience: each
    // was chosen because "constant rounded into f16, multiply at f16" and
    // "constant at f32, multiply at f32, round once" give DIFFERENT f16
    // results for BOTH methods, so every one of those twenty values fails
    // if the widen ever stops covering f16. The remaining rows are
    // regression guards — `recip` and `fract` were always correct at f16
    // (1.0 is exact at every width, and `x - trunc(x)` is exact), and
    // bf16 / f32 / f64 were correct throughout, so they must stay so.
    //
    // Bit-exact string pins are portable HERE, unlike the libm
    // transcendentals `approx_line` exists for: an fmul by a constant, an
    // fdiv by 1.0 and `x - trunc(x)` are IEEE-exact operations, identical
    // on every platform.
    let src = r#"
fn main() {
    let a: f16 = 0.0304718017578125f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = -0.0304718017578125f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = 0.060943603515625f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = -0.060943603515625f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = -3.900390625f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = 7.80078125f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = -7.80078125f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = -249.625f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = 499.25f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = -499.25f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let b: f16 = 1.25f16;
    println(f"f16  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f16 = -3.5f16;
    println(f"f16  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f16 = 0.375f16;
    println(f"f16  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f16 = -17.75f16;
    println(f"f16  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: bf16 = 1.25bf16;
    println(f"bf16 {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: bf16 = -3.5bf16;
    println(f"bf16 {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: bf16 = 0.375bf16;
    println(f"bf16 {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: bf16 = -17.75bf16;
    println(f"bf16 {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f32 = 1.25f32;
    println(f"f32  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f32 = -3.5f32;
    println(f"f32  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f32 = 0.375f32;
    println(f"f32  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f32 = -17.75f32;
    println(f"f32  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f64 = 1.25f64;
    println(f"f64  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f64 = -3.5f64;
    println(f"f64  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f64 = 0.375f64;
    println(f"f64  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f64 = -17.75f64;
    println(f"f64  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
}
"#;
    let want = concat!(
        "f16  1.74609375 0.0005316734313964844 32.8125 0.0304718017578125\n",
        "f16  -1.74609375 -0.0005316734313964844 -32.8125 -0.0304718017578125\n",
        "f16  3.4921875 0.0010633468627929688 16.40625 0.060943603515625\n",
        "f16  -3.4921875 -0.0010633468627929688 -16.40625 -0.060943603515625\n",
        "f16  -223.5 -0.06805419921875 -0.25634765625 -0.900390625\n",
        "f16  447 0.1361083984375 0.128173828125 0.80078125\n",
        "f16  -447 -0.1361083984375 -0.128173828125 -0.80078125\n",
        "f16  -14304 -4.35546875 -0.00400543212890625 -0.625\n",
        "f16  28608 8.7109375 0.002002716064453125 0.25\n",
        "f16  -28608 -8.7109375 -0.002002716064453125 -0.25\n",
        "f16  71.625 0.021820068359375 0.7998046875 0.25\n",
        "f16  -200.5 -0.06109619140625 -0.28564453125 -0.5\n",
        "f16  21.484375 0.0065460205078125 2.666015625 0.375\n",
        "f16  -1017 -0.309814453125 -0.05633544921875 -0.75\n",
        "bf16 71.5 0.0218505859375 0.80078125 0.25\n",
        "bf16 -201 -0.06103515625 -0.28515625 -0.5\n",
        "bf16 21.5 0.00653076171875 2.671875 0.375\n",
        "bf16 -1016 -0.310546875 -0.056396484375 -0.75\n",
        "f32  71.6197280883789 0.021816615015268326 0.800000011920929 0.25\n",
        "f32  -200.5352325439453 -0.06108652427792549 -0.2857142984867096 -0.5\n",
        "f32  21.485918045043945 0.006544984877109528 2.6666667461395264 0.375\n",
        "f32  -1017.0001220703125 -0.30979594588279724 -0.056338027119636536 -0.75\n",
        "f64  71.6197243913529 0.02181661564992912 0.8 0.25\n",
        "f64  -200.53522829578813 -0.061086523819801536 -0.2857142857142857 -0.5\n",
        "f64  21.48591731740587 0.006544984694978736 2.6666666666666665 0.375\n",
        "f64  -1017.0000863572112 -0.3097959422289935 -0.056338028169014086 -0.75\n",
    );
    assert_eq!(run_program(src), Some(want.to_string()));
}

#[test]
fn e2e_alloc_error_codegen() {
    // The `AllocError` prelude type (struct + unit variant) compiles: both
    // variants construct, `==` compares payload + tag, `match` binds the
    // struct-variant field, and it flows through `Result[i64, AllocError]`.
    if let Some(out) = run_program(
        "fn make(fail: bool) -> Result[i64, AllocError] {\n\
                 if fail { Err(AllocError.OutOfMemory { requested_bytes: 2048 }) } else { Ok(7) }\n\
             }\n\
             fn classify(e: AllocError) -> i64 {\n\
                 match e {\n\
                     AllocError.OutOfMemory { requested_bytes } => requested_bytes as i64,\n\
                     AllocError.CapacityOverflow => -1,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let a = AllocError.OutOfMemory { requested_bytes: 2048 };\n\
                 let a2 = AllocError.OutOfMemory { requested_bytes: 2048 };\n\
                 let a3 = AllocError.OutOfMemory { requested_bytes: 99 };\n\
                 let b = AllocError.CapacityOverflow;\n\
                 println(f\"{a == a2}\");\n\
                 println(f\"{a == a3}\");\n\
                 println(f\"{a == b}\");\n\
                 println(f\"{b == b}\");\n\
                 println(f\"{classify(a)}\");\n\
                 match make(true) {\n\
                     Ok(n) => println(f\"ok:{n}\"),\n\
                     Err(e) => println(f\"err:{classify(e)}\"),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "true\nfalse\nfalse\ntrue\n2048\nerr:2048\n");
    }
}

/// B-2026-08-11-20 — `to_bits` / `to_bits32` render UNSIGNED under codegen.
/// Both are declared `-> u64` / `-> u32`, but codegen picks the printf
/// conversion from `expr_is_unsigned_int`, a syntactic classifier that had
/// no arm for them: it recursed on the receiver, which is a FLOAT, got
/// `false`, and emitted `%lld`. So every float with the sign bit set
/// printed its signed reinterpretation under JIT and AOT while the
/// interpreter printed the declared unsigned value — a real backend split,
/// not a shared convention. `(-1.0).to_bits()` was -4616189618054758400
/// against the interpreter's 13830554455654793216.
///
/// Positive values are the control: they agree either way, which is exactly
/// why this survived — only the sign bit distinguishes the two renderings.
#[test]
fn e2e_to_bits_renders_unsigned() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println(f\"{(0.0 - 1.0).to_bits()}\");\n\
                 println(f\"{(0.0 - 2.5).to_bits()}\");\n\
                 println(f\"{(1.0).to_bits()}\");\n\
                 println(f\"{(0.0 - 1.0).to_bits32()}\");\n\
                 println((0.0 - 1.0).to_bits());\n\
             }",
    ) {
        assert_eq!(
                out,
                "13830554455654793216\n13836183955189006336\n4607182418800017408\n3212836864\n13830554455654793216\n"
            );
    }
}

/// roadmap Phase 8 § std.cmp — `min` / `max` / `clamp` are ordinary
/// generic stdlib free functions (`ordering.kara`) monomorphized on
/// demand: the codegen seeds them into `generic_fns`, so a bare
/// `min(a, b)` call lowers through the same path as a user generic fn.
/// The `T → i64` monomorph body's `a.cmp(b)` hits the primitive `.cmp`
/// intercept, so no stdlib span-table swap is needed. This is the first
/// plain (non-intercepted) generic stdlib free function to reach codegen.
#[test]
fn e2e_std_cmp_min_max_clamp_codegen() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println(f\"{min(3i64, 5i64)}\");\n\
                 println(f\"{max(3i64, 5i64)}\");\n\
                 println(f\"{min(5i64, 3i64)}\");\n\
                 println(f\"{max(5i64, 3i64)}\");\n\
                 println(f\"{clamp(-2i64, 0i64, 10i64)}\");\n\
                 println(f\"{clamp(7i64, 0i64, 10i64)}\");\n\
                 println(f\"{clamp(15i64, 0i64, 10i64)}\");\n\
             }",
    ) {
        assert_eq!(out, "3\n5\n3\n5\n0\n7\n10\n");
    }
}

/// roadmap Phase 8 § Eq/Ord: `.cmp() -> Ordering` on a `#[derive(Ord)]`
/// struct/enum under codegen — the method form of the `<`/`>` operators,
/// routed through the SAME lexicographic `karac_cmp_<T>` comparator via
/// `compile_user_cmp_to_ordering` (sign-select to the Ordering tag). Also
/// exercises `min`/`max` on a struct type (their bodies call `.cmp`), which
/// were uncallable before. Output must match the interpreter oracle
/// (`test_derive_ord_cmp_method`).
#[test]
fn e2e_derive_ord_cmp_method_codegen() {
    if let Some(out) = run_program(
            "#[derive(Ord, Eq, PartialEq, PartialOrd)]\n\
             struct Rec { name: String, age: i64 }\n\
             #[derive(Ord, Eq, PartialEq, PartialOrd)]\n\
             enum Priority { Low, Med, High }\n\
             fn tag(o: Ordering) -> String {\n\
                 match o { Less => \"lt\".to_string(), Equal => \"eq\".to_string(), Greater => \"gt\".to_string() }\n\
             }\n\
             fn main() {\n\
                 let r1 = Rec { name: \"alice\".to_string(), age: 30i64 };\n\
                 let r2 = Rec { name: \"alice\".to_string(), age: 40i64 };\n\
                 let r3 = Rec { name: \"bob\".to_string(), age: 10i64 };\n\
                 println(tag(r1.cmp(r2)));\n\
                 println(tag(r1.cmp(r3)));\n\
                 println(tag(r2.cmp(r1)));\n\
                 let lo = Priority.Low; let hi = Priority.High; let md = Priority.Med;\n\
                 println(tag(lo.cmp(hi)));\n\
                 println(tag(hi.cmp(md)));\n\
                 println(tag(md.cmp(md)));\n\
                 let mx = max(r1, r3);\n\
                 println(mx.name);\n\
             }",
        ) {
            assert_eq!(out, "lt\nlt\ngt\nlt\ngt\neq\nbob\n");
        }
}

/// roadmap Phase 8 § std.mem — `swap` / `replace` compiled. Both are
/// `#[compiler_builtin]` intrinsics intercepted in `compile_call`:
/// `swap` load/load/store/store exchanges two `mut ref` places; `replace`
/// stores the new value and returns the old. Covers i64, String (heap),
/// struct values, and `mut ref` param forwarding — the output must match
/// the interpreter exactly (memory safety of the heap case is pinned by
/// `tests/memory_sanitizer.rs::asan_std_mem_swap_replace_*`).
#[test]
fn e2e_std_mem_swap_replace_codegen() {
    if let Some(out) = run_program(
        "struct P { x: i64, y: i64 }\n\
             fn reset(slot: mut ref i64) -> i64 { let mut z = 0i64; swap(slot, mut z); z }\n\
             fn main() {\n\
                 let mut a = 1i64; let mut b = 2i64;\n\
                 swap(mut a, mut b);\n\
                 println(f\"{a} {b}\");\n\
                 let old = replace(mut a, 99i64);\n\
                 println(f\"{a} {old}\");\n\
                 let mut s = \"hello\".to_string(); let mut t = \"world\".to_string();\n\
                 swap(mut s, mut t);\n\
                 let prev = replace(mut s, \"new\".to_string());\n\
                 println(f\"{s} {t} {prev}\");\n\
                 let mut p = P { x: 3i64, y: 4i64 }; let mut q = P { x: 5i64, y: 6i64 };\n\
                 swap(mut p, mut q);\n\
                 println(f\"{p.x} {q.x}\");\n\
                 let mut n = 77i64; let pv = reset(mut n);\n\
                 println(f\"{n} {pv}\");\n\
             }",
    ) {
        assert_eq!(out, "2 1\n99 2\nnew hello world\n5 3\n0 77\n");
    }
}

/// roadmap Phase 8 § std.mem — `take[T: Default](dest: mut ref T) -> T`
/// under codegen. Unlike swap/replace, `take` is a REAL generic Kāra body
/// (`replace(dest, T.default())`) seeded into `generic_fns` from the baked
/// `std.mem` program, so this exercises the whole monomorphization path:
/// the mono resolves `T` → the concrete type, dispatches `T.default()` to
/// the derived `S.default` (or the primitive-default fallthrough for i64),
/// and composes the `replace` intercept. Output must match the interpreter
/// oracle (`test_std_mem_take`); heap memory safety is pinned by
/// `tests/memory_sanitizer.rs::asan_std_mem_take_*`.
#[test]
fn e2e_std_mem_take_codegen() {
    if let Some(out) = run_program(
        "#[derive(Default)]\n\
             struct S { x: i64, name: String }\n\
             fn steal(slot: mut ref String) -> String { take(slot) }\n\
             fn main() {\n\
                 let mut n = 7i64;\n\
                 let prev = take(mut n);\n\
                 println(f\"{prev} {n}\");\n\
                 let mut a = S { x: 42i64, name: \"hello\".to_string() };\n\
                 let old = take(mut a);\n\
                 println(f\"{old.x} {old.name}\");\n\
                 println(f\"{a.x} [{a.name}]\");\n\
                 let mut s = \"owned\".to_string();\n\
                 let got = steal(mut s);\n\
                 println(f\"{got} [{s}]\");\n\
             }",
    ) {
        assert_eq!(out, "7 0\n42 hello\n0 []\nowned []\n");
    }
}

#[test]
fn e2e_chars_count_and_len_codegen() {
    // B-2026-07-11-9 gap 1: `s.chars().count()` (idiomatic) and its alias
    // `s.chars().len()` return the char count. `chars()` compiles to an
    // eager `Vec[char]`, so both extract that Vec's length. Pre-fix
    // `count()` hit "no handler for method 'count' on non-identifier
    // receiver" at codegen and `len()` was rejected at typecheck.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let s: String = \"hello\";\n\
                 println(f\"{s.chars().count()}\");\n\
                 println(f\"{s.chars().len()}\");\n\
                 let e: String = \"\";\n\
                 println(f\"{e.chars().count()}\");\n\
             }",
    ) {
        assert_eq!(out, "5\n5\n0\n");
    }
}

/// gap-d IR pin: `unreachable()` (type `!`) as the tail of a
/// value-returning fn must lower to an `unreachable` terminator, NOT a
/// `ret <i64 placeholder>`. Before the fix, `boom`'s body emitted
/// `ret i64 0` against the `{ i64 }` (FakeClock) return type and failed
/// module verification ("return type does not match operand type").
/// `boom` is private, so its `panics` effect is inferred — no annotation
/// needed.
#[test]
fn test_ir_diverge_unreachable_tail_emits_unreachable_not_ret() {
    let ir = ir_for(
        "struct FakeClock { t: i64 }\n\
             fn boom() -> FakeClock { unreachable() }\n\
             fn main() { let _c = boom(); }",
    );
    let body = function_body(&ir, "boom").expect("boom body");
    assert!(
        body.contains("unreachable"),
        "diverging tail should emit an `unreachable` terminator; body was:\n{}",
        body
    );
    assert!(
        !body.contains("ret i64"),
        "diverging tail must not emit `ret i64 <placeholder>` against the struct \
             return type; body was:\n{}",
        body
    );
}

// ── FFI unions slice 4: codegen lowering ─────────────────────────
//
// E2E pin for `#[repr(C)] union Foo { ... }` LLVM lowering. The
// storage struct is built so `size_of[Foo]` = max(field_sizes),
// `align_of[Foo]` = max(field_aligns), and an in-place write through
// one field followed by a read through another returns the same
// bytes (the union semantics the typechecker's `unsafe { }` gate
// holds users responsible for).

#[test]
fn test_e2e_size_of_user_union() {
    // `union FloatBits { f: f32, bits: u32 }` — both fields are
    // 4 bytes, 4-aligned, so the storage struct collapses to
    // `{ <primary> }` with no padding tail.
    let out = run_program(
        "#[repr(C)] union FloatBits { f: f32, bits: u32 }\n\
             fn main() { println(size_of[FloatBits]()); }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "4");
    }
}

// ── Strict-provenance ptr APIs (line 511 slice 3) ────────────────
//
// Codegen lowering for the seven `ptr.*` module functions per
// `design.md § Pointer Provenance` (v60 item 20). `*const T` /
// `*mut T` lower to genuine LLVM `ptr` (the Phase-8 CStr/as_ptr
// slice lifted `TypeKind::Pointer` off the historical i64
// fall-through in `llvm_type_for_type_expr`), so ptr→usize ops emit
// `ptrtoint` and usize→ptr ops emit `inttoptr` — the spec's
// provenance shape. The IR tests here pin the *function shape*
// (callable, returns the right type, does not crash codegen); the
// `!provenance` metadata refinement remains open in the tracker.

#[test]
fn test_ir_ptr_addr_compiles() {
    // Verifies the dispatch arm is reached and codegen succeeds.
    // The actual LLVM op may be a no-op under the i64-pointer ABI
    // (the receiver flows as IntValue → return as-is), so the
    // assertion is presence of the caller function — not a specific
    // cast opcode. Pin against the "method dispatch fell through"
    // diagnostic the fallthrough emits when an arm is missing.
    let ir = ir_for("fn caller(p: *const i64) -> usize { ptr.addr(p) } fn main() {}");
    assert!(
        ir.contains("@caller"),
        "caller fn should be emitted; got IR:\n{ir}"
    );
    assert!(
        !ir.contains("method dispatch fell through"),
        "ptr.addr dispatch must not fall through; got IR:\n{ir}"
    );
}

#[test]
fn test_ir_ptr_with_addr_compiles() {
    let ir = ir_for(
        "fn caller(p: *const i64, a: usize) -> *const i64 { ptr.with_addr(p, a) } fn main() {}",
    );
    assert!(
        ir.contains("@caller"),
        "caller fn should be emitted; got IR:\n{ir}"
    );
}

// ── ptr.container_of / ptr.container_of_mut (line 509 follow-up) ─

#[test]
fn test_ir_ptr_container_of_compiles() {
    let ir = ir_for(
        "struct Inner { x: i32, y: i32 } \
             struct Outer { a: i32, inner: Inner } \
             fn recover(fp: *const i32) -> *const Outer { \
                 unsafe { ptr.container_of(fp, offset_of[Outer](inner.y)) } \
             } \
             fn main() {}",
    );
    assert!(
        ir.contains("@recover"),
        "recover fn should be emitted; got IR:\n{ir}"
    );
    // The lowering subtracts the offset from the field pointer's
    // address bits — `sub` instruction must appear.
    assert!(
        ir.contains("sub"),
        "`ptr.container_of` should emit an integer subtract; got IR:\n{ir}"
    );
}

// ── ptr.null / ptr.dangling / ptr.is_null (line 573 slice 3) ─────

#[test]
fn test_ir_ptr_null_compiles() {
    let ir = ir_for("fn caller() -> *const i32 { ptr.null() } fn main() {}");
    assert!(
        ir.contains("@caller"),
        "caller fn should be emitted; got IR:\n{ir}"
    );
    assert!(
        !ir.contains("method dispatch fell through"),
        "ptr.null dispatch must not fall through; got IR:\n{ir}"
    );
}

#[test]
fn test_ir_ptr_dangling_compiles() {
    let ir = ir_for("fn caller() -> *const u8 { ptr.dangling() } fn main() {}");
    assert!(
        ir.contains("@caller"),
        "caller fn should be emitted; got IR:\n{ir}"
    );
}

#[test]
fn test_ir_ptr_is_null_compiles() {
    let ir = ir_for("fn caller(p: *const i32) -> bool { ptr.is_null(p) } fn main() {}");
    assert!(
        ir.contains("@caller"),
        "caller fn should be emitted; got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_request_line_routing_parse() {
    // The from_utf8-enabled Layer-7 routing parse the Relay dogfood needs:
    // decode an HTTP request line from a raw byte window
    // (`Vec.from_slice(arr[0..n])` -> `String.from_utf8`), split on ' ',
    // take the path, and route by prefix. Composes the from_slice
    // range-slice fix (B-2026-06-18-12), from_utf8 (B-2026-06-18-11),
    // `split`, `clone`, and `starts_with` — the request-line parse end to
    // end, the exact shape `examples/relay`'s router runs.
    let src = r#"
fn classify(line: String) -> i64 {
    let parts = line.split(' ');
    if parts.len() >= 2 {
        let path = parts[1].clone();
        if path.starts_with("/api") { return 1; }
        if path.starts_with("/static") { return 2; }
    }
    return 0;
}
fn main() {
    let r1: Array[u8, 18] = [71u8,69,84,32,47,97,112,105,47,120,32,72,84,84,80,47,49,49];
    let v1: Vec[u8] = Vec.from_slice(r1[0..18]);
    match String.from_utf8(v1) { Ok(l) => println(classify(l)), Err(_) => println(0) }
    let r2: Array[u8, 18] = [71u8,69,84,32,47,115,116,97,116,105,99,47,121,32,72,84,84,80];
    let v2: Vec[u8] = Vec.from_slice(r2[0..18]);
    match String.from_utf8(v2) { Ok(l) => println(classify(l)), Err(_) => println(0) }
    let r3: Array[u8, 10] = [71u8,69,84,32,47,32,72,84,84,80];
    let v3: Vec[u8] = Vec.from_slice(r3[0..10]);
    match String.from_utf8(v3) { Ok(l) => println(classify(l)), Err(_) => println(0) }
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "1\n2\n0\n");
    }
}

/// B-2026-08-13-10 — the REPLICATION-COST gates, one per regression the
/// widening in the test above actually caused. Each of these was measured
/// slower on a kata bench lane before its gate existed, so each is a pin on
/// a loss, not a hypothetical:
///
///   nested loop  — #259's `while r < rounds` is a benchmark's OUTER loop;
///                  26x-ing the kernel cost 25% (562 -> 704 ms)
///   calls        — #134's 8-iteration setup loop does `push(lcg(..))`;
///                  replicating the calls cost 40% (484 -> 677 ms)
///   heap operands— #291's inner loop does `sj = sj + alpha[..]` over a
///                  `String` and a `Vec[String]`; 30 allocating concats
///                  cost 1.9x (224 -> 425 ms)
///
/// The heap case is why the cost check is TYPE-AWARE and not pure AST:
/// `a + b` and `v[i]` look identical whether they are `i64` or `String`,
/// and the pure-AST version passed #291 straight through. The scalar
/// control at the end is the same shape with `Vec[i64]` and must still be
/// hinted — otherwise the gate would have thrown away #265's win with it.
#[test]
fn test_ir_full_unroll_declines_bodies_too_costly_to_replicate() {
    let nested = ir_for(
        // The inner bound is over the cap so the INNER loop is not itself
        // hintable — otherwise this module-wide grep would match its hint
        // and say nothing about the outer one.
        "fn main() { let k = 8i64; let mut j = 0i64; let mut s = 0i64; \
             while j < k { let mut q = 0i64; while q < 100000i64 { s = s + q; q = q + 1i64; } \
             j = j + 1i64; } println(s); }",
    );
    assert!(
        !nested.contains("llvm.loop.unroll.full"),
        "a body containing a nested loop must NOT be hinted; IR:\n{nested}"
    );

    let calls = ir_for(
        "fn f(x: i64) -> i64 { x * 3i64 } \
             fn main() { let k = 8i64; let mut j = 0i64; let mut s = 0i64; \
             while j < k { s = s + f(j); j = j + 1i64; } println(s); }",
    );
    assert!(
        !calls.contains("llvm.loop.unroll.full"),
        "a body containing a call must NOT be hinted; IR:\n{calls}"
    );

    let heap = ir_for(
        "fn main() { let k = 8i64; let mut acc = String.new(); \
             let mut xs: Vec[String] = Vec.new(); xs.push(\"a\"); \
             let mut j = 0i64; \
             while j < k { acc = acc + xs[0]; j = j + 1i64; } println(acc.len()); }",
    );
    assert!(
        !heap.contains("llvm.loop.unroll.full"),
        "a body with HEAP operands must NOT be hinted — `a + b` over Strings \
             is indistinguishable from integer addition in the AST alone; IR:\n{heap}"
    );

    // Scalar control: the same indexed shape over `Vec[i64]` IS the loop
    // this hint exists for and must survive every gate above.
    let scalar_index = ir_for(
        "fn main() { let k = 8i64; let mut v: Vec[i64] = Vec.filled(8i64, 3i64); \
             let mut j = 0i64; let mut s = 0i64; \
             while j < k { if v[j] < s { s = v[j]; } j = j + 1i64; } println(s); }",
    );
    assert!(
        scalar_index.contains("llvm.loop.unroll.full"),
        "a scalar-element indexed reduction SHOULD still be hinted; IR:\n{scalar_index}"
    );
}

#[test]
fn test_e2e_ptr_dangling_is_not_null() {
    // ptr.dangling() returns a non-null pointer; ptr.is_null
    // observes false.
    let src = "fn main() { \
                       let p: *const u8 = ptr.dangling(); \
                       if ptr.is_null(p) { println(1); } else { println(0); } \
                   }";
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "0\n");
    }
}

// ── ptr.const / ptr.mut construction (line 573 slices 1a-1b) ─────

#[test]
fn test_ir_ptr_const_on_local_compiles() {
    let ir = ir_for("fn main() { let x: i32 = 7; let p: *const i32 = ptr.const(x); }");
    assert!(
        !ir.contains("method dispatch fell through"),
        "ptr.const dispatch must not fall through; got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_and_short_circuit_skips_rhs_call() {
    // `false and boom()` must not call boom() at runtime.
    let out = run_program(
        r#"
fn boom() -> bool { println("called"); true }
fn main() {
    if false and boom() { println("then"); } else { println("else"); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "else\n");
    }
}

#[test]
fn test_e2e_or_short_circuit_skips_rhs_call() {
    // `true or boom()` must not call boom() at runtime.
    let out = run_program(
        r#"
fn boom() -> bool { println("called"); true }
fn main() {
    if true or boom() { println("then"); } else { println("else"); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "then\n");
    }
}

#[test]
fn test_e2e_arithmetic() {
    let out = run_program(
        r#"
fn add(a: i64, b: i64) -> i64 { a + b }
fn main() { println(add(3, 4)); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_e2e_fibonacci() {
    let out = run_program(
        r#"
fn fib(n: i64) -> i64 {
    if n <= 1 { n } else { fib(n - 1) + fib(n - 2) }
}
fn main() { println(fib(10)); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "55");
    }
}

#[test]
fn fn_value_named_fn_as_fn_typed_param_runs() {
    let out = run_program(
        "fn doubler(n: i64) -> i64 { n * 2i64 }\n\
             fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
             fn main() { println(f\"{apply(doubler, 21i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// The same bare fn passed in `Fn(...)` position at two call sites reuses a
/// single memoized trampoline (one `define` of `__karac_fnval_doubler`),
/// and both higher-order calls run.
#[test]
fn fn_value_named_fn_reused_across_call_sites() {
    let src = "fn doubler(n: i64) -> i64 { n * 2i64 }\n\
                   fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
                   fn main() {\n\
                       let a = apply(doubler, 10i64);\n\
                       let b = apply(doubler, 11i64);\n\
                       println(f\"{a + b}\");\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("42\n"));
    let ir = ir_for(src);
    let tramp_defs = ir
        .lines()
        .filter(|l| l.starts_with("define") && l.contains("@__karac_fnval_doubler"))
        .count();
    assert_eq!(
        tramp_defs, 1,
        "trampoline must be synthesized exactly once (memoized):\n{ir}"
    );
}

#[test]
fn test_e2e_swap_pairs_iterative_pair_relink() {
    // The full kata-#24 iterative shape: a dummy-anchored cursor
    // loop that re-links each adjacent pair via three stores
    // (`first.next = second.next; second.next = Some(first);
    // prev.next = Some(second)`), with `break` exiting from inside
    // the `if let` arms while bindings are live. Exercises the
    // pattern-binding alias acquire (the `second` node's only ref
    // between stores 1 and 3 is the binding) AND the
    // `break`-past-frames drain (`first` is bound when the inner
    // else-arm breaks). Pre-fix: SIGSEGV / garbage output.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build(n: i64) -> Option[ListNode] {
    let head = ListNode { val: 1, next: None };
    let mut tail = head;
    for i in 2..n + 1 {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
    }
    Some(head)
}
fn swap_pairs(head: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: head };
    let mut prev = dummy;
    loop {
        if let Some(first) = prev.next {
            if let Some(second) = first.next {
                first.next = second.next;
                second.next = Some(first);
                prev.next = Some(second);
                prev = first;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    dummy.next
}
fn main() {
    let mut cur = swap_pairs(build(5));
    loop {
        match cur {
            Some(node) => {
                println(node.val);
                cur = node.next;
            }
            None => break,
        }
    }
}
"#,
    );
    if let Some(out) = out {
        let got: Vec<&str> = out.trim().lines().collect();
        assert_eq!(got, ["2", "1", "4", "3", "5"]);
    }
}

/// A SPEC'D 128-BIT HOLE MUST COMPILE AND RENDER ITS FULL WIDTH.
///
/// `karac_runtime_int_fmt` took a single `i64` when it was first added, so
/// an `i128` hole passed a 128-bit value to a 64-bit parameter and LLVM's
/// module verifier REJECTED THE MODULE — `f"{x:44}"` on an `i128` failed to
/// compile at all, where the older `snprintf` path had silently truncated
/// to the low 64 bits instead. The value now crosses as two 64-bit words,
/// like `karac_runtime_i128_to_str`'s.
///
/// `2^100` is the discriminating value: its LOW WORD IS ZERO, so a 64-bit
/// truncation renders it as `0` rather than as something visibly corrupt.
#[test]
fn e2e_spec_128_bit_holes_compile_and_keep_their_top_half() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let big: u128 = 170141183460469231731687303715884105727u128;
    let pow: i128 = 1267650600228229401496703205376i128;
    let neg: i128 = -1267650600228229401496703205376i128;
    let small: u128 = 42u128;
    println(f"[{small:6}]");
    println(f"[{pow}]");
    println(f"[{pow:44}]");
    println(f"[{neg:44}]");
    println(f"[{big:44}]");
    println(f"[{pow:<34}]");
}
"#,
    ) {
        let want = "[    42]\n\
                        [1267650600228229401496703205376]\n\
                        [             1267650600228229401496703205376]\n\
                        [            -1267650600228229401496703205376]\n\
                        [     170141183460469231731687303715884105727]\n\
                        [1267650600228229401496703205376   ]\n";
        assert_eq!(out, want, "128-bit spec'd rendering drifted");
    }
}

#[test]
fn test_ir_bug8_method_call_receive_no_double_inc() {
    // `MethodCall` RHS shape — `let x = h.make()` — must follow
    // the same convention as `Call`: the method's return delivers
    // +1, the caller does not inc again on receive.  Without
    // covering this variant, every shared-struct return crossing
    // through a method call (the common case for any user type
    // with a `pub fn new() -> Self` style constructor) would
    // re-introduce the same leak.
    let ir = ir_for(
        r#"
shared struct S { val: i64 }
struct Holder { tag: i64 }
impl Holder {
    fn make(self) -> S { let s = S { val: 99 }; s }
}
fn use_it() {
    let h = Holder { tag: 0 };
    let x = h.make();
}
"#,
    );
    let inc_count = ir.matches("add i64 %rc").count();
    let dec_count = ir.matches("sub i64 %rc").count();
    assert_eq!(
        inc_count, 1,
        "method-call RHS must not emit a receive-side rc_inc; only \
             the callee-side move-out inc inside `make` should appear. \
             Found {} `add i64 %rc` ops in:\n{}",
        inc_count, ir
    );
    assert_eq!(
        dec_count, 2,
        "expected 2 rc_decs (callee scope-exit + caller scope-exit); \
             found {} in:\n{}",
        dec_count, ir
    );
}

// ── Async `sleep_ms` — park-on-timer lowering (phase-5 auto-par ──
//    divergence A2a-2.2) ──
//
// `sleep_ms(ms)` (the leaf `suspends` primitive, `std.time`) lowers to
// the `karac_park_on_timer` state machine: the call site converts ms →
// ns, allocs the state struct, drives the poll-fn once (state_0 arms a
// reactor deadline via `karac_runtime_event_loop_register_timer` and
// returns Pending), then blocks on a completion slot the dispatcher
// signals on expiry. Unlike a `blocks` libc `usleep`, two `sleep_ms`
// calls under `par {}` overlap on the timer wheel rather than pin two
// OS threads for their full naps.

#[test]
fn sleep_ms_lowers_to_park_on_timer() {
    // Runtime-valued duration so the ms→ns multiply is NOT
    // constant-folded away (a literal `sleep_ms(500)` folds
    // `500 * 1_000_000` to a constant and the named `mul` vanishes).
    let ir = ir_for(
        "fn nap(ms: i64) {\n\
                 sleep_ms(ms);\n\
             }\n\
             fn main() {\n\
                 nap(500);\n\
             }",
    );
    // The deadline is armed through the timer-register FFI (NOT
    // register_fd — there is no fd).
    assert!(
        ir.contains("karac_runtime_event_loop_register_timer"),
        "expected sleep_ms to arm a reactor deadline via \
             karac_runtime_event_loop_register_timer; IR:\n{ir}"
    );
    // The park-on-timer poll-fn + constructor are emitted.
    assert!(
        ir.contains("__kara_poll_karac_park_on_timer")
            && ir.contains("__kara_state_new_karac_park_on_timer"),
        "expected the karac_park_on_timer poll-fn + constructor in IR:\n{ir}"
    );
    // The call site composes with the primitive: ms→ns convert (survives
    // because `ms` is runtime-valued), alloc the state, drive the poll,
    // and block on the completion slot (dispatcher-yield model).
    let nap_body = function_body(&ir, "nap").unwrap_or_else(|| {
        panic!("nap body not found in IR:\n{ir}");
    });
    assert!(
        nap_body.contains("kara.timer.ms_to_nanos"),
        "expected the call site to convert ms→ns; nap body:\n{nap_body}"
    );
    assert!(
        nap_body.contains("kara.timer.dur.field") && nap_body.contains("kara.timer.poll_wait"),
        "expected the call site to store the duration + block on the \
             completion slot; nap body:\n{nap_body}"
    );
    // Unlike the fd park, sleep_ms performs NO fd deregister (timers
    // have no fd / no epoll registration; the dispatcher claims its own).
    assert!(
        !nap_body.contains("karac_runtime_event_loop_deregister_fd"),
        "sleep_ms must NOT deregister an fd (timers have none); nap body:\n{nap_body}"
    );
}

#[test]
fn e2e_sleep_ms_resumes_in_order() {
    // A short async sleep runs to completion and resumes the caller in
    // program order — proving the full arm→park→dispatcher-signal→resume
    // path works end-to-end (not just that the IR shape is right).
    if let Some(out) = run_program(
        "fn main() {\n\
                 println(\"a\");\n\
                 sleep_ms(20);\n\
                 println(\"b\");\n\
                 sleep_ms(20);\n\
                 println(\"c\");\n\
             }",
    ) {
        assert_eq!(out, "a\nb\nc\n");
    }
}

#[test]
fn test_ir_converging_two_pointer_bce_skip_wiring() {
    // B-2026-08-04-8: the converging skip proves `base + lo` and
    // `base + hi` are both in range for the row-major shape — upper from
    // the length pin + enclosing counter bound, lower from a non-negative
    // row origin — so BOTH halves are elided and no check block is emitted
    // at all.
    let ir = ir_for(CONV_TWO_POINTER_SRC);
    assert!(
        !ir.contains("vidx."),
        "expected the converging two-pointer loads to carry NO bounds \
             check, but found a `vidx.` block:\n{ir}"
    );
}

/// B-2026-08-26-22 — the fallible-allocation remainder, twinned against the
/// interpreter.
///
/// `String.reserve` carries no `capacity()` companion (see its typechecker
/// arm), so what the two backends are compared on is the CONTENT, which is
/// the whole of what a hint owes.
#[test]
fn e2e_fallible_alloc_remainder_agrees_with_the_interpreter() {
    let cases: &[(&str, &str)] = &[
        (
            "String.reserve leaves content alone",
            "let mut s: String = String.new();\n\
                 s.push_str(\"hello\");\n\
                 s.reserve(1000);\n\
                 println(f\"[{s}] {s.len()}\");\n\
                 s.push_str(\" world\");\n\
                 s.reserve(-5);\n\
                 s.reserve(0);\n\
                 println(f\"[{s}] {s.len()}\");",
        ),
        (
            "Vec and String reserve share one name",
            "let mut v: Vec[i64] = Vec.new();\n\
                 v.reserve(32);\n\
                 v.push(1);\n\
                 let mut s: String = String.new();\n\
                 s.reserve(32);\n\
                 s.push_str(\"x\");\n\
                 println(f\"{v.len()} {v.capacity() >= 32} [{s}]\");",
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

/// B-2026-09-19-20 — an indexed-receiver method whose CONTAINER is a call.
///
/// `mkv(k)[0].len()` died on "indexed-receiver method 'len' requires the
/// indexed container to be a named variable in v1", and so did every
/// borrow-returning spelling of the same shape, while `--interp` printed
/// the right answer for all of them.
///
/// THREE DEFECTS, not one, which is why the cells below are grouped the
/// way they are.
///
/// `boundarr:` is the first and the one that is not about calls at all: a
/// ref-local bound to a borrowed `Array[T, N]` never recorded its element
/// type, so even `let a: ref Array[String, 2] = h.pa(); a[1].len()` failed
/// — on "element TypeExpr unknown (outer is not a tracked Vec/Slice/Array
/// variable)", a DIFFERENT message from the other cells, on a binding that
/// plainly is one. The `ref Vec[String]` sibling one line away worked,
/// because an Array records its element type in its own table
/// (B-2026-07-30-3) and the hand-rolled ladder had only the Vec arm. Two
/// copies of that ladder had drifted apart; they are one shared registrar
/// now, which is what stops the next asymmetry costing two fixes.
///
/// `borrmeth:` / `borrvec:` / `borrfree:` are the second: a
/// borrow-returning call container, hoisted to an anonymous ref-local —
/// the fourth member of a hoist family this function already had, and the
/// closest sibling of its `map.get(k).unwrap()` arm. Non-owning, like all
/// three before it.
///
/// `ownfree:` / `ownmeth:` / `nested:` are the third, and the one that is
/// NOT a hoist. Every existing hoist points its synth at storage somebody
/// else owns — a field, a tuple element, a map's bucket, a borrow's
/// referent — so its teardown emits no IR. A call's result is a fresh
/// owned temp, so hoisting it as a container would leak the whole thing.
/// That arm lowers the INDEX instead, through machinery that has dropped
/// exactly this temporary since B-2026-07-15-27, and binds the standalone
/// element it hands back.
///
/// `once:` is the cell that pins single evaluation. The container call
/// prints, so a hoist that let a later arm re-compile the receiver would
/// show a second `once` line here rather than in a leak count — and the
/// whole point of emitting the accessor at the top of this function is
/// that it happens once. `orig:` reads the borrowed fields after three
/// borrows of them, so an over-eager teardown dangles rather than leaks.
///
/// NOT COVERED, deliberately: the RANGE spelling (`mkv(k)[1..3].len()`).
/// `container[a..b]` is a slice rather than an element, so both new arms
/// decline it and the container-must-be-a-named-variable diagnostic fires
/// unchanged — the same fail-closed move the older hoists make when their
/// own lookup comes up empty.
#[test]
fn test_e2e_indexed_receiver_method_on_a_call_container() {
    assert_eq!(
        run_program(
            "struct Hold { arr: Array[String, 2], v: Vec[String] }\n\
                 impl Hold {\n\
                 \x20   fn pa(ref self) -> ref Array[String, 2] { return self.arr; }\n\
                 \x20   fn pv(ref self) -> ref Vec[String] { return self.v; }\n\
                 }\n\
                 struct Mk {}\n\
                 impl Mk {\n\
                 \x20   fn arr(ref self, n: i64) -> Array[String, 2] {\n\
                 \x20       return Array[f\"marr-{n}\", f\"mbrr-{n}\"];\n\
                 \x20   }\n\
                 }\n\
                 fn pav(h: ref Hold) -> ref Vec[String] { return h.v; }\n\
                 fn mkv(n: i64) -> Vec[String] { return [f\"free-{n}\", f\"beta-{n}\"]; }\n\
                 fn mkn(n: i64) -> Vec[Vec[i64]] { return [[n, n + 1, n + 2], [n]]; }\n\
                 fn shout(n: i64) -> Vec[String] { println(\"once\"); return [f\"sh-{n}\"]; }\n\
                 fn main() {\n\
                 \x20   let k = 7;\n\
                 \x20   let h = Hold {\n\
                 \x20       arr: [f\"harr-{k}\", f\"hbrr-{k}\"],\n\
                 \x20       v: [f\"hvec-{k}\", f\"hwec-{k}\"],\n\
                 \x20   };\n\
                 \x20   let m = Mk {};\n\
                 \x20   println(f\"ownfree:{mkv(k)[0].len()}\");\n\
                 \x20   println(f\"ownmeth:{m.arr(k)[1].to_uppercase()}\");\n\
                 \x20   println(f\"borrmeth:{h.pa()[0].len()}\");\n\
                 \x20   println(f\"borrvec:{h.pv()[1].to_uppercase()}\");\n\
                 \x20   println(f\"borrfree:{pav(h)[0].len()}\");\n\
                 \x20   let a: ref Array[String, 2] = h.pa();\n\
                 \x20   println(f\"boundarr:{a[1].len()}\");\n\
                 \x20   println(f\"nested:{mkn(k)[0].len()}\");\n\
                 \x20   println(f\"once:{shout(k)[0].len()}\");\n\
                 \x20   println(f\"orig:{h.arr[0]}/{h.v[1]}\");\n\
                 }"
        )
        .as_deref(),
        Some(
            "ownfree:6\nownmeth:MBRR-7\nborrmeth:6\nborrvec:HWEC-7\nborrfree:6\n\
                 boundarr:6\nnested:3\nonce\nonce:4\norig:harr-7/hwec-7\n"
        )
    );
}

// ── Built-in `abs` on numeric primitives ──────────────────────────
// `x.abs() -> Self` on signed ints (checked: `iN::MIN.abs()` traps as
// integer overflow, reusing the checked-neg lowering) and floats.

#[test]
fn test_e2e_abs_signed_int() {
    if let Some(out) = run_program(
        r#"
fn main() {
    println((-5i64).abs());
    println((7i64).abs());
    println((0i64).abs());
}
"#,
    ) {
        assert_eq!(out, "5\n7\n0\n");
    }
}

#[test]
fn test_e2e_abs_float() {
    if let Some(out) = run_program(
        r#"
fn main() {
    println((-2.5f64).abs());
    println((2.5f64).abs());
}
"#,
    ) {
        assert_eq!(out, "2.5\n2.5\n");
    }
}

#[test]
fn test_e2e_byvalue_aggregate_param_transferred_out_enum() {
    // #14 (phase-12 self-hosting): an owned by-value ENUM param moved into a
    // call that transfers it OUT (the callee returns it) must reach the
    // consumer intact and free exactly once. `wrap(f)` returns its param;
    // both the caller's source `f` and the result `g` previously aliased and
    // double-freed the same payload buffer. The param is now entry
    // deep-copied + callee-owned (param_own.rs), so the two own independent
    // buffers.
    if let Some(out) = run_program(
        r#"
enum E { A(String), N(i64) }
fn wrap(e: E) -> E { e }
fn main() {
    let f = E.A("hi".to_string());
    let g = wrap(f);
    match g {
        A(s) => println(s),
        N(n) => println(n.to_string()),
    }
}
"#,
    ) {
        assert_eq!(out, "hi\n");
    }
}

#[test]
fn test_e2e_byvalue_aggregate_param_read_then_reused() {
    // #14 guard: passing an owned aggregate by value to a function that only
    // READS it must NOT break re-use of the source (`take(x); take(x)`).
    // Entry deep-copy keeps this correct (each call copies at entry; the
    // caller retains and frees its original once) — the reason the fix is
    // entry-copy rather than a caller-side move (which would use-after-free
    // here, since Kāra's move-checker does not reject double-consume).
    if let Some(out) = run_program(
        r#"
struct S { s: String }
fn take(v: S) { println(v.s) }
fn main() {
    let x = S { s: "hi".to_string() };
    take(x);
    take(x);
    println(x.s);
}
"#,
    ) {
        assert_eq!(out, "hi\nhi\nhi\n");
    }
}

/// B-2026-08-15-20 — an arithmetic trap is reported at the expression that
/// faulted, and `karac build` agrees with `--interp` on where that is.
///
/// `compile_expr` stamps `current_span` on entry and `emit_panic` bakes
/// whatever it holds into the runtime message. Compiling the two operands
/// overwrote it, so the trap inherited the last subexpression evaluated
/// inside the RIGHT operand: `a * (m + 0)` blamed the `0` literal and
/// `(a + b) * m` blamed `m`. Neither of those can overflow, and neither
/// matched the interpreter, which reports the binary expression — a
/// run-vs-build divergence in a diagnostic. The unary sibling had its own
/// cause: the `i64.neg` assoc lowering rebuilt the `Unary` node carrying the
/// OPERAND's span, so `-iN::MIN` blamed the operand instead of the `-`.
///
/// Each case pins the absolute column as well as the parity — agreement
/// alone is also satisfied by both backends being wrong in the same way.
#[test]
fn arithmetic_trap_location_matches_the_interpreter() {
    // Every program puts the faulting expression on line 3, indented four
    // spaces after `let c = `, so the expression's first character is at
    // column 13. A parenthesized left operand reports column 14 instead:
    // the parser makes parens transparent, so the binary node's span starts
    // at the first token INSIDE them (`a`), not at the `(`.
    let cases: [(&str, &str, usize, usize); 4] = [
        (
            "mul overflows; the right operand is the deeper expression",
            r#"
fn boom(a: i64, b: i64, m: i64) -> i64 {
    let c = a * (m + 0);
    return c;
}

fn main() {
    println(boom(4611686018427387904, 4611686018427387903, 4));
}
"#,
            3,
            13,
        ),
        (
            "mul overflows; the left operand is a parenthesized sum",
            r#"
fn boom(a: i64, b: i64, m: i64) -> i64 {
    let c = (a + b) * m;
    return c;
}

fn main() {
    println(boom(4611686018427387904, 4611686018427387903, 4));
}
"#,
            3,
            14,
        ),
        (
            "division by zero, both operands parenthesized",
            r#"
fn boom(a: i64, b: i64, m: i64) -> i64 {
    let c = (a + 1) / (m + 0);
    return c;
}

fn main() {
    println(boom(1, 1, 0));
}
"#,
            3,
            14,
        ),
        (
            "checked negate of i64::MIN blames the operator, not the operand",
            r#"
fn boom(a: i64) -> i64 {
    let c = -a;
    return c;
}

fn main() {
    println(boom(-9223372036854775807 - 1));
}
"#,
            3,
            13,
        ),
    ];

    for (label, src, line, col) in cases {
        let (_out, errs, _trace, _) = karac::run_program_full_checked(src);
        assert!(
            !errs.is_empty(),
            "{label}: the interpreter must trap on this program"
        );
        assert_eq!(
            (errs[0].span.line, errs[0].span.column),
            (line, col),
            "{label}: interpreter blamed the wrong location ({})",
            errs[0].message
        );
        if let Some(cap) = run_program_capturing_with_filename(src, "trap.kara") {
            let got = panic_location(&cap.stderr).unwrap_or_else(|| {
                panic!("{label}: no `panic at` location in stderr={:?}", cap.stderr)
            });
            assert_eq!(
                got,
                (line, col),
                "{label}: compiled binary blamed a different location than \
                     the interpreter; stderr={:?}",
                cap.stderr
            );
        }
    }
}

#[test]
fn test_e2e_div_by_zero_traps() {
    let captured = run_program_capturing(
        r#"
fn main() {
    let mut z = 0;
    z = z + 0;
    let q = 7 / z;
    println(q);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("division by zero"),
            "expected division-by-zero panic, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_mod_by_zero_traps() {
    let captured = run_program_capturing(
        r#"
fn main() {
    let mut z = 0;
    z = z + 0;
    let r = 7 % z;
    println(r);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("division by zero"),
            "expected division-by-zero panic on %, got stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_neg_min_traps() {
    let captured = run_program_capturing(
        r#"
fn main() {
    let mut m = -9223372036854775807;
    m = m - 1;
    let n = -m;
    println(n);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("integer overflow"),
            "expected integer-overflow panic on -iN::MIN (interpreter \
                 parity: checked_neg), got stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_negative_min_literal_folds_without_trap() {
    // `-2147483648i32` parses as Neg(Integer(2147483648)) and is
    // range-valid as a UNIT. Codegen folds it to one constant; without
    // the fold, the positive half wraps at i32 width and the checked
    // neg spuriously traps (caught by asan_kata_8 atoi, 2026-06-07).
    let out = run_program(
        r#"
fn main() {
    let int_min: i32 = -2147483648i32;
    println(int_min);
    let i64_min = -9223372036854775807 - 1;
    println(i64_min);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "-2147483648\n-9223372036854775808\n");
    }
}

#[test]
fn test_e2e_checked_arith_non_faulting_results_unchanged() {
    // The checked lowering must not perturb in-range arithmetic.
    let out = run_program(
        r#"
fn main() {
    let mut a = 20;
    a = a + 0;
    let b = 22;
    println(a + b);
    println(a - b);
    println(a * b);
    println(b / a);
    println(b % a);
    println(-a);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "42\n-2\n440\n1\n2\n-20\n");
    }
}

#[test]
fn test_ir_div_emits_zero_and_min_guards() {
    let ir = ir_for(
        r#"
fn main() {
    let mut z = 3;
    z = z + 0;
    let q = 7 / z;
    println(q);
}
"#,
    );
    assert!(
        ir.contains("div.zero.trap"),
        "division must emit a zero-guard trap block, IR:\n{ir}"
    );
    assert!(
        ir.contains("div.ovf.trap"),
        "signed division must emit the MIN/-1 overflow guard, IR:\n{ir}"
    );
}

#[test]
fn test_ir_mixed_direction_var_emits_no_assume() {
    // k moves both directions — not monotone, scan must reject.
    let ir = ir_for(
        r#"
fn wobble(v: mut Slice[i64], n: i64) -> i64 {
    let mut k = 1;
    for i in 1..n {
        if v[i] > 0 {
            k = k + 1;
        } else {
            k = k - 1;
        }
        v[k] = v[i];
    }
    k
}
"#,
    );
    assert!(
        !ir.contains("k.mono.fact"),
        "mixed-direction var must not get an assume, IR:\n{ir}"
    );
}

#[test]
fn test_e2e_monotone_dedup_results_unchanged() {
    // The kata-26 shape end-to-end: assumes must not perturb results.
    let out = run_program(
        r#"
fn remove_duplicates(nums: mut Slice[i64], len: i64) -> i64 {
    if len == 0 {
        return 0;
    }
    let mut k = 1;
    for i in 1..len {
        if nums[i] != nums[k - 1] {
            nums[k] = nums[i];
            k = k + 1;
        }
    }
    k
}
fn main() {
    let mut a: Array[i64, 10] = [0, 0, 1, 1, 1, 2, 2, 3, 3, 4];
    let k = remove_duplicates(mut a, 10);
    println(k);
    for i in 0..k {
        println(a[i]);
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "5\n0\n1\n2\n3\n4\n");
    }
}

#[test]
fn test_e2e_monotone_decreasing_cursor_results_unchanged() {
    // Kata-88 shape: decreasing write head + decreasing read cursors.
    let out = run_program(
        r#"
fn merge(nums1: mut Slice[i64], m: i64, nums2: Slice[i64], n: i64) {
    let mut i = m - 1;
    let mut j = n - 1;
    let mut k = m + n - 1;
    while j >= 0 {
        if i >= 0 and nums1[i] > nums2[j] {
            nums1[k] = nums1[i];
            i = i - 1;
        } else {
            nums1[k] = nums2[j];
            j = j - 1;
        }
        k = k - 1;
    }
}
fn main() {
    let mut a: Array[i64, 6] = [1, 2, 3, 0, 0, 0];
    let b: Array[i64, 3] = [2, 5, 6];
    merge(mut a, 3, b, 3);
    for i in 0..6 {
        println(a[i]);
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1\n2\n2\n3\n5\n6\n");
    }
}

// ── Level 2 crash diagnostics — Part 2: DWARF debug-info emission ──────
// Part 2 emits actual DWARF (DICompileUnit / DISubprogram / per-instruction
// !dbg) via DIBuilder when enabled, for gdb/lldb symbolic backtraces.
// `compile_to_ir_with_debug_info` forces it on race-free (no env mutation).

#[test]
fn test_dwarf_debug_info_present_when_enabled() {
    // With debug info forced on, the emitted IR must carry the module-level
    // debug-info scaffolding and a per-function DISubprogram. Positive proof
    // that DWARF is actually emitted — not just that codegen doesn't crash.
    let mut parsed = karac::parse(
        r#"
fn helper(x: i64) -> i64 { x + 1 }
fn main() { let y = helper(5); println(y); }
"#,
    );
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ir = karac::codegen::compile_to_ir_with_debug_info(&parsed.program, None, None)
        .expect("compile_to_ir_with_debug_info");
    assert!(
        ir.contains("DICompileUnit"),
        "expected a DICompileUnit in debug IR, got:\n{}",
        ir
    );
    assert!(
        ir.contains("DISubprogram"),
        "expected at least one DISubprogram in debug IR"
    );
    assert!(
        ir.contains("Debug Info Version"),
        "expected the `Debug Info Version` module flag in debug IR"
    );
    // Per-instruction locations: at least one `!dbg` attachment in a body.
    assert!(
        ir.contains("!dbg"),
        "expected per-instruction !dbg locations in debug IR"
    );
    // The function's DWARF display name should appear in a DISubprogram.
    assert!(
        ir.contains("name: \"helper\"") || ir.contains("\"helper\""),
        "expected the `helper` function name in DWARF metadata"
    );
}

#[test]
fn test_dwarf_debug_info_absent_by_default() {
    // The default path (no force, no env) must emit NO debug metadata, so
    // release AOT binaries keep the Phase 1-3 size floor. Guards the gate.
    let mut parsed = karac::parse(r#"fn main() { println(1); }"#);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ir = karac::codegen::compile_to_ir(&parsed.program, None, None).expect("compile_to_ir");
    assert!(
        !ir.contains("DICompileUnit") && !ir.contains("DISubprogram"),
        "default codegen must emit no DWARF debug metadata, got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_indexed_receiver_chained_rejected() {
    // MR5: `outer[i][j].method()` is rejected up front by codegen with
    // a clear diagnostic. Pin the diagnostic so the rejection doesn't
    // silently regress to a fall-through compile.
    let src = r#"
fn main() {
    let mut outer: Vec[Vec[Vec[i64]]] = Vec.new();
    let mut a: Vec[Vec[i64]] = Vec.new();
    let inner: Vec[i64] = Vec.new();
    a.push(inner);
    outer.push(a);
    outer[0][0].push(7);
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let err = compile_to_ir(&parsed.program, None, None)
        .expect_err("expected codegen to reject chained indexed receivers")
        .message;
    assert!(
        err.contains("chained indexed receivers"),
        "expected chained-rejection diagnostic; got: {}",
        err
    );
}

#[test]
fn test_ir_method_chain_receiver_temp_freed() {
    // Slice 3 (method-chain receiver temps): `make_vec().len()` — the
    // receiver is a fresh-owned Vec temp borrowed read-only by `len`. The
    // len/is_empty fast path extracted the length and discarded the
    // struct, orphaning its `data` buffer. Now the receiver is routed
    // through `materialize_owned_temp` → `__owned_tmp` slot + a
    // `FreeVecBuffer` drain (`cleanup.free`). Archive-independent
    // leak-closure gate (macOS ASAN has no LeakSanitizer).
    let src = r#"
fn make_vec() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    return v;
}

fn main() {
    let n = make_vec().len();
    println(n);
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__owned_tmp"),
        "expected the fresh Vec receiver materialized into __owned_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.free"),
        "expected a FreeVecBuffer drain for the method-chain receiver temp; got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_into_drives_user_from_impl() {
    // User `impl From[Inches] for Cm` compiles as `Cm.from`; `.into()`
    // at a `let: Cm` position lowers to `Cm.from(...)` and routes to it.
    let out = run_program(
        r#"
struct Inches { n: i64 }
struct Cm { n: i64 }
impl From for Cm {
    fn from(i: Inches) -> Cm { Cm { n: i.n * 254 / 100 } }
}
fn main() {
    let i: Inches = Inches { n: 10 };
    let c: Cm = i.into();
    println(c.n);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "25");
    }
}

#[test]
fn test_e2e_into_wraps_value_in_some_and_ok() {
    // `From[T] for Option[T]` / `From[T] for Result[T, E]` blanket wraps
    // (design.md § Conversion Traits): `.into()` lowers to `Some(x)` /
    // `Ok(x)`, indistinguishable from hand-written variant construction, so
    // codegen reuses its existing enum-build path. Covers the let,
    // if/else-tail return, and struct-field positions plus a heap `String`
    // payload; the `Err`/`None` arms stay hand-writable. Build==run parity
    // with the interpreter sibling `test_into_wraps_value_in_some_and_ok`.
    // (Leak-clean under valgrind — the `String` payloads are freed on the
    // match/discard paths; verified by hand.)
    let out = run_program(
        r#"
fn get_opt(present: bool) -> Option[i64] {
    if present { 42.into() } else { None }
}
fn get_res(ok: bool) -> Result[i64, String] {
    if ok { 7.into() } else { Err("nope") }
}
struct Holder { slot: Option[String] }
fn main() {
    let o: Option[i64] = 5.into();
    match o { Some(v) => println(v), None => println(-1) };
    match get_opt(true) { Some(v) => println(v), None => println(-1) };
    match get_opt(false) { Some(v) => println(v), None => println(-1) };
    match get_res(true) { Ok(v) => println(v), Err(e) => println(e) };
    match get_res(false) { Ok(v) => println(v), Err(e) => println(e) };
    let h: Holder = Holder { slot: "hi".into() };
    match h.slot { Some(s) => println(s), None => println("none") };
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5\n42\n-1\n7\nnope\nhi");
    }
}

#[test]
fn test_e2e_tryinto_drives_user_tryfrom_impl() {
    // A user `impl TryFrom[Celsius] for Kelvin` compiles as `Kelvin.try_from`;
    // `.try_into()` at a `let: Result[Kelvin, _]` position lowers to
    // `Kelvin.try_from(...)` and routes to it. Exercises the Ok arm and the
    // Err arm (a heap `String` error payload). The fallible sibling of
    // `test_e2e_into_drives_user_from_impl`; the whole `.try_into()` desugar
    // chain had no codegen coverage before. (Leak-clean under valgrind — the
    // `Result[Kelvin, String]` Err String is freed on the discard/`e.len()`
    // paths; verified by hand.)
    let out = run_program(
        r#"
struct Celsius { deg: i64 }
struct Kelvin { deg: i64 }
impl TryFrom for Kelvin {
    type Error = String;
    fn try_from(c: Celsius) -> Result[Kelvin, String] {
        if c.deg < -273 { Err("below absolute zero") }
        else { Ok(Kelvin { deg: c.deg + 273 }) }
    }
}
fn main() {
    match Kelvin.try_from(Celsius { deg: 27 }) {
        Ok(k) => println(k.deg),
        Err(e) => println(e),
    }
    let r: Result[Kelvin, String] = (Celsius { deg: -300 }).try_into();
    match r {
        Ok(k) => println(k.deg),
        Err(e) => println(e),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "300\nbelow absolute zero");
    }
}

#[test]
fn test_ir_an_escaping_gpu_buffer_is_disarmed_but_a_call_argument_is_not() {
    // GPU-SLIP-4b-5. A buffer that ESCAPES the scope that bound it — by
    // being returned, or by being moved into a struct that is returned —
    // must have its binding's queued free disarmed, or that free runs while
    // the caller still holds the handle. That is a use-after-free, not a
    // leak: the runtime catches it as "a field reduction on an already-freed
    // device buffer".
    //
    // A by-value CALL ARGUMENT is deliberately not disarmed, and that
    // asymmetry is the whole design. A by-value aggregate parameter is
    // normally callee-owned because the callee deep-copies on entry — but a
    // device buffer cannot be copied by duplicating its handle, so the
    // callee only aliases it and registers no free of its own. Disarming
    // the caller there would leave the allocation with no owner at all, and
    // a leaked DEVICE allocation is invisible to LeakSanitizer.
    //
    // The named GEP is the marker: it exists only where a move was
    // suppressed.
    let escaping = r#"
struct Body { mass: f32, speed: f32 }

fn make() -> GpuBuffer[Body] {
    let bodies: Vec[Body] = [Body { mass: 1.0, speed: 2.0 }];
    let buf = gpu.upload(bodies);
    buf
}

fn main() {
    println(f"{gpu.sum(make().mass)}");
}
"#;
    let ir = ir_for_with_ownership(escaping);
    assert!(
        ir.contains("gpu.moved.handle.p"),
        "a returned buffer must have its binding's free disarmed; got no \
             suppression at all:\n{ir}"
    );

    let argument = r#"
struct Body { mass: f32, speed: f32 }

fn total(b: GpuBuffer[Body]) -> f32 { gpu.sum(b.mass) }

fn main() {
    let bodies: Vec[Body] = [Body { mass: 1.0, speed: 2.0 }];
    let buf = gpu.upload(bodies);
    println(f"{total(buf)}");
}
"#;
    let ir = ir_for_with_ownership(argument);
    assert!(
        !ir.contains("gpu.moved.handle.p"),
        "a by-value call argument must NOT disarm the caller's free — the \
             callee aliases the buffer rather than owning a copy, so disarming \
             leaves it with no owner:\n{ir}"
    );
    assert!(
        ir.contains("karac_runtime_gpu_free_soa"),
        "the caller must still free the buffer it passed by value:\n{ir}"
    );
}

#[test]
fn test_ir_a_resident_reduction_frees_a_temporary_buffer_but_never_a_binding() {
    // GPU-SLIP-4b-3b. The receiver of `gpu.<reduce>(<expr>.field)` may be a
    // binding or a temporary, and the ONLY difference that reaches the
    // machine is who frees the device buffer. Both halves are failure modes
    // with no output to betray them — a missed free leaks device memory
    // silently, and an extra one leaves a live binding holding a dangling
    // handle — so the count is asserted structurally here rather than left
    // to an E2E that would print the right answer either way.
    //
    // Codegen-only: no GPU is touched, so this is CI-safe. LeakSanitizer
    // could not cover it in any case; the allocation is on the device, not
    // in the host heap LSan walks.
    let bound = r#"
struct Cell { m: f32, t: f32 }

fn main() {
    let cells: Vec[Cell] = [Cell { m: 1.0, t: 2.0 }];
    let buf = gpu.upload(cells);
    let s = gpu.sum(buf.m);
    let u = gpu.sum(buf.t);
    println(s + u);
}
"#;
    let ir = ir_for_with_ownership(bound);
    let frees = ir.matches("karac_runtime_gpu_free_soa").count();
    // One `call`, one `declare`. TWO reductions over the binding and still
    // a single free — the one its `let` registered at scope exit. If a
    // reduction freed its receiver, `buf` would be dangling for the second.
    assert_eq!(
        frees, 2,
        "a reduction over a BINDING must not free it — expected only the \
             scope-exit free (one declare + one call), got {frees}:\n{ir}"
    );

    let temporary = r#"
struct Cell { m: f32, t: f32 }

fn main() {
    let a: Vec[Cell] = [Cell { m: 1.0, t: 2.0 }];
    let b: Vec[Cell] = [Cell { m: 3.0, t: 4.0 }];
    let s = gpu.sum(gpu.upload(a).m);
    let u = gpu.sum(gpu.upload(b).t);
    println(s + u);
}
"#;
    let ir = ir_for_with_ownership(temporary);
    let frees = ir.matches("karac_runtime_gpu_free_soa").count();
    // One `declare` + one `call` per temporary. No `let` bound either
    // buffer, so nothing else will ever free them and the reduction site is
    // the only place that can.
    assert_eq!(
        frees, 3,
        "each TEMPORARY receiver must be freed exactly once at the \
             reduction (one declare + two calls), got {frees}:\n{ir}"
    );
}

#[test]
fn test_ir_headerless_and_headered_same_type_coexist() {
    // Phase D keys the layout per (fn, type): the cluster fn goes
    // headerless while another fn using the same type (a free
    // literal in `main` — no cluster there) stays on the headered
    // layout with standard RC. The two never exchange values
    // (program purity guarantees no signature mentions the type),
    // so both layouts are simultaneously correct in one binary.
    let ir = ir_for_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build_and_sum(n: i64) -> i64 {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 1;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    let mut sum = 0;
    let mut cur = dummy.next;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn main() {
    let lone = ListNode { val: 7, next: None };
    println(build_and_sum(5) + lone.val);
}
"#,
    );
    let build = function_body(&ir, "build_and_sum").expect("build body");
    let main = function_body(&ir, "main").expect("main body");
    assert!(
        build.contains("hl_alloc") && !build.contains("rc_alloc"),
        "cluster fn must be headerless; body:\n{build}"
    );
    assert!(
        main.contains("rc_alloc") && !main.contains("hl_alloc"),
        "non-cluster fn must stay headered; body:\n{main}"
    );
}

#[test]
fn test_ir_param_coexisting_builder_transfers_chain() {
    // Phase C1a: member-type params no longer poison the cluster —
    // kata #2's exact `add_two_numbers` shape (params walked via
    // if-let, canonical-triple append, RootLink tail) now elides.
    // The params keep FULL RC (their walk traffic may inc/dec), so
    // the pins here are the structural cluster footprint only: the
    // root frees alone (elide_free, no cw_loop) and allocation
    // stays headered (a param is a signature mention of the member
    // type, so phase D demotes).
    let ir = ir_for_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn add_two_numbers(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut a = l1;
    let mut b = l2;
    let mut carry: i64 = 0;
    loop {
        let mut s: i64 = carry;
        let mut done = true;
        if let Some(n) = a {
            s = s + n.val;
            a = n.next;
            done = false;
        }
        if let Some(n) = b {
            s = s + n.val;
            b = n.next;
            done = false;
        }
        if done and s == 0 {
            break;
        }
        let node = ListNode { val: s % 10, next: None };
        tail.next = Some(node);
        tail = node;
        carry = s / 10;
    }
    dummy.next
}
fn main() {
    let x = ListNode { val: 7, next: None };
    let y = ListNode { val: 5, next: None };
    let r = add_two_numbers(Some(x), Some(y));
    if r.is_some() { println(r.unwrap().val); }
}
"#,
    );
    let body = function_body(&ir, "add_two_numbers").expect("fn body");
    assert!(
        body.contains("elide_free") && !body.contains("cw_loop"),
        "RootLink root frees alone despite member-type params; body:\n{body}"
    );
    assert!(
        body.contains("rc_alloc") && !body.contains("hl_alloc"),
        "param sig mention keeps the chain headered; body:\n{body}"
    );
}

#[test]
fn test_e2e_param_coexisting_builders_add_two_numbers() {
    // Kata #2 end-to-end under C1a: count-free builders (C1b
    // transfer) feed a count-free param-walking adder whose own
    // cluster transfers out; the caller walks and dec-drops every
    // chain. A wall failure is a deterministic UAF or a wrong sum.
    let out = run_program_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn from_three(a: i64, b: i64, c: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 0;
    while i < 3 {
        let mut v = a;
        if i == 1 { v = b; }
        if i == 2 { v = c; }
        let node = ListNode { val: v, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.next
}
fn add_two_numbers(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut a = l1;
    let mut b = l2;
    let mut carry: i64 = 0;
    loop {
        let mut s: i64 = carry;
        let mut done = true;
        if let Some(n) = a {
            s = s + n.val;
            a = n.next;
            done = false;
        }
        if let Some(n) = b {
            s = s + n.val;
            b = n.next;
            done = false;
        }
        if done and s == 0 {
            break;
        }
        let node = ListNode { val: s % 10, next: None };
        tail.next = Some(node);
        tail = node;
        carry = s / 10;
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
        let l1 = from_three(2, 4, 3);
        let l2 = from_three(5, 6, 4);
        let r = add_two_numbers(l1, l2);
        total = total + sum_chain(r);
        iter = iter + 1;
    }
    println(total);
}
"#,
    );
    // 342 + 465 = 807 → digits 7,0,8 sum 15; 100 iterations = 1500.
    assert_eq!(out.as_deref(), Some("1500\n"));
}

#[test]
fn test_ir_cluster_root_takes_free_walk() {
    // Phase B1 WITHOUT B2: the root cleanup is the link-following
    // free-walk (cw_loop) while cursors keep their RcDec (they
    // drain first). The store+advance pair is wrapped in an
    // always-true `if` so the B2 canonical-triple recognizer
    // rejects (non-adjacent statements inside a loop) — pinning
    // the B1-only behavior; the fully-recognized b2 sibling is
    // `test_ir_b2_build_loop_is_count_free`.
    let ir = ir_for_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build_and_sum(n: i64) -> i64 {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 1;
    while i <= n {
        let node = ListNode { val: i, next: None };
        if i > 0 {
            tail.next = Some(node);
            tail = node;
        }
        i = i + 1;
    }
    let mut sum = 0;
    let mut cur = dummy.next;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn main() { println(build_and_sum(5)); }
"#,
    );
    let body = function_body(&ir, "build_and_sum").expect("fn body");
    assert!(
        body.contains("cw_loop") && body.contains("cw_next"),
        "root should free-walk; body:\n{body}"
    );
    assert!(
        !body.contains("dummy_rc_cleanup"),
        "root must not take the RcDec path; body:\n{body}"
    );
    // The cursor `tail` keeps its standard RcDec.
    assert!(
        body.contains("tail_rc_cleanup"),
        "cursor keeps RcDec (drains before the walk); body:\n{body}"
    );
}

#[test]
fn test_e2e_cluster_append_builder_walk() {
    // The canonical phase-B1 shape end-to-end, repeated so a
    // free-walk soundness break (double free / missed node)
    // corrupts deterministically.
    let out = run_program_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build_and_sum(n: i64) -> i64 {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 1;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    let mut sum = 0;
    let mut cur = dummy.next;
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
    while iter < 64 {
        total = total + build_and_sum(50);
        iter = iter + 1;
    }
    println(total);
}
"#,
    );
    if let Some(out) = out {
        // 64 × Σ1..50 = 64 × 1275
        assert_eq!(out.trim(), "81600");
    }
}

#[test]
fn test_e2e_cluster_link_displacement_orphan() {
    // Link overwrite orphans the displaced sub-chain mid-build:
    // the store's release-old frees it through normal RC (build
    // traffic untouched in B1), and the root free-walk sees only
    // the surviving chain.
    let out = run_program_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn run() -> i64 {
    let dummy = ListNode { val: 0, next: None };
    let a = ListNode { val: 10, next: None };
    let b = ListNode { val: 20, next: None };
    dummy.next = Some(a);
    dummy.next = Some(b);
    let mut sum = 0;
    let mut cur = dummy.next;
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
    while iter < 64 {
        total = total + run();
        iter = iter + 1;
    }
    println(total);
}
"#,
    );
    if let Some(out) = out {
        // surviving chain is just b: 20 × 64
        assert_eq!(out.trim(), "1280");
    }
}

#[test]
fn test_ir_used_emits_llvm_used_global() {
    // `#[used]` should add the symbol to `@llvm.used`. The global has
    // appending linkage and lives in section "llvm.metadata".
    let ir = ir_for("#[used]\nfn keep() -> i64 { 7 }\nfn main() { println(keep()); }");
    assert!(
        ir.contains("@llvm.used"),
        "expected @llvm.used global; IR: {}",
        ir
    );
    assert!(
        ir.contains("appending"),
        "expected appending linkage on @llvm.used; IR: {}",
        ir
    );
    assert!(
        ir.contains("@keep"),
        "expected @keep symbol referenced from @llvm.used; IR: {}",
        ir
    );
}

#[test]
fn test_ir_no_used_means_only_jit_template_in_llvm_used_global() {
    // Without `#[used]`, the only entry in `@llvm.used` is the
    // phase-7-line-14 `.kara_jit_template` manifest, which always
    // emits (the 4-byte marker is reserved at v1 freeze for the
    // post-v1 JIT-template story — see
    // `Codegen::emit_jit_template_section`).
    let ir = ir_for("fn keep() -> i64 { 7 }\nfn main() { println(keep()); }");
    let used_line = ir
        .lines()
        .find(|l| l.contains("@llvm.used"))
        .unwrap_or_else(|| panic!("expected one @llvm.used line; IR: {ir}"));
    assert!(
        used_line.contains("@karac_jit_template_manifest"),
        "expected jit-template manifest to be the lone @llvm.used entry; line: {used_line}",
    );
    // Bound: a `[1 x ptr]` shape means exactly one entry.
    assert!(
        used_line.contains("[1 x ptr]"),
        "expected @llvm.used to carry exactly one entry; line: {used_line}",
    );
}

// ── Repeat-literal `[v; N]` const-aggregate fast path (regression) ──

#[test]
fn test_repeat_literal_const_zero_uses_memset() {
    // Regression: `compile_repeat_literal` originally emitted N
    // `insertvalue` instructions, scaling karac build time linearly
    // in N. The first fix tried `store [N x T] zeroinitializer` —
    // O(1) IR, but LLVM's downstream codegen passes crashed on the
    // aggregate store at N≥80K (verified SIGSEGV in `write_to_file`).
    // The current fix detects `let buf: Array[T, N] = [0; N]` at the
    // let-binding site and lowers it to `alloca + llvm.memset.*`,
    // bypassing the aggregate store entirely. memset is O(1) IR AND
    // O(1) codegen — it's what LLVM would lower the aggregate store
    // to anyway, just emitted directly.
    let ir = ir_for(
        r#"
fn main() {
    let buf: Array[i64, 100] = [0; 100];
    let _ = buf[0];
}
"#,
    );
    assert!(
        ir.contains("call void @llvm.memset"),
        "expected llvm.memset call for `[0; 100]` let-binding; got IR:\n{}",
        ir
    );
    assert!(
        !ir.contains("insertvalue"),
        "const-zero repeat literal must not emit per-element insertvalue; got IR:\n{}",
        ir
    );
    assert!(
        !ir.contains("store [100 x i64] zeroinitializer"),
        "let-binding fast path must avoid aggregate-store IR \
             (LLVM crashes on it at large N); got IR:\n{}",
        ir
    );
}

#[test]
fn test_repeat_literal_large_n_compiles_without_per_element_ir() {
    // Workload-realistic case: a 64K LUT used to hang the build at
    // O(N) IR construction; the next iteration crashed LLVM at
    // codegen time on the giant aggregate store. The let-binding
    // memset fast path is O(1) at both IR-construction AND codegen
    // time and works at any N.
    let ir = ir_for(
        r#"
fn main() {
    let buf: Array[i64, 65536] = [0; 65536];
    let _ = buf[0];
}
"#,
    );
    assert!(
        ir.contains("call void @llvm.memset"),
        "expected llvm.memset call for the 64K LUT; got IR truncated:\n{}",
        &ir[..ir.len().min(2000)]
    );
    assert!(
        !ir.contains("insertvalue"),
        "64K LUT must not emit per-element insertvalue (would hang the build); \
             grep for insertvalue failed; got IR truncated:\n{}",
        &ir[..ir.len().min(2000)]
    );
    assert!(
        !ir.contains("store [65536 x i64] zeroinitializer"),
        "64K LUT must not emit aggregate-store IR (LLVM crashes on it at this size); \
             got IR truncated:\n{}",
        &ir[..ir.len().min(2000)]
    );
}

/// Regression pin for substep (e): the new `concurrency` param is
/// genuinely optional. Compiling the same program through
/// `compile_to_object` (the param-light wrapper) with `None` for both
/// ownership and concurrency must still succeed. Slice 1 promised no
/// behavior change — this is the regression guard.
#[test]
fn test_concurrency_analysis_none_compiles_unchanged() {
    use karac::codegen::compile_to_object;
    let src = r#"
effect resource Net;
effect resource Disk;
effect resource Db;

fn fetch_net() -> i64 reads(Net) { 1 }
fn fetch_disk() -> i64 reads(Disk) { 2 }
fn fetch_db() -> i64 reads(Db) { 3 }

fn main() {
    let a = fetch_net();
    let b = fetch_disk();
    let c = fetch_db();
    println(a + b + c);
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);

    let obj_path = "/tmp/karac_test_concurrency_none.o";
    let result = compile_to_object(&parsed.program, obj_path, None, None);
    assert!(
        result.is_ok(),
        "compile_to_object with None concurrency failed: {:?}",
        result
    );
    let _ = std::fs::remove_file(obj_path);
}

/// Slice 2 pin: replays the same pipeline shape that `cmd_build` uses
/// (resolve → typecheck → lower → effectcheck → ownershipcheck →
/// concurrencycheck) and asserts that `concurrency_analyze` produces
/// a non-empty analysis. Locks in sub-step (a)'s wiring of
/// `pipeline.concurrencycheck()` into `cmd_build` against future
/// regression — without this call, the auto-par codegen path stays
/// dormant on the build path.
#[test]
fn test_cmd_build_pipeline_populates_concurrency() {
    let src = r#"
effect resource Net;
effect resource Disk;

fn fetch_net() -> i64 reads(Net) { 1 }
fn fetch_disk() -> i64 reads(Disk) { 2 }

fn main() {
    let _ = fetch_net();
    let _ = fetch_disk();
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let effects = karac::effectcheck(&parsed.program);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    super::common::assert_check_clean(&resolved, &typed, src);
    // Effects are the THIRD phase of the same gate, and were simply
    // absent — a test could pin behaviour for a program `karac build`
    // refuses (B-2026-08-19-5). Runs after `lower`, threaded with the
    // typechecker's tables, exactly as `Pipeline::run_all_checks` does.
    super::common::assert_effects_clean_for(&parsed.program, &typed, src);
    super::common::assert_ownership_clean(&ownership, src);
    let analysis = karac::concurrency_analyze(&parsed.program, &effects);

    // The analysis should at minimum have an entry for `main`.
    assert!(
        analysis.function_decisions.contains_key("main"),
        "expected `main` in function_decisions; got keys: {:?}",
        analysis.function_decisions.keys().collect::<Vec<_>>()
    );
}

/// `pub const X: T = lit;` declared at module scope is visible from
/// function bodies and lowers correctly through codegen. Pre-fix the
/// codegen had no `Item::ConstDecl` registration so any reference to
/// a top-level const fired `Undefined variable 'X'` from
/// `load_variable`. The interpreter path always handled it (matching
/// `Item::ConstDecl` arm in `eval_program`). Surfaced 2026-05-08
/// during slice 6 (Parallax-lite) when a `pub const WORK: i64 =
/// 50000000;` was hoisted out of the busy-compute kernels and
/// rejected by `karac build`.
#[test]
fn test_pub_const_visible_in_fn_body() {
    let out = run_program(
        r#"
pub const WORK: i64 = 100;

fn use_work() -> i64 {
    let mut sum: i64 = 0;
    let mut i: i64 = 0;
    while i < WORK {
        sum = sum + i;
        i = i + 1;
    }
    sum
}

fn main() {
    println(use_work());
}
"#,
    );
    if let Some(out) = out {
        // sum(0..100) == 4950
        assert_eq!(out.trim(), "4950");
    }
}

/// Const-of-const: a const whose value expression references another
/// const must compile correctly. The codegen fix re-compiles the
/// stored value expression at every use site, so transitive const
/// references work for free as long as they hit the
/// `ExprKind::Identifier` lookup path on the inner reference.
#[test]
fn test_pub_const_references_other_const() {
    let out = run_program(
        r#"
pub const BASE: i64 = 10;
pub const SCALED: i64 = BASE + BASE;

fn main() {
    println(SCALED);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "20");
    }
}

#[test]
fn test_with_provider_resource_id_matches_declaration_order() {
    // Resource IDs are assigned in source-declaration order from the
    // top-level walk in compile_program. With three resources, the
    // third (Disk) has ID 2; verify the push call carries i32 2.
    let ir = ir_for(
        "pub trait Recorder { fn record(value: i64); }\n\
             pub struct Counter { n: i64 }\n\
             impl Recorder for Counter { fn record(value: i64) { } }\n\
             pub effect resource Net: Recorder;\n\
             pub effect resource Mem: Recorder;\n\
             pub effect resource Disk: Recorder;\n\
             fn main() {\n\
               let p = Counter { n: 0 };\n\
               with_provider[Disk](p, || { 0 });\n\
             }",
    );
    // The push call is `karac_provider_push(frame, id, data, vtable)`.
    // Matcher: any line containing both `karac_provider_push` and
    // `i32 2` confirms the third resource's ID flowed through.
    let push_lines: Vec<&str> = ir
        .lines()
        .filter(|l| l.contains("karac_provider_push"))
        .collect();
    assert!(
        push_lines.iter().any(|l| l.contains("i32 2")),
        "expected push with i32 2 (resource Disk has declaration index 2); push lines: {:?}",
        push_lines
    );
}

#[test]
fn test_codegen_primitive_const_f64_infinity() {
    let out = run_program("fn main() { let x = f64.INFINITY; println(x); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "inf");
    }
}

#[test]
fn test_codegen_primitive_const_f64_neg_infinity() {
    let out = run_program("fn main() { let x = f64.NEG_INFINITY; println(x); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "-inf");
    }
}

#[test]
fn test_codegen_primitive_const_f64_nan() {
    // Float printing now uses `karac_runtime_f64_to_str` (Rust's `{}`),
    // so AOT renders NaN as "NaN" — matching the interpreter's Display
    // exactly (the old C `printf("%g")` path rendered lowercase "nan").
    let out = run_program("fn main() { let x = f64.NAN; println(x); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "NaN");
    }
}

#[test]
fn test_codegen_primitive_const_f32_max_usable_in_arithmetic() {
    // f32 widths preserved through codegen. Confirms the
    // const_float emission picks f32_type rather than collapsing to
    // f64 (which would silently widen and lose the typing
    // invariant). Float printing now goes through
    // `karac_runtime_f64_to_str` (Rust's shortest-round-trip `{}`), so
    // AOT renders the full decimal expansion identically to the
    // interpreter — not the old C `%g` scientific form `3.40282e+38`.
    let out = run_program("fn main() { let x: f32 = f32.MAX; let y: f32 = x; println(y); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "340282346638528860000000000000000000000");
    }
}

/// Sub-step (c) pin — `Server.serve(handle)` with a free-fn handler
/// builds end-to-end. The runtime ABI mismatch between the user
/// `fn handle(req: Request) -> Response` shape and the FFI extern's
/// `extern "C" fn(*const KaracHttpRequest, *mut KaracHttpResponse)`
/// is acknowledged in the slice plan's hard-stop trigger 2 fallback —
/// LLVM's indirect-call boundary is structurally `ptr`, so codegen
/// passes the user fn-pointer through and the build succeeds. End-
/// to-end runtime invocation needs trampoline glue tracked
/// separately; this test pins the codegen path itself.
#[test]
fn test_server_serve_with_free_fn_handler_compiles() {
    let src = r#"
struct Response { status: i64, body: String }

fn handle(req: Request) -> Response {
    Response { status: 200, body: "{}" }
}

fn main() {
    let _result = Server.serve("127.0.0.1:0", handle);
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ir = compile_to_ir(&parsed.program, None, None);
    assert!(
        ir.is_ok(),
        "expected Server.serve(addr, handle) to compile cleanly; got: {:?}",
        ir.err()
    );
    let ir_text = ir.unwrap();
    assert!(
        ir_text.contains("karac_runtime_serve_http"),
        "expected the IR to call `karac_runtime_serve_http`; not found"
    );
}

/// HTTP handler ABI trampoline (2026-05-09): two `Server.serve(handle)`
/// calls in one program emit exactly one `_karac_http_shim_handle`
/// definition. Pins the per-handler-fn shim cache — without it,
/// duplicate emission would either trigger a `module already has a
/// function named ...` panic from `LLVMModuleRef::add_function` or
/// produce two separate shim definitions and bloat the IR.
#[test]
fn test_server_serve_handler_shim_caches() {
    let src = r#"
struct Response { status: i64, body: String }

fn handle(req: Request) -> Response {
    Response { status: 200, body: "{}" }
}

fn main() {
    let _r1 = Server.serve("127.0.0.1:0", handle);
    let _r2 = Server.serve("127.0.0.1:0", handle);
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ir = compile_to_ir(&parsed.program, None, None)
        .expect("expected dual Server.serve(addr, handle) calls to compile cleanly");
    let define_count = ir
        .lines()
        .filter(|l| l.contains("_karac_http_shim_handle") && l.contains("define"))
        .count();
    assert_eq!(
        define_count, 1,
        "expected exactly 1 `_karac_http_shim_handle` definition; got {define_count}.\nIR:\n{ir}"
    );
}

/// Side benefit of the bug #5 fix: `mut ref VecDeque[T]`
/// parameters dispatch through the Vec method surface
/// (VecDeque shares Vec's `{ptr, len, cap}` runtime layout).
/// Pre-fix this errored with the same "no handler for method
/// Phase-7 line 5 sub-item 4 — smoke test the `--enable-hot-swap`
/// codegen path. Compiles a minimal program through
/// `compile_to_object_with_hot_swap(_, _, _, _, _, _, true)`, links
/// it, and runs the binary. Asserts:
/// 1. The build produces a valid object + executable (no LLVM
///    module verification failure, no linker error).
/// 2. The binary runs and prints `42`, confirming the indirection
///    table + global ctor populator wire through correctly and the
///    pub-fn call lands on the intended target.
///
/// Without this test, future cross-cutting codegen edits could
/// break the indirection path silently — the flag is off by
/// default in production so no other test exercises it.
#[test]
fn test_e2e_enable_hot_swap_minimal_pub_fn() {
    use karac::codegen::{compile_to_object_with_hot_swap, link_executable};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let src = r#"
pub fn answer() -> i64 { 42 }

fn main() {
    println(answer());
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse failed: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let obj_path = format!("/tmp/karac_hotswap_smoke_{}_{}.o", std::process::id(), id);
    let exe_path = format!("/tmp/karac_hotswap_smoke_{}_{}", std::process::id(), id);

    let result = compile_to_object_with_hot_swap(
        &parsed.program,
        &obj_path,
        None,
        None,
        None,
        None,
        true,  // enable_hot_swap
        false, // strip_contracts
        false, // coro_enabled (this hot-swap test asserts the legacy path)
    );
    assert!(
        result.is_ok(),
        "compile_to_object_with_hot_swap(enable=true) failed: {:?}",
        result
    );
    if link_executable(&obj_path, &exe_path).is_err() {
        // Linker missing / no runtime archive — skip rather than fail.
        let _ = std::fs::remove_file(&obj_path);
        eprintln!("hot-swap smoke test: link skipped (libkarac_runtime.a missing?)");
        return;
    }
    let output = std::process::Command::new(&exe_path)
        .output()
        .expect("running hot-swap smoke binary failed");
    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&exe_path);
    assert!(
        output.status.success(),
        "binary exited with non-zero status: {output:?}",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "42",
        "expected '42' from indirect call to pub fn; got {stdout:?}",
    );
}

/// Phase-7 line 5 sub-item 1 — verify that with `--enable-hot-swap`
/// the emitted IR contains the indirection table global, the
/// populator ctor, and an indirect call shape at the pub-fn call
/// site. Locks in the codegen surface so future refactors don't
/// silently regress the indirection.
#[test]
fn test_hot_swap_ir_shape() {
    use karac::codegen::compile_to_ir_with_hot_swap;

    let src = r#"
pub fn answer() -> i64 { 42 }

fn main() {
    let _ = answer();
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse failed: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);

    let ir = compile_to_ir_with_hot_swap(&parsed.program, None, None, None, None, true)
        .expect("codegen failed");
    assert!(
        ir.contains("@karac_hotswap_table"),
        "expected @karac_hotswap_table global in IR; got:\n{ir}",
    );
    assert!(
        ir.contains("__karac_init_hot_swap_table"),
        "expected init ctor in IR; got:\n{ir}",
    );
    assert!(
        ir.contains("@llvm.global_ctors"),
        "expected llvm.global_ctors registration in IR; got:\n{ir}",
    );

    // Negative — same source without hot-swap must not emit any
    // of the indirection scaffolding.
    let ir_off = compile_to_ir_with_hot_swap(&parsed.program, None, None, None, None, false)
        .expect("codegen failed");
    assert!(
        !ir_off.contains("@karac_hotswap_table"),
        "hot-swap off must not emit table; got:\n{ir_off}",
    );
}

#[test]
fn test_caller_side_intercept_multi_call_sites_get_distinct_labels() {
    // Two calls to network-boundary fns in the same caller produce
    // two distinct poll-loop / poll-done block pairs (LLVM appends
    // numeric suffixes to disambiguate; the second call site gets
    // `kara.poll_loop1` / `kara.poll_done1` etc.). Pins that each
    // intercept produces its own loop instead of accidentally
    // sharing blocks across call sites.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }
             fn main() {
                 driver();
                 driver();
             }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    // Two state constructor calls (one per call site).
    let ctor_count = main_body
        .matches("call ptr @__kara_state_new_driver()")
        .count();
    assert_eq!(
        ctor_count, 2,
        "two driver() call sites must produce two ctor invocations:\n{main_body}"
    );
    // Two poll loops (one per call site).
    let loop_count = main_body.matches("kara.poll_loop").count();
    // Each loop label appears twice in the IR (definition + branches)
    // but at minimum we need 2 distinct loop labels to exist.
    assert!(
        loop_count >= 2,
        "expected at least two `kara.poll_loop` occurrences (one per call site):\n{main_body}"
    );
}

// ── Phase 6 line 26 slice 8f: caller-side arg-storing ─────────────
//
// After the state-struct constructor call but before the poll loop,
// the intercept threads each call arg into the corresponding state
// struct captured-local field via `getelementptr inbounds + store`.
// Args[i] lands in state struct field i+1 (skipping the i32 tag at
// field 0); slice 4's layout puts parameters first in the field
// order so the index mapping is direct.

#[test]
fn test_caller_arg_storing_single_primitive_arg() {
    // `fn driver(n: i64)` → state struct = { i32 tag, i64 n }.
    // Caller's `driver(42)` must emit:
    //   %kara.state = call ptr @__kara_state_new_driver()
    //   %kara.arg0.field_ptr = getelementptr ... i32 0, i32 1
    //   store i64 42, ptr %kara.arg0.field_ptr
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) { fetch(); }
             fn main() { driver(42); }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    assert!(
        main_body
            .contains("getelementptr inbounds %kara.state.driver, ptr %kara.state, i32 0, i32 1"),
        "intercept must GEP into state struct field 1 for arg 0:\n{main_body}"
    );
    assert!(
        main_body.contains("store i64 42, ptr %kara.arg0.field_ptr"),
        "intercept must store i64 42 (the literal arg) into field 1:\n{main_body}"
    );
}

#[test]
fn test_caller_arg_storing_multi_arg_function() {
    // Two params → two arg-stores into fields 1 and 2.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(a: i64, b: i64) { fetch(); }
             fn main() { driver(1, 2); }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    assert!(
        main_body
            .contains("getelementptr inbounds %kara.state.driver, ptr %kara.state, i32 0, i32 1"),
        "first arg must GEP into field 1:\n{main_body}"
    );
    assert!(
        main_body
            .contains("getelementptr inbounds %kara.state.driver, ptr %kara.state, i32 0, i32 2"),
        "second arg must GEP into field 2:\n{main_body}"
    );
    assert!(
        main_body.contains("store i64 1, ptr %kara.arg0.field_ptr"),
        "first arg literal 1 must be stored into field 1:\n{main_body}"
    );
    assert!(
        main_body.contains("store i64 2, ptr %kara.arg1.field_ptr"),
        "second arg literal 2 must be stored into field 2:\n{main_body}"
    );
}

#[test]
fn test_caller_arg_storing_identifier_arg_stores_loaded_value() {
    // An identifier-arg (`driver(n)` where `n` is a let-bound local)
    // compiles to a load of the let-binding's alloca followed by a
    // store of the loaded value into the state struct field. Pins
    // that non-literal args route through the existing compile_expr
    // path correctly.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) { fetch(); }
             fn main() {
                 let x: i64 = 7;
                 driver(x);
             }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    assert!(
        main_body
            .contains("getelementptr inbounds %kara.state.driver, ptr %kara.state, i32 0, i32 1"),
        "identifier arg must GEP into field 1:\n{main_body}"
    );
    // The store transports the loaded i64 SSA value (not a literal)
    // into the field — LLVM names this `%x` / `%x1` / similar
    // depending on inkwell renaming, so we just check `store i64 %`.
    assert!(
        main_body.contains("store i64 %") && main_body.contains(", ptr %kara.arg0.field_ptr"),
        "store must carry an SSA-named loaded value into field 1:\n{main_body}"
    );
}

#[test]
fn test_method_call_intercept_multi_arg_stores_args_after_receiver() {
    // A method with additional args: receiver into field 1, args
    // into fields 2..K. Pins that the receiver claims field 1 and
    // method args shift past it.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub {
                 fn run(self, n: i64) { fetch(); }
             }
             fn main() {
                 let h = Hub { count: 0 };
                 h.run(42);
             }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    // Receiver stored into field 1.
    assert!(
        main_body
            .contains("getelementptr inbounds %kara.state.Hub.run, ptr %kara.state, i32 0, i32 1"),
        "receiver must GEP into field 1:\n{main_body}"
    );
    // Arg n=42 stored into field 2.
    assert!(
        main_body
            .contains("getelementptr inbounds %kara.state.Hub.run, ptr %kara.state, i32 0, i32 2"),
        "first method arg must GEP into field 2 (after receiver):\n{main_body}"
    );
    assert!(
        main_body.contains("store i64 42, ptr %kara.arg0.field_ptr"),
        "method arg literal 42 must be stored into field 2:\n{main_body}"
    );
}

#[test]
fn test_e2e_f64_parse_basic() {
    // Float parse (decimal / scientific / negative / reject / integer-form)
    // — the self-hosting lexer's float-literal path. Exercises the
    // Option[f64] payload bitcast end-to-end.
    let output = run_program(
        "fn pr(o: Option[f64]) { match o { Some(x) => println(x), None => println(-1.0) } }\n\
             fn main() {\n\
                 pr(f64.parse(\"3.14\"));\n\
                 pr(f64.parse(\"1e10\"));\n\
                 pr(f64.parse(\"-2.5\"));\n\
                 pr(f64.parse(\"notnum\"));\n\
                 pr(f64.parse(\"42\"));\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "3.14\n10000000000\n-2.5\n-1\n42\n");
}

#[test]
fn test_e2e_modbind_two_distinct_bindings_independent() {
    // Two distinct `let mut` bindings get their own globals; a
    // write to one doesn't leak into the other. Mirrors the
    // slice-6 effect-checker test which proves synthetic
    // resources are per-binding; this is the codegen analogue.
    let output = run_program(
        "let mut LHS: i64 = 0;\n\
             let mut RHS: i64 = 0;\n\
             fn main() {\n\
                 LHS = 7;\n\
                 RHS = 11;\n\
                 println(LHS);\n\
                 println(RHS);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "7\n11\n");
}

#[test]
fn test_e2e_modbind_read_from_callee_observes_writes_from_caller() {
    // Writer in main, reader in private fn — confirms the
    // global-state visibility property end-to-end. If the read
    // path resolved to a stale local snapshot instead of the
    // global, the second `read()` call would print `0`.
    let output = run_program(
        "let mut STATE: i64 = 0;\n\
             fn read() -> i64 { STATE }\n\
             fn main() {\n\
                 println(read());\n\
                 STATE = 100;\n\
                 println(read());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "0\n100\n");
}

#[test]
fn test_e2e_modbind_computed_cross_referencing_initializer() {
    // B-2026-07-11-16: a COMPUTED / cross-referencing module-binding
    // initializer (`let DOUBLED = COUNT * 2`, referencing another binding) is
    // not a foldable const shape, so it is deferred to `__karac_static_init`
    // (compiled via `compile_expr`, stored before `main`). Declaration order
    // is preserved, so `TRIPLED = DOUBLED + COUNT` sees DOUBLED's computed
    // value. Matches the interpreter output exactly (run==build parity).
    let output = run_program(
        "let COUNT: i64 = 42;\n\
             let DOUBLED: i64 = COUNT * 2;\n\
             let TRIPLED: i64 = DOUBLED + COUNT;\n\
             let SUM: i64 = 2 + 3 * 4;\n\
             fn main() {\n\
                 println(COUNT);\n\
                 println(DOUBLED);\n\
                 println(TRIPLED);\n\
                 println(SUM);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "42\n84\n126\n14\n");
}

#[test]
fn test_e2e_modbind_computed_unannotated_initializer() {
    // B-2026-07-11-16 residual: a COMPUTED binding with NO `: TYPE`
    // annotation (`let DOUBLED = COUNT * 2;`). There is no declared type to
    // size the placeholder global, so codegen sizes it from the
    // typechecker's inferred type (`program.module_binding_types`, threaded
    // through lowering). Both an i64 chain and an i32 binding are exercised
    // to confirm the inferred width is honored (not defaulted to i64).
    // Matches the interpreter output exactly (run==build parity).
    let output = run_program(
        "let COUNT: i64 = 42;\n\
             let DOUBLED = COUNT * 2;\n\
             let TRIPLED = DOUBLED + COUNT;\n\
             let SMALL: i32 = 7i32;\n\
             let SMALL2 = SMALL + 3i32;\n\
             fn main() {\n\
                 println(DOUBLED);\n\
                 println(TRIPLED);\n\
                 println(SMALL2);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "84\n126\n10\n");
}

#[test]
fn test_e2e_modbind_two_distinct_maps_independent() {
    // Two distinct module-scope Map bindings each get their own global
    // handle filled by the prologue; a write to one doesn't leak into
    // the other (the static-init emits one karac_map_new per binding).
    let src = "let mut LHS: Map[i64, i64] = Map.new();\n\
                   let mut RHS: Map[i64, i64] = Map.new();\n\
                   fn main() {\n\
                       LHS.insert(1, 100);\n\
                       RHS.insert(1, 200);\n\
                       println(LHS.get(1).unwrap_or(0));\n\
                       println(RHS.get(1).unwrap_or(0));\n\
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
        "two module-scope Maps must typecheck clean, got: {:?}",
        typed.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
    let output = run_program(src).expect("compile + run failed");
    assert_eq!(output, "100\n200\n");
}

#[test]
fn test_ir_modbind_emits_internal_global() {
    // The lowered IR contains the global with `internal` linkage
    // and the right constant/non-constant flag for the
    // `let` / `let mut` distinction.
    let ir = ir_for(
        "let MAX: i64 = 100;\n\
             let mut COUNTER: i64 = 0;\n\
             fn main() { println(MAX); println(COUNTER); }",
    );
    assert!(
        ir.contains("@MAX = internal constant i64 100"),
        "expected `@MAX = internal constant i64 100` in IR, got:\n{}",
        ir
    );
    assert!(
        ir.contains("@COUNTER = internal global i64 0"),
        "expected `@COUNTER = internal global i64 0` in IR, got:\n{}",
        ir
    );
}

#[test]
fn test_ir_modbind_vecdeque_new_emits_empty_aggregate() {
    // `VecDeque.new()` shares Vec's `{ptr, len, cap}` layout per
    // `assoc_call.rs`; same const-init lowering applies.
    let ir = ir_for(
        "let mut QUEUE: VecDeque[i64] = VecDeque.new();\n\
             fn main() {}",
    );
    assert!(
        ir.contains("@QUEUE = internal global { ptr, i64, i64 } zeroinitializer"),
        "expected @QUEUE VecDeque global as zeroinitializer, got:\n{}",
        ir
    );
}

/// B-2026-08-30-55, leg 2 — compiled twin of `tests/interpreter.rs`'s
/// `method_frame_retracts_to_the_caller_only_when_the_caller_fires`, same
/// programs and same expectations.
#[test]
fn test_e2e_method_frame_retracts_to_the_caller_only_when_the_caller_fires() {
    const H: &str = "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
             struct T { n: i64 }\n";
    for (label, body, want) in [
        (
            "assign-spelling",
            "impl T { #[allow(partial_move_of_drop_enum)]\nfn take(ref self, b: E) -> i64 {\n\
                 let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                 match b { E.A(r) => { out = r; } E.B => { } }\n\
                 println(\"m\"); return out.id; } }\n\
                 fn main() { let t: T = T { n: 1 }; let c: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 let v: i64 = t.take(c); println(f\"v{v}\") }\n",
            "dR0\nm\ndE\ndR8\nv8\n",
        ),
        (
            "let-spelling",
            "impl T { #[allow(partial_move_of_drop_enum)]\nfn take(ref self, b: E) -> i64 {\n\
                 match b { E.A(r) => { let inner: R = r; println(\"m\"); return inner.id; }\n\
                 E.B => { } }\n\
                 return 0; } }\n\
                 fn main() { let t: T = T { n: 1 }; let c: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 let v: i64 = t.take(c); println(f\"v{v}\") }\n",
            "m\ndE\ndR8\nv8\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{H}{body}")),
            Some(want.to_string()),
            "{label}"
        );
    }
}

#[test]
fn test_e2e_modbind_repeat_literal() {
    // `[v; n]` materialises an `[n x T]` constant with each slot
    // initialised to `v`. Verifies the count fold + element-value
    // replication path.
    let output = run_program(
        "let REPS: Array[i64, 4] = [7; 4];\n\
             fn main() {\n\
                 let mut s: i64 = 0;\n\
                 let mut i: i64 = 0;\n\
                 while i < 4 {\n\
                     s = s + REPS[i];\n\
                     i = i + 1;\n\
                 }\n\
                 println(s);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "28\n");
}

// ── Large-N sort ────────────────────────────────────────────────
//
// The runtime `karac_vec_sort_by` is a hand-rolled stable, panic-free
// merge sort (replaced `slice::sort_by` to drop the ~262 KiB DWARF
// symbolizer floor). It is still the path for comparator shapes the
// mono gate declines — a capturing closure, a named function, a
// closure-typed local, or a non-eligible element type.
//
// It is NO LONGER selected by length. Until B-2026-07-30-2 a runtime
// `len > 64` check sent larger sorts here because the mono path was an
// O(N²) insertion sort; the mono path is now a stable O(N log N) merge
// sort, so an inline non-capturing comparator inlines at every N. These
// cases still exercise the runtime path for correctness (asc/desc,
// multiple elem sizes) and stability. See
// docs/implementation_checklist/phase-7-codegen.md "Lean large-N sort
// entry".

#[test]
fn test_e2e_large_n_sort_by_ascending() {
    // 100 pseudo-shuffled i64 (> 64 → runtime merge sort). Assert fully
    // sorted by counting inversions in-program (0 ⇒ sorted).
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    let mut i: i64 = 0;
    while i < 100 {
        v.push((i * 37 + 11) % 100);
        i = i + 1;
    }
    v.sort_by(|a, b| a.cmp(b));
    let mut bad: i64 = 0;
    let mut prev: i64 = -1;
    for x in v.iter() {
        if x < prev { bad = bad + 1; }
        prev = x;
    }
    println(bad);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0");
    }
}

/// B-2026-08-10-9: the mono sort is a NATURAL-RUN merge sort, so input
/// ORDER now selects the code path — a strictly-descending run is
/// detected and reversed in place. The extension test must stay STRICT
/// (`cmp > 0`): reversing a run that contains equal keys would invert
/// them and break stability, which design.md requires.
///
/// Both halves have teeth. `.1` carries the original position, so with a
/// non-strict (`>=`) extension the first block prints 33 before 32 and
/// the second prints 40 before 39. The first case's descending run is
/// shorter than RUN=32 so it is then padded by insertion sort; the
/// second is longer, so it is recorded as-is and the padding never runs
/// — the two exercise different exits from phase 1.
#[test]
fn natural_run_sort_keeps_descending_runs_stable() {
    let out = run_program(
        r#"
fn main() {
    // Short strictly-descending run ending at a TIE: 5,4,3,3,2,1.
    let mut v: Vec[(i64, i64)] = Vec.new();
    v.push((5i64, 0i64)); v.push((4i64, 1i64)); v.push((3i64, 2i64));
    v.push((3i64, 3i64)); v.push((2i64, 4i64)); v.push((1i64, 5i64));
    v.sort_by(|x, y| x.0.cmp(y.0));
    let mut i: i64 = 0i64;
    while i < v.len() {
        println(v[i].0 * 10i64 + v[i].1);
        i = i + 1i64;
    }
    // Descending run LONGER than RUN=32, also ending at a tie.
    let mut w: Vec[(i64, i64)] = Vec.new();
    let mut j: i64 = 0i64;
    while j < 40i64 {
        w.push((100i64 - j, j));
        j = j + 1i64;
    }
    w.push((61i64, 40i64));
    w.sort_by(|x, y| x.0.cmp(y.0));
    println(w[0i64].1);
    println(w[1i64].1);
}
"#,
    );
    assert_eq!(
        out.as_deref().map(str::trim),
        Some("15\n24\n32\n33\n41\n50\n39\n40")
    );
}

/// B-2026-08-10-9 sibling: sortedness AND stability over the input
/// shapes the run detector treats differently (one ascending run, one
/// descending run, an all-equal run, and shuffled-with-ties), at sizes
/// straddling RUN=32 where phase 1 switches between padding a short run
/// and keeping a natural one. Counts violations in-program so a single
/// `0` covers all 24 combinations; `.1` is the original index, so an
/// out-of-order equal pair counts as a violation too.
#[test]
fn natural_run_sort_stable_across_shapes_and_sizes() {
    let out = run_program(
        r#"
fn viol(v: Vec[(i64, i64)]) -> i64 {
    let mut bad: i64 = 0i64;
    let mut i: i64 = 1i64;
    while i < v.len() {
        let p: (i64, i64) = v[i - 1i64];
        let c: (i64, i64) = v[i];
        if p.0 > c.0 { bad = bad + 1i64; }
        if p.0 == c.0 and p.1 > c.1 { bad = bad + 1i64; }
        i = i + 1i64;
    }
    return bad;
}
fn main() {
    let mut bad: i64 = 0i64;
    let mut si: i64 = 0i64;
    while si < 6i64 {
        let mut n: i64 = 0i64;
        if si == 0i64 { n = 31i64; }
        if si == 1i64 { n = 32i64; }
        if si == 2i64 { n = 33i64; }
        if si == 3i64 { n = 63i64; }
        if si == 4i64 { n = 64i64; }
        if si == 5i64 { n = 65i64; }
        let mut k: i64 = 0i64;
        while k < 4i64 {
            let mut v: Vec[(i64, i64)] = Vec.new();
            let mut i: i64 = 0i64;
            while i < n {
                let mut key: i64 = 0i64;
                if k == 0i64 { key = i; }
                if k == 1i64 { key = n - i; }
                if k == 2i64 { key = 7i64; }
                if k == 3i64 { key = (i * 37i64) % 11i64; }
                v.push((key, i));
                i = i + 1i64;
            }
            v.sort_by(|x, y| x.0.cmp(y.0));
            bad = bad + viol(v);
            k = k + 1i64;
        }
        si = si + 1i64;
    }
    println(bad);
}
"#,
    );
    assert_eq!(out.as_deref().map(str::trim), Some("0"));
}

#[test]
fn test_e2e_large_n_sort_by_descending() {
    // Custom descending comparator over 80 elements (> 64).
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    let mut i: i64 = 0;
    while i < 80 {
        v.push((i * 53 + 7) % 80);
        i = i + 1;
    }
    v.sort_by(|a, b| b.cmp(a));
    let mut bad: i64 = 0;
    let mut prev: i64 = 1000000;
    for x in v.iter() {
        if x > prev { bad = bad + 1; }
        prev = x;
    }
    println(bad);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0");
    }
}

#[test]
fn test_e2e_large_n_sort_by_is_stable() {
    // Stability gate: 80 records with duplicate keys (`.0 = i % 8`) and a
    // strictly increasing tag (`.1 = i`). Sorting by `.0` ALONE must
    // keep equal-key records in original (`.1`-ascending) order. Counts
    // both unsorted-key and stability violations; 0 ⇒ stable sort.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut i: i64 = 0;
    while i < 80 {
        v.push((i % 8, i));
        i = i + 1;
    }
    v.sort_by(|x, y| x.0.cmp(y.0));
    let mut bad: i64 = 0;
    let mut pk: i64 = -1;
    let mut po: i64 = -1;
    let mut j: i64 = 0;
    while j < 80 {
        let t = v.get(j).unwrap();
        if t.0 < pk { bad = bad + 1; }
        if t.0 == pk {
            if t.1 < po { bad = bad + 1; }
        }
        pk = t.0;
        po = t.1;
        j = j + 1;
    }
    println(bad);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0");
    }
}

#[test]
fn e2e_user_cmp_method_call_is_not_answered_by_the_builtin_comparator() {
    // The silent-wrong-answer half of B-2026-08-26-10, and the reason the
    // operator fix could not stop at lowering.
    //
    // codegen's builtin `method == "cmp"` arm ran BEFORE user-impl dispatch,
    // and its own comment asserted "user-struct `.cmp` is rejected at
    // typecheck and never arrives" — untrue since `type_supports_ord` grew
    // its `has_user_impl_ord` fallback. A single-field struct lowers to a
    // bare int, so the call matched the arm's `(IntValue, IntValue)` case and
    // was answered by a DECLARATION-ORDER integer compare.
    //
    // `cmp` returns `Ordering.Greater` unconditionally here, so the body says
    // `is_lt() == false` while a field compare of 1 vs 5 says `true`. The
    // interpreter said `false` and the compiled binary said `true`: a wrong
    // answer in compiled output only, invisible to any single-backend test.
    // The interpreter twin
    // (`test_user_cmp_method_call_is_not_answered_by_the_builtin_comparator`)
    // pins the other side of that pair.
    let out = run_program(
        r#"
struct Item { id: i64 }
impl PartialEq for Item { fn eq(ref self, other: ref Item) -> bool { self.id == other.id } }
impl Eq for Item {}
impl PartialOrd for Item { fn partial_cmp(ref self, other: ref Item) -> Option[Ordering] { Some(Ordering.Greater) } }
impl Ord for Item { fn cmp(ref self, other: ref Item) -> Ordering { Ordering.Greater } }
fn main() {
    let a = Item { id: 1 };
    let b = Item { id: 5 };
    println(a.cmp(b).is_lt());
    println(a < b);
}
"#,
    );
    let out = out.expect("user-`impl Ord` `.cmp` must build");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["false", "false"]);
}

#[test]
fn e2e_priority_queue_honours_a_hand_written_ord_impl() {
    // B-2026-08-26-24 — the shipping casualty. `PriorityQueue`'s sift loops
    // compare `self.xs[i] < self.xs[j]` with `T: Ord`, and operator lowering
    // resolved only CONCRETE operand types, so a type-param comparison
    // survived unlowered into both backends: "operator 'Lt' is not defined
    // for operands of type 'Struct'" / "Unsupported struct binary op: Gt".
    // The queue worked for `#[derive(Ord)]`, whose ordering reaches the
    // declaration-order comparator without passing through the operator
    // rewrite, and failed for EVERY hand-written impl.
    //
    // The impl here REVERSES the order, so the queue must pop 3, 2, 1. A
    // derive would pop 1, 2, 3 — asserted separately below so a regression
    // that quietly falls back to declaration order fails rather than passes.
    let out = run_program(
        r#"
struct Item { id: i64 }
impl PartialEq for Item { fn eq(ref self, other: ref Item) -> bool { self.id == other.id } }
impl Eq for Item {}
impl PartialOrd for Item { fn partial_cmp(ref self, other: ref Item) -> Option[Ordering] { Some(other.id.cmp(self.id)) } }
impl Ord for Item { fn cmp(ref self, other: ref Item) -> Ordering { other.id.cmp(self.id) } }
fn main() {
    let mut q: PriorityQueue[Item] = PriorityQueue.new();
    q.push(Item { id: 1 });
    q.push(Item { id: 3 });
    q.push(Item { id: 2 });
    while q.len() > 0 { match q.pop() { Some(it) => println(it.id), None => {} } }
}
"#,
    );
    assert_eq!(
        out.expect("a PriorityQueue over a hand-written Ord must build")
            .trim(),
        "3
2
1"
    );

    // The derive path must be untouched by the fix.
    let derived = run_program(
        r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Item { id: i64 }
fn main() {
    let mut q: PriorityQueue[Item] = PriorityQueue.new();
    q.push(Item { id: 1 });
    q.push(Item { id: 3 });
    q.push(Item { id: 2 });
    while q.len() > 0 { match q.pop() { Some(it) => println(it.id), None => {} } }
}
"#,
    );
    assert_eq!(
        derived.expect("the derived queue must still build").trim(),
        "1
2
3"
    );
}

#[test]
fn e2e_partial_ord_alone_drives_the_operators_through_partial_cmp() {
    // design.md § Comparison Traits specifies `a < b` as
    // `PartialOrd.partial_cmp(ref a, ref b).is_lt()` and says the operators
    // go "through `PartialOrd`, never `Ord` directly". The compiler could
    // not do that until B-2026-08-26-23: `Option[Ordering].is_lt()` had no
    // codegen lowering, so the desugaring was routed through `Ord.cmp` and
    // an `impl PartialOrd` with no `impl Ord` was REJECTED rather than
    // accepted into a program that would check, interpret, then fail to
    // build.
    //
    // `partial_cmp` REVERSES the order here, so declaration order and the
    // body disagree on every one of the four operators — a test whose
    // comparator agreed with declaration order would pass without the body
    // ever being called.
    let out = run_program(
        r#"
struct Item { id: i64 }
impl PartialEq for Item { fn eq(ref self, other: ref Item) -> bool { self.id == other.id } }
impl PartialOrd for Item {
    fn partial_cmp(ref self, other: ref Item) -> Option[Ordering] { Some(other.id.cmp(self.id)) }
}
fn main() {
    let a = Item { id: 1 };
    let b = Item { id: 5 };
    println(f"{a < b} {a > b} {a <= b} {a >= b}");
}
"#,
    );
    // Reversed: a(1) vs b(5) compares as Greater, so `<` is false and `>`
    // is true — the opposite of what the field values suggest.
    assert_eq!(
        out.expect("`impl PartialOrd` alone must build").trim(),
        "false true false true"
    );
}

#[test]
fn e2e_ordering_predicates_build_on_every_receiver_shape() {
    // B-2026-08-26-23 — three shapes ran correctly under `--interp` and
    // failed to BUILD, each for its own reason:
    //
    //   1. a CHAINED `a.cmp(b).is_lt()` on a primitive, and
    //   2. the same comparator bound to a local first,
    //      both because the builtin `cmp` arm materializes its `Ordering`
    //      aggregate inline with no declaration, so nothing named the
    //      result type (a user `impl Ord`'s `cmp` IS declared, which is why
    //      that shape always worked);
    //   3. EVERY `Option[Ordering]` receiver, including a plainly annotated
    //      local, because `impl Option[Ordering]`'s five predicates lived in
    //      `option.kara`, which codegen does not compile.
    //
    // All three are asserted here rather than only the chained one, because
    // they were three separate defects wearing one error message ("no
    // handler for method 'is_lt'").
    let out = run_program(
        r#"
fn main() {
    let a: i64 = 1;
    let b: i64 = 5;
    println(a.cmp(b).is_lt());
    let o = a.cmp(b);
    println(o.is_gt());
    let lt: Option[Ordering] = Some(Ordering.Less);
    let no: Option[Ordering] = None;
    println(f"{lt.is_lt()} {lt.is_ge()}");
    println(f"{no.is_lt()} {no.is_eq()}");
}
"#,
    );
    let out = out.expect("all three Ordering-predicate shapes must build");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(
        lines,
        vec![
            "true",  // 1 < 5
            "false", // not greater
            // `Some(Less)`: is_lt yes, is_ge no.
            "true false",
            // `None` is INCOMPARABLE — design.md § Comparison Traits has
            // every predicate answer false, matching IEEE-754's `NaN < 5.0`
            // and `NaN == NaN` both being false. Asserted because a naive
            // "unwrap and compare" lowering would answer true for `is_eq`.
            "false false",
        ]
    );
}

#[test]
fn e2e_derived_ord_still_compares_by_declaration_order() {
    // The counterweight to the three tests above: `#[derive(Ord)]` must keep
    // comparing field-by-field in DECLARATION ORDER. The operator fix routes
    // to a user `cmp` only when an ordering impl supplies one — a derived
    // type has no impl body, so nothing about its lowering changed, and this
    // pins that. Same `Pair` shape as the dispatch test, so the two answers
    // are directly opposed: `b`-only dispatch says `true`, declaration order
    // (reading `a` first, 9 vs 1) says `false`.
    let out = run_program(
        r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Pair { a: i64, b: i64 }
fn main() {
    let p1 = Pair { a: 9, b: 1 };
    let p2 = Pair { a: 1, b: 9 };
    println(p1 < p2);
    println(p1 > p2);
}
"#,
    );
    let out = out.expect("derived-`Ord` comparison must build");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["false", "true"]);
}

#[test]
fn test_e2e_zip_with_rejects_noninline() {
    // Same inline-literal boundary as `map` / the folds, on both containers.
    let col = r#"
fn main() {
    let a: Column[i64] = Column.from_vec([1, 2, 3]);
    let b: Column[i64] = Column.from_vec([4, 5, 6]);
    let g = |x: i64, y: i64| x + y;
    let z = a.zip_with(b, g);
    println(f"{z.sum()}");
}
"#;
    let err = ir_result(col).expect_err("a non-inline closure must be rejected");
    assert!(err.contains("inline closure literal"), "got: {err}");

    let ten = r#"
fn main() {
    let a: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);
    let b: Tensor[i64, [3]] = Tensor.from([4, 5, 6]);
    let g = |x: i64, y: i64| x + y;
    let z = a.zip_with(b, g);
    println(f"{z.sum()}");
}
"#;
    let err = ir_result(ten).expect_err("a non-inline closure must be rejected");
    assert!(err.contains("inline closure literal"), "got: {err}");
}

#[test]
fn test_e2e_stdlib_reduce_default_method_inherited_by_user_impl() {
    // S6b-4 (B-2026-07-03-19): a user `impl Reduce[T] for MyType` inherits
    // the BAKED stdlib trait's DEFAULT method `range` (`max - min`) without
    // writing a body. `synthesize_trait_default_methods` now also collects
    // default-bodied methods from `STDLIB_PROGRAMS` (not just user-declared
    // traits), substitutes the impl's concrete `T`, and splices the body
    // into the impl — so `self.max() - self.min()` lowers as ordinary
    // `T - T` arithmetic. Covers an i64 and an f64 implementor; both `run`
    // and `build` (and default auto-par) agree. Generic user impls
    // (`impl[T: Sub] Reduce[T] for Pair[T]`) are blocked on a pre-existing
    // bounded-generic-impl method-resolution gap (B-2026-07-03-20), not this
    // slice.
    let src = r#"
struct Trio { a: i64, b: i64, c: i64 }
impl Reduce[i64] for Trio {
    fn sum(ref self) -> i64 { self.a + self.b + self.c }
    fn min(ref self) -> i64 {
        let mut m = self.a;
        if self.b < m { m = self.b; }
        if self.c < m { m = self.c; }
        m
    }
    fn max(ref self) -> i64 {
        let mut m = self.a;
        if self.b > m { m = self.b; }
        if self.c > m { m = self.c; }
        m
    }
    fn mean(ref self) -> f64 { (self.sum() as f64) / 3.0 }
}
struct FDuo { x: f64, y: f64 }
impl Reduce[f64] for FDuo {
    fn sum(ref self) -> f64 { self.x + self.y }
    fn min(ref self) -> f64 { if self.x < self.y { self.x } else { self.y } }
    fn max(ref self) -> f64 { if self.x > self.y { self.x } else { self.y } }
    fn mean(ref self) -> f64 { self.sum() / 2.0 }
}
fn main() {
    let t = Trio { a: 5, b: 2, c: 8 };
    println(f"{t.range()}");
    let d = FDuo { x: 1.5, y: 9.0 };
    println(f"{d.range()}");
}
"#;
    // Trio range = 8 - 2 = 6; FDuo range = 9.0 - 1.5 = 7.5.
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "6\n7.5\n");
}

#[test]
fn test_void_fn_tail_call_without_semicolon_emits_ret_void() {
    // Pre-fix: `fn helper() { println(1) }` (no trailing `;`)
    // tripped LLVM module verification with "Found return instr
    // that returns non-void in Function of void return type! ret
    // i64 0 void". `compile_print` returns an i64-0 unit
    // placeholder; the parser treats the no-`;` call as the
    // block's `final_expr`, so `compile_block` hands it back as
    // `Some(val)`; `compile_function` then emitted
    // `build_return(Some(i64-0))` against a void LLVM signature.
    // Post-fix: the LLVM fn's return type is checked; void
    // signatures emit `build_return(None)` regardless of any
    // placeholder value compile_block surfaced.
    let out = run_program(
        r#"
fn helper() {
    println(7)
}

fn main() {
    helper();
}
"#,
    );
    if let Some(s) = out {
        assert_eq!(s.trim(), "7");
    }
}

#[test]
fn test_void_main_tail_call_without_semicolon() {
    // Same bug shape exercised at `main`'s tail. `main` returns
    // i32 (via the dedicated main arm in compile_function), but
    // the body's final-expr handling is uniform — pre-fix this
    // emitted `ret i32` against a value from `compile_block`'s
    // `Some(val)`. With the main arm overriding to const-i32-0,
    // this specific shape always worked; pin it as a regression
    // test against future drift.
    let out = run_program(
        r#"
fn main() {
    println(11)
}
"#,
    );
    if let Some(s) = out {
        assert_eq!(s.trim(), "11");
    }
}

// ── B-2026-08-18-5: unsigned refinement/contract predicates ──
//
// A primitive scalar binop is normally rewritten by lowering into
// `Call(Path([u16, le]))`, whose receiver carries the signedness. A
// SYNTHESIZED predicate never goes through lowering, so it reached
// `compile_binop`, which hardcodes `is_unsigned = false` — and an
// unsigned operand was compared with the SIGNED predicate. `self <=
// 65535` on a `u16` emitted `icmp sle i16 %self, -1`, so a valid value
// was rejected at run time with `contract violated` while the
// interpreter accepted it. The failure boundary was exactly "the bound's
// high bit is set for the base width", which is the sign-bit reading.

#[test]
fn test_ir_unsigned_refinement_predicate_uses_unsigned_compare() {
    // The mechanism, pinned at the IR: `uge`/`ule`, never `sge`/`sle`.
    // The bound's BITS were always right (65535 is 0xFFFF is i16 -1) —
    // only the interpretation was wrong, so the predicate IS the fix.
    let ir = ir_for(
        "distinct type P = u16 where self >= 1 and self <= 65535;\n\
             fn main() { let p = P(80); println(\"built\"); }",
    );
    let pred: Vec<&str> = ir
        .lines()
        .filter(|l| l.contains("__karac_refine_self") && l.contains("icmp"))
        .collect();
    assert!(!pred.is_empty(), "no refinement compare in IR:\n{ir}");
    for l in &pred {
        assert!(
            !l.contains("icmp sle") && !l.contains("icmp sge"),
            "unsigned refinement compared with a SIGNED predicate: {l}"
        );
    }
}

// ── Refinement runtime predicate emission (phase-9 step 5c) ──

#[test]
fn test_e2e_refinement_as_cast_predicate_holds() {
    // `4 as Even` passes `self % 2 == 0`, so the value flows through and
    // execution continues normally (no fault).
    let out = run_program(
        r#"
type Even = i64 where self % 2 == 0;
fn main() {
    let e = 4 as Even;
    println(e);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "4");
    }
}

#[test]
fn test_e2e_refinement_as_cast_violation_aborts() {
    // `3 as Even` fails the predicate: codegen emits a contract-violation
    // abort, and code after the cast does not run.
    let captured = run_program_capturing(
        r#"
type Even = i64 where self % 2 == 0;
fn main() {
    let e = 3 as Even;
    println(e);
    println(42);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated") && c.stderr.contains("Even"),
            "expected a contract-violation abort naming Even, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("42"),
            "code after a violated refinement cast must not run"
        );
    }
}

// ── ExitCode entry-point return type (Phase-8 Slice B) ─────────

#[test]
fn test_e2e_main_exitcode_success() {
    // `main() -> ExitCode { ... ExitCode.SUCCESS }` exits 0.
    let cap = run_program_capturing(
        r#"
fn main() -> ExitCode {
    println("ran");
    ExitCode.SUCCESS
}
"#,
    );
    if let Some(cap) = cap {
        assert_eq!(cap.status.code(), Some(0), "stderr={:?}", cap.stderr);
        assert_eq!(cap.stdout.trim(), "ran");
    }
}

#[test]
fn test_e2e_main_exitcode_failure() {
    // `ExitCode.FAILURE` exits 1 (the literal `1`, not EXIT_FAILURE).
    let cap = run_program_capturing(
        r#"
fn main() -> ExitCode {
    ExitCode.FAILURE
}
"#,
    );
    if let Some(cap) = cap {
        assert_eq!(cap.status.code(), Some(1), "stderr={:?}", cap.stderr);
    }
}

#[test]
fn test_e2e_main_exitcode_from_arbitrary() {
    // `ExitCode.from(code)` returns an arbitrary process exit code.
    let cap = run_program_capturing(
        r#"
fn main() -> ExitCode {
    ExitCode.from(42)
}
"#,
    );
    if let Some(cap) = cap {
        assert_eq!(cap.status.code(), Some(42), "stderr={:?}", cap.stderr);
    }
}

#[test]
fn test_e2e_inverse_hyperbolics_agree_with_the_interpreter() {
    // The COMPILED half of the oracle pair for B-2026-08-29-60, and the
    // half that states the A/B rule directly: the compiled program must
    // print what `karac run --interp` prints. Codegen was already right
    // here (it calls `asinh` / `asinhf`); the interpreter was evaluating
    // Rust's std FORMULA, a different algorithm, so this pair could not be
    // closed by choosing a rounding width.
    //
    // Expectations come from libm rather than being hardcoded, exactly as
    // in the interpreter twin — the invariant is "both backends call the
    // same symbol", and freezing one host's bits would assert something
    // narrower and break on another libm. Every input is one where Rust's
    // formula and libm disagree at both widths; character-identical to
    // `tests/interpreter.rs::
    //  test_inverse_hyperbolics_agree_with_the_libm_symbols_codegen_calls`.
    extern "C" {
        fn asinh(x: f64) -> f64;
        fn asinhf(x: f32) -> f32;
        fn acosh(x: f64) -> f64;
        fn acoshf(x: f32) -> f32;
        fn atanh(x: f64) -> f64;
        fn atanhf(x: f32) -> f32;
    }
    let cases: &[(&str, f64)] = &[
        ("asinh", 8.8),
        ("asinh", 9.9),
        ("asinh", 17.1),
        ("acosh", 1.1),
        ("acosh", 1.6),
        ("acosh", 3.7),
        ("atanh", 0.03),
        ("atanh", 0.04),
    ];
    let mut src = String::from("fn main() {\n");
    let mut want = String::new();
    for (i, (m, v)) in cases.iter().enumerate() {
        src.push_str(&format!("    let d{i}: f64 = {v};\n"));
        src.push_str(&format!("    println(d{i}.{m}());\n"));
        src.push_str(&format!("    let s{i}: f32 = {v}f32;\n"));
        src.push_str(&format!("    println(s{i}.{m}());\n"));
        let (wide, narrow) = unsafe {
            match *m {
                "asinh" => (asinh(*v), asinhf(*v as f32) as f64),
                "acosh" => (acosh(*v), acoshf(*v as f32) as f64),
                _ => (atanh(*v), atanhf(*v as f32) as f64),
            }
        };
        want.push_str(&format!("{wide}\n{narrow}\n"));
    }
    src.push_str("}\n");
    assert_eq!(run_program(&src), Some(want));
}

#[test]
fn test_e2e_cbrt_agrees_with_the_interpreter() {
    // The COMPILED half of the oracle pair for B-2026-08-30-4, twinned
    // with `tests/interpreter.rs::
    // test_cbrt_agrees_with_the_libm_symbol_codegen_calls` — read that one
    // for why `cbrt` needed a shim at all and why it is not simply a
    // fourth inverse hyperbolic.
    //
    // Under `KARAC_TEST_JIT=1` this fixture also runs on the JIT lane,
    // which is where the row's real hazard lived: `compiler_builtins`
    // ships a weak `cbrt`/`cbrtf` that shadows the platform libm's in any
    // Rust link, so the interpreter and this AOT binary agree while the
    // JIT — resolving through `dlsym`, which cannot see a local archive
    // symbol — used to get the other implementation. The unconditional
    // pin for that lane is `tests/lljit_e2e.rs::
    // jit_e2e_cbrt_resolves_the_implementation_the_other_lanes_link`.
    //
    // Expectation COMPUTED from the linked symbol rather than hardcoded,
    // as in the inverse-hyperbolic pair: the invariant is that every lane
    // calls one implementation, not that the answer is these bits on this
    // host. Verified byte-identical under `karac run --interp`,
    // `karac run`, `karac build`, and `KARAC_AUTO_PAR=0 karac build`.
    extern "C" {
        fn cbrt(x: f64) -> f64;
        fn cbrtf(x: f32) -> f32;
    }
    let cases: &[f64] = &[27.0, 0.2, 1.6, 4.1, 4.7, 5.3, 7.3, 7.9, -10.6];
    let mut src = String::from("fn main() {\n");
    let mut want = String::new();
    for (i, v) in cases.iter().enumerate() {
        src.push_str(&format!("    let d{i}: f64 = {v:?};\n"));
        src.push_str(&format!("    println(d{i}.cbrt());\n"));
        src.push_str(&format!("    let s{i}: f32 = {v:?}f32;\n"));
        src.push_str(&format!("    println(s{i}.cbrt());\n"));
        let (wide, narrow) = unsafe { (cbrt(*v), cbrtf(*v as f32) as f64) };
        want.push_str(&format!("{wide}\n{narrow}\n"));
    }
    src.push_str("}\n");
    assert_eq!(run_program(&src), Some(want));
}

// ── Stdlib non-builtin body compilation (L889 slice 1) ───────────────
//
// Before this slice codegen never walked STDLIB_PROGRAMS, so a real
// (non-`#[compiler_builtin]`) stdlib method like `Ordering.is_lt` had no
// emitted body and a call to it had no `Type.method` symbol to dispatch
// to. The generalized declare/compile passes now compile such bodies;
// `Ordering` is the first module wired in.

#[test]
fn test_stdlib_ordering_method_emits_function_ir() {
    // The compiled body must appear as an LLVM function. (It's pruned if
    // unused, so the program calls it.)
    let ir = ir_for(
        r#"
fn main() {
    let o = Ordering.Less;
    if o.is_lt() { println(1); } else { println(0); }
}
"#,
    );
    assert!(
        ir.contains("@\"Ordering.is_lt\"") || ir.contains("@Ordering.is_lt"),
        "expected the compiled `Ordering.is_lt` body in IR; got:\n{}",
        ir
    );
}

#[test]
fn test_stdlib_ordering_methods_run() {
    let out = run_program(
        r#"
fn main() {
    let lt = Ordering.Less;
    let gt = Ordering.Greater;
    let eq = Ordering.Equal;
    if lt.is_lt() { println(1); } else { println(0); }
    if gt.is_lt() { println(1); } else { println(0); }
    if gt.is_gt() { println(1); } else { println(0); }
    if eq.is_eq() { println(1); } else { println(0); }
    if eq.is_le() { println(1); } else { println(0); }
}
"#,
    );
    if let Some(out) = out {
        // lt.is_lt=1, gt.is_lt=0, gt.is_gt=1, eq.is_eq=1, eq.is_le=1
        assert_eq!(out, "1\n0\n1\n1\n1\n");
    }
}

#[test]
fn test_ir_chained_zip_reduce_fuses() {
    // `a.zip_with(b, |x,y| x*y).sum()` must FUSE the product + reduction
    // into one accumulating loop (`emit_fused_map_reduce`, `fmr.*` blocks/
    // slots) — the intermediate products tensor never materializes. The
    // fused emitter's names prove it fired instead of the materialize path,
    // and the fold still carries `reassoc` so the reduction vectorizes.
    let ir = ir_for(
        "fn d(a: ref Tensor[f32, [64]], b: ref Tensor[f32, [64]]) -> f32 \
             { a.zip_with(b, |x, y| x * y).sum() }\n\
             fn main() {\n\
                 let a: Tensor[f32, [64]] = Tensor.ones([64]);\n\
                 let b: Tensor[f32, [64]] = Tensor.ones([64]);\n\
                 println(d(a, b));\n\
             }\n",
    );
    assert!(
        ir.contains("fmr."),
        "chained `zip_with(...).sum()` must fuse (expect `fmr.*` blocks); IR:\n{ir}"
    );
    assert!(
        ir.contains("fadd reassoc"),
        "the fused reduction must keep `fadd reassoc` for vectorization; IR:\n{ir}"
    );
}

#[test]
fn test_e2e_expression_position_container_ctors() {
    // B-2026-08-02-12 — `Map.new()` / `Set.new()` / `SortedMap.new()` in
    // a non-`let` EXPRESSION position (push / push_back / Vec.insert
    // args, `Vec.filled` fill values) used to compile to the silent
    // `i64 0` assoc-call default: a NULL handle in the container slot,
    // segfaulting at the first element use. With the fix the handle is
    // built from the typechecker's resolved span-keyed type (the
    // container-method arms re-record the ctor arg post-unify), so
    // String keys get the right key size + content hash — `get` finds
    // what `insert` stored — and filled slots deep-clone independently
    // (incl. SortedMap, whose clone/drop/vec-elem dispatch heads were
    // widened onto Map's KaracMap paths).
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[Map[String, i64]] = Vec.new();
    v.push(Map.new());
    v.push(Map.new());
    let _ = v[0].insert(f"k{1}", 5);
    match v[0].get(f"k{1}") { Some(x) => println(f"got {x}"), None => println("miss") }
    println(f"m0 {v[0].len()} m1 {v[1].len()}");
    let mut g: Vec[Map[String, i64]] = Vec.filled(2, Map.new());
    let _ = g[0].insert(f"a{2}", 7);
    println(f"g0 {g[0].len()} g1 {g[1].len()}");
    let mut s: Vec[Set[i64]] = Vec.new();
    s.push(Set.new());
    let _ = s[0].insert(9i64);
    println(f"s0 {s[0].len()}");
    let mut d: Vec[SortedMap[i64, i64]] = Vec.filled(2, SortedMap.new());
    let _ = d[0].insert(7i64, 70i64);
    println(f"d0 {d[0].len()} d1 {d[1].len()}");
    let mut q: VecDeque[Map[String, i64]] = VecDeque.new();
    q.push_back(Map.new());
    let _ = q[0].insert(f"z{3}", 4);
    match q[0].get(f"z{3}") { Some(x) => println(f"qgot {x}"), None => println("qmiss") }
    let mut w: Vec[Set[i64]] = Vec.new();
    w.insert(0, Set.new());
    let _ = w[0].insert(9i64);
    println(f"w0 {w[0].len()}");
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "got 5\nm0 1 m1 0\ng0 1 g1 0\ns0 1\nd0 1 d1 0\nqgot 4\nw0 1\nend"
        );
    }
}

#[test]
fn test_e2e_indexed_elem_method_on_call_rhs_binding() {
    // `let combos = make();` (no annotation) followed by
    // `combos[0].len()` — the binding's element TypeExpr comes from
    // the typechecker's `pattern_binding_inner_types`, which spells
    // String as "str" (`Type::Str` → `type_to_type_expr`). The
    // indexed-receiver synth registration must accept that spelling
    // (`is_string_type_expr`), else method dispatch falls through
    // with "no handler for method 'len' on variable
    // '__indexed_elem_0'" (kata-22 bench, 2026-06-06).
    let out = run_program(
        r#"
fn make() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("hello");
    v
}

fn main() {
    let combos = make();
    println(combos[0].len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5");
    }
}

#[test]
fn test_e2e_secret_expose() {
    // `std.secret.Secret[T]` — `.expose()` is a `#[compiler_builtin]` field
    // borrow (returns the field-0 pointer under the `-> ref T` ABI). `karac
    // build` output must match `karac run`. Soft-skips without the runtime
    // archive, like the rest of the E2E suite.
    if let Some(out) = run_program(
        r#"
import std.secret.{Secret};
fn main() {
    let s = Secret.new(42);
    let v = s.expose();
    println(v);
    let t = Secret.new("hunter2");
    let w = t.expose();
    println(w);
}
"#,
    ) {
        assert_eq!(out, "42\nhunter2\n");
    }
}

#[test]
fn test_e2e_embeddings_batched() {
    // `std.embeddings` batched single-query forms (phase-11): score one
    // query against every row of an `[N, D]` corpus via `iter_axis` (the
    // B-2026-07-13-7 row-view fix unblocks this). Generic in BOTH N and D.
    // Exact oracles: query [1,0] vs rows {[1,0], [0,1]} → cosine [1, 0],
    // dot [1, 0].
    if let Some(out) = run_program(
        r#"
import std.embeddings.{cosine_similarity_batched, dot_batched};
fn main() {
    let q: Tensor[f32, [2]] = Tensor.from([1.0f32, 0.0f32]);
    let corpus: Tensor[f32, [2, 2]] = Tensor.from([[1.0f32, 0.0f32], [0.0f32, 1.0f32]]);
    let cs = cosine_similarity_batched(q, corpus);
    println(cs[0]);
    println(cs[1]);
    let db = dot_batched(q, corpus);
    println(db[0]);
    println(db[1]);
}
"#,
    ) {
        assert_eq!(out, "1\n0\n1\n0\n");
    }
}

#[test]
fn test_e2e_embeddings_cosine_similarity_matrix() {
    // `std.embeddings.cosine_similarity_matrix` (phase-11): the Q×N
    // bulk-scoring path — a `[Q, D]` query block vs a `[N, D]` corpus →
    // `[Q, N]` matrix, generic in Q, N, and D. Builds its `[?, ?]` result
    // tensor from the runtime shape headers and index-writes each cell.
    // Oracle uses vectors [3,4]/[4,-3]/[6,8] (norms 5, 5, 10) so every
    // cosine is an exact 0 or 1 while genuinely exercising the sqrt +
    // division path (not just unit vectors). Rows queries {[3,4],[4,-3]}
    // vs corpus {[3,4],[4,-3],[6,8]}:
    //   [1,0,1]  ([3,4]·{itself, orthogonal, colinear})
    //   [0,1,0]  ([4,-3]·{orthogonal, itself, orthogonal})
    if let Some(out) = run_program(
        r#"
import std.embeddings.{cosine_similarity_matrix};
fn main() {
    let queries: Tensor[f32, [2, 2]] = Tensor.from([[3.0f32, 4.0f32], [4.0f32, -3.0f32]]);
    let corpus: Tensor[f32, [3, 2]] = Tensor.from([[3.0f32, 4.0f32], [4.0f32, -3.0f32], [6.0f32, 8.0f32]]);
    let m = cosine_similarity_matrix(queries, corpus);
    println(m[0, 0]); println(m[0, 1]); println(m[0, 2]);
    println(m[1, 0]); println(m[1, 1]); println(m[1, 2]);
}
"#,
    ) {
        assert_eq!(out, "1\n0\n1\n0\n1\n0\n");
    }
}

#[test]
fn test_e2e_embeddings_top_k() {
    // `std.embeddings.top_k` (phase-11): the ranking step after a
    // similarity scan — the k highest scores as `(index, score)` pairs,
    // descending, ties left-biased. Partial-selection O(n·k), no full
    // sort. Scores are exact-f32 powers of two [0.125, 0.5, 0.25, 0.75]
    // so the printed scores are byte-clean. Descending order is
    // 0.75(idx 3), 0.5(idx 1), 0.25(idx 2), 0.125(idx 0):
    //   top_k(·, 3) → [(3,0.75), (1,0.5), (2,0.25)] (index+score printed);
    //   the k-cap top_k(·, 10) returns all four descending, indices
    //   [3, 1, 2, 0].
    if let Some(out) = run_program(
        r#"
import std.embeddings.{top_k};
fn main() {
    let mut s: Vec[f32] = Vec.new();
    s.push(0.125f32); s.push(0.5f32); s.push(0.25f32); s.push(0.75f32);
    let a = top_k(s, 3);
    for pair in a.iter() {
        let (idx, score) = pair;
        println(idx);
        println(score);
    }
    println(-1);
    let b = top_k(s, 10);
    for pair in b.iter() {
        let (idx, _) = pair;
        println(idx);
    }
}
"#,
    ) {
        assert_eq!(out, "3\n0.75\n1\n0.5\n2\n0.25\n-1\n3\n1\n2\n0\n");
    }
}

#[test]
fn test_e2e_secret_ct_eq() {
    // `std.secret.Secret[String].ct_eq(other)` — constant-time equality
    // lowered to the `karac_secret_ct_eq` runtime helper (OR-accumulate +
    // `black_box`, NOT the short-circuiting `karac_string_cmp`). `karac
    // build` output must match `karac run`. Equal contents → true;
    // differing contents (same OR different length) → false. Soft-skips
    // without the runtime archive. Matches the interpreter's
    // `test_secret_ct_eq`.
    if let Some(out) = run_program(
        r#"
import std.secret.{Secret};
fn main() {
    let a = Secret.new("s3cr3t-token-01");
    let b = Secret.new("s3cr3t-token-01");
    let c = Secret.new("s3cr3t-token-99");
    let d = Secret.new("short");
    println(a.ct_eq(b));
    println(a.ct_eq(c));
    println(a.ct_eq(d));
}
"#,
    ) {
        assert_eq!(out, "true\nfalse\nfalse\n");
    }
}

#[test]
fn test_ir_cold_emits_cold() {
    let ir = ir_for("#[cold]\nfn rare(a: i64) -> i64 { a - 1 }");
    // `cold` appears as a function attribute on `@rare`. The panic
    // path also emits `cold`, so anchor the check on the function's
    // own attribute group rather than a bare substring: the IR must
    // both name `@rare` and carry a `cold` attribute.
    assert!(ir.contains("@rare"), "function should be emitted:\n{ir}");
    assert!(
        ir.contains("cold"),
        "expected `cold` attribute in IR:\n{ir}"
    );
}

/// The same reuse with the consumer OUTLIVING the read. Correct on both
/// sides of the fix, and kept for exactly that reason: this is the shape
/// every earlier fixture for this class was written in, and it is why the
/// class kept reading as closed. A pin that passes before the fix is not
/// worthless — it is the control that says where to put the real one.
#[test]
fn e2e_uam_call_argument_main_shaped_control_is_correct_either_way() {
    let src = r#"
struct Stat { service: String }
struct Entry { service: String }

fn main() {
    let mut es: Vec[Entry] = Vec.new();
    es.push(Entry { service: "alphabetical" });
    let mut index: Map[String, usize] = Map.new();
    let mut stats: Vec[Stat] = Vec.new();
    let mut i = 0;
    while i < es.len() {
        let e = ref es[i];
        let _ = index.insert(e.service, 0 as usize);
        stats.push(Stat { service: e.service });
        i = i + 1;
    }
    println(stats[0].service);
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("alphabetical\n"));
}

/// B-2026-08-17-19 — call-site default-parameter fill. The stored
/// defaults used to be inert (omitting one was an arity error, and a
/// label that skipped one was a label mismatch), so this program did
/// not check on any backend. The fill is a pre-resolve AST rewrite, so
/// codegen sees an ordinary full-arity call — this pins that the
/// compiled backends agree with the interpreter oracle
/// (`test_default_parameter_call_site_fill_oracle`) on the filled values.
#[test]
fn test_e2e_default_parameter_call_site_fill() {
    assert_eq!(
        run_program(
            r#"
fn create_server(host: i64, port: i64 = 8080, max_connections: i64 = 1000, timeout_ms: i64 = 5000) -> i64 {
    host + port + max_connections + timeout_ms
}

fn main() {
    println(create_server(1));
    println(create_server(1, 9090));
    println(create_server(1, max_connections: 100));
    println(create_server(1, 9090, max_connections: 100, timeout_ms: 250));
}
"#
        ),
        Some("14081\n15091\n13181\n9441\n".to_string())
    );
}

/// B-2026-09-01-20 — the INSTANCE-METHOD spelling of the same fill.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_default_parameter_fill_through_an_instance_method_oracle`; same four
/// shapes and same values as both siblings, with a receiver whose `id` is 0 so
/// the arithmetic stays identical across all three fixtures.
///
/// THIS IS THE HALF THAT FAILS IF ONLY THE TYPECHECKER IS TAUGHT ABOUT METHOD
/// DEFAULTS. With the plan recorded but not spliced, `karac check` passes and
/// the tree-walk interpreter answers correctly, while codegen dies in the
/// module verifier with `Incorrect number of arguments passed to called
/// function` — measured, that was the state of the first draft. The splice
/// lives in `lowering::lower_program`, which runs before effectcheck,
/// ownership, the interpreter and codegen alike, so this fixture and its
/// interpreter twin cannot drift.
#[test]
fn test_e2e_default_parameter_fill_through_an_instance_method() {
    assert_eq!(
        run_program(
            r#"
struct Server { id: i64 }

impl Server {
    fn create(ref self, host: i64, port: i64 = 8080, max_connections: i64 = 1000, timeout_ms: i64 = 5000) -> i64 {
        self.id + host + port + max_connections + timeout_ms
    }
}

fn main() {
    let s = Server { id: 0 };
    println(s.create(1));
    println(s.create(1, 9090));
    println(s.create(1, max_connections: 100));
    println(s.create(1, 9090, max_connections: 100, timeout_ms: 250));
}
"#
        ),
        Some("14081\n15091\n13181\n9441\n".to_string())
    );
}

/// EVERY container element-bodies walker fires at its binding's NLL
/// live-range end, not at scope exit (B-2026-08-27-8).
///
/// design.md § Drop: "destructors fire at each binding's live-range end,
/// not lexical scope end … a value whose last use is mid-scope is dropped
/// at that use and does not appear in the end-of-scope stack at all." The
/// interpreter implements that; codegen admits a walker to the same
/// placement through `CleanupAction::UserDrop`'s `kind`.
///
/// WHY THIS TEST HAS TO ASSERT SEQUENCE, when every other walker test in
/// this file deliberately asserts COUNTS. Counting is the right convention
/// for container walks because iteration order is unspecified
/// (B-2026-08-27-7) — but it is exactly what let this class hide: a walker
/// demoted to scope exit still fires each body exactly once, so a
/// count-asserting test stays green while `karac build` prints in a
/// different order than `karac run`. The `|marker` after each binding's
/// last use is what separates the two placements; nothing else in the tree
/// does.
///
/// Order-safety is bought by having ONE entry per container rather than by
/// sorting: with a single element there is one permutation, so the
/// unspecified-order rule is not in play and the sequence assertion is
/// still legitimate. The `K`/`V` split covers the map's two halves, which
/// are separate walkers registered under one binding.
///
/// Placement per case, all measured: `v`/`t`/`o`/`m`/`s` are last used by
/// the `[..]` line, so their bodies land between it and the marker; `e` is
/// never used after its `let`, so its body lands BEFORE `[enum]` — still
/// the live-range end, and still distinguishable from scope exit, which
/// would put it after `|enum`.
#[test]
fn test_e2e_container_bodies_walks_fire_at_the_nll_point() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq)]
struct K { id: i64 }
struct V { id: i64 }
impl Drop for K { fn drop(mut ref self) { println(f"K{self.id}"); } }
impl Drop for V { fn drop(mut ref self) { println(f"V{self.id}"); } }
enum Payload { Wrap(V), Empty }

fn vec_case() {
    let mut v: Vec[V] = Vec.new();
    v.push(V { id: 1 });
    println(f"[vec {v.len()}]");
    println("|vec");
}
fn tuple_case() {
    let t: (V, i64) = (V { id: 2 }, 7);
    println(f"[tup {t.1}]");
    println("|tup");
}
fn enum_case() {
    let e: Payload = Payload.Wrap(V { id: 3 });
    println("[enum]");
    println("|enum");
}
fn opt_case() {
    let o: Option[V] = Some(V { id: 4 });
    println(f"[opt {o.is_some()}]");
    println("|opt");
}
fn map_case() {
    let mut m: Map[K, V] = Map.new();
    m.insert(K { id: 5 }, V { id: 5 });
    println(f"[map {m.len()}]");
    println("|map");
}
fn set_case() {
    let mut s: Set[K] = Set.new();
    s.insert(K { id: 6 });
    println(f"[set {s.len()}]");
    println("|set");
}
fn main() {
    vec_case();
    tuple_case();
    enum_case();
    opt_case();
    map_case();
    set_case();
}
"#,
        ),
        Some(
            "[vec 1]\nV1\n|vec\n\
                 [tup 7]\nV2\n|tup\n\
                 V3\n[enum]\n|enum\n\
                 [opt true]\nV4\n|opt\n\
                 [map 1]\nK5\nV5\n|map\n\
                 [set 1]\nK6\n|set\n"
                .to_string()
        )
    );
}

/// The `sort` element gate and codegen's comparator family agree about
/// which element types are orderable (B-2026-08-27-51).
///
/// This is the anti-drift device, and it is the point of the fix as much
/// as the gate is. `Vec[T].sort()` is checked by
/// `require_ord_element` in the typechecker and lowered by
/// `emit_cmp_fn_for_type_expr` in `codegen/vec_method.rs`; the two encode
/// the same question in different places, and B-2026-08-11-7's first
/// attempt at this gate ALREADY drifted once — it gated on
/// `type_supports_ord`, which answers `false` for `Vec[Vec[String]]`, and
/// broke a sort that works. A comment cannot hold two definitions in step.
/// A table that fails when either side moves can.
///
/// The assertion is the run-vs-build property itself: every element type
/// the CHECKER admits must BUILD. Widen codegen without widening the gate
/// and the rejected-table sibling in `tests/typechecker.rs` fails; narrow
/// codegen and this one does.
///
/// Each row also compares against the interpreter, because "it built" is
/// not the bar — a comparator that lowers but orders wrongly would pass a
/// build-only assertion.
#[test]
fn test_e2e_sort_element_gate_matches_codegen_support() {
    // (label, element type, literal, per-element print expression, binary_search needle)
    let rows: &[(&str, &str, &str, &str, &str)] = &[
        ("i64", "i64", "[3, 1, 2]", "f\"{e}\"", "2"),
        ("u8", "u8", "[3, 1, 2]", "f\"{e}\"", "2"),
        ("char", "char", "['c', 'a', 'b']", "f\"{e}\"", "'b'"),
        // Restored with B-2026-08-28-5's fix. This table is what found
        // that bug: the row was held out while `Vec[bool].sort()` sorted
        // DESCENDING on all three compiled surfaces (bool lowers to `i1`,
        // whose set value reads as -1 when interpreted as signed), because
        // an entry here would have asserted the bug rather than caught it.
        ("bool", "bool", "[true, false, true]", "f\"{e}\"", "false"),
        ("String", "String", "[\"c\", \"a\"]", "f\"{e}\"", "\"c\""),
        (
            "F64",
            "F64",
            "[F64.from(2.0), F64.from(1.0)]",
            "f\"{e.value}\"",
            "F64.from(2.0)",
        ),
        (
            "tuple",
            "(i64, String)",
            "[(2, \"b\"), (1, \"a\")]",
            "f\"{e.0}{e.1}\"",
            "(2, \"b\")",
        ),
        (
            "nested Vec",
            "Vec[String]",
            "[[\"b\"], [\"a\"]]",
            "f\"{e[0]}\"",
            "[\"b\"]",
        ),
        (
            "derived struct",
            "P",
            "[P { a: 3 }, P { a: 1 }]",
            "f\"{e.a}\"",
            "P { a: 3 }",
        ),
        (
            "derived enum",
            "Suit",
            "[Suit.Spades, Suit.Clubs]",
            "f\"{tag_suit(e)}\"",
            "Suit.Spades",
        ),
    ];
    for (label, elem, lit, print, needle) in rows {
        let src = format!(
            "#[derive(Ord, Eq)]\n\
                 struct P {{ a: i64 }}\n\
                 #[derive(Ord, Eq)]\n\
                 enum Suit {{ Clubs, Spades }}\n\
                 fn tag_suit(s: Suit) -> i64 {{\n\
                     match s {{ Clubs => return 0, Spades => return 1, }}\n\
                 }}\n\
                 fn main() {{\n\
                     let mut v: Vec[{elem}] = {lit};\n\
                     v.sort();\n\
                     for e in v {{ println({print}); }}\n\
                 }}"
        );
        // The checker admits this element type...
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "[{label}] the sort gate rejected an element type codegen supports — \
                 the gate over-rejected, or this row is genuinely unsupported and \
                 belongs in the typechecker sibling instead: {interp_errs:?}"
        );
        let expected = interp_out.join("");
        // Anti-vacuity: a comparator that lowers but never orders would
        // leave the input sequence untouched, and every row above starts
        // DESCENDING so that shows up.
        assert!(
            !expected.is_empty(),
            "[{label}] interpreter oracle produced no output"
        );
        // ...so it must also BUILD. This is the run-vs-build property.
        let Some(aot) = run_program(&src) else { return };
        assert_eq!(
            aot, expected,
            "[{label}] compiled `Vec[{elem}].sort()` must match the interpreter"
        );

        // ...and so must `binary_search`, which shares `sort`'s element
        // gate (`require_ord_element`) and therefore accepts exactly the
        // same element types at CHECK. Before B-2026-08-28-6 its codegen
        // support was narrower still — a hand-rolled integer/String
        // compare with an `else` that errored — so six of the rows above
        // sorted fine and refused to build here. Asserting both methods
        // over ONE table is what makes that class of split impossible to
        // reintroduce: widen or narrow either method alone and a row fails.
        let bs_src = format!(
            "#[derive(Ord, Eq)]\n\
                 struct P {{ a: i64 }}\n\
                 #[derive(Ord, Eq)]\n\
                 enum Suit {{ Clubs, Spades }}\n\
                 fn main() {{\n\
                     let mut v: Vec[{elem}] = {lit};\n\
                     v.sort();\n\
                     let r = v.binary_search({needle});\n\
                     match r {{\n\
                         Some(i) => println(f\"found {{i}}\"),\n\
                         None => println(\"absent\"),\n\
                     }}\n\
                 }}"
        );
        let (bs_interp, bs_errs, _, _) = karac::run_program_full_checked(&bs_src);
        assert!(
            bs_errs.is_empty(),
            "[{label}] the element gate rejected `binary_search` on a type it admits \
                 for `sort` — the two share `require_ord_element`, so this means the gate \
                 or the call site drifted: {bs_errs:?}"
        );
        let bs_expected = bs_interp.join("");
        // Anti-vacuity: every needle IS present, so `absent` here would
        // mean the search ran and got the wrong answer rather than that
        // the row is inert.
        assert!(
            bs_expected.starts_with("found "),
            "[{label}] interpreter oracle did not find a needle that is present: \
                 {bs_expected:?}"
        );
        let Some(bs_aot) = run_program(&bs_src) else {
            return;
        };
        assert_eq!(
            bs_aot, bs_expected,
            "[{label}] compiled `Vec[{elem}].binary_search(..)` must match the \
                 interpreter — including the index chosen among duplicate keys"
        );
    }
}

/// `binary_search` orders elements the same way `sort` does — including
/// unsigned 64-bit (B-2026-08-28-6).
///
/// This is the SILENT half of that row. The element-coverage half is loud
/// (a build error, pinned by the table above); this one returns a wrong
/// ANSWER: `binary_search`'s inline integer compare widened to i64 and
/// then compared SIGNED, which is right for `u8`..`u32` — they
/// zero-extend into the positive half — and wrong for `u64` / `usize` /
/// `u128`, where the widening is a no-op and the top of the range rides as
/// a negative i64.
///
/// So on a Vec that `sort` ITSELF produced, and that `is_sorted` agrees is
/// sorted, `binary_search` searched a different order and answered `None`
/// for an element that is present. Rows 02 and 03 are that case.
///
/// Neither backend could catch this by comparison: the interpreter's
/// `binary_search` called the signed `value_compare` while its own `sort`
/// used `value_compare_u64`, so BOTH BACKENDS AGREED on `None`. Pinned to
/// literal expected output for that reason — a twin against the
/// interpreter would have certified the shared wrong answer, which is
/// exactly how this survived.
///
/// Row 04 is the guard against over-correction: a SIGNED element must stay
/// signed, and making every integer unsigned would satisfy rows 01-03.
/// Row 05 pins the duplicate-key index, which the fix had to preserve
/// while swapping the comparator underneath — Rust's branchless
/// `binary_search_by` picks a specific index among equal keys and the
/// interpreter uses std's, so codegen must agree.
#[test]
fn test_e2e_binary_search_orders_like_sort() {
    let src = r#"
fn main() {
    // 01 — a plain unsigned vec, no high-bit element.
    let a: Vec[u64] = [1u64, 2u64, 3u64];
    let ra = a.binary_search(2u64);
    match ra { Some(i) => println(f"01 {i}"), None => println("01 absent"), }

    // 02-03 — the same vec with u64.MAX, which `sort` puts LAST and the
    // signed compare treats as -1. Both lookups used to answer `absent`.
    let b: Vec[u64] = [1u64, 2u64, 18446744073709551615u64];
    let srt = b.is_sorted();
    let rb = b.binary_search(2u64);
    match rb { Some(i) => println(f"02 {srt} {i}"), None => println(f"02 {srt} absent"), }
    let rc = b.binary_search(18446744073709551615u64);
    match rc { Some(i) => println(f"03 {i}"), None => println("03 absent"), }

    // 04 — signed elements must stay signed.
    let d: Vec[i64] = [0 - 5, 0 - 1, 3];
    let rd = d.binary_search(0 - 5);
    match rd { Some(i) => println(f"04 {i}"), None => println("04 absent"), }

    // 05 — duplicate keys: the index must match std's branchless variant.
    let e: Vec[i64] = [1, 2, 2, 2, 3];
    let re = e.binary_search(2);
    match re { Some(i) => println(f"05 {i}"), None => println("05 absent"), }

    // 06 — absent element still answers None.
    let f: Vec[u64] = [1u64, 9u64];
    let rf = f.binary_search(5u64);
    match rf { Some(i) => println(f"06 {i}"), None => println("06 absent"), }
}
"#;
    // Written out rather than borrowed from a backend: 2 sits at index 1,
    // `[1, 2, u64.MAX]` is sorted and holds 2 at index 1 and MAX at index
    // 2, -5 is first among signed elements, and 5 is genuinely absent.
    let expected = "01 1\n\
                        02 true 1\n\
                        03 2\n\
                        04 0\n\
                        05 3\n\
                        06 absent\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    assert_eq!(
        interp_out.join(""),
        expected,
        "the tree-walker must order binary_search like sort — rows 02/03 are the \
             ones it used to get wrong, in agreement with codegen"
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "compiled `binary_search` must order elements exactly as `sort` does"
    );
}

/// Primitive comparison and widening are signedness-aware, and `bool`
/// counts as unsigned (B-2026-08-28-5).
///
/// `bool` lowers to `i1`, whose one set value reads as `-1` when
/// interpreted as SIGNED. Five separate codegen lists answered "is this
/// name unsigned?" and only one of them — the comparator family — had
/// `bool`, so the ordering of a bool was correct through a tuple, a nested
/// `Vec`, or a derived struct field, and INVERTED everywhere else:
/// `false < true` answered `false`, `Vec[bool].sort()` sorted descending,
/// `false.cmp(true)` answered `Greater`, and `true as i64` produced `-1`.
/// Silent wrong answers on all three compiled surfaces, with the
/// interpreter correct.
///
/// Rows 05-07 are the reason this test also covers unsigned INTEGERS.
/// `.cmp` was the one comparison spelling with no signedness input at all
/// — hardcoded `SLT`/`SGT` — so it was wrong for every unsigned value that
/// sets the top bit of its width, not only for bool. Fixing bool's `.cmp`
/// means giving that site a signedness flag, and once the flag exists the
/// integer rows come with it; leaving them untested would leave the wider
/// half of the same defect unpinned.
///
/// Row 06 is the one that could not be caught by a run-vs-build
/// differential before this fix, and is worth keeping for that reason
/// alone: the INTERPRETER's `.cmp` dropped signedness too, so both
/// backends agreed on `Less` for `u64.MAX.cmp(1)`. The two-backend
/// comparison below only sees it because both sides were fixed.
///
/// Row 10 is the over-correction guard, and it is GREEN before the fix by
/// design — it exists to catch the failure mode of the FIX (making every
/// integer unsigned would satisfy every other row here), not to reproduce
/// the bug.
///
/// Pinned to LITERAL expected output, not merely twinned against the
/// interpreter. The interpreter is the oracle for the bool rows, but it
/// was itself wrong on row 06, so a pure twin would have happily certified
/// a shared wrong answer — the exact shape this bug took.
#[test]
fn test_e2e_bool_and_unsigned_comparisons_are_not_signed() {
    let src = r#"
fn tag(o: Ordering) -> i64 {
    if o.is_lt() { return 0; }
    if o.is_eq() { return 1; }
    return 2;
}

fn main() {
    // 01 — the operators, on literals and on bindings.
    let f = false;
    let t = true;
    println(f"01 {false < true} {false <= true} {true > false} {true >= false}");
    println(f"02 {f < t} {f <= t} {t > f} {t >= f}");

    // 03 — `.cmp` on bool, literal and bound receiver.
    println(f"03 {tag(false.cmp(true))} {tag(true.cmp(false))} {tag(t.cmp(t))}");

    // 04 — `Vec[bool].sort()`. Starts descending, so an unsorted or
    // reverse-sorted result is visible.
    let mut v: Vec[bool] = [true, false, true, false];
    v.sort();
    let mut s = "";
    for e in v { s = s + f"{e} "; }
    println(f"04 {s}");

    // 05-07 — unsigned integers whose value sets the top bit of the width.
    let a: u8 = 200;
    let b: u8 = 100;
    let c: u64 = 18446744073709551615u64;
    let d: u64 = 1;
    let g: u32 = 4000000000;
    let h: u32 = 1;
    println(f"05 {tag(a.cmp(b))}");
    println(f"06 {tag(c.cmp(d))}");
    println(f"07 {tag(g.cmp(h))}");

    // 08 — widening a bool. `sext` of `i1` gives -1 / 255; `zext` gives 1.
    let w1 = t as i64;
    let w2 = t as u8;
    let w3 = t as i32;
    println(f"08 {w1} {w2} {w3}");

    // 09 — equality and branching must be untouched by the signedness change.
    if t { println("09 true true"); } else { println("09 BAD"); }

    // 10 — the over-correction guard. Making EVERY integer unsigned would
    // satisfy every row above; a signed receiver must stay signed.
    let s1: i64 = 0 - 5;
    let s2: i64 = 1;
    println(f"10 {tag(s1.cmp(s2))} {s1 < s2}");
}
"#;
    // Every value here is the mathematically correct answer, written out
    // rather than borrowed from a backend: `false < true`, `false.cmp(true)`
    // is `Less` (0), sorting ascending puts `false` first, 200 > 100,
    // u64.MAX > 1, 4e9 > 1, and `true` widens to 1 at every width.
    let expected = "01 true true true true\n\
                        02 true true true true\n\
                        03 0 2 1\n\
                        04 false false true true \n\
                        05 2\n\
                        06 2\n\
                        07 2\n\
                        08 1 1 1\n\
                        09 true true\n\
                        10 0 true\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    assert_eq!(
        interp_out.join(""),
        expected,
        "the interpreter must produce the mathematically correct answers — \
             row 06 is the one it used to get wrong"
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "compiled bool / unsigned comparisons must be unsigned, not signed"
    );
}

/// `.cmp()` on a receiver with NO NAME TO LOOK UP — a struct literal, a
/// call result, an index, a tuple element (B-2026-08-27-47).
///
/// Codegen resolved the `.cmp` receiver through `inferred_receiver_type`,
/// which reads `var_type_names` and so answers only for an `Identifier` or
/// `self`. Every other receiver had no type here and fell out of method
/// dispatch entirely: `P { a: 1, b: 2 }.cmp(q)` and `mk(1).cmp(mk(2))` both
/// typechecked, ran correctly under `--interp`, and failed to BUILD with
/// "no handler for method 'cmp' on non-identifier receiver".
///
/// Row 08 is what localises the defect, and it is why this is a receiver
/// bug rather than a comparator bug: binding the identical value first
/// (`let p = P { .. }; p.cmp(q)`) compiled and answered correctly the whole
/// time. The comparator and the lowering were never involved.
///
/// Row 09 is the guard that the fix cannot hijack a user ordering. `Rev`'s
/// hand-written `cmp` REVERSES the order, so the structural
/// declaration-order comparator would answer `Less` for `1.cmp(2)` where
/// the user impl answers `Greater` — the B-2026-08-26-10 shape, which is a
/// SILENT wrong answer in compiled output only. It must read `2` on both
/// backends, from the LITERAL receiver as much as from the bound one: the
/// `user_owns_cmp` guard consults `type_name_of_expr`, the same widened
/// resolver this fix falls back to, so widening one without the other is
/// exactly how a user impl would start losing to the builtin.
///
/// Rows 05-07 vary the receiver shape rather than the type, because that
/// is the axis that broke: an enum through a call result, a field chain,
/// an index, and a tuple element each reach a different arm of
/// `type_name_of_expr` and none of them has a `var_type_names` entry.
///
/// Twinned against the interpreter rather than pinned to a literal, for
/// the reason the tuple sibling above gives: the defect WAS the two
/// backends disagreeing about whether the program exists.
#[test]
fn test_e2e_cmp_on_a_non_identifier_receiver() {
    let src = r#"
#[derive(Ord, Eq)]
struct P { a: i64, b: i64 }

#[derive(Ord, Eq)]
enum Suit { Clubs, Hearts, Spades }

struct Holder { p: P }

struct Rev { v: i64 }
impl PartialEq for Rev { fn eq(ref self, other: ref Rev) -> bool { self.v == other.v } }
impl Eq for Rev {}
impl PartialOrd for Rev { fn partial_cmp(ref self, other: ref Rev) -> Option[Ordering] { Some(other.v.cmp(self.v)) } }
impl Ord for Rev { fn cmp(ref self, other: ref Rev) -> Ordering { other.v.cmp(self.v) } }

fn mk(a: i64) -> P { return P { a: a, b: 0 }; }
fn suit() -> Suit { return Suit.Clubs; }

fn tag(o: Ordering) -> i64 {
    if o.is_lt() { return 0; }
    if o.is_eq() { return 1; }
    return 2;
}

fn main() {
    // 01-03: struct LITERAL receiver, all three answers.
    println(f"01 {tag(P { a: 1, b: 2 }.cmp(P { a: 1, b: 3 }))}");
    println(f"02 {tag(P { a: 1, b: 2 }.cmp(P { a: 1, b: 2 }))}");
    println(f"03 {tag(P { a: 9, b: 0 }.cmp(P { a: 1, b: 0 }))}");
    // 04: CALL-RESULT receiver, and a call result against a literal.
    println(f"04 {tag(mk(1).cmp(mk(2)))} {tag(mk(5).cmp(P { a: 5, b: 0 }))}");
    // 05: call result on a derived-Ord ENUM (variant declaration order).
    println(f"05 {tag(suit().cmp(Suit.Spades))} {tag(suit().cmp(Suit.Clubs))}");
    // 06: FIELD-CHAIN receiver.
    let h = Holder { p: P { a: 1, b: 2 } };
    println(f"06 {tag(h.p.cmp(P { a: 1, b: 3 }))}");
    // 07: INDEX receiver and TUPLE-ELEMENT receiver.
    let v = [P { a: 1, b: 2 }, P { a: 1, b: 3 }];
    let t = (P { a: 5, b: 0 }, P { a: 1, b: 0 });
    println(f"07 {tag(v[0].cmp(v[1]))} {tag(t.0.cmp(t.1))}");
    // 08: the BOUND control, which compiled correctly before the fix.
    let p = P { a: 1, b: 2 };
    println(f"08 {tag(p.cmp(P { a: 1, b: 3 }))}");
    // 09: a user `impl Ord` still wins, from a literal receiver as well as a
    // bound one. Structural order would say 0 here; the user impl says 2.
    println(f"09 {tag(Rev { v: 1 }.cmp(Rev { v: 2 }))}");
    let r = Rev { v: 1 };
    println(f"10 {tag(r.cmp(Rev { v: 2 }))}");
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // Anti-vacuity. The oracle has to show the comparator genuinely
    // ORDERING (a missing comparison arm degrades to Equal everywhere, and
    // two backends agreeing on `1` for every row would still pass an
    // equality-only assertion) AND the hand-written `impl Ord` winning from
    // both receiver spellings.
    assert!(
        expected.contains("01 0")
            && expected.contains("02 1")
            && expected.contains("03 2")
            && expected.contains("09 2")
            && expected.contains("10 2"),
        "interpreter oracle is not ordering these receivers: {expected:?}"
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "compiled `.cmp()` on a non-identifier receiver must match the interpreter",
    );
}

/// B-2026-08-17-33 — derive dependency auto-resolution. design.md § Derive
/// states it unconditionally ("`Copy` without `Clone` is NEVER a compile
/// error, because the compiler fills in the missing dependency"), but only
/// the `Ord` chain resolved: `#[derive(Copy)]` alone was exactly that
/// error, and `#[derive(Hash)]` alone left the type unusable as a `Map`
/// key. The fill has to produce WORKING derives on the compiled backends,
/// not just a quiet typechecker — `Copy` leaves the source binding usable,
/// and the auto-filled `Eq`/`PartialEq` are what collapse two equal keys
/// into one Map entry. Paired with
/// `test_derive_dependency_auto_fill_oracle` in `tests/interpreter.rs`.
#[test]
fn test_e2e_derive_dependency_auto_fill() {
    assert_eq!(
        run_program(
            r#"
#[derive(Copy)]
struct C { a: i64 }

#[derive(Hash)]
struct K { a: i64, b: i64 }

fn main() {
    let x = C { a: 3 };
    let y = x;
    println("copy = " + x.a.to_string() + "," + y.a.to_string());
    let mut m: Map[K, i64] = Map.new();
    m.insert(K { a: 1, b: 2 }, 10);
    m.insert(K { a: 1, b: 2 }, 20);
    m.insert(K { a: 9, b: 9 }, 30);
    println("len = " + m.len().to_string());
    match m.get(K { a: 1, b: 2 }) {
        Some(v) => println("k12 = " + v.to_string()),
        None => println("k12 missing"),
    }
}
"#
        ),
        Some("copy = 3,3\nlen = 2\nk12 = 20\n".to_string())
    );
}

/// B-2026-08-17-25 — `ExprKind::Pipe` had no arm in `compile_expr`, so
/// every compiled pipe fell to the catch-all's constant 0 while `--interp`
/// evaluated it correctly. Silent for a scalar; fatal for a String, whose
/// zero was read back as a header pointer (SIGSEGV, exit 139).
///
/// Each case pins the COMPILED answer against the value the direct call
/// spelling produces, because that equivalence is the whole definition of
/// `|>`: `a |> f` is `f(a)`. A test that only pinned literals would still
/// pass if the desugar drifted in a way that broke the correspondence.
#[test]
fn pipe_compiles_to_the_call_it_desugars_to() {
    let prelude = "fn dbl(n: i64) -> i64 { n * 2 }\n\
                       fn add(a: i64, b: i64) -> i64 { a + b }\n\
                       fn tag(s: String) -> String { \"[\" + s + \"]\" }\n\
                       fn pair(a: String, b: i64) -> String { a + b.to_string() }\n\
                       fn ident[T](x: T) -> T { x }\n";
    // (piped spelling, equivalent direct call) — both compiled, compared.
    for (piped, direct) in [
        // The row's minimal repro: a let-bound scalar pipe printed 0.
        ("5 |> dbl", "dbl(5)"),
        // Expression position — the row measured this separately.
        ("(5 |> dbl) + 1", "dbl(5) + 1"),
        // Chained stages are left-associative.
        ("5 |> dbl |> dbl", "dbl(dbl(5))"),
        // Extra args land AFTER the piped value.
        ("5 |> add(10)", "add(5, 10)"),
        // The `_` hole moves the piped value to the marked position.
        ("5 |> add(10, _)", "add(10, 5)"),
        // `|>` binds looser than `+`, so the sum is what gets piped.
        ("2 + 1 |> dbl", "dbl(2 + 1)"),
        // A generic stage: the desugared call must reach the same
        // instantiation the direct spelling does.
        ("3 |> ident", "ident(3)"),
    ] {
        let src = |e: &str| format!("{prelude}fn main() {{ println({e}); }}\n");
        let Some(got) = run_program(&src(piped)) else {
            return;
        };
        let want = run_program(&src(direct)).expect("direct-call control must build");
        assert_eq!(got, want, "`{piped}` must compile to what `{direct}` does");
        assert_ne!(got, "0\n", "`{piped}` returned the catch-all's zero");
    }

    // The String legs, kept separate because their failure was a SEGFAULT
    // rather than a wrong number: `run_program` returns None on a crash, so
    // an `assert_eq!` against the expected text is what proves it survived.
    for (piped, want) in [
        ("\"x\" |> tag", "[x]\n"),
        ("\"y\" |> tag |> tag", "[[y]]\n"),
        ("7 |> pair(\"v\", _)", "v7\n"),
        ("\"g\" |> ident", "g\n"),
    ] {
        let Some(got) = run_program(&format!(
            "{prelude}fn main() {{ let a = {piped}; println(a); }}\n"
        )) else {
            return;
        };
        assert_eq!(got, want, "String-typed pipe `{piped}`");
    }
}

/// B-2026-08-18-30 — a LEFT-CHAINED `??` needs its two nodes on distinct
/// side-table keys, and nothing pinned that until now.
///
/// `NilCoalesce` still copies its LHS's span (one of seven arms the
/// postfix-span family left behind), so both nodes of `a ?? b ?? c` carry
/// `a`'s span. What tells them apart is `method_call_key`'s preference for
/// the args-close span: `desugar_nil_coalesce` passes the FALLBACK's span
/// in that slot precisely because it differs per node. Drop that
/// preference — as an attempt to retire the parameter did, on the premise
/// that B-2026-08-18-24 had made it redundant — and the outer `??` reads
/// the INNER's payload type, failing the build with "Option/Result method
/// 'unwrap_or' expected struct receiver, got IntValue".
///
/// The whole 14k-test suite passed under that change. This test is the
/// gate that was missing: the only prior chained-`??` coverage
/// (`tests/typechecker.rs`) parenthesizes the inner one, which gives the
/// two nodes different spans and hides the collision, and asserts no
/// runtime value.
///
/// The chain needs a nested `Option` payload to typecheck at all — `a ?? b`
/// already unwraps one layer, so `find(2) ?? find(3) ?? -1` over
/// `Option[i64]` is a type error, not a chain. That is why the shape here
/// looks contrived: it is the smallest one that actually chains.
#[test]
fn chained_nil_coalesce_keeps_its_two_nodes_apart() {
    let src = "fn outer(k: i64) -> Option[Option[i64]] {\n\
                   if k == 1 { return Some(Some(11)); }\n\
                   if k == 2 { return Some(None); }\n\
                   return None;\n\
                   }\n\
                   fn mid(k: i64) -> Option[i64] {\n\
                   if k == 5 { return Some(55); }\n\
                   return None;\n\
                   }\n\
                   fn main() {\n\
                   println(outer(1) ?? mid(5) ?? -1);\n\
                   println(outer(2) ?? mid(5) ?? -1);\n\
                   println(outer(3) ?? mid(9) ?? -1);\n\
                   }";
    // Some(Some(11)) -> the inner payload survives both peels;
    // Some(None)     -> the outer supplies `Some(None)`'s payload, then -1;
    // None           -> the middle falls back to `mid(9)` = None, then -1.
    // The interpreter agrees on all three (it keys nothing by span).
    assert_eq!(run_program(src).as_deref(), Some("11\n-1\n-1\n"));
}

/// B-2026-08-17-27 — `??` fell to the same catch-all, so all four of its
/// legs compiled to 0. It is lowered to `unwrap_or` now, and the point of
/// this test is that the two spellings agree: `??` IS `unwrap_or`, so any
/// future divergence between them is a bug in the lowering.
#[test]
fn nil_coalesce_compiles_to_unwrap_or() {
    let prelude = "fn find(k: i64) -> Option[i64] { if k == 7 { return Some(7); } return None; }\n\
                       fn name(k: i64) -> Option[String] { if k == 1 { return Some(\"hit\"); } return None; }\n\
                       fn res(k: i64) -> Result[i64, String] { if k == 1 { return Ok(12); } return Err(\"bad\"); }\n\
                       fn sres(k: i64) -> Result[String, String] { if k == 1 { return Ok(\"fine\"); } return Err(\"bad\"); }\n";
    // (receiver, fallback, expected) across all four legs of the matrix the
    // row measured — Option/Some, Option/None, Result/Ok, Result/Err — in
    // both a scalar and a heap payload.
    for (recv, fallback, want) in [
        ("find(7)", "-1", "7\n"),
        ("find(3)", "-1", "-1\n"),
        ("res(1)", "-1", "12\n"),
        ("res(2)", "-1", "-1\n"),
        ("name(1)", "\"miss\"", "hit\n"),
        ("name(2)", "\"miss\"", "miss\n"),
        ("sres(1)", "\"fell\"", "fine\n"),
        ("sres(2)", "\"fell\"", "fell\n"),
    ] {
        let Some(got) = run_program(&format!(
            "{prelude}fn main() {{ println({recv} ?? {fallback}); }}\n"
        )) else {
            return;
        };
        assert_eq!(got, want, "`{recv} ?? {fallback}`");
        let via_method = run_program(&format!(
            "{prelude}fn main() {{ println({recv}.unwrap_or({fallback})); }}\n"
        ))
        .expect("the unwrap_or control must build");
        assert_eq!(
            got, via_method,
            "`{recv} ?? {fallback}` must equal `{recv}.unwrap_or({fallback})`"
        );
    }

    // The stripped payload has to be USABLE as `T`, not just printable.
    // The interpreter's old `??` returned the wrapper untouched, and
    // `(find(3) ?? 5) + 1` was where that showed: it died on "operator
    // 'Add' is not defined for 'EnumVariant' and 'Int'".
    let Some(arith) = run_program(&format!(
        "{prelude}fn main() {{ println((find(3) ?? 5) + 1); }}\n"
    )) else {
        return;
    };
    assert_eq!(arith, "6\n", "the `??` result must be the bare payload");
}

/// B-2026-08-17-20 — the four default-parameter forms design.md lists and
/// the validator refused, exercised END TO END: the declaration-side test
/// in tests/typechecker.rs can only show they are ACCEPTED, because that
/// harness does not run the call-site fill. This shows they produce the
/// right value when the argument is actually omitted.
#[test]
fn spec_listed_default_parameter_values_run() {
    for (decls, want) in [
            // The spec's own quoted example.
            (
                "fn f(h: String = \"localhost\") -> String { return h; }",
                "localhost\n",
            ),
            // A reference to a module-level binding.
            (
                "let LIMIT: i64 = 7;\nfn f(n: i64 = LIMIT) -> i64 { return n; }",
                "7\n",
            ),
            // A struct literal — the tuple sibling always worked.
            (
                "struct P { x: i64, y: i64 }\n\
                 fn f(p: P = P { x: 1, y: 2 }) -> i64 { return p.x; }",
                "1\n",
            ),
            // `Option[T] = None`, which design.md calls idiomatic, and its
            // `Some` sibling.
            (
                "fn f(o: Option[i64] = None) -> i64 { match o { Some(v) => { return v; } None => { return -1; } } }",
                "-1\n",
            ),
            (
                "fn f(o: Option[i64] = Option.Some(42)) -> i64 { match o { Some(v) => { return v; } None => { return -1; } } }",
                "42\n",
            ),
        ] {
            let Some(got) = run_program(&format!("{decls}\nfn main() {{ println(f()); }}\n")) else {
                return;
            };
            assert_eq!(got, want, "default value for: {decls}");
        }
}

/// B-2026-08-17-25 / -27 — the catch-all at the end of `compile_expr`
/// USED TO RETURN `Ok(0)`. Three bugs were filed against that one line —
/// `providers` blocks compiling to a body that never ran
/// (B-2026-07-31-9), then `|>` and `??` each to zero — and each was
/// mechanical to fix once someone noticed. Noticing was the whole cost.
///
/// It is a loud bail now, and this test USED TO ASSERT THAT by naming the
/// two kinds it immediately caught: a `Range` bound to a variable, and
/// `?.`. Both have since been implemented — B-2026-08-17-29 drives a
/// let-bound range from bounds captured at the binding, B-2026-08-18-3
/// indexes by one, and B-2026-08-17-28 lowers `?.` to its `match` — so
/// the assertion was flipped rather than deleted, exactly as its own doc
/// said would be required ("implementing either one should flip its case
/// to a passing build, which is a deliberate edit here").
///
/// There is no source-reachable expression kind left for the bail to
/// catch, which is the state it exists to produce; a test cannot exercise
/// it without an unimplemented kind to feed it. What is worth pinning
/// instead is that these two no longer fall in — a regression to the
/// silent zero would show up here as a wrong answer rather than as a
/// build error nobody sees.
#[test]
fn the_kinds_the_loud_catch_all_caught_now_compile() {
    let Some(range_out) = run_program("fn main() { let r = 0..4; for i in r { println(i); } }\n")
    else {
        return;
    };
    assert_eq!(
        range_out, "0\n1\n2\n3\n",
        "a let-bound range must drive the loop, not compile to a constant"
    );

    let Some(chain_out) = run_program(
            "struct P { x: i64 }\n\
             fn get(k: i64) -> Option[P] { if k == 1 { return Some(P { x: 5 }); } return None; }\n\
             fn main() { match get(1)?.x { Some(v) => { println(v); } None => { println(\"none\"); } } }\n",
        ) else {
            return;
        };
    assert_eq!(
        chain_out, "5\n",
        "an optional chain must project, not compile to a constant"
    );
}

/// The same fix, at the shape that was SILENTLY WRONG rather than
/// rejected. `for x in self` over a `Slice[i64]` or `Set[i64]` impl head
/// typechecked before — iteration does not need the element type the way
/// `self[0]` does — so it built, and the compiled binary summed NOTHING
/// while `--interp` summed correctly. Measured on the pre-fix source:
/// interpreter 12, `karac build` 0, for both heads.
///
/// Kept as its own test because a wrong answer that builds is the failure
/// this whole row is really about: the rejected shapes announced
/// themselves, this one did not.
#[test]
fn iterating_a_container_self_sums_its_elements() {
    for (head, fill, self_mode) in [
        (
            "Slice[i64]",
            "let v: Vec[i64] = [5, 7];\n let c: Slice[i64] = v[0..2];",
            "ref self",
        ),
        (
            "Set[i64]",
            "let mut c: Set[i64] = Set.new();\n c.insert(5); c.insert(7);",
            "ref self",
        ),
        // The OWNED spelling of the Set case is B-2026-08-18-11's own
        // repro: the head fix made the borrowed one correct and left this
        // one at 0, because the caller passed the ADDRESS of the owned
        // pointer-shaped receiver rather than its value, so
        // `karac_map_iter_new` got the receiver's stack slot and iterated
        // nothing.
        (
            "Set[i64]",
            "let mut c: Set[i64] = Set.new();\n c.insert(5); c.insert(7);",
            "self",
        ),
    ] {
        let src = format!(
            "trait Cnt {{ fn total({self_mode}) -> i64; }}\n\
                 impl Cnt for {head} {{\n\
                     fn total({self_mode}) -> i64 {{\n\
                         let mut t = 0;\n\
                         for x in self {{ t = t + x; }}\n\
                         return t;\n\
                     }}\n\
                 }}\n\
                 fn main() {{\n\
                     {fill}\n\
                     println(c.total().to_string());\n\
                 }}\n"
        );
        let Some(out) = run_program(&src) else {
            return;
        };
        assert_eq!(
            out, "12\n",
            "{head} / `{self_mode}`: iterating `self` compiled to a sum over nothing"
        );
    }
}

/// B-2026-08-20-39 — `last(n)` end-relative access, end to end.
///
/// The typechecker half is pinned in tests/typechecker.rs; this is the half
/// that catches a backend that ACCEPTS the argument and then ignores it,
/// which is exactly what codegen did at first: it took `v.last(2)`,
/// type-checked it, and returned the last element on every call while the
/// interpreter walked back correctly — a silent run-vs-build divergence
/// that only differing values expose.
///
/// Each receiver has its OWN lowering (`Vec`/`Slice` share the length-driven
/// arm; `Array` indexes a static length through a separate one), so all
/// three are swept, and every element is distinct so an off-by-one shows up
/// as a wrong number rather than a coincidence. The out-of-range and
/// NEGATIVE arms matter as much as the hits: `.last` sits on the `Option`
/// side of the split design.md draws in this very paragraph — `v[i]`
/// panics, `.first`/`.get`/`.last` answer `None` — so walking off either
/// end must yield `None` and not a panic or a wild read.
#[test]
fn test_e2e_last_takes_an_end_relative_index() {
    let src = r#"
fn main() {
    let v: Vec[i64] = [10, 20, 30];
    match v.last()   { Some(x) => { println(x); } None => { println("none"); } }
    match v.last(0)  { Some(x) => { println(x); } None => { println("none"); } }
    match v.last(1)  { Some(x) => { println(x); } None => { println("none"); } }
    match v.last(2)  { Some(x) => { println(x); } None => { println("none"); } }
    match v.last(3)  { Some(x) => { println(x); } None => { println("none"); } }
    match v.last(-1) { Some(x) => { println(x); } None => { println("none"); } }

    let s = v.as_slice();
    match s.last(1)  { Some(x) => { println(x); } None => { println("none"); } }

    let a: Array[i64, 3] = [7, 8, 9];
    match a.last(0)  { Some(x) => { println(x); } None => { println("none"); } }
    match a.last(2)  { Some(x) => { println(x); } None => { println("none"); } }
    match a.last(3)  { Some(x) => { println(x); } None => { println("none"); } }
    match a.last(-1) { Some(x) => { println(x); } None => { println("none"); } }

    // An empty receiver has no element at any n, including the default.
    let e: Vec[i64] = [];
    match e.last()   { Some(x) => { println(x); } None => { println("none"); } }
    match e.last(0)  { Some(x) => { println(x); } None => { println("none"); } }
    match e.last(1)  { Some(x) => { println(x); } None => { println("none"); } }

    // A RUNTIME n, so the index is not a constant the backend can fold —
    // and one that walks past the front on the last iteration.
    let mut i = 0i64;
    while i < 4 {
        match v.last(i) { Some(x) => { println(x); } None => { println("none"); } }
        i = i + 1;
    }

    // `first` is unaffected by the arm split.
    match v.first()  { Some(x) => { println(x); } None => { println("none"); } }
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(concat!(
            "30\n30\n20\n10\nnone\nnone\n",
            "20\n",
            "9\n7\nnone\nnone\n",
            "none\nnone\nnone\n",
            "30\n20\n10\nnone\n",
            "10\n"
        ))
    );
}

/// B-2026-08-21-10 — `to_ne_bytes()`, end to end.
///
/// Lowered as store-then-reload, so the answer IS the native byte order by
/// construction with no endianness constant in the compiler. Both halves
/// of the surface are swept: a BOUND result (which needs the binding
/// registered as `Array[u8, N]` — without that its element width is
/// unknown and handing it to a `ref Slice[u8]` parameter built a header
/// striding at the i64 default, which SEGFAULTED) and a TEMPORARY passed
/// straight to a slice parameter (which has no alloca to point at, so the
/// value is spilled at the call boundary).
#[test]
fn test_e2e_to_ne_bytes_is_the_native_order_image() {
    let src = r#"
fn total(b: ref Slice[u8]) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < b.len() { s = s + b[i] as i64; i = i + 1; }
    s
}
fn by_value(b: Slice[u8]) -> i64 { b.len() }

fn main() {
    let n: u16 = 4660u16;      // 0x1234
    let b = n.to_ne_bytes();
    println(b.len());
    println(b[0]);
    println(b[1]);

    // A BOUND array handed to a `ref Slice[u8]` parameter.
    println(total(b));

    // A TEMPORARY handed straight to a slice parameter, both param shapes.
    let w: u32 = 16909060u32;  // 0x01020304
    println(total(w.to_ne_bytes()));
    println(by_value(w.to_ne_bytes()));

    // Negative receiver: the bytes are the two's-complement image at the
    // RECEIVER's width, not the i64 model's.
    let sg: i16 = -2i16;
    let sb = sg.to_ne_bytes();
    println(sb[0]);
    println(sb[1]);

    // Every width the surface admits.
    let a8: u8 = 7u8;
    let a64: u64 = 7u64;
    let ab = a8.to_ne_bytes();
    let db = a64.to_ne_bytes();
    println(ab.len());
    println(db.len());
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("2\n52\n18\n70\n10\n4\n254\n255\n1\n8\n")
    );
}

/// B-2026-08-21-46 — `for (i, x) in [1, 2, 3].iter().enumerate()`.
///
/// The one source shape B-2026-08-21-41's widening could not reach. The
/// plain `for x in [1, 2, 3]` and `for x in [1, 2, 3].iter()` both lowered
/// fine; only the `.enumerate()` peel declined and fell to the loud adaptor
/// backstop, build-failing a program `--interp` answers.
///
/// WHAT THE ROW GOT WRONG, and why the pin is written this way. It reasoned
/// that a bracketed literal "carries no declared element anywhere" so the
/// predicate would need the typechecker's recorded type plumbed in. It does
/// not: such a literal never reaches codegen as `ArrayLiteral` at all —
/// synthesis mode types it as a Vec, so it arrives already labelled,
/// `PrefixCollectionLiteral { type_name: "Vec" }`. The type is read off the
/// node, not guessed from syntax.
///
/// The cases, and what each is here to catch:
///   * the row's own repro (`e`), which is the regression proper.
///   * a SECOND loop whose body uses `i` multiplicatively, because the
///     failure this replaces would have been an index stuck at 0 — and
///     `0 * x` is 0, which a sum-only assertion could not tell from a
///     correct run. `i * 100 + x` makes every index observable.
///   * a NESTED loop in the body: the index binding is `take()`n exactly so
///     an inner loop does not re-bind it, and this is the shape that proves
///     the recursion still honours that.
///   * a float element, so the lowering cannot be assuming integers.
///   * `.into_iter()` alongside `.iter()`, since the peel accepts both.
#[test]
fn test_e2e_enumerate_over_a_bracketed_literal() {
    let src = r#"
fn main() {
    let mut e = 0;
    for (i, x) in [1, 2, 3].iter().enumerate() { e = e + i * x; }
    println(e);

    for (i, x) in [10, 20, 30].iter().enumerate() { println(i * 100 + x); }

    let mut d = 0;
    for (i, x) in [4, 5].into_iter().enumerate() { d = d + i * x; }
    println(d);

    for (i, x) in [1, 2].iter().enumerate() {
        for y in [100, 200] { println(i * 1000 + x * 10 + y); }
    }

    let mut f = 0.0;
    for (i, x) in [1.5, 2.25].iter().enumerate() { f = f + (i as f64) * x; }
    println(f);
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("8\n10\n120\n230\n5\n110\n210\n1120\n1220\n2.25\n")
    );
}

/// Leg 2 — the ORACLE pairing, and the sharpest form of the defect: both
/// spellings against ONE callee in ONE program. Before the fix this printed
/// `--after temp arg--  drop 2  --after named arg--  pushed 2  drop 1
/// drop 2`, the extra body sitting between the two markers where only the
/// named call could have produced it.
#[test]
fn e2e_stored_arg_agrees_between_temp_and_named_spellings() {
    assert_eq!(
        run_program(
            "struct Res { id: i64 }\n\
                 impl Drop for Res {\n\
                 \x20   fn drop(mut ref self) { println(f\"drop {self.id}\"); }\n\
                 }\n\
                 fn take(sink: mut ref Vec[Res], r: Res) { sink.push(r); }\n\
                 fn main() {\n\
                 \x20   let mut sink: Vec[Res] = Vec.new();\n\
                 \x20   take(mut sink, Res { id: 1 });\n\
                 \x20   println(\"--after temp arg--\");\n\
                 \x20   let carg: Res = Res { id: 2 };\n\
                 \x20   take(mut sink, carg);\n\
                 \x20   println(\"--after named arg--\");\n\
                 \x20   println(f\"pushed {sink.len()}\");\n\
                 }\n"
        ),
        Some("--after temp arg--\n--after named arg--\npushed 2\ndrop 1\ndrop 2\n".to_string())
    );
}

/// B-2026-08-30-28 — the OTHER path of the same conditional callee: the one
/// where the store does NOT happen. This is the half B-2026-08-29-49 priced
/// and left open, and it is the half with no owner at all — the caller stood
/// down because the callee MIGHT store, and the callee registered nothing
/// because the same per-callee predicate said the value leaves.
///
/// Both spellings, because the spellings were wrong in OPPOSITE directions
/// before -49 and a test on one of them proves nothing about the other.
#[test]
fn e2e_conditional_store_that_misses_still_runs_one_body() {
    for (label, call) in [
        (
            "named",
            "let carg: Res = Res { id: 7 };\n     take(mut sink, carg);",
        ),
        ("temp", "take(mut sink, Res { id: 7 });"),
    ] {
        let src = format!(
            "struct Res {{ id: i64 }}\n\
                 impl Drop for Res {{\n\
                 \x20   fn drop(mut ref self) {{ println(f\"drop {{self.id}}\"); }}\n\
                 }}\n\
                 fn take(sink: mut ref Vec[Res], r: Res) {{ if r.id > 100 {{ sink.push(r); }} }}\n\
                 fn main() {{\n\
                 \x20   let mut sink: Vec[Res] = Vec.new();\n\
                 \x20   {call}\n\
                 \x20   println(f\"pushed {{sink.len()}}\");\n\
                 }}\n"
        );
        assert_eq!(
            run_program(&src),
            Some("drop 7\npushed 0\n".to_string()),
            "[{label}] a conditional store that MISSES must still run the body exactly once"
        );
    }
}

/// B-2026-08-30-28 — the guard must not reach a store that is really
/// UNCONDITIONAL. Two spellings of "always stores": the plain one, and an
/// `if`/`else` whose BOTH arms store. If the must-analysis mistook either
/// for conditional AND the flag failed to clear, the container's drain and
/// the callee frame would each run a body for one object.
#[test]
fn e2e_unconditional_store_still_runs_exactly_one_body() {
    for (label, callee) in [
            (
                "plain",
                "fn take(sink: mut ref Vec[Res], r: Res) { sink.push(r); }",
            ),
            (
                "both-arms",
                "fn take(sink: mut ref Vec[Res], r: Res) { if r.id > 100 { sink.push(r); } else { sink.push(r); } }",
            ),
        ] {
            let src = format!(
                "struct Res {{ id: i64 }}\n\
                 impl Drop for Res {{\n\
                 \x20   fn drop(mut ref self) {{ println(f\"drop {{self.id}}\"); }}\n\
                 }}\n\
                 {callee}\n\
                 fn main() {{\n\
                 \x20   let mut sink: Vec[Res] = Vec.new();\n\
                 \x20   take(mut sink, Res {{ id: 7 }});\n\
                 \x20   println(f\"pushed {{sink.len()}}\");\n\
                 }}\n"
            );
            assert_eq!(
                run_program(&src),
                Some("pushed 1\ndrop 7\n".to_string()),
                "[{label}] an unconditional store keeps exactly one owner (the container)"
            );
        }
}

/// B-2026-09-15-24 — a METHOD ARGUMENT that is a PROJECTION out of a caller
/// binding (`k.eat(w.r)`) ran the projected value's user `Drop` body TWICE
/// under `--interp` against once on the JIT and the AOT binary.
///
/// The caller still owns `w`, and `w`'s death fires every Drop-bearing field
/// through `drop_user_drop_fields_of_binding` — so the caller was already
/// firing `w.r`'s body. The three method-frame ownership predicates asked
/// "does the caller still own this argument?" as
/// `matches!(.., ExprKind::Identifier(_))`, which answers for a whole binding
/// and says NO for a projection out of one, so the frame claimed the parameter
/// as well and both fired. The FREE-FUNCTION spelling of the same program was
/// always correct, and that is what places the defect on the method path:
/// `eval_call` claims only conditionally-returned parameters
/// (`cond_returned_param_drop_names`) and never consults an argument's shape.
///
/// Fixed by `arg_place_reaches_caller_drop_fire`, which admits a chain of
/// field / tuple-index projections rooted at an identifier or `self`, at all
/// three sites — `method_param_drop_names` (the parameter's own slot),
/// `method_frame_caller_retains_args` (the let-rebind and destructure slots
/// inside the body), and `method_frame_sole_owned_params`. The first alone
/// fixes the plain cell and leaves the rebind and destructure cells doubled,
/// which is why all three moved together.
///
/// The escape guards are untouched, and the last two rows are what pins that:
/// a callee that HANDS THE PROJECTION BACK and one that STORES it each run two
/// bodies on every surface, before the fix and after — those parameters exit
/// through `fn_always_returns_param` / `fn_always_moves_param_into_outliving_place`
/// before the predicate is ever consulted. They are also rows 3 and 5 of the
/// table in `warn_borrow_projection_copy`'s doc, whose subject is this same
/// copy; that table still reads exactly as written.
///
/// MEMORY: bodies only, no second free. The AOT binary is valgrind-clean on
/// the whole program — 47 allocs / 47 frees, 0 errors — before and after.
#[test]
fn e2e_method_projection_arg_runs_one_body() {
    let hdr = "struct R { id: i64, name: String }\n\
                    impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n\
                    fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\" }; }\n\
                    struct W { r: R }\n\
                    struct Hold { r: R, n: i64 }\n\
                    struct Wh { h: Hold }\n\
                    struct K { n: i64, xs: Vec[R] }\n\
                    impl K {\n\
                    \x20\x20\x20\x20fn eat(mut ref self, x: R) -> i64 { return x.id; }\n\
                    \x20\x20\x20\x20fn reb(mut ref self, x: R) -> i64 { let y: R = x; return y.id; }\n\
                    \x20\x20\x20\x20fn des(mut ref self, h: Hold) -> i64 { let Hold { r, n } = h; return r.id + n; }\n\
                    \x20\x20\x20\x20fn hand(mut ref self, x: R) -> R { return x; }\n\
                    \x20\x20\x20\x20fn store(mut ref self, x: R) -> i64 { let n: i64 = x.id; self.xs.push(x); return n; }\n\
                    }\n\
                    struct Outer { w: W, k: K }\n\
                    impl Outer { fn go(mut ref self) -> i64 { return self.k.eat(self.w.r); } }\n\
                    fn viaref(w: ref W) -> i64 { let mut k: K = K { n: 0, xs: Vec.new() }; return k.eat(w.r); }\n\
                    trait Eater { fn chew(ref self, x: R) -> i64; }\n\
                    struct E1 { n: i64 }\n\
                    impl Eater for E1 { fn chew(ref self, x: R) -> i64 { return x.id; } }\n\
                    fn viagen[T: Eater](e: ref T, w: ref W) -> i64 { return e.chew(w.r); }\n\
                    struct Bref { n: i64 }\n\
                    impl Bref { fn peek(ref self, x: ref R) -> i64 { return x.id; } }\n
                    ";
    for (label, body, want) in [
            (
                "the row: projection out of a `ref` parameter",
                "let w: W = W { r: mk(1) };\n\
                  println(f\"a{viaref(w)}\");",
                "a1\ndrop 1 h1\n",
            ),
            (
                "projection out of an OWNED local",
                "let w: W = W { r: mk(2) };\n\
                  let mut k: K = K { n: 0, xs: Vec.new() };\n\
                  println(f\"b{k.eat(w.r)}\");",
                "b2\ndrop 2 h2\n",
            ),
            (
                "projection whose callee REBINDS the param whole (`let y = x;`)",
                "let w: W = W { r: mk(3) };\n\
                  let mut k: K = K { n: 0, xs: Vec.new() };\n\
                  println(f\"c{k.reb(w.r)}\");",
                "c3\ndrop 3 h3\n",
            ),
            (
                "projection whose callee DESTRUCTURES the param",
                "let wh: Wh = Wh { h: Hold { r: mk(4), n: 1 } };\n\
                  let mut k: K = K { n: 0, xs: Vec.new() };\n\
                  println(f\"d{k.des(wh.h)}\");",
                "d5\ndrop 4 h4\n",
            ),
            (
                "projection rooted at `self` inside another method",
                "let mut o: Outer = Outer { w: W { r: mk(9) }, k: K { n: 0, xs: Vec.new() } };\n\
                  println(f\"j{o.go()}\");",
                "j9\ndrop 9 h9\n",
            ),
            (
                "control: a NAMED binding argument — always agreed",
                "let r: R = mk(5);\n\
                  let mut k: K = K { n: 0, xs: Vec.new() };\n\
                  println(f\"e{k.eat(r)}\");",
                "e5\ndrop 5 h5\n",
            ),
            (
                "control: a FRESH TEMP argument — always agreed",
                "let mut k: K = K { n: 0, xs: Vec.new() };\n\
                  println(f\"f{k.eat(mk(6))}\");",
                "drop 6 h6\nf6\n",
            ),
            (
                "control: the callee HANDS THE PROJECTION BACK — two bodies on every surface, before and after",
                "let w: W = W { r: mk(7) };\n\
                  let mut k: K = K { n: 0, xs: Vec.new() };\n\
                  let o: R = k.hand(w.r);\n\
                  println(f\"g{o.id}\");",
                "drop 7 h7\ng7\ndrop 7 h7\n",
            ),
            (
                "control: the callee STORES the projection — two bodies on every surface, before and after",
                "let w: W = W { r: mk(8) };\n\
                  let mut k: K = K { n: 0, xs: Vec.new() };\n\
                  println(f\"i{k.store(w.r)}\");",
                "i8\ndrop 8 h8\ndrop 8 h8\n",
            ),
            // B-2026-09-15-24's last two NOT-MEASURED items, answered as
            // controls rather than as fixes: neither spelling ever diverged, and
            // both are here so a later widening of the predicate cannot move them
            // without saying so.
            //
            // A trait method reached through a GENERIC BOUND takes a different
            // dispatch route in the typechecker (`dispatch_trait_assoc_fn`, the
            // one arm that warns W0299 on a method argument), so it is worth its
            // own row: measured agreeing 15/15 runs on all three surfaces, on the
            // fixed tree and on the parent.
            (
                "control: a trait method through a generic bound",
                "let w: W = W { r: mk(10) };\n\
                 let e: E1 = E1 { n: 0 };\n\
                 println(f\"k{viagen(e, w)}\");",
                "k10\ndrop 10 h10\n",
            ),
            // A `ref R` PARAMETER separates the projection READ from the by-value
            // parameter: the read alone was never the defect, which is why this
            // one body was always right on every surface.
            (
                "control: a `ref R` parameter — the read alone is not the defect",
                "let w: W = W { r: mk(11) };\n\
                 let b: Bref = Bref { n: 0 };\n\
                 println(f\"m{b.peek(w.r)}\");",
                "m11\ndrop 11 h11\n",
            ),
        ] {
            let src = format!("{hdr}fn main() {{\n{body}\nprintln(\"end\");\n}}\n");
            assert_eq!(
                run_program(&src).as_deref(),
                Some(format!("{want}end\n").as_str()),
                "[{label}]"
            );
        }
}

/// B-2026-09-21-4 — ONE BINDING PASSED TWICE BY VALUE IN A SINGLE CALL.
///
/// `two(g, g)` over `enum G1[T] { Y(T), N }` built and then printed its
/// first arm, a garbled second, and `free(): double free detected in
/// tcache 2`; the concrete twin `twoc(g, g)` built and SIGSEGV'd having
/// printed only its first arm. `--interp` was correct on both and is the
/// oracle here.
///
/// THE COPY FIRED FOR ONE ARGUMENT AND NEVER THE OTHER. Its gate is
/// `source_outlives_move`, which a trace showed answering `UseAfterMove`
/// for the first `g` and `No` for the second — so only the first was
/// copied, and that argument's own move-out ZEROED the caller's slot the
/// second then loaded. The emitted IR is explicit: `load`, `store
/// zeroinitializer`, `load`, `store zeroinitializer`, `call`. The callee's
/// second parameter therefore arrived as a `Y` variant holding a null box.
/// That is not the aliasing the row proposed — it guessed both arguments
/// loaded the same copied box — and the difference matters, because a
/// per-argument copy through the caller's SLOT cannot fix either shape: a
/// binding has exactly one slot.
///
/// The fix gives a later occurrence its own value cloned from the SAVED
/// original, and gives the FINAL use the original itself — the same thing
/// `one(g); one(g)` already hands its second call, which is why that shape
/// was balanced all along while this one was not. Cloning the final use
/// instead leaves the original owned by nobody: measured at 16 allocs / 14
/// frees with 24 direct plus 2 indirect bytes lost, because both move-outs
/// zero the slot before the call, the scope-exit drop reads that zeroed
/// slot and frees nothing, and the restore lands after it.
///
/// Cells: generic dup, concrete dup, triple, and — as controls that must
/// not move — distinct bindings, a single argument, two sequential calls,
/// and an alias with a gap (`three(g, h, g)`). Measured 53 allocs / 53
/// frees, 0 valgrind errors, all four surfaces byte-identical.
#[test]
fn test_e2e_one_binding_passed_twice_by_value_in_one_call() {
    let src = r#"
enum G1[T] { Y(T), N }

fn one[T](a: G1[T]) {
    match a { G1.Y(v) => { println(f"  1x {v}") } G1.N => { println("  1x NONE") } }
}
fn two[T](a: G1[T], b: G1[T]) {
    match a { G1.Y(v) => { println(f"  ax {v}") } G1.N => { println("  ax NONE") } }
    match b { G1.Y(v) => { println(f"  bx {v}") } G1.N => { println("  bx NONE") } }
}
fn three[T](a: G1[T], b: G1[T], c: G1[T]) {
    match a { G1.Y(v) => { println(f"  ax {v}") } G1.N => { println("  ax NONE") } }
    match b { G1.Y(v) => { println(f"  bx {v}") } G1.N => { println("  bx NONE") } }
    match c { G1.Y(v) => { println(f"  cx {v}") } G1.N => { println("  cx NONE") } }
}
fn twoc(a: G1[String], b: G1[String]) {
    match a { G1.Y(v) => { println(f"  ax {v}") } G1.N => { println("  ax NONE") } }
    match b { G1.Y(v) => { println(f"  bx {v}") } G1.N => { println("  bx NONE") } }
}

fn a_gen_dup() { println("a"); let g: G1[String] = G1.Y(f"pa"); two(g, g) }
fn b_con_dup() { println("b"); let g: G1[String] = G1.Y(f"pb"); twoc(g, g) }
fn c_triple() { println("c"); let g: G1[String] = G1.Y(f"pc"); three(g, g, g) }
fn d_distinct() { println("d"); let g: G1[String] = G1.Y(f"pd"); let h: G1[String] = G1.Y(f"qd"); two(g, h) }
fn e_single() { println("e"); let g: G1[String] = G1.Y(f"pe"); one(g) }
fn f_twocalls() { println("f"); let g: G1[String] = G1.Y(f"pf"); one(g); one(g) }
fn g_middle() { println("g"); let g: G1[String] = G1.Y(f"pg"); let h: G1[String] = G1.Y(f"qg"); three(g, h, g) }

fn main() {
    a_gen_dup();
    b_con_dup();
    c_triple();
    d_distinct();
    e_single();
    f_twocalls();
    g_middle();
    println("end");
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("a\n  ax pa\n  bx pa\nb\n  ax pb\n  bx pb\nc\n  ax pc\n  bx pc\n  cx pc\nd\n  ax pd\n  bx qd\ne\n  1x pe\nf\n  1x pf\n  1x pf\ng\n  ax pg\n  bx qg\n  cx pg\nend\n"));
}
