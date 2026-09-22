//! Shared harness for the `tests/codegen.rs` fixtures.
//!
//! This is the module that used to be written inline in
//! `tests/codegen.rs` as `mod codegen_tests`. It keeps the helpers; the
//! fixtures themselves live in the area modules declared below.

// Re-exported so the area modules keep resolving `super::common::…`
// exactly as they did when every fixture was in one file.
pub(crate) use crate::common;
pub(crate) use karac::codegen::compile_to_ir;

// ── Area modules ─────────────────────────────
// Each file's header states which fixtures belong in it.
mod arrays;
mod attrs_ir;
mod borrows;
mod closures;
mod concurrency;
mod control;
mod drop_order;
mod effects;
mod enums;
mod frames;
mod generics;
mod gpu_tensor;
mod io_runtime;
mod iter_range;
mod map_set;
mod misc;
mod moves;
mod numerics;
mod option_result;
mod patterns;
mod rc_shared;
mod slices;
mod strings;
mod structs;
mod vecs;

/// Codegen result (Ok IR / Err diagnostic) without the `ir_for` panic.
fn ir_result(src: &str) -> Result<String, String> {
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    compile_to_ir(&parsed.program, None, None).map_err(|e| e.message)
}

/// The error text codegen declines a program with. Runs the FULL pipeline
/// (ownership included) so the span-keyed display tables the diagnostic
/// reads are populated exactly as `karac build` populates them.
fn codegen_error(src: &str) -> String {
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
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    match compile_to_ir(&parsed.program, Some(&ownership), None) {
        Ok(_) => panic!("expected codegen to decline this program, but it compiled"),
        Err(e) => e.message,
    }
}

fn ir_for(src: &str) -> String {
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
    compile_to_ir(&parsed.program, None, None).expect("codegen failed")
}

/// Like [`ir_for`] but runs `desugar_program` first, so AST-rewriting
/// pre-resolve passes (e.g. `#[derive(Default)]` → synthetic
/// `default()` impl) are reflected in the IR.
fn ir_for_desugared(src: &str) -> String {
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
    compile_to_ir(&parsed.program, None, None).expect("codegen failed")
}

/// Like [`ir_for_desugared`] but also splices gated stdlib modules
/// (`import std.secret.{Secret};` etc.) before resolve — mirrors the real
/// CLI pipeline so a gated program's synthesized fns (e.g. the `Secret`
/// drop) appear in the IR.
fn ir_for_gated(src: &str) -> String {
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
    compile_to_ir(&parsed.program, None, None).expect("codegen failed")
}

/// Like [`ir_for`] but compiles with contract machinery stripped
/// (design.md § Contracts: "stripped in release"). Race-free — forces the
/// decision via the explicit codegen entry, not the process-global
/// `KARAC_STRIP_CONTRACTS` env var.
fn ir_for_contracts_stripped(src: &str) -> String {
    use karac::codegen::compile_to_ir_with_contracts_stripped;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    compile_to_ir_with_contracts_stripped(&parsed.program, None, None).expect("codegen failed")
}

/// Like [`ir_for`] but compiles with the `?`-error-return-trace
/// instrumentation stripped (the `release` strip). Race-free — forces the
/// decision via the explicit codegen entry, not `KARAC_STRIP_ERROR_TRACE`.
fn ir_for_error_trace_stripped(src: &str) -> String {
    use karac::codegen::compile_to_ir_with_error_trace_stripped;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    compile_to_ir_with_error_trace_stripped(&parsed.program, None, None).expect("codegen failed")
}

/// The B-2026-08-29-37 fixture, shared by the E2E and its IR guard so the
/// two can never drift onto different programs. See
/// `e2e_defensive_scrutinee_copy_does_not_rerun_the_enum_own_drop_body`.
const SCRUTINEE_CLONE_DROP_BODY_SRC: &str = r#"struct R { id: i64, v: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct S { e: E }

fn mk(n: i64) -> E {
    let mut v = Vec.new();
    v.push(n);
    return E.A(R { id: n, v: v })
}

#[allow(partial_move_of_drop_enum)]
fn via_ref(s: ref S) -> i64 {
    match s.e { E.A(r) => { let m = r; return m.id + m.v.len() } E.B => { return 0 } }
}

fn leg_refchain() {
    println("refchain");
    let s = S { e: mk(1) };
    println(f"got {via_ref(s)}");
    println("refchain end");
}

#[allow(partial_move_of_drop_enum)]
fn leg_loop_elem() {
    println("loop");
    let mut v = Vec.new();
    v.push(mk(2));
    for p in v {
        match p { E.A(r) => { let m = r; println(f"got {m.id + m.v.len()}"); } E.B => { } }
    }
    println("loop end");
}

fn leg_vec_index() {
    println("index");
    let mut v = Vec.new();
    v.push(mk(3));
    match v[0] { E.A(r) => { println(f"got {r.id + r.v.len()}"); } E.B => { } }
    println("index end");
}

#[allow(partial_move_of_drop_enum)]
fn leg_fresh_temp() {
    println("fresh");
    match mk(4) { E.A(r) => { let m = r; println(f"got {m.id + m.v.len()}"); } E.B => { } }
    println("fresh end");
}

fn main() {
    leg_refchain();
    leg_loop_elem();
    leg_vec_index();
    leg_fresh_temp();
    println("end");
}
"#;

/// The interpreter's answer, which is the oracle here: it never copies a
/// scrutinee, so each `Drop` body fires exactly as often as the source
/// program says.
const SCRUTINEE_CLONE_DROP_BODY_EXPECTED: &str = r#"refchain
dR1
got 2
dE
dR1
refchain end
loop
got 3
dR2
dE
dR2
loop end
index
got 4
dE
dR3
index end
fresh
got 5
dR4
dE
fresh end
end
"#;

/// The B-2026-09-02-11 fixture, shared by the E2E and its IR guard so the
/// two can never drift onto different programs. See
/// `e2e_index_element_clone_does_not_rerun_the_payload_drop_body`.
const INDEX_ELEM_CLONE_PAYLOAD_BODY_SRC: &str = r#"struct R { id: i64, v: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct H { xs: Vec[E] }

struct P { id: i64 }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
enum Q { A(P), B }
impl Drop for Q { fn drop(mut ref self) { println("dQ") } }

fn mk(n: i64) -> E { let mut v: Vec[i64] = Vec.new(); v.push(n); return E.A(R { id: n, v: v }) }
fn mkq(n: i64) -> Q { return Q.A(P { id: n }) }

fn leg_bare() {
    println("bare");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(1));
    match v[0] { E.A(r) => { println(f"got {r.id + r.v.len()}"); } E.B => { } }
    println("bare end");
}

fn leg_field_rooted() {
    println("field");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(2));
    let h = H { xs: v };
    match h.xs[0] { E.A(r) => { println(f"got {r.id + r.v.len()}"); } E.B => { } }
    println("field end");
}

fn leg_scalar_payload() {
    println("scalar");
    let mut v: Vec[Q] = Vec.new();
    v.push(mkq(3));
    match v[0] { Q.A(p) => { println(f"got {p.id}"); } Q.B => { } }
    println("scalar end");
}

fn leg_unbound() {
    println("wild");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(4));
    match v[0] { E.A(_) => { println("got"); } E.B => { } }
    println("wild end");
}

fn leg_fresh() {
    println("fresh");
    match mk(5) { E.A(r) => { println(f"got {r.id + r.v.len()}"); } E.B => { } }
    println("fresh end");
}

fn leg_local() {
    println("local");
    let e = mk(6);
    match e { E.A(r) => { println(f"got {r.id + r.v.len()}"); } E.B => { } }
    println("local end");
}

fn main() {
    leg_bare();
    leg_field_rooted();
    leg_scalar_payload();
    leg_unbound();
    leg_fresh();
    leg_local();
    println("end");
}
"#;

/// The interpreter's answer, and the oracle: it never clones an element, so
/// each `Drop` body fires exactly as often as the source program says.
const INDEX_ELEM_CLONE_PAYLOAD_BODY_EXPECTED: &str = r#"bare
got 2
dE
dR1
bare end
field
got 3
dE
dR2
field end
scalar
got 3
dQ
dP3
scalar end
wild
got
dE
dR4
wild end
fresh
got 6
dR5
dE
fresh end
local
got 7
dE
dR6
local end
end
"#;

/// The B-2026-09-02-15 fixture, shared by the E2E and its interpreter twin.
const IFLET_INDEX_ELEM_CLONE_SRC: &str = r#"struct R { id: i64, v: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct H { xs: Vec[E] }

struct N { id: i64, v: Vec[i64] }
enum F { A(N), B }

fn mk(n: i64) -> E { let mut v: Vec[i64] = Vec.new(); v.push(n); return E.A(R { id: n, v: v }) }
fn mkf(n: i64) -> F { let mut v: Vec[i64] = Vec.new(); v.push(n); return F.A(N { id: n, v: v }) }

fn leg_iflet() {
    println("iflet");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(1));
    if let E.A(r) = v[0] { println(f"got {r.id + r.v.len()}"); }
    println("iflet end");
}

fn leg_field() {
    println("field");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(2));
    let h = H { xs: v };
    if let E.A(r) = h.xs[0] { println(f"got {r.id + r.v.len()}"); }
    println("field end");
}

fn leg_whilelet() {
    println("whilelet");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(3));
    while let E.A(r) = v[0] {
        println(f"got {r.id + r.v.len()}");
        break
    }
    println("whilelet end");
}

fn leg_unbound() {
    println("unbound");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(4));
    if let E.A(_) = v[0] { println("got"); }
    println("unbound end");
}

fn leg_nodrop() {
    println("nodrop");
    let mut v: Vec[F] = Vec.new();
    v.push(mkf(5));
    if let F.A(n) = v[0] { println(f"got {n.id + n.v.len()}"); }
    println("nodrop end");
}

fn leg_fresh() {
    println("fresh");
    if let E.A(r) = mk(6) { println(f"got {r.id + r.v.len()}"); }
    println("fresh end");
}

fn main() {
    leg_iflet();
    leg_field();
    leg_whilelet();
    leg_unbound();
    leg_nodrop();
    leg_fresh();
    println("end");
}
"#;

/// The interpreter's answer, and the oracle.
const IFLET_INDEX_ELEM_CLONE_EXPECTED: &str = r#"iflet
got 2
dE
dR1
iflet end
field
got 3
dE
dR2
field end
whilelet
got 4
dE
dR3
whilelet end
unbound
got
dE
dR4
unbound end
nodrop
got 6
nodrop end
fresh
got 7
dR6
dE
fresh end
end
"#;

/// The value-equality fixture for B-2026-08-31-24, shared by the codegen
/// E2E below and its `tests/interpreter.rs` twin. It sweeps the PLACEMENTS
/// an over-aligned element can occupy — bare `Vec[Vector]`, a wider lane
/// count, a struct with a vector field, an enum payload, a `Map` value, an
/// `Option` element, an `Array` element — because the alignment an
/// aggregate inherits from its most-aligned member is what the fix has to
/// account for, and the IR test that pins the aligned allocator covers only
/// the bare case.
const HEAP_ALIGN_SRC: &str = r#"#[derive(Display)]
struct Holder { v: Vector[i64, 4], n: i64 }
#[derive(Display)]
enum E { V(Vector[i64, 4]), N }

fn mk(n: i64) -> Vec[Vector[i64, 4]] {
    let mut v: Vec[Vector[i64, 4]] = Vec.new();
    v.push(Vector[i64, 4](n, n + 1, n + 2, n + 3));
    return v
}

fn main() {
    let n = env.args().len() as i64;
    let vv: Vector[i64, 4] = Vector[i64, 4](n, n + 1, n + 2, n + 3);
    let ww: Vector[i64, 4] = Vector[i64, 4](n + 4, n + 5, n + 6, n + 7);
    let v8: Vector[i64, 8] = Vector[i64, 8](n, n+1, n+2, n+3, n+4, n+5, n+6, n+7);

    let lit: Vec[Vector[i64, 4]] = [vv, ww];
    println(f"lit  {lit}");
    println(f"idx  {lit[0]}");

    let mut p: Vec[Vector[i64, 4]] = Vec.new();
    p.push(vv);
    p.push(ww);
    p.push(vv);
    println(f"push {p}");
    for x in p { println(f"for  {x}"); }
    p[1] = ww;
    println(f"set  {p}");
    match p.pop() { Some(w) => { println(f"pop  {w}"); } None => {} }
    p.insert(0, ww);
    println(f"ins  {p}");
    println(f"rem  {p.remove(0)}");

    println(f"ret  {mk(n)}");

    let mut wide: Vec[Vector[i64, 8]] = Vec.new();
    wide.push(v8);
    println(f"w8   {wide}");

    let mut hs: Vec[Holder] = Vec.new();
    hs.push(Holder { v: vv, n: n });
    println(f"hs   {hs}");

    let mut es: Vec[E] = Vec.new();
    es.push(E.V(vv));
    println(f"es   {es}");

    let mut m: Map[String, Vector[i64, 4]] = Map.new();
    m.insert(f"k", vv);
    println(f"map  {m}");

    let mut os: Vec[Option[Vector[i64, 4]]] = Vec.new();
    os.push(Some(vv));
    println(f"os   {os}");

    let arr: Array[Vector[i64, 4], 2] = [vv, ww];
    let mut av: Vec[Array[Vector[i64, 4], 2]] = Vec.new();
    av.push(arr);
    println(f"av   {av}");

    let mut big: Vec[Vector[i64, 4]] = Vec.new();
    let mut i = 0;
    while i < 40 {
        big.push(Vector[i64, 4](i + n, i + n + 1, i + n + 2, i + n + 3));
        i = i + 1;
    }
    let mut s: i64 = 0;
    for x in big { s = s + x.reduce_sum(); }
    println(f"sum  {s}");
}
"#;

/// The fixture B-2026-08-31-29's two halves share: the codegen E2E below
/// and `tests/interpreter.rs`'s `test_ref_binding_over_an_array_base`.
/// One string, so a shape added to either widens both.
const ARRAY_REF_BINDING_SRC: &str = r#"struct P { a: i64, b: i64 }

fn main() {
    let sa: Array[i64, 3] = Array[10, 20, 30];
    let g = ref sa[2];
    println(f"scalar {g}");

    let v: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);
    let va: Array[Vector[i64, 4], 3] = Array[v, v, v];
    let gv = ref va[2];
    println(f"vector {gv.reduce_sum()}");

    let pa: Array[P, 2] = Array[P { a: 1, b: 2 }, P { a: 3, b: 4 }];
    let gp = ref pa[1];
    println(f"struct {gp.a} {gp.b}");

    let mut ma: Array[i64, 3] = Array[1, 2, 3];
    let gm = ref ma[0];
    ma[0] = 99;
    println(f"alias {gm}");

    let vv: Vec[i64] = [7, 8, 9];
    let gc = ref vv[1];
    println(f"vec {gc}");

    let sv: Vec[i64] = [10, 20, 30];
    let sl: Slice[i64] = sv.as_slice();
    let gs = ref sl[2];
    println(f"slice {gs}");
    let ssum: i64 = gs + 5;
    println(f"slicearith {ssum}");

    let pv: Vec[P] = [P { a: 1, b: 2 }, P { a: 3, b: 4 }];
    let pl: Slice[P] = pv.as_slice();
    let gsp = ref pl[1];
    println(f"slicestruct {gsp.a} {gsp.b}");

    let mut mv: Vec[i64] = [1, 2, 3];
    let ml: Slice[i64] = mv.as_slice();
    let gsa = ref ml[0];
    mv[0] = 77;
    println(f"slicealias {gsa}");
}
"#;

/// The fixture B-2026-08-31-48's two halves share: this E2E and
/// `tests/interpreter.rs`'s `test_generic_fn_at_nameless_aggregate_args`.
const NAMELESS_MONO_ARG_SRC: &str = r#"struct H { n: i64 }

impl H {
    fn pick[T](self, x: T) -> T { return x }
}

fn ident[T](x: T) -> T { return x }

fn main() {
    let a2: Array[i64, 2] = Array[3, 4];
    let a3: Array[i64, 3] = Array[7, 8, 9];
    let r2: Array[i64, 2] = ident(a2);
    let r3: Array[i64, 3] = ident(a3);
    println(f"len {r2[1]} {r3[2]}");

    let ai: Array[i64, 2] = Array[1, 2];
    let asx: Array[String, 2] = Array["a", "b"];
    let ri: Array[i64, 2] = ident(ai);
    let rs: Array[String, 2] = ident(asx);
    println(f"elem {ri[0]} {rs[1]}");

    let v: Vec[i64] = [5, 6];
    let u: Vec[String] = ["z"];
    let s1: Slice[i64] = v.as_slice();
    let s2: Slice[String] = u.as_slice();
    let q1: Slice[i64] = ident(s1);
    let q2: Slice[String] = ident(s2);
    println(f"slice {q1[1]} {q2[0]}");

    let v4: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);
    let v8: Vector[i64, 8] = Vector[i64, 8](1, 2, 3, 4, 5, 6, 7, 8);
    println(f"vector {ident(v4).reduce_sum()} {ident(v8).reduce_sum()}");

    let t1: (i64, i64) = (1, 2);
    let t2: (i64, String) = (3, "x");
    println(f"tuple {ident(t1).0} {ident(t2).0}");

    let m2: Array[i64, 2] = H { n: 0 }.pick(a2);
    let m3: Array[i64, 3] = H { n: 0 }.pick(a3);
    println(f"method {m2[0]} {m3[0]}");

    let st: String = "hi";
    let bv: Vec[i64] = [8, 9];
    let bu: Vec[String] = ["k"];
    let cs: String = ident(st);
    let cv: Vec[i64] = ident(bv);
    let cu: Vec[String] = ident(bu);
    println(f"prior {cs} {cv[0]} {cu[0]}");
}
"#;

// ── B-2026-08-22-6: a USER-written hasher, compiled ──────────────
//
// The compiled arm of `emit_hash_bytes_call` calls no runtime entry point
// at all: it emits `B.build()` → `S.write(bytes)` → `S.finish()` into the
// synthesized per-key-type `hash_fn`, whose pointer the `karac_map_*`
// control block already held. These pin that the three calls land, that
// the result is a usable index, and that two builders do not collide in
// the `hash_fn` symbol cache.

/// FNV-1a over the key's bytes, and a second builder that reverses it, as
/// a source prefix. `wrapping_mul` because a hasher's job is to overflow —
/// a plain `*` traps under the default overflow checks.
const USER_HASHERS: &str = "\
struct Fnv { h: u64 }\n\
impl Hasher for Fnv {\n\
    fn write(mut ref self, bytes: ref Slice[u8]) {\n\
        for b in bytes { self.h = (self.h ^ (b as u64)).wrapping_mul(1099511628211u64); }\n\
    }\n\
    fn finish(ref self) -> u64 { self.h }\n\
}\n\
struct FnvBuild { }\n\
impl BuildHasher for FnvBuild {\n\
    type Hasher = Fnv;\n\
    fn build(ref self) -> Fnv { Fnv { h: 14695981039346656037u64 } }\n\
}\n\
struct Sum { h: u64 }\n\
impl Hasher for Sum {\n\
    fn write(mut ref self, bytes: ref Slice[u8]) {\n\
        for b in bytes { self.h = self.h.wrapping_mul(31u64).wrapping_add(b as u64); }\n\
    }\n\
    fn finish(ref self) -> u64 { self.h }\n\
}\n\
struct SumBuild { }\n\
impl BuildHasher for SumBuild {\n\
    type Hasher = Sum;\n\
    fn build(ref self) -> Sum { Sum { h: 0u64 } }\n\
}\n";

/// Spawn a built kara test binary and capture stdout+stderr, with a
/// per-spawn 15s hang watchdog. Thin wrapper over the shared helper
/// in `tests/common/mod.rs` — see the module doc there for the full
/// rationale (concurrent-spawn deadlock, 2026-05-25 incident).
fn output_with_hang_watchdog(cmd: std::process::Command) -> Option<std::process::Output> {
    super::common::output_with_hang_watchdog(cmd, std::time::Duration::from_secs(15))
}

fn run_program(src: &str) -> Option<String> {
    run_program_capturing(src).map(|c| c.stdout)
}

// ── Temporary receiver inside a generic impl (B-2026-08-25-28) ──────
//
// `Bag { xs: v }.inner()` inside `impl[T: Ord] Bag[T]` resolved its
// receiver instantiation to `Bag[T]` — the impl's own type PARAMETER, not
// the concrete type the enclosing monomorph was generated for. `Bag[T]`
// pins nothing, so no monomorph was selected and `inner` was emitted ONCE,
// unmangled, at the erased base layout. Both `mk$i64` and
// `mk$struct$T_ct_String` called that single prototype, and the String
// instantiation read a heap pointer through the wrong layout — SIGSEGV.
//
// The collapse cascaded: with `inner` unmangled, the `self`-receiver calls
// inside it lost their instantiation too, so one unsubstituted receiver
// took `inner` -> `arrange` -> `swap2` down together.
//
// Masked entirely at a scalar `T`, which is why the i64 leg is a control
// rather than the test.

const B28_BAG: &str = "\
struct Bag[=T] { xs: Vec[T] }
impl[T: Ord] Bag[T] {
    fn swap2(mut ref self, i: i64, j: i64) { self.xs.swap(i, j); }
    fn arrange(mut ref self) { let n = self.xs.len(); if n > 1 { self.swap2(0, n - 1); } }
    fn inner(self) -> Vec[T] { let mut b = self; b.arrange(); b.xs }
";

// ── Borrow-elision for read-only `let r = v[i]` (B-2026-06-19-6) ──

/// Count `karac_clone_Vec*` call sites inside the `@main` function body.
fn main_vec_clone_calls(ir: &str) -> usize {
    let mut in_main = false;
    let mut n = 0;
    for line in ir.lines() {
        if line.starts_with("define ") && line.contains("@main") {
            in_main = true;
        }
        if in_main {
            if line.contains("call void @karac_clone_Vec") {
                n += 1;
            }
            if line == "}" {
                break;
            }
        }
    }
    n
}

// ── Closure-scoped `return` in with_provider bodies (B-2026-07-31-16) ──
//
// Per design.md § with_provider signature, the body is a genuine closure
// (`f: Fn() -> T with R, E`) and the call returns `T` — so `return E`
// inside it returns from the CLOSURE (E becomes the with_provider call's
// value), matching the interpreter. Codegen inlines the body, so returns
// are RETARGETED to a merge block (draining body locals then the
// ProviderPop, the interpreter's order) instead of emitting a fn-level
// `ret` — which used to verifier-fail non-tail shapes and, after the
// B-2026-07-31-17 guard, silently returned from the wrong scope.

const WP_PREAMBLE: &str = "trait Counter { fn get(ref self) -> i64; }\n\
         effect resource Ctr: Counter;\n\
         struct InMem { n: i64 }\n\
         impl Counter for InMem { fn get(ref self) -> i64 { self.n } }\n\
         fn read() -> i64 with reads(Ctr) { Ctr.get() }\n";

/// Source for [`test_e2e_two_from_impls_dispatch_by_source_type`], with the
/// two `From` impls emitted in the given order. See the interpreter twin
/// `two_from_impls_dispatch_by_source_type` for why order is a parameter.
fn two_from_impls_src(parse_impl_first: bool) -> String {
    let p = "impl From[ParseError] for AppError { fn from(e: ParseError) -> AppError { return AppError.Parse(e); } }";
    let d = "impl From[DbError] for AppError { fn from(e: DbError) -> AppError { return AppError.Db(e); } }";
    let (a, b) = if parse_impl_first { (p, d) } else { (d, p) };
    format!(
            "struct ParseError {{ tag: String }}\n\
             struct DbError {{ code: i64 }}\n\
             enum AppError {{ Parse(ParseError), Db(DbError) }}\n\
             {a}\n\
             {b}\n\
             fn label(a: AppError) -> String {{\n\
                 match a {{\n\
                     AppError.Parse(p) => return f\"PARSE:{{p.tag}}\",\n\
                     AppError.Db(d) => return f\"DB:{{d.code}}\",\n\
                 }}\n\
             }}\n\
             fn fails_parse() -> Result[i64, ParseError] {{ return Err(ParseError {{ tag: \"p\" }}); }}\n\
             fn fails_db() -> Result[i64, DbError] {{ return Err(DbError {{ code: 7 }}); }}\n\
             fn q_parse() -> Result[i64, AppError] {{ let n = fails_parse()?; return Ok(n); }}\n\
             fn q_db() -> Result[i64, AppError] {{ let n = fails_db()?; return Ok(n); }}\n\
             fn main() {{\n\
                 match q_parse() {{ Ok(_) => println(\"ok\"), Err(e) => println(label(e)), }}\n\
                 match q_db() {{ Ok(_) => println(\"ok\"), Err(e) => println(label(e)), }}\n\
                 println(label(AppError.from(ParseError {{ tag: \"p\" }})));\n\
                 println(label(AppError.from(DbError {{ code: 7 }})));\n\
                 let a: AppError = (ParseError {{ tag: \"p\" }}).into();\n\
                 let b: AppError = (DbError {{ code: 7 }}).into();\n\
                 println(label(a));\n\
                 println(label(b));\n\
             }}"
        )
}

/// Fixture shared by the codegen test below and, verbatim, by
/// `sorted_map_and_set_display_prefix_and_order_interp` in
/// `tests/interpreter.rs` — the interpreter is the oracle for this row, so
/// the two must exercise the same program.
///
/// Insertion order is deliberately NOT sorted order anywhere in it
/// (zebra/apple/mango, 30/10/20), so a regression to hash-bucket or
/// insertion order fails rather than passing by luck.
const SORTED_DISPLAY_SRC: &str = r#"
struct Holder { m: SortedMap[String, i64], s: SortedSet[i64] }

fn mkm() -> SortedMap[String, i64] {
    let mut m: SortedMap[String, i64] = SortedMap.new();
    let _ = m.insert("zebra", 1);
    let _ = m.insert("apple", 2);
    let _ = m.insert("mango", 3);
    return m;
}

fn mks() -> SortedSet[i64] {
    let mut s: SortedSet[i64] = SortedSet.new();
    s.insert(30);
    s.insert(10);
    s.insert(20);
    return s;
}

fn main() {
    let mut bm: SortedMap[String, i64] = SortedMap.new();
    let _ = bm.insert("zebra", 1);
    let _ = bm.insert("apple", 2);
    let _ = bm.insert("mango", 3);
    println(f"{bm}");
    println(bm);
    let mut bs: SortedSet[i64] = SortedSet.new();
    bs.insert(30);
    bs.insert(10);
    bs.insert(20);
    println(f"{bs}");
    println(bs);
    let h = Holder { m: mkm(), s: mks() };
    println(f"{h.m}");
    println(h.s);
    println(f"{mkm()}");
    println(f"{mks()}");
    let vm: Vec[SortedMap[String, i64]] = [mkm()];
    println(f"{vm}");
    let vs: Vec[SortedSet[i64]] = [mks()];
    println(f"{vs}");
    let em: SortedMap[String, i64] = SortedMap.new();
    println(f"{em}");
    let es: SortedSet[i64] = SortedSet.new();
    println(f"{es}");
}
"#;

/// Expected render of [`SORTED_DISPLAY_SRC`] — the `karac run --interp`
/// oracle's byte-for-byte output.
const SORTED_DISPLAY_EXPECTED: &str = "\
SortedMap{apple: 2, mango: 3, zebra: 1}
SortedMap{apple: 2, mango: 3, zebra: 1}
SortedSet{10, 20, 30}
SortedSet{10, 20, 30}
SortedMap{apple: 2, mango: 3, zebra: 1}
SortedSet{10, 20, 30}
SortedMap{apple: 2, mango: 3, zebra: 1}
SortedSet{10, 20, 30}
[SortedMap{apple: 2, mango: 3, zebra: 1}]
[SortedSet{10, 20, 30}]
SortedMap{}
SortedSet{}
";

/// B-2026-09-04-16 — a plain `Map`/`Set` whose binding name is also used by
/// a `SortedMap`/`SortedSet` in ANOTHER function. `twin()` is never called;
/// its mere presence used to be enough.
const SORTED_MARKER_LEAK_SRC: &str = r#"
fn twin() -> i64 {
    let mut cols: SortedMap[i64, i64] = SortedMap.new();
    cols.insert(1, 1);
    let mut only: SortedSet[i64] = SortedSet.new();
    only.insert(4);
    return cols.len() + only.len();
}
fn colliding() -> String {
    let mut cols: Map[i64, i64] = Map.new();
    for i in 0..12 { cols.insert((i * 37) % 97 - 50, i); }
    let k = cols.keys();
    let mut s = "".to_string();
    for i in 0..k.len() { s = s + k[i].to_string() + ","; }
    return s;
}
fn clean() -> String {
    let mut zzz: Map[i64, i64] = Map.new();
    for i in 0..12 { zzz.insert((i * 37) % 97 - 50, i); }
    let k = zzz.keys();
    let mut s = "".to_string();
    for i in 0..k.len() { s = s + k[i].to_string() + ","; }
    return s;
}
fn main() {
    println(if colliding() == clean() { "01 same" } else { "01 DIVERGED" });
    let mut cols: Map[i64, i64] = Map.new();
    cols.insert(7, 7);
    println(cols);
    println(f"03 {cols}");
    let mut only: Set[i64] = Set.new();
    only.insert(5);
    println(only);
    let mut keep: SortedMap[i64, i64] = SortedMap.new();
    keep.insert(9, 9);
    keep.insert(3, 3);
    println(keep);
    println(twin());
}
"#;

/// Expected render of [`SORTED_MARKER_LEAK_SRC`] — the `karac run --interp`
/// oracle's byte-for-byte output. Before the fix the compiled backends
/// produced `01 DIVERGED`, `SortedMap{7: 7}`, `03 SortedMap{7: 7}` and
/// `SortedSet{5}` for the first four lines.
const SORTED_MARKER_LEAK_EXPECTED: &str = "\
01 same
{7: 7}
03 {7: 7}
Set{5}
SortedMap{3: 3, 9: 9}
2
";

/// Stdout + stderr capture. Used by tests that assert against trace
/// output written to stderr by the runtime's atexit handler.
struct CapturedRun {
    stdout: String,
    stderr: String,
    /// Child process exit status. Lets tests distinguish a clean exit
    /// from a crash (SIGSEGV / abort) when stdout alone can't — e.g.
    /// B-2026-06-09-1, where the program prints its output and *then*
    /// faults at scope-exit drop.
    status: std::process::ExitStatus,
}

fn run_program_capturing(src: &str) -> Option<CapturedRun> {
    run_program_capturing_inner(src, None)
}

/// Like `run_program_capturing` but threads `source_filename` into codegen
/// so `?` propagation traces print as `<file>:<line>:<col>`.
fn run_program_capturing_with_filename(src: &str, filename: &str) -> Option<CapturedRun> {
    run_program_capturing_inner(src, Some(filename))
}

fn run_program_capturing_inner(src: &str, filename: Option<&str>) -> Option<CapturedRun> {
    use karac::codegen::{compile_to_object_with_options, link_executable};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    /// Per-process nonce for E2E artifact paths — see the path construction
    /// below (B-2026-08-25-3).
    static E2E_NONCE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

    // Parse errors are programming bugs in the test source, not a
    // legitimate "skip" condition — panic with a clear message so
    // failures surface instead of being swallowed by the downstream
    // `if let Some(out) = out { ... }` accept-on-None pattern.
    let mut parsed = karac::parse(src);
    if !parsed.errors.is_empty() {
        let mut msg = String::from("test source failed to parse:\n");
        for e in &parsed.errors {
            msg.push_str(&format!("  {:?}\n", e));
        }
        panic!("{}", msg);
    }
    // The `karac build` front end between parse and resolve, as one call
    // shared with `cli.rs` — see `lib.rs` `prepare_for_resolve` for the
    // three passes and what skipping any of them costs. This harness used
    // to spell two of them out by hand, in the opposite order from the
    // CLI and without the `#[target(...)]` strip (B-2026-08-11-34).
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    // Resolve/typecheck gate — `karac build` refuses to reach codegen
    // when either phase errors (cli.rs `has_fatal_errors`), so a program
    // that flunks them is input production never compiles. Runs before
    // `lower` so the diagnostic names the real cause instead of whatever
    // the mistyped program later trips in the backend. See the fn doc.
    super::common::assert_check_clean(&resolved, &typed, src);
    karac::lower(&mut parsed.program, &typed);
    // Ownership-loaded by default — `karac build` (cli.rs) always
    // passes `pipeline.ownership` to codegen, so a harness that
    // passes `None` leaves the entire RC-fallback boxing surface
    // (`is_rc_fallback_binding` / `rc_fallback_heap_types`) dead
    // across the suite and systematically diverges from shipped
    // binaries on exactly the bindings the ownership checker flags.
    // That blind spot hid the Option[shared] RC-fallback boxing
    // collision (b027fc15 bug 3): every real build of the
    // prepend-builder shape segfaulted while 1100+ E2E tests stayed
    // green. The suite tests what ships.
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    // And ships only ownership-CLEAN programs — `karac check` rejects
    // on ownership errors, so a test program that flunks the checker
    // feeds codegen input production never reaches (that masked
    // B-2026-07-01-10 for weeks).
    super::common::assert_check_clean(&resolved, &typed, src);
    // Effects are the THIRD phase of the same gate, and were simply
    // absent — a test could pin behaviour for a program `karac build`
    // refuses (B-2026-08-19-5). Runs after `lower`, threaded with the
    // typechecker's tables, exactly as `Pipeline::run_all_checks` does.
    super::common::assert_effects_clean_for(&parsed.program, &typed, src);
    super::common::assert_ownership_clean(&ownership, src);

    // W3.3 — LLJIT dispatch under env-var control. Routes the whole
    // 543-test E2E suite through `LLJITEngine` instead of the AOT
    // object+link+spawn path. Stderr is always empty under this
    // path (the runtime's atexit handler runs at test-binary exit,
    // not per-test) — stderr-dependent assertions need a different
    // approach and are currently the known failure bucket.
    // Arrow IPC and `String.normalize` programs cannot run on the JIT lane:
    // `to_arrow_ipc` / `from_arrow_ipc` lower to `karac_arrow_*`, and
    // `.normalize(form)` lowers to `karac_unicode_*` — both resolved only
    // from their opt-in archives (`libkarac_runtime_arrow.a` /
    // `libkarac_runtime_unicode.a`), but the `karac_jit_runner` links the
    // runtime WITHOUT the `arrow` or `unicode` feature (`Symbols not found:
    // [ karac_arrow_column_to_ipc ]` / a `karac_unicode_*` crash). `karac
    // run` handles this by routing such programs to the interpreter (cli.rs
    // `program_uses_arrow_ipc`, and the `@karac_unicode_` IR scan beside the
    // regex one); the test harness has no interpreter oracle here, so it
    // routes them to the AOT path instead, where they run against the real
    // archive when present and otherwise soft-skip on its controlled "build
    // libkarac_runtime_<feature>.a" link error — identical behavior to the
    // AOT leg. `.normalize(` matches the String method, not the
    // `l2_normalize(` embeddings function, which stays on the JIT lane. A
    // cheap source scan (matching cli.rs) keeps the gate simple.
    #[cfg(feature = "llvm")]
    if std::env::var("KARAC_TEST_JIT").as_deref() == Ok("1")
        && !(src.contains(".to_arrow_ipc(")
            || src.contains("from_arrow_ipc(")
            || src.contains(".normalize("))
    {
        return jit_dispatch(&parsed.program, Some(&ownership), filename);
    }

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    // B-2026-08-25-3 — the path carries a per-PROCESS nonce as well as the
    // pid and the counter. pid+counter is unique among LIVE processes, but
    // it is NOT unique over time: a killed run leaks its artifacts (cleanup
    // only runs on the success path — 51 stale `/tmp/karac_e2e_*` files were
    // present when this was written), and a later process that reuses the
    // pid restarts the counter at 0, landing on exactly those paths. The
    // nonce makes that aliasing impossible rather than unlikely.
    let nonce = *E2E_NONCE.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::from(d.subsec_nanos()) ^ d.as_secs())
            .unwrap_or(0)
    });
    let obj_path = format!("/tmp/karac_e2e_{}_{}_{}.o", std::process::id(), nonce, id);
    let exe_path = format!("/tmp/karac_e2e_{}_{}_{}", std::process::id(), nonce, id);
    // Belt and braces: never inherit a file at either path. If one somehow
    // exists, executing it would run SOMEBODY ELSE'S PROGRAM and the
    // assertion would be about the wrong binary entirely — the failure mode
    // B-2026-08-25-3 has to rule out before it can blame codegen.
    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&exe_path);

    // Codegen failures are also programming bugs (in the compiler or in
    // the test program) — surface them loudly. Link and exec failures
    // stay as soft-skip because they can fire in environments that
    // lack libkarac_runtime.a or a working linker — EXCEPT an
    // undefined-symbol link failure, which means the archive was found
    // but is stale; `link_or_skip` panics on that rather than letting a
    // stale archive silently void the suite (see its doc comment).
    if let Err(e) = compile_to_object_with_options(
        &parsed.program,
        &obj_path,
        Some(&ownership),
        None,
        filename,
        None,
    ) {
        panic!("codegen failed for test program: {}", e);
    }
    super::common::link_or_skip(link_executable(&obj_path, &exe_path))?;
    // B-2026-08-25-3 — prove the thing we are about to execute was produced
    // by the link that just ran. A soft-skip returns above, so reaching here
    // means the linker reported success; if the file is missing anyway, the
    // run that follows would be meaningless (or would exec a leftover), and
    // a loud failure beats an assertion about an unknown binary.
    assert!(
        std::path::Path::new(&exe_path).exists(),
        "link reported success but produced no executable at {exe_path} — \
             refusing to run whatever else may be at that path"
    );

    let output = output_with_hang_watchdog(std::process::Command::new(&exe_path))?;

    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&exe_path);

    Some(CapturedRun {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        status: output.status,
    })
}

/// W3.4 LLJIT dispatch — **subprocess** helper instead of
/// in-process JIT. Writes the codegen-emitted LLVM IR to a tempfile
/// and shells out to the `karac_jit_runner` bin target; that helper
/// runs the LLJIT compile + executes `main` and exits with main's
/// return code (or with `emit_panic`'s `exit(1)` when a runtime
/// check fires). `Command::output` via the existing hang-watchdog
/// captures stdout + stderr + exit code identically to the AOT path.
///
/// **Why subprocess rather than in-process (W3.3 → W3.4).** W3.3 ran
/// the JIT in the test runner itself. That works for the 134 tests
/// that don't panic, but tests which *intentionally* trip a bounds
/// check, map-miss, or runtime abort terminate the runner via
/// `exit(1)` / `abort()`, killing the whole `cargo test` invocation.
/// Subprocess isolation collapses two known stop-point classes —
/// panic-asserting tests **and** stderr-atexit `?`-trace tests (the
/// runtime's atexit printer now fires on the child's exit, not on
/// test-binary exit) — into a non-issue.
///
/// The "always-JIT" promise lives in production (users running
/// `karac run foo.kara` get true in-process JIT) and in the direct
/// engine-driven tests under `tests/lljit_prototype.rs` /
/// `tests/lljit_e2e.rs`. The codegen suite uses subprocess JIT
/// only as a test-runner artifact, parallel to how the AOT codegen
/// suite uses subprocess-execed binaries.
#[cfg(feature = "llvm")]
fn jit_dispatch(
    program: &karac::ast::Program,
    ownership: Option<&karac::ownership::OwnershipCheckResult>,
    filename: Option<&str>,
) -> Option<CapturedRun> {
    use karac::codegen::compile_to_ir_with_options;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let _ = super::force_link_karac_runtime();

    // Codegen failures are programming bugs in the compiler or test
    // — surface loudly, mirroring the AOT path's `panic!` on
    // `compile_to_object` failure. `filename` is threaded through
    // so `?`-propagation traces print `<file>:<line>:<col>`
    // consistently with the AOT path.
    //
    // `ownership` is threaded through for the SAME reason the AOT leg
    // passes it (see the comment at the `ownershipcheck` call above), and
    // it is load-bearing for THIS lane specifically. This lane's entire
    // purpose is run==build parity, so it has to feed codegen the same
    // inputs `karac build` and `karac run` do — cli.rs's JIT path runs
    // `pipeline.ownershipcheck()` before `compile_to_ir_with_options`
    // precisely "so the emitted IR matches `karac build`'s". Passing
    // `None` here made the lane emit IR that matched NEITHER shipped
    // path, silently voiding every ownership-derived codegen decision on
    // it: the RC-fallback boxing surface, and the `UseAfterMove`
    // defensive copy (B-2026-08-10-21), whose hint channel
    // (`use_after_move_consume_sites`) rides on this argument. Its two
    // pins then failed on this lane ALONE, reading as a JIT miscompile
    // when the JIT was fine and the harness was starving it — the same
    // shape of blind spot as b027fc15 bug 3, one lane over.
    let ir = match compile_to_ir_with_options(program, ownership, None, filename, None) {
        Ok(ir) => ir,
        Err(e) => panic!("compile_to_ir failed for JIT dispatch: {}", e),
    };

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ir_path = format!("/tmp/karac_jit_ir_{}_{}.ll", std::process::id(), id);
    {
        let mut f = std::fs::File::create(&ir_path).expect("create IR tempfile");
        f.write_all(ir.as_bytes()).expect("write IR");
    }

    // `CARGO_BIN_EXE_<name>` is a cargo-set compile-time env var
    // resolving to the helper binary's path. Cargo guarantees the
    // bin target is built before the test crate when both share
    // the workspace — no runtime path-hunting needed.
    let runner = env!("CARGO_BIN_EXE_karac_jit_runner");
    let mut cmd = std::process::Command::new(runner);
    cmd.arg(&ir_path);
    // Run==build parity for `env.args()`: under the JIT the hosting process
    // is `karac_jit_runner`, so an unset KARAC_PROGRAM_ARGS makes
    // `karac_runtime_env_args_into` fall back to the RUNNER's argv
    // (`[runner, <ir>.ll]`, len 2) instead of the program's own (an AOT
    // binary sees just `[argv0]`, len 1). A test that folds
    // `env.args().len()` into its output (a deliberate not-a-constant, e.g.
    // `e2e_named_source_moved_into_a_tuple_element_is_disarmed`) then
    // diverges from the AOT oracle by exactly that off-by-one. `karac run`
    // already sets this var (cli.rs) — the harness must too. One synthetic
    // argv0 element ⇒ len 1, matching AOT; the value is unread (tests use
    // `.len()`). B-2026-07-29-18 built this channel for precisely this.
    cmd.env("KARAC_PROGRAM_ARGS", &ir_path);

    let output = output_with_hang_watchdog(cmd);

    let _ = std::fs::remove_file(&ir_path);

    let output = output?;

    // A SIGNAL death here is a crash of the JIT runner, and `run_program`
    // keeps only `.stdout` — so without this a crash surfaces as
    // `left: ""` against the expected output, with the actual cause
    // discarded. B-2026-09-19-3 was a `free(): double free detected in
    // tcache 2` that cost a full investigation to see, because nothing in
    // the failure report mentioned it: buffered stdout is lost to the
    // abort, so the assertion compares "" and blames the backend.
    //
    // Two things make this safe to report unconditionally. A test that
    // asserts on a deliberate non-zero EXIT uses `run_program_capturing`
    // and reads `.status` itself, so this does not steal its case; and a
    // signal is never a program's own choice, unlike an exit code.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = output.status.signal() {
            eprintln!(
                "[jit-lane] karac_jit_runner died on SIGNAL {sig} with {} byte(s) of \
                     stdout — the assertion below compares that truncated output, not a \
                     backend result. NOTE: a label reading \"AOT\" in the failing test is a \
                     hardcoded string and runs on this lane too. Runner stderr:\n{}",
                output.stdout.len(),
                String::from_utf8_lossy(&output.stderr),
            );
        }
    }

    Some(CapturedRun {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        status: output.status,
    })
}

/// Historical alias from when the default harness passed
/// `ownership: None`. `run_program` is ownership-loaded now (see
/// `run_program_capturing_inner`), so this just delegates — kept so
/// the tests written against the split read unchanged and the name
/// keeps documenting *why* those tests exist (they exercise
/// RC-fallback-flagged shapes).
fn run_program_with_ownership(src: &str) -> Option<String> {
    run_program(src)
}

/// The canonical row-major converging two-pointer, shared by the two IR
/// wiring tests below. Its ONLY Vec index sites are `v[base + lo]` and
/// `v[base + hi]`, so any surviving `vidx.` bounds-check block in the IR
/// belongs to them.
const CONV_TWO_POINTER_SRC: &str = r#"
fn row_scan() -> i64 {
    let n = 20i64;
    let len = 32i64;
    let v: Vec[u8] = Vec.filled(n * len, 48u8);
    let mut acc = 0i64;
    let mut i = 0i64;
    while i < n {
        let base = i * len;
        let mut lo = 0i64;
        let mut hi = len - 1i64;
        while lo <= hi {
            acc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);
            lo = lo + 1i64;
            hi = hi - 1i64;
        }
        i = i + 1i64;
    }
    acc
}
"#;

/// The same program with `lo`'s step moved BEFORE the index, which defeats
/// the bounds proof. Used as the differential baseline: it has exactly the
/// same arithmetic, so any difference in `sadd.with.overflow` count is the
/// index adds and nothing else.
fn conv_two_pointer_unproven_src() -> String {
    let src = CONV_TWO_POINTER_SRC.replace(
        "acc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);\n            lo = lo + 1i64;",
        "lo = lo + 1i64;\n            acc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);",
    );
    assert_ne!(src, CONV_TWO_POINTER_SRC, "reorder anchor did not match");
    src
}

fn count_add_overflow_intrinsics(ir: &str) -> usize {
    ir.matches("@llvm.sadd.with.overflow").count()
}

/// The row-helper shape: the length pin, the enclosing counter and the
/// linear base all sit in the CALLER, so nothing inside `row_scan` can
/// prove its own index in range. Shared by the interprocedural wiring
/// tests below. `{CALLER_LOOP}` is substituted per test.
const INTERPROC_ROW_HELPER_SRC: &str = r#"
fn row_scan(v: ref Vec[u8], base: i64, len: i64) -> i64 {
    let mut lo = 0i64;
    let mut hi = len - 1i64;
    let mut acc = 0i64;
    while lo <= hi {
        acc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);
        lo = lo + 1i64;
        hi = hi - 1i64;
    }
    acc
}

fn driver() -> i64 {
    let n = 20i64;
    let len = 32i64;
    let v: Vec[u8] = Vec.filled(n * len, 48u8);
    let mut acc = 0i64;
    let mut i = 0i64;
    {CALLER_LOOP}
    acc
}
"#;

fn interproc_row_helper_src(caller_loop: &str) -> String {
    let out = INTERPROC_ROW_HELPER_SRC.replace("{CALLER_LOOP}", caller_loop);
    assert_ne!(out, INTERPROC_ROW_HELPER_SRC, "caller-loop anchor missing");
    out
}

// ── Arithmetic fault traps (design.md § Arithmetic Overflow) ───────
// AOT parity with the interpreter's checked arithmetic
// (src/interpreter/eval_ops.rs): + - * trap "integer overflow",
// / and % by zero trap "division by zero", iN::MIN / -1 and % -1
// trap "integer overflow" (same family), unary -iN::MIN traps.
// Before 2026-06-07 these were nsw/raw ops — IR-level UB that
// happened to wrap (or print garbage, for div) on arm64. Operands
// are built via `let mut` + reassignment so neither const-eval nor
// instcombine folds the fault away before the runtime check.

/// Pull `(line, column)` out of the AOT panic line
/// `panic at <file>:<line>:<col> in <fn>: <msg>`.
fn panic_location(stdout: &str) -> Option<(usize, usize)> {
    let tail = stdout.split("panic at ").nth(1)?;
    let loc = tail.split(" in ").next()?;
    let mut parts = loc.rsplitn(3, ':');
    let col: usize = parts.next()?.trim().parse().ok()?;
    let line: usize = parts.next()?.trim().parse().ok()?;
    Some((line, col))
}

// ── pattern-arm unbound heap-field drop (B) ──
//
// docs/spikes/pattern-arm-unbound-field-drop.md: a fresh-temp enum
// scrutinee (`if let Full(_, n) = make()`) has no source `EnumDrop`, so an
// arm that leaves a heap payload field unbound leaks it. The fix
// materializes the temp into `__freshtemp_enum_scrut` + `track_enum_var`
// (the `__karac_drop_<E>` walk frees unbound fields); the per-arm
// suppression zeroes the caps of fields the pattern moved into bindings.
// These IR tests are the macOS-reliable gate (no LeakSanitizer there);
// the ASAN suite adds the bound-field double-free gate.

const B_ENUM_PRELUDE: &str = r#"
enum Holder { Full(Vec[i64], i64), Empty }
fn make() -> Holder {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    return Holder.Full(v, 42_i64);
}
"#;

// ── Atomic RC for par-block bindings ──────────────────────────
//
// The ownership pass produces `arc_values` (a per-function subset of
// `rc_values`) for bindings that cross a `par {}` thread boundary. Codegen
// routes inc/dec on those bindings through `atomicrmw add` / `atomicrmw
// sub` (`SeqCst`) so the refcount mutates race-free across threads.
// Bindings in `rc_values` but not `arc_values` continue to use the plain
// non-atomic load+arith+store sequence.

/// Compile to LLVM IR with the ownership-pass result threaded through, so
/// the codegen `arc_fallback_fns` table is populated. The plain `ir_for`
/// helper passes `None` for ownership and never exercises the atomic path.
fn ir_for_with_ownership(src: &str) -> String {
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
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    super::common::assert_check_clean(&resolved, &typed, src);
    // Effects are the THIRD phase of the same gate, and were simply
    // absent — a test could pin behaviour for a program `karac build`
    // refuses (B-2026-08-19-5). Runs after `lower`, threaded with the
    // typechecker's tables, exactly as `Pipeline::run_all_checks` does.
    super::common::assert_effects_clean_for(&parsed.program, &typed, src);
    super::common::assert_ownership_clean(&ownership, src);
    compile_to_ir(&parsed.program, Some(&ownership), None).expect("codegen failed")
}

/// Count `call void @free(...)` instructions inside `@main`'s body.
fn main_free_count(ir: &str) -> usize {
    let mut in_main = false;
    let mut frees = 0;
    for line in ir.lines() {
        if line.starts_with("define") && line.contains("@main(") {
            in_main = true;
        }
        if in_main {
            // Vec/String data-buffer releases route through the
            // recycling entry `karac_free_buf` (large-buffer cache);
            // box/handle frees stay on libc `free`. Count both — these
            // tests pin THAT a buffer is released, not which allocator
            // entry releases it.
            if line.contains("call void @free(") || line.contains("call void @karac_free_buf(") {
                frees += 1;
            }
            if line == "}" {
                break;
            }
        }
    }
    frees
}

/// The IR of `@main`'s body only (between its `define` and the closing
/// `}`), so a `.contains(...)` check can't false-positive on a module-level
/// `declare` line or a call inside a different function.
fn main_body_ir(ir: &str) -> String {
    let mut in_main = false;
    let mut body = String::new();
    for line in ir.lines() {
        if line.starts_with("define") && line.contains("@main(") {
            in_main = true;
        }
        if in_main {
            body.push_str(line);
            body.push('\n');
            if line == "}" {
                break;
            }
        }
    }
    body
}

// ── B follow-up #3: owned struct-destructure dispatch + cleanup ──
//
// `let Point { items, count } = make()` used to register neither method
// dispatch nor scope-exit cleanup for the field bindings, so `items.len()`
// failed to compile ("no handler for method") and the heap field leaked.
// The fix (`finish_owned_struct_destructure`, src/codegen/stmts.rs) wires
// both per field; cleanup is gated to a fresh-temp RHS. See
// docs/spikes/pattern-arm-unbound-field-drop.md.

const STRUCT_DESTRUCTURE_PRELUDE: &str = "struct Point { items: Vec[i64], count: i64 }\nfn make() -> Point { let mut v: Vec[i64] = Vec.new(); v.push(10_i64); v.push(20_i64); return Point { items: v, count: 5 }; }\n";

// `Set` struct field — closes the Set leg of the struct-destructure
// remaining-leak list (phase-6 § "Pattern-arm unbound heap-field drop").
// Set lowers to `Map[T, ()]`, so cleanup routes through the same
// `track_map_var` / `karac_map_free` path as a Map field (key_is_vec read
// from `set_elem_types`; no value half). Both-primitive → plain
// `karac_map_free`, not `_with_drop_vec`.
const SET_DESTRUCTURE_PRELUDE: &str = "struct Bag { tags: Set[i64], count: i64 }\nfn make() -> Bag { let mut s: Set[i64] = Set.new(); s.insert(10_i64); s.insert(20_i64); return Bag { tags: s, count: 5 }; }\n";

// ── ? error_return_trace KARAC_ERROR_TRACE_FORMAT env-var dispatch ───────
//
// The runtime's atexit printer reads `KARAC_ERROR_TRACE_FORMAT` and
// dispatches between three emitters:
//   - text   (default; missing/unrecognized values fall back here)
//   - json   (single-document — bare array, or `{frames,truncated}`
//            when the ring buffer dropped older entries)
//   - jsonl  (line-delimited JSON; one event per line)
// The JSON shape mirrors the interpreter's `format_error_trace_json`
// verbatim. These tests exercise the full compile → link → run path
// with the env var threaded into the child process.

fn run_program_capturing_with_env(
    src: &str,
    filename: Option<&str>,
    env: &[(&str, &str)],
) -> Option<CapturedRun> {
    use karac::codegen::{compile_to_object_with_options, link_executable};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut parsed = karac::parse(src);
    if !parsed.errors.is_empty() {
        let mut msg = String::from("test source failed to parse:\n");
        for e in &parsed.errors {
            msg.push_str(&format!("  {:?}\n", e));
        }
        panic!("{}", msg);
    }
    // The `karac build` front end between parse and resolve, as one call
    // shared with `cli.rs` — see `lib.rs` `prepare_for_resolve` for the
    // three passes and what skipping any of them costs. This harness used
    // to spell two of them out by hand, in the opposite order from the
    // CLI and without the `#[target(...)]` strip (B-2026-08-11-34).
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    // Ownership-loaded, same rationale as `run_program_capturing_inner`.
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    super::common::assert_check_clean(&resolved, &typed, src);
    // Effects are the THIRD phase of the same gate, and were simply
    // absent — a test could pin behaviour for a program `karac build`
    // refuses (B-2026-08-19-5). Runs after `lower`, threaded with the
    // typechecker's tables, exactly as `Pipeline::run_all_checks` does.
    super::common::assert_effects_clean_for(&parsed.program, &typed, src);
    super::common::assert_ownership_clean(&ownership, src);

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let obj_path = format!("/tmp/karac_e2e_envtrace_{}_{}.o", std::process::id(), id);
    let exe_path = format!("/tmp/karac_e2e_envtrace_{}_{}", std::process::id(), id);

    if let Err(e) = compile_to_object_with_options(
        &parsed.program,
        &obj_path,
        Some(&ownership),
        None,
        filename,
        None,
    ) {
        panic!("codegen failed for test program: {}", e);
    }
    super::common::link_or_skip(link_executable(&obj_path, &exe_path))?;

    let mut cmd = std::process::Command::new(&exe_path);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = output_with_hang_watchdog(cmd)?;

    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&exe_path);

    Some(CapturedRun {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        status: output.status,
    })
}

/// Source common to all three format-dispatch tests: a single `?`
/// site so the trace has exactly one frame, threaded through a
/// source filename so each frame carries `<file>:<line>:<col>` —
/// gives the JSON / JSONL emitters something non-empty to escape
/// (and keeps the JSON shape assertion easy to read).
const TRACE_FORMAT_SRC: &str = r#"
fn boom() -> Result[i64, i64] { Err(7_i64) }
fn caller() -> Result[i64, i64] {
    let _ = boom()?;
    Ok(0_i64)
}
fn main() {
    match caller() {
        Ok(_) => println(0_i64),
        Err(e) => println(e),
    }
}
"#;

// ── Debugger Contract: SpawnSiteId metadata table ──
//
// Slice 3 of the four-piece Debugger Contract (`design.md § AI-First
// Compiler Interface > Debugger Contract`). For every `par {}` block
// (explicit or compiler-inferred) codegen records a `(id, file,
// line, col, worker_count)` tuple and emits a module-scope
// `KARAC_SPAWN_SITES` array, plus the companion `KARAC_SPAWN_SITES_LEN`
// and `KARAC_SPAWN_SITES_ENABLED` globals. The IDs are stable per
// binary and serve as the join key consumed by slices 4 and 5 (and
// the future `std.panic` crash report's `parallel_context` field).
//
// Tests use IR-level string-grep — same precedent as
// `test_repeat_literal_const_zero_uses_memset`.
//
// Test isolation: the gate is read at `Codegen::new` time, and these
// tests pin it per-thread through `karac::codegen::pin_runtime_debug_
// metadata` rather than by setting `KARAC_RUNTIME_DEBUG_METADATA`
// process-wide. They used to do the latter under a mutex shared only
// with each other, which left every OTHER concurrently-compiling test in
// this binary seeing the gate flip mid-compile (B-2026-08-20-26). The
// pin needs no lock: nothing outside the pinning thread observes it.

/// Compile to IR with explicit source-text plumbing, threading
/// the source through the new `source_text` parameter so
/// `record_spawn_site` resolves byte offsets to `(line, col)`.
fn ir_for_with_source(src: &str) -> String {
    use karac::codegen::compile_to_ir_with_options;
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
    compile_to_ir_with_options(&parsed.program, None, None, Some("test.kara"), Some(src))
        .expect("codegen failed")
}

// ── Phase 6 line 26 slice 5: state-struct LLVM type emission ───────
//
// For each entry in `Program.state_struct_layouts` (built by slice 4
// in `Pipeline::effectcheck`), codegen emits a named LLVM struct
// `%kara.state.<fn_key>` carrying field 0 = i32 yield-point tag and
// fields 1..n = one slot per captured local sized via the
// typechecker-recorded `type_name`. This slice only emits the types;
// function-body lowering against them lands in slice 6.

/// Drive parse → resolve → typecheck → lower → effectcheck → build
/// network-yield / yield-points / state-struct-layouts side-tables
/// on the program, then compile to LLVM IR. Mirrors
/// `Pipeline::effectcheck`'s wiring exactly so codegen sees the same
/// program state it would in the cli path.
fn ir_for_with_state_struct_layouts(src: &str) -> String {
    use karac::cli::{
        build_call_effect_subs_table, build_callee_network_yield_effect_table,
        build_callee_purely_polymorphic_effects_set, build_state_struct_layouts,
        build_yield_points_table,
    };
    use karac::codegen::compile_to_ir;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "type errors: {:?}", typed.errors);
    let method_types = typed.method_callee_types.clone();
    let call_type_subs = typed.call_type_subs.clone();
    let pattern_binding_types = typed.pattern_binding_types.clone();
    karac::lower(&mut parsed.program, &typed);
    let effects = karac::effectcheck_with_typecheck_data(
        &parsed.program,
        karac::effectchecker::PublicEffectsPolicy::default(),
        karac::manifest::CompileProfile::Default,
        method_types.clone(),
        call_type_subs,
    );
    parsed.program.callee_network_yield_effect = build_callee_network_yield_effect_table(&effects);
    let yield_points = build_yield_points_table(
        &parsed.program,
        &parsed.program.callee_network_yield_effect,
        &method_types,
    );
    parsed.program.yield_points = yield_points;
    parsed.program.state_struct_layouts = build_state_struct_layouts(
        &parsed.program,
        &parsed.program.callee_network_yield_effect,
        &method_types,
        &pattern_binding_types,
    );
    // Slice 8aa/8ab/8y inputs: per-call effect-variable
    // substitutions and the purely-polymorphic-callee marker set.
    // Empty for the legacy fixtures (no `with E` callees) so
    // `compile_generic_call`'s slice 8y gate degrades to the
    // pre-8y conservative default (state-machine intercept fires
    // for every state_struct_layouts member).
    parsed.program.call_effect_subs = build_call_effect_subs_table(&effects);
    parsed.program.callee_purely_polymorphic_effects =
        build_callee_purely_polymorphic_effects_set(&effects);
    compile_to_ir(&parsed.program, None, None).expect("codegen failed")
}

// ── Phase 6 line 26 slice 7: switch-on-tag dispatch ────────────────
//
// Slice 6 emitted a poll-fn stub that loaded the yield-point tag and
// unconditionally returned Pending. Slice 7 replaces that
// unconditional return with a `switch i32 %tag` against N+1 arm
// labels (entry state + one post-yield state per yield point). Each
// arm still returns Pending — slice 8 fills in the per-arm
// captured-locals reload + actual user-code resume.

/// Extract the textual LLVM IR for a named function from a module
/// dump. Returns the substring starting at `define internal ... @<name>(`
/// and ending at the matching closing `}`. Used by slice-7+ tests to
/// assert against per-function shapes without grepping a global IR
/// blob (where arm blocks from other functions could collide).
fn extract_fn_ir<'a>(ir: &'a str, fn_name: &str) -> &'a str {
    let needle = format!("@{fn_name}(");
    let start = ir.find(&needle).unwrap_or_else(|| {
        panic!("function @{fn_name} not found in IR:\n{ir}");
    });
    // Find the start of the `define` line so we capture the full
    // signature, not just from the @-name.
    let line_start = ir[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    // End: scan forward for the standalone `}` that closes the fn.
    let tail = &ir[line_start..];
    let end_rel = tail.find("\n}\n").unwrap_or(tail.len());
    &tail[..end_rel + 3]
}

// ── Phase 6 line 17/170: karac_park_on_fd leaf parking primitive ──
//
// The leaf primitive recognised by codegen. When effectcheck classifies
// it as a network-yield callee (via the declared
// `sends(Network) receives(Network)` effects), the state-machine
// emission pass overrides its body with a hand-rolled 2-state machine.
// Async-sched slice 2/3 (dispatcher-yield): state_0 allocates a
// per-park completion slot + registers the fd and returns Pending;
// state_1 — re-invoked by the dispatcher only on real readiness, routed
// by the wakeup's parked pointer — signals that slot and returns Ready.
// The caller blocks on the slot (rather than re-polling) and deregisters
// the fd afterwards. The state struct carries the `KaracParkedTask
// { poll_fn, state }` record (so the dispatcher can re-invoke the
// poll-fn) plus trailing `token` + `slot` fields.

fn park_on_fd_source() -> &'static str {
    "effect resource Network;
         pub fn karac_park_on_fd(fd: i32, direction: u8) \
             with sends(Network) receives(Network) {}
         fn driver(socket_fd: i32) { karac_park_on_fd(socket_fd, 0); }"
}

/// Locate a function definition body by name and return its lines
/// from the opening `{` (exclusive) through the closing `}`.
/// Returns `None` if the function is not defined in the IR. Counts
/// braces to handle nested blocks; LLVM IR doesn't currently emit
/// braces inside function bodies except as block delimiters, but
/// the counting approach future-proofs against any inline-asm or
/// metadata strings that might include them.
fn function_body(ir: &str, name: &str) -> Option<String> {
    let needle = format!("@{name}(");
    let mut found_define = false;
    let mut depth = 0i32;
    let mut body = String::new();
    for line in ir.lines() {
        if !found_define {
            if line.starts_with("define ") && line.contains(&needle) {
                found_define = true;
                depth = line.matches('{').count() as i32 - line.matches('}').count() as i32;
                continue;
            }
        } else {
            body.push_str(line);
            body.push('\n');
            depth += line.matches('{').count() as i32;
            depth -= line.matches('}').count() as i32;
            if depth <= 0 {
                return Some(body);
            }
        }
    }
    None
}

/// Extract `@takes`'s single parameter type from an IR dump. Shared by the
/// alias-layout tests below; mirrors the local helper in
/// `test_ir_refinement_over_string_matches_base_layout`.
fn takes_param_type(ir: &str) -> String {
    let i = ir.find("@takes(").expect("no @takes in IR");
    let rest = &ir[i + "@takes(".len()..];
    let end = rest.find([' ', ')']).unwrap_or(rest.len());
    rest[..end].to_string()
}

// ── Contracts — release-mode stripping (design.md § Contracts) ────
//
// "Checked at runtime in debug builds, stripped in release." With
// stripping on, no contract assert (`requires` / `ensures` / `old` /
// `invariant`) is emitted — the `contract violated` fault string, the
// marker of an emitted assert, disappears from the IR while the function
// body itself remains intact. `ir_for_contracts_stripped` forces the
// decision race-free (no global env mutation).

// A program exercising all four contract kinds in one module.
const ALL_CONTRACTS_SRC: &str = r#"
struct Account { balance: i64, invariant self.balance >= 0 }
impl Account {
    pub fn withdraw(mut ref self, amount: i64) -> i64
        requires amount > 0
        ensures(result) self.balance == old(self.balance) - amount
    { self.balance = self.balance - amount; amount }
}
fn checked(x: i64) -> i64 requires x > 0 ensures(result) result > x { x * 2 }
fn main() {
    let mut a = Account { balance: 100 };
    println(a.withdraw(30));
    println(checked(5));
}
"#;

// ── Portable SIMD `Vector[T, N]` — slice 1 ───────────────────────────
//
// design.md § Portable SIMD. Slice 1 surface: construction
// `Vector[T, N](lane0, …)`, element-wise arithmetic (`+ - * / %`), and
// lane read `v[i]`. Codegen lowers to LLVM `<N x T>` (insertelement chain
// for construction, native vector arithmetic, extractelement for lane
// read); LLVM's instruction selector handles the native-vs-scalar
// auto-fallback. Phase-7 line 289 sub-slices cover splat / dot / cross /
// reductions / masks / `#[require_simd]` / `--simd-report` / interpreter
// parity.

/// Helper: parse → resolve → typecheck, returning the typechecker errors as
/// debug strings. `run_program` deliberately ignores typecheck errors
/// (see the documented codegen-bypasses-typecheck gap), so reject-tests
/// assert against this directly.
fn vector_typecheck_errors(src: &str) -> Vec<String> {
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    typed.errors.iter().map(|e| format!("{:?}", e)).collect()
}

// ── Ownership-derived parameter alias attributes (noalias-ref-params) ──
//
// A `mut ref T` parameter (exclusive borrow) and an owned value-semantics
// `ptr` type both carry `noalias`. A `ref T` (shared read borrow) of a
// Freeze type — no transitive `Atomic`/`Mutex`/`shared` interior
// mutability — carries `readonly` (NOT `noalias`: shared borrows may
// alias). See `emit_param_alias_attrs` in src/codegen/functions.rs.
fn define_line<'a>(ir: &'a str, sym: &str) -> &'a str {
    ir.lines()
        .find(|l| l.contains("define") && l.contains(sym))
        .unwrap_or_else(|| panic!("no `define` line for {sym} in IR"))
}

// ── Scoped-alias metadata for slice params (alias-metadata slice 4) ──
//
// A `mut Slice[T]` is an exclusive borrow → disjoint from every other slice
// param; a shared `Slice[T]` is disjoint from every exclusive one. That
// restrict-like fact is lowered onto the element loads/stores as
// `!alias.scope` / `!noalias`. See src/codegen/slice_alias.rs.

/// The text of a single `define … @sym(…) { … }` block, for asserting
/// metadata on the accesses inside one function.
fn fn_body(ir: &str, sym: &str) -> String {
    let mut out = String::new();
    let mut in_fn = false;
    for l in ir.lines() {
        if !in_fn && l.starts_with("define") && l.contains(sym) {
            in_fn = true;
        }
        if in_fn {
            out.push_str(l);
            out.push('\n');
            if l == "}" {
                break;
            }
        }
    }
    assert!(!out.is_empty(), "no define block for {sym}");
    out
}

/// The names a user ENUM may shadow without codegen describing it with the
/// built-in's layout (B-2026-08-15-12).
///
/// `builtin_opaque_ptr_handle`'s list, verbatim. Every one of these lowers
/// to a bare `ptr` when it is the built-in, so a user enum of the same name
/// whose shadow went unrecorded had its `ref`/owned parameter lowered to a
/// pointer instead of its tagged-union type — and the match's payload
/// binding was then never registered.
const SHADOWABLE_HANDLE_NAMES: [&str; 14] = [
    "Map",
    "Set",
    "SortedMap",
    "SortedSet",
    "Tensor",
    "Column",
    "DataFrame",
    "Interner",
    "Arena",
    "Request",
    "File",
    "Sender",
    "Receiver",
    "Channel",
];
