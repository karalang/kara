//! Vec -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen vecs::
//!
//! New fixtures about Vec belong in this file.

use super::*;

#[test]
fn e2e_owned_param_into_vec_literal_no_double_free() {
    // B-2026-07-18-38: an owned (caller-retains) String/Vec PARAM used as a
    // Vec-literal element (`fn dup(x: String) -> Vec[String] { [x] }`)
    // double-freed under AOT/JIT (interp was correct). The by-value header
    // ABI leaves the buffer's free with the CALLER, so the returned Vec's
    // element aliased the caller's arg and both freed it. The move-out
    // cap-zero doesn't cover this — an owned param registers no callee-side
    // FreeVecBuffer, so zeroing its slot is a no-op. `compile_vec_prefix_
    // literal` now deep-copies owned-param elements (like tuple/push/return/
    // struct-field/map-value do). Covers single-elem, multi-elem, a
    // non-escaping local Vec of a param, and a Vec[Vec] param.
    if let Some(out) = run_program(
        "fn dup(x: String) -> Vec[String] { [x] }\n\
             fn two(a: String, b: String) -> Vec[String] { [a, b] }\n\
             fn local_use(x: String) -> i64 { let v = [x]; v.len() }\n\
             fn dupv(x: Vec[i64]) -> Vec[Vec[i64]] { [x] }\n\
             fn main() {\n\
                 println(dup(\"z\".to_string())[0]);\n\
                 let t = two(\"p\".to_string(), \"q\".to_string());\n\
                 println(t[0]);\n\
                 println(t[1]);\n\
                 println(local_use(\"hi\".to_string()));\n\
                 let vv = dupv([1, 2, 3]);\n\
                 println(vv[0][2]);\n\
             }",
    ) {
        assert_eq!(out, "z\np\nq\n1\n3\n");
    }
}

#[test]
fn e2e_generic_struct_vec_field_move_out_no_double_free() {
    // B-2026-07-18-45: the whole-`Vec`-typed-T residual of B-2026-07-18-44 —
    // a generic struct whose type param is bound to a whole collection
    // (`get[T](b: Box[T])` at T=Vec[i64]) and whose field is returned
    // double-freed under AOT/JIT. `infer_type_args` can't recover the element
    // (Box[Vec[i64]] shares Box[String]'s erased LLVM shape), so the mono
    // subst dropped it and the entry-copy couldn't deep-copy the Vec field.
    // Fixed by unifying the declared `Box[T]` against the arg's recorded
    // concrete instantiation to bind T's element-aware TypeExpr. Covers
    // free-fn and method, T=Vec[i64] and T=Vec[String], a two-field struct,
    // and two distinct instantiations (Vec + String) in one program.
    if let Some(out) = run_program(
        "struct Box[T] { v: T }\n\
             struct Box2[T] { v: T, n: i64 }\n\
             fn get[T](b: Box[T]) -> T { b.v }\n\
             fn get2[T](b: Box2[T]) -> T { b.v }\n\
             impl[T] Box[T] { fn take(self) -> T { self.v } }\n\
             fn main() {\n\
                 let a = Box { v: [1, 2, 3] };\n\
                 println(get(a).len());\n\
                 let m = Box { v: [4, 5] };\n\
                 println(m.take().len());\n\
                 let s = Box { v: [\"x\".to_string(), \"y\".to_string()] };\n\
                 println(get(s).len());\n\
                 let b2 = Box2 { v: [9, 8], n: 1 };\n\
                 println(get2(b2).len());\n\
                 let str_box = Box { v: \"hi\".to_string() };\n\
                 println(get(str_box));\n\
             }",
    ) {
        assert_eq!(out, "3\n2\n2\n2\nhi\n");
    }
}

/// B-2026-09-05-1 — a `shared` FIELD read into an owning destination is a
/// COPY of the handle, not a move: the source struct stays alive and
/// readable, exactly as `let s = w.s` (which incs) and the interpreter say.
///
/// Before: the direct-shared arm of `zero_struct_field_move_cap_impl`
/// NULLED the source slot (B-2026-08-06-8, chosen for a returned handle,
/// whose source dies). Two destinations reach it with the source still
/// live — an owned or shared struct literal `H { s: w.s }` and
/// `v.push(w.s)` — and every later read of `w.s` dereferenced null:
/// SIGSEGV before the first print on all three compiled surfaces. The arm
/// now incs, null-guarded, and the source's own dec balances it. The
/// caller-retains param returns are the regression guard for the fix
/// itself: they already inc at the return, so the funnel must not
/// neutralize them a second time (measured 40 B lost at -O0 mid-fix).
#[test]
fn e2e_shared_field_into_literal_and_push_aliases_source() {
    let Some(out) = run_program(
            "shared struct S { id: i64 }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\"); } }\n\
             struct W { s: S }\n\
             struct H { s: S }\n\
             shared struct Sh { s: S }\n\
             struct G { w: W }\n\
             fn pick(w: W) -> S { return w.s }\n\
             fn pick2(w: W) -> S { w.s }\n\
             fn pickr(w: ref W) -> S { return w.s }\n\
             fn main() {\n\
             \x20   { let w: W = W { s: S { id: 1 } }; let h: H = H { s: w.s }; println(f\"v{h.s.id}{w.s.id}\"); println(\"one\"); }\n\
             \x20   { let w: W = W { s: S { id: 2 } }; let h: Sh = Sh { s: w.s }; println(f\"v{h.s.id}{w.s.id}\"); println(\"two\"); }\n\
             \x20   { let g: G = G { w: W { s: S { id: 3 } } }; let h: H = H { s: g.w.s }; println(f\"v{h.s.id}{g.w.s.id}\"); println(\"three\"); }\n\
             \x20   { let w: W = W { s: S { id: 4 } }; let mut v: Vec[S] = []; v.push(w.s); println(f\"v{v[0].id}{w.s.id}\"); println(\"four\"); }\n\
             \x20   { let w: W = W { s: S { id: 5 } }; let s: S = pick(w); println(f\"v{s.id}\"); println(\"five\"); }\n\
             \x20   { let w: W = W { s: S { id: 6 } }; let s: S = pick2(w); println(f\"v{s.id}\"); println(\"six\"); }\n\
             \x20   { let w: W = W { s: S { id: 7 } }; let s: S = pickr(w); println(f\"v{s.id}{w.s.id}\"); println(\"seven\"); }\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(
            out,
            "v11\none\ndS1\nv22\ntwo\ndS2\nv33\nthree\ndS3\nv44\nfour\ndS4\nv5\nfive\ndS5\nv6\nsix\ndS6\nv77\nseven\ndS7\nend\n"
        );
}

/// B-2026-08-06-4, codegen half — a `shared` struct's Vec field coerced to
/// `Slice[T]`.
///
/// `coerce_to_slice`'s place arm (B-2026-08-05-40) refused a shared
/// receiver for the right reason — its slot holds an RC HANDLE, not the
/// aggregate — but returning `None` for it is not fail-closed: the caller
/// reads `None` as "carry on", forwards the raw 3-word `{ptr,len,cap}`
/// against the 2-word `Slice[T]` formal, and LLVM module verification
/// hard-fails. EVERY program of this shape failed to build, on both the
/// mutable and the read-only spelling. Same structure as B-2026-08-05-41's
/// codegen half; the outcome here is loud rather than a silent miscompile,
/// which is the only reason it was not worse.
///
/// Arms (a)–(d) each reach the arm differently: a bare shared local, a
/// field at a NON-ZERO index behind both a scalar and another Vec (so an
/// offset error shows as a wrong value rather than a crash), `self` inside
/// a shared method, and a READ-ONLY `Slice[T]` on a NON-`mut` field —
/// which is legal precisely because it does not write, and which was
/// equally unbuildable before.
///
/// Arm (b) is the offset witness and the reason this test is not a
/// one-liner: the shared box lays fields out past a refcount header, so a
/// field resolved at the plain-struct offset would read the neighbouring
/// word. It writes `w` and prints `tag` and `v` alongside, so a
/// mis-resolved GEP corrupts something visible.
///
/// The mutability gate is this row's typechecker half and lives in
/// `tests/typechecker.rs`: every field WRITTEN here is declared `mut`,
/// which is what makes these programs legal. The two halves must ship
/// together — this one alone would turn a compile failure into a silently
/// accepted unsound write.
///
/// Seeded from `env.args().len()` so no arm folds away at `-O2`
/// (B-2026-08-04-17).
#[test]
fn e2e_shared_struct_vec_field_coerces_to_slice() {
    let Some(out) = run_program(
        "shared struct H { mut v: Vec[i64] }\n\
             shared struct M { tag: i64, mut v: Vec[i64], mut w: Vec[i64] }\n\
             shared struct R { v: Vec[i64] }\n\
             impl H {\n\
             \x20   fn go(ref self) { zap(mut self.v); }\n\
             }\n\
             fn zap(s: mut Slice[i64]) { s[0] = 99i64; }\n\
             fn total(s: Slice[i64]) -> i64 {\n\
             \x20   let mut t = 0i64;\n\
             \x20   let mut i = 0i64;\n\
             \x20   while i < s.len() { t = t + s[i]; i = i + 1i64; }\n\
             \x20   return t;\n\
             }\n\
             fn main() {\n\
             \x20   let n: i64 = env.args().len();\n\
             \x20   // (a) bare shared local receiver, written through a slice\n\
             \x20   let a = H { v: [n, n + 1i64, n + 2i64] };\n\
             \x20   zap(mut a.v);\n\
             \x20   println(f\"a:{a.v[0]}:{a.v[1]}\");\n\
             \x20   // (b) field at a NON-ZERO index, behind a scalar and a Vec\n\
             \x20   let b = M { tag: n + 6i64, v: [n, n], w: [n, n + 1i64, n + 2i64] };\n\
             \x20   zap(mut b.w);\n\
             \x20   println(f\"b:{b.tag}:{b.v[0]}:{b.w[0]}:{b.w[2]}\");\n\
             \x20   // (c) `self` inside a shared method\n\
             \x20   let c = H { v: [n, n + 1i64, n + 2i64] };\n\
             \x20   c.go();\n\
             \x20   println(f\"c:{c.v[0]}\");\n\
             \x20   // (d) READ-ONLY slice of a NON-`mut` field — legal, and\n\
             \x20   //     equally unbuildable before this fix\n\
             \x20   let d = R { v: [n, n + 1i64, n + 2i64] };\n\
             \x20   println(f\"d:{total(d.v)}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a:99:2\nb:7:1:99:3\nc:99\nd:6\n");
}

/// B-2026-07-30-11 (discarded-temp leg, insert-displacement shape) —
/// `let _ = m.insert(k, v2)` over an existing key returns `Some(old)`;
/// the discarded temp owns the displaced value, so its Drop body fires
/// at the `;` (drop 7), before the map's own value walk at `m`'s NLL
/// death (drop 8). Twin of `tests/interpreter.rs`'s
/// `test_wildcard_let_insert_displaced_payload_drop`.
#[test]
fn e2e_wildcard_let_insert_displaced_payload_drop() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let mut m: Map[i64, Res] = Map.new();\n\
             \x20   let _ = m.insert(1, Res { id: 7 });\n\
             \x20   println(\"first insert done\");\n\
             \x20   let _ = m.insert(1, Res { id: 8 });\n\
             \x20   println(\"displacing insert done\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "first insert done\ndrop 7\ndrop 8\ndisplacing insert done\nend\n"
    );
}

#[test]
fn e2e_declared_vec_enum_payload_runs_element_drop_bodies() {
    let Some(out) = run_program(
        r#"struct R2 { s: String, t: String, u: String }
    impl Drop for R2 { fn drop(mut ref self) { println(f"  d2:{self.s.len()}") } }
    fn mkr(i: i64) -> R2 { return R2 { s: f"aaaaaaaaa", t: f"b", u: f"c" } }
    struct S1 { v: i64 }
    impl Drop for S1 { fn drop(mut ref self) { println(f"  dS{self.v}") } }
    enum Mono { P(R2), Q }
    shared enum SMono { P(R2), Q }
    enum G[T] { X(T), Y }
    enum H3 { P(SMono), Q }
    enum H4 { P(Vec[Mono]), Q }
    enum H5 { P(Vec[S1]), Q }
    enum H6 { P(Vec[S1], i64), Q }
    enum H7 { P(Array[S1, 2]), Q }

    fn main() {
        println("vecenum"); { let mut w: Vec[Mono] = []; w.push(Mono.P(mkr(1))); let h = H4.P(w); println("  x") }
        println("vecstruct"); { let mut w: Vec[S1] = []; w.push(S1 { v: 7 }); w.push(S1 { v: 8 }); let h = H5.P(w); println("  x") }
        println("vecmixed"); { let mut w: Vec[S1] = []; w.push(S1 { v: 9 }); let h = H6.P(w, 3); println("  x") }
        println("vecempty"); { let w: Vec[S1] = []; let h = H5.P(w); println("  x") }
        println("unitvar"); { let h = H5.Q; println("  x") }
        println("array"); { let h = H7.P([S1 { v: 1 }, S1 { v: 2 }]); println("  x") }
        println("struct"); { let g = G.X(mkr(1)); println("  x") }
        println("sharedec"); { let h = H3.P(SMono.P(mkr(1))); println("  x") }
        println("genvec"); { let mut w: Vec[Mono] = []; w.push(Mono.P(mkr(1))); let g = G.X(w); println("  x") }
        println("gensh"); { let g = G.X(SMono.P(mkr(1))); println("  x") }
        println("end")
    }
"#,
    ) else {
        return;
    };
    // B-2026-09-17-19 — `sharedec` MOVED here, and its interpreter twin
    // stopped matching: the shared enum's payload bodies now run at the
    // refcount's 0-transition, so `H3.P(SMono.P(mkr(1)))` printed `d2:9`
    // at the holder's death on this side and nothing under `--interp`.
    // That was a one-backend fix to an AGREED gap, which B-2026-09-12-6
    // refuses as a rule, taken deliberately because the interpreter half
    // looked like a REPRESENTATION that does not exist rather than a walk
    // that is missing — a `shared enum` value is a plain
    // `Value::EnumVariant` with no `Arc`.
    //
    // B-2026-09-19-17 CLOSED that remainder, and it needed no refcount:
    // the interpreter's enum-payload walk simply stopped at an enum
    // payload, where its struct / tuple / `Vec` / `Option` siblings
    // already ran the body countless-ly and agreed with this side. So the
    // twin now prints `d2:9` too — BEFORE its `x`, where this side prints
    // it after, which is the placement gap the four siblings also have
    // (B-2026-09-19-18, with design.md `:866` putting the correct point at
    // the live-range end, i.e. the interpreter's).
    //
    // `gensh` is the cell that must NOT move with it, and the reason this
    // expectation is worth reading beside the twin's: `enum G[T] { X(T), Y }`
    // at `T = SMono` is silent here, so the twin withholds the
    // own-generic-param exception from an enum payload rather than firing
    // a body this side does not.
    assert_eq!(out, "vecenum\n  d2:9\n  x\nvecstruct\n  dS7\n  dS8\n  x\nvecmixed\n  dS9\n  x\nvecempty\n  x\nunitvar\n  x\narray\n  dS1\n  dS2\n  x\nstruct\n  d2:9\n  x\nsharedec\n  x\n  d2:9\ngenvec\n  d2:9\n  x\ngensh\n  x\nend\n");
}

/// B-2026-08-01-24 — a heap-owning `for`-loop STRUCT element pushed whole
/// into another container (`for h in headers { out.push(h) }`). The loop
/// binding is a shallow bit-copy of the source container's element slot;
/// pre-fix the push's move-suppression zeroed only the binding's ALLOCA
/// caps, so the source vec's per-element drain and the destination both
/// freed the same String buffers (`free(): double free detected in
/// tcache 2`, exit 134). The fix deep-copies the stored element's heap
/// fields in place (copy-depth == drop-depth, the push twin of the
/// `let x = a` whole-move arm). Twin of `tests/interpreter.rs`'s
/// `test_for_loop_struct_elem_push_move`.
#[test]
fn e2e_for_loop_struct_elem_push_move() {
    let Some(out) = run_program(
        "struct Header { name: String, value: String }\n\
             fn move_all(headers: Vec[Header]) -> Vec[Header] {\n\
             \x20   let mut out: Vec[Header] = Vec.new();\n\
             \x20   for h in headers {\n\
             \x20       out.push(h);\n\
             \x20   }\n\
             \x20   out\n\
             }\n\
             fn main() {\n\
             \x20   let stamp = \"20150830T123600Z\";\n\
             \x20   let mut hs: Vec[Header] = Vec.new();\n\
             \x20   hs.push(Header { name: \"X-Amz-Date\", value: stamp.clone() });\n\
             \x20   let moved = move_all(hs);\n\
             \x20   for h in moved { println(h.value); }\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "20150830T123600Z\n");
}

/// B-2026-08-01-24 (enum leg) — the value-ENUM sibling of the struct
/// element push above: `for it in src { out.push(it) }` over a
/// `Vec[Item]` with a String payload double-freed the live variant's
/// payload buffer pre-fix (source drain + destination element drop).
/// The fix routes the stored slot through
/// `deep_copy_enum_heap_payload_in_place` (the entry-copy twin of the
/// enum drop). Twin of `tests/interpreter.rs`'s
/// `test_for_loop_enum_elem_push_move`.
#[test]
fn e2e_for_loop_enum_elem_push_move() {
    let Some(out) = run_program(
        "enum Item {\n\
             \x20   Named(String),\n\
             \x20   Plain(i64),\n\
             }\n\
             fn main() {\n\
             \x20   let mut src: Vec[Item] = Vec.new();\n\
             \x20   src.push(Item.Named(f\"x{1}\"));\n\
             \x20   src.push(Item.Plain(7));\n\
             \x20   src.push(Item.Named(f\"y{2}\"));\n\
             \x20   let mut out: Vec[Item] = Vec.new();\n\
             \x20   for it in src {\n\
             \x20       out.push(it);\n\
             \x20   }\n\
             \x20   for it in out {\n\
             \x20       match it {\n\
             \x20           Item.Named(s) => println(s),\n\
             \x20           Item.Plain(n) => println(f\"{n}\"),\n\
             \x20       }\n\
             \x20   }\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "x1\n7\ny2\n");
}

/// B-2026-08-01-22 leg b — a struct-FIELD `Vec[DropT]`'s elements fire
/// their Drop bodies at the OWNER's death (pre-fix: silent on both
/// backends — the b55743b element-bodies leg covered direct Vec
/// bindings only, and `type_runs_user_drop` read the field head "Vec"
/// as no-drop so the parent never registered a bodies action). Twin of
/// `tests/interpreter.rs`'s `test_struct_field_vec_elem_bodies_at_owner_death`.
#[test]
fn e2e_struct_field_vec_elem_bodies_at_owner_death() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             struct Holder { xs: Vec[Res] }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let mut h = Holder { xs: Vec.new() };\n\
             \x20   h.xs.push(Res { id: 7, name: f\"q{7}\" });\n\
             \x20   h.xs.push(Res { id: 8, name: f\"r{8}\" });\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 7 q7\ndrop 8 r8\nend\n");
}

/// B-2026-09-01-6 — `ref v[i][j]` over a `Vec[Vec[T]]` lowers, at any
/// depth.
///
/// The interpreter always handled this: `v[i]` evaluates to the inner
/// `Value::Array`, which its existing arm matches. Codegen had no arm for
/// an INDEX base at all, so the whole chain hit the decline and
/// `karac build` failed on a program `--interp` ran correctly — a gap
/// rather than a divergence in either direction, which is why it was filed
/// separately from B-2026-08-31-37 (the message that decline lands on).
///
/// No new address arithmetic: the element pointer of `v[i]` IS the inner
/// `Vec` header, and `lower_indexed_elem_ptr_vec_at` already indexes a
/// header it is handed. So each hop reuses one of the two existing
/// lowerings and inherits its bounds check. `triple` and `quad` are here
/// because the first cut handled exactly one level — the recursion is what
/// makes the rule "any depth" rather than "two".
///
/// `alias` is the load-bearing line: a write through the container is
/// visible through the borrow, so this is a real pointer into the nested
/// buffer and not a copy of the element. `flat` is the control that must
/// keep working.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_ref_binding_over_a_nested_vec_chain`, pinned to the same string.
#[test]
fn e2e_ref_binding_over_a_nested_vec_chain() {
    let Some(out) = run_program(
        "fn main() {\n\
     let d: Vec[Vec[i64]] = [[1, 2], [3, 4]];\n\
     let g2 = ref d[0][1];\n\
     println(f\"double {g2}\");\n\
     let t: Vec[Vec[Vec[i64]]] = [[[5, 6]]];\n\
     let g3 = ref t[0][0][1];\n\
     println(f\"triple {g3}\");\n\
     let q: Vec[Vec[Vec[Vec[i64]]]] = [[[[7, 8]]]];\n\
     let g4 = ref q[0][0][0][1];\n\
     println(f\"quad {g4}\");\n\
     let s: Vec[Vec[String]] = [[\"a\", \"b\"]];\n\
     let gs = ref s[0][1];\n\
     println(f\"str {gs}\");\n\
     let mut m: Vec[Vec[i64]] = [[1, 2]];\n\
     let ga = ref m[0][1];\n\
     m[0][1] = 99;\n\
     println(f\"alias {ga}\");\n\
     let f: Vec[i64] = [10, 20];\n\
     let gf = ref f[1];\n\
     println(f\"flat {gf}\");\n\
     }",
    ) else {
        return;
    };
    assert_eq!(
        out, "double 2\ntriple 6\nquad 8\nstr b\nalias 99\nflat 20\n",
        "a `ref` binding over a nested Vec chain must read the element"
    );
}

#[test]
fn test_e2e_vec_swap_relocates_without_running_drop_bodies() {
    let Some(out) = run_program(
        r#"
struct Item { id: i64, tag: String }
impl Drop for Item { fn drop(mut ref self) { println(f"D{self.id}"); } }
struct Bag { xs: Vec[Item] }
fn main() {
    let mut b = Bag { xs: Vec.new() };
    b.xs.push(Item { id: 1, tag: "first_payload_long_enough_to_heap".to_string() });
    b.xs.push(Item { id: 2, tag: "second_payload_long_enough_to_heap".to_string() });
    println("built");
    b.xs.swap(0, 1);
    println(f"{b.xs[0].id}{b.xs[1].id}");
}
"#,
    ) else {
        return;
    };
    // `built` then the swapped ids, then exactly two bodies at scope exit.
    // A body printed BEFORE the id line would mean the swap destroyed a
    // live value; a third body would mean one was dropped twice.
    assert_eq!(out, "built\n21\nD2\nD1\n", "got: {out:?}");
}

/// B-2026-08-12-27 — the SILENT half, and the reason the fix had to be a
/// clone rather than a move. A heap field read out of a Vec element used
/// to cap-zero the SOURCE, i.e. treat `let w = ps[0].word` as a MOVE.
/// `karac check` accepts reading `ps[0].word` afterwards and the
/// interpreter still has the value, so the semantics is a COPY — and the
/// element was left pointing at a buffer it no longer owned.
///
/// Mutating the binding is what made it visible: the reassignment freed
/// the buffer the element still referenced, and the element's own field
/// then read back garbage. Measured pre-fix on this exact program —
/// interpreter `a1`, `karac build` `~`. No abort and no sanitizer trip in
/// a plain run: WRONG OUTPUT, which is why the memory fixture alone would
/// not have caught it.
///
/// The whole-element read was already a copy — `let b = ps[0]` then
/// `b.word = ..` leaves `ps[0].word` intact on both backends — so this
/// pins the field read to the rule its sibling already followed.
#[test]
fn e2e_vec_elem_field_read_is_a_copy() {
    let Some(out) = run_program(
        "struct Pair { word: String, n: i64 }\n\
             fn main() {\n\
             \x20   let k = 1;\n\
             \x20   let mut ps: Vec[Pair] = Vec.new();\n\
             \x20   ps.push(Pair { word: f\"a{k}\", n: 1 });\n\
             \x20   let mut w = ps[0].word;\n\
             \x20   w = w + \"X\";\n\
             \x20   println(w);\n\
             \x20   println(ps[0].word);\n\
             }\n",
    ) else {
        return;
    };
    // `karac run --interp` on the identical source.
    assert_eq!(out, "a1X\na1\n");
}

/// B-2026-09-05-14 — a discarded `Option`/`Result` temporary whose payload is
/// a TUPLE carrying a `Drop` type (`let _ = f();` where `f -> Option[(R, i64)]`)
/// runs the nested element's `Drop` body ONCE on the compiled backends.
///
/// Before this the compiled discard-drop walker declined a tuple payload
/// outright: `emit_optres_payload_user_drop_bodies_fn`'s target filter opened
/// with `let TypeKind::Path(pp) = &pte.kind else { return None }`, so a tuple
/// payload produced an empty `targets`, emitted no walker, and the discard ran
/// no body — while `--interp` ran it, a run-vs-build divergence. Valgrind was
/// clean for the INLINE payload here (the tuple fits `Option`'s 3-word area and
/// `Result`'s 5-word area, so no box is allocated), which is why only a
/// body-count pin catches it; the fix admits a tuple payload whose elements run
/// a user drop and drains it through the existing `emit_tuple_elem_user_drop_
/// bodies_fn`, body-only like the struct and enum arms beside it. The DIRECT
/// (`Option.Some(r)`) and struct-payload spellings already dropped correctly.
///
/// A BOXED tuple payload (a heap-carrying element widening the tuple past the
/// area) is a SEPARATE, pre-existing memory leak — `try_track_discarded_boxed_
/// option` frees a boxed struct payload but declines a boxed tuple one, so the
/// box is never freed regardless of this body fix — tracked on its own row.
/// This pin stays on the inline shape, which the fix makes correct on every
/// surface with no leak.
/// B-2026-09-13-29 — a discarded `Option`/`Result` whose payload is a `Vec`
/// of Drop-bearing elements runs those elements' bodies, once, at the
/// discard.
///
/// It ran them on the interpreter (B-2026-09-10-27's payload walk) and on
/// NEITHER compiled backend, a run-vs-build divergence on the DEFAULT
/// spelling: a bare `[..]` literal types as `Vec`, and the `Array[E, N]`
/// spelling — which has had its arm since B-2026-09-12-6 — was already
/// correct. `array_elem_and_len` needs a compile-time length, so the `Vec`
/// payload fell past the array arm to the struct arm, whose head lookup
/// answers `None` for `Vec`, and no walker was emitted at all.
///
/// THE ARM WAS SCOPED TO THE DISCARD POSITION when this landed, because a
/// bound `Option[Vec[D]]`, a consuming `match` arm and a plain move ran no
/// bodies on EITHER backend — an agreed gap — and turning the arm on for the
/// shared emitter converted all three into divergences. B-2026-09-14-2 wrote
/// the interpreter half those positions needed and lifted the scoping, so
/// every caller now takes the arm; the cells here are unchanged by that,
/// which is the point of keeping them. `e2e_optres_vec_payload_runs_element_
/// bodies_outside_the_discard` is the lift's own fixture.
#[test]
fn e2e_discarded_optres_vec_payload_runs_element_bodies() {
    const HDR: &str = "struct D { a: String, b: i64 }\n\
                           impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.b}\") } }\n\
                           fn pay() -> String { return \"heap\"; }\n\
                           fn mkd(n: i64) -> D { return D { a: pay(), b: n }; }\n\
                           fn mkopt() -> Option[Vec[D]] { return Option.Some([mkd(1), mkd(2)]); }\n";
    for (label, body, want) in [
        (
            "the row's repro: a BINDING discarded",
            "let o = Option.Some([mkd(1), mkd(2)]);\nlet _ = o;",
            "dD1\ndD2\nmid\n",
        ),
        (
            "the literal discarded directly",
            "let _ = Option.Some([mkd(1), mkd(2)]);",
            "dD1\ndD2\nmid\n",
        ),
        (
            "a call returning the envelope",
            "let _ = mkopt();",
            "dD1\ndD2\nmid\n",
        ),
        (
            "a discarded branch yielding the envelope",
            "let _ = if n == 0 { Option.Some([mkd(1), mkd(2)]) } else { Option.None };",
            "dD1\ndD2\nmid\n",
        ),
        (
            "the Result Err arm",
            "let o: Result[i64, Vec[D]] = Result.Err([mkd(1), mkd(2)]);\nlet _ = o;",
            "dD1\ndD2\nmid\n",
        ),
        (
            "a NESTED Vec payload",
            "let o: Option[Vec[Vec[D]]] = Option.Some([[mkd(1)], [mkd(2)]]);\nlet _ = o;",
            "dD1\ndD2\nmid\n",
        ),
        // Controls. The Array spelling was correct before this and must stay
        // so; `None` and a non-Drop element must stay silent.
        (
            "control: the Array payload spelling",
            "let o: Option[Array[D, 2]] = Option.Some([mkd(1), mkd(2)]);\nlet _ = o;",
            "dD1\ndD2\nmid\n",
        ),
        (
            "control: None runs nothing",
            "let o: Option[Vec[D]] = Option.None;\nlet _ = o;",
            "mid\n",
        ),
        (
            "control: a Vec of non-Drop elements",
            "let o: Option[Vec[i64]] = Option.Some([1, 2]);\nlet _ = o;",
            "mid\n",
        ),
    ] {
        let src = format!("{HDR}fn main() {{\nlet n = 0;\n{body}\nprintln(\"mid\");\n}}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "[{label}] interp errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
        if let Some(aot) = run_program(&src) {
            assert_eq!(aot, want, "[{label}] AOT");
        }
    }
}

/// B-2026-09-15-23 — a `Vec`/`VecDeque` STRUCT FIELD whose ELEMENT is
/// itself an aggregate runs the innermost value's `Drop` body.
///
/// Agreed-silent on all four surfaces before this: `Vec[Vec[D]]`,
/// `Vec[Array[D, 2]]`, `Vec[(D, i64)]`, `Vec[VecDeque[D]]` and
/// `Vec[Vec[Vec[D]]]` fields all printed nothing where the flat `Vec[D]`
/// field beside them printed correctly. A lost BODY with the memory
/// channel intact is invisible to ASAN, to valgrind and to both ratchet
/// legs (B-2026-08-28-57), and invisible to the kata A/B rule too because
/// all four surfaces agreed — they were simply all wrong together. An
/// output comparison is the only thing that sees it, which is what this
/// fixture is.
///
/// SIX SITES, because three gates stand in front of the walk on each side
/// and every one was keyed on a HEAD NAME that a container defeats:
///
///   * codegen `type_runs_user_drop` — the type-level classifier. Its own
///     comment records that this `Vec`-level recursion was DELIBERATELY
///     declined in B-2026-09-10-17, on the ground that repairing codegen
///     alone would leave the interpreter silent and manufacture
///     divergences. That reasoning was right; the missing piece was the
///     interpreter's matching recursion, which lands with it here.
///   * codegen `user_drop_field_indices_mono` — which fields enter the set.
///   * codegen `emit_user_drop_field_bodies_fn_skipping` — its hand-written
///     loop admits only a plain named struct/enum element, so an aggregate
///     fell through. The arm added is a call to
///     `emit_nested_vec_elem_bodies_fn`, the walker the `Vec` LOCAL
///     position already uses, whose parameter is a pointer to the vec
///     HEADER — which the field slot is.
///   * interpreter `field_te_runs_user_drop` — the classifier twin.
///   * interpreter `drop_user_drop_fields_of_value` — the walk's
///     per-element dispatch, where an `Array`/`Tuple` element fell to
///     `_ => {}`.
///   * the same dispatch again, for an `Option`/`Result` element. That one
///     is a defect this row's own fix introduced and then measured: the
///     first five sites made `Vec[Option[D]]` fire on the three compiled
///     surfaces while `--interp` stayed silent, because codegen's walker
///     has an Option arm the dispatch lacked. Agreed-silent before, agreed
///     -firing after; the intermediate state is why a per-element-shape
///     sweep is the check and symmetric gate-widening is not.
///
/// POSITION, not just presence: the bodies land BEFORE the holder's last
/// statement, at its live-range end (design.md line 866), matching the flat
/// control exactly. The route this row first tried — the `FieldDrop::
/// VecOrString` arm of `emit_struct_drop_synthesis_impl`, whose
/// `vec_element_drain_fn` returns a MEMORY drain for a container element —
/// fired at frame exit instead, AFTER that statement. The bodies channel is
/// this function, already called at the right point, so the position comes
/// out right without touching any ordering.
#[test]
fn e2e_nested_container_in_a_vec_field_runs_its_innermost_drop_bodies() {
    const HDR: &str = "struct D { id: i64, s: String }\n\
                           impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
                           fn mkd(n: i64) -> D { return D { id: n, s: f\"heap-{n}\" }; }\n\
                           struct N { v: i64 }\n";
    for (label, decls, body, want) in [
        (
            "the row's own cell — a Vec[Vec[D]] field",
            "struct H { xs: Vec[Vec[D]] }\n",
            "let h: H = H { xs: [[mkd(1)]] };",
            "dD1\nend\n",
        ),
        (
            "a Vec of fixed Arrays",
            "struct H { xs: Vec[Array[D, 2]] }\n",
            "let h: H = H { xs: [[mkd(1), mkd(2)]] };",
            "dD1\ndD2\nend\n",
        ),
        (
            "a Vec of tuples — no head name at all on the element",
            "struct H { xs: Vec[(D, i64)] }\n",
            "let h: H = H { xs: [(mkd(1), 5)] };",
            "dD1\nend\n",
        ),
        (
            "three container levels — the gate and the emitter recurse in step",
            "struct H { xs: Vec[Vec[Vec[D]]] }\n",
            "let h: H = H { xs: [[[mkd(1)]]] };",
            "dD1\nend\n",
        ),
        (
            "a VecDeque inner",
            "struct H { xs: Vec[VecDeque[D]] }\n",
            "let mut d: VecDeque[D] = VecDeque.new();\n\
                 d.push_back(mkd(1));\n\
                 let h: H = H { xs: [d] };",
            "dD1\nend\n",
        ),
        (
            // The divergence the first five sites created. See the note above.
            "an Option element — codegen's walker had the arm, the interpreter's dispatch did not",
            "struct H { xs: Vec[Option[D]] }\n",
            "let h: H = H { xs: [Some(mkd(1))] };",
            "dD1\nend\n",
        ),
        (
            // The row's own "single most decisive missing cell": it
            // separates the field's drop synthesis from the literal's
            // element ownership. Fires exactly once — the push disarms the
            // source local.
            "the field is built through push rather than a literal (the row's decisive cell)",
            "struct H { xs: Vec[Vec[D]] }\n",
            "let mut v: Vec[Vec[D]] = [];\n\
                 let e: Vec[D] = [mkd(1)];\n\
                 v.push(e);\n\
                 let h: H = H { xs: v };",
            "dD1\nend\n",
        ),
        (
            "a tuple element that itself holds a Vec",
            "struct H { xs: Vec[(Vec[D], i64)] }\n",
            "let h: H = H { xs: [([mkd(1)], 3)] };",
            "dD1\nend\n",
        ),
        (
            "the holder declares its own Drop — own body first, then the elements",
            "struct H { xs: Vec[Vec[D]] }\n\
                 impl Drop for H { fn drop(mut ref self) { println(\"dH\") } }\n",
            "let h: H = H { xs: [[mkd(1)]] };",
            "dH\ndD1\nend\n",
        ),
        (
            "one struct deeper",
            "struct H { xs: Vec[Vec[D]] }\nstruct G { h: H }\n",
            "let g: G = G { h: H { xs: [[mkd(1)]] } };",
            "dD1\nend\n",
        ),
        (
            "two inner elements, and a sibling scalar field",
            "struct H { xs: Vec[Vec[D]], n: i64 }\n",
            "let h: H = H { xs: [[mkd(1), mkd(2)]], n: 7 };\nprintln(f\"k:{h.n}\");",
            "k:7\ndD1\ndD2\nend\n",
        ),
        // Controls that must not move.
        (
            "control: the flat Vec[D] field, which always worked",
            "struct H { xs: Vec[D] }\n",
            "let h: H = H { xs: [mkd(1)] };",
            "dD1\nend\n",
        ),
        (
            "control: non-Drop inner elements run nothing",
            "struct H { xs: Vec[Vec[N]] }\n",
            "let h: H = H { xs: [[N { v: 1 }]] };",
            "end\n",
        ),
        (
            "control: an EMPTY outer vec fires no body",
            "struct H { xs: Vec[Vec[D]] }\n",
            "let h: H = H { xs: [] };\nprintln(f\"n:{h.xs.len()}\");",
            "n:0\nend\n",
        ),
        (
            // PINNED at the agreed silence. `type_runs_user_drop` returns
            // false for a `shared` type on its first line, and an RC
            // holder's hook fires from `rc == 0` rather than from scope
            // exit — a different channel, and the row's own open question
            // about it. Not this row's to move.
            "pinned: a shared holder stays an agreed silence",
            "shared struct H { xs: Vec[Vec[D]] }\n",
            "let h: H = H { xs: [[mkd(1)]] };",
            "end\n",
        ),
        (
            // PINNED: both gates now ADMIT this field (the element's
            // map-value head reaches `D` through
            // `elem_te_runs_user_drop`), but `emit_nested_vec_elem_bodies_fn`
            // has no `Map` arm and returns `None`, and the interpreter's
            // dispatch has no `Value::Map` arm — so it stays agreed-silent.
            // A gate admitting what its walker declines is a no-op rather
            // than a wrong answer, but it is the shape that produces silent
            // no-ops, so it is pinned deliberately.
            "pinned: a Map element stays an agreed silence — the walkers have no Map arm",
            "struct H { xs: Vec[Map[i64, D]] }\n",
            "let mut m: Map[i64, D] = Map.new();\n\
                 m.insert(1, mkd(1));\n\
                 let h: H = H { xs: [m] };",
            "end\n",
        ),
    ] {
        let src = format!("{HDR}{decls}fn main() {{\n{body}\nprintln(\"end\");\n}}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "[{label}] interp errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
        if let Some(aot) = run_program(&src) {
            assert_eq!(aot, want, "[{label}] AOT");
        }
    }
}

/// B-2026-09-14-2 (gate-lift half) — an `Option`/`Result` whose payload is a
/// `Vec` runs its elements' `Drop` bodies OUTSIDE the discard position too:
/// a bound envelope, a plain move, a consuming `match` / `if let` arm, and
/// `Result`'s `Ok` arm. Each cell is paired with its `Array[E, N]` twin,
/// which was already correct and defines the target.
///
/// These were an AGREED gap — both backends silent — until this landed, so
/// no A/B gate reported them, and B-2026-09-13-29 deliberately scoped its
/// `Vec` arm to the discard rather than trade three agreed gaps for three
/// run-vs-build divergences. The lift is the interpreter half catching up:
/// `array_payload_elem_te` resolves `Vec[E]` as well as `Array[E, N]`, and
/// the payload walk recurses into a nested container element.
///
/// TWO THINGS MOVED WITH THE LIFT, both measured and both pinned below. The
/// arm-binding registration had to DECLINE a `Vec` payload — the envelope
/// still holds the handle after the arm copies it, so both walks reach one
/// buffer and every element ran twice (`d1 d2 d1 d2`) — and the `let _ = o;`
/// hook B-2026-09-13-29 added for a BINDING had to go, for the same reason
/// one level up: its safety argument was that the binding's own walker
/// skipped every `Vec` arm, which the lift makes false.
///
/// A NESTED FIXED ARRAY (`Option[Array[Array[D, 1], 2]]`) was silent on
/// both backends when this landed, and its cell pinned that. It was
/// codegen's own one-level horizon, not this family's:
/// `emit_array_elem_user_drop_bodies_fn` admitted an element on
/// `elem_te_runs_user_drop`, which reads the head name `Array`, so no
/// walker existed at any nesting — a bare
/// `let v: Array[Array[D, 1], 2] = ..;` with no envelope in sight diverged
/// identically. B-2026-09-14-15 closed that horizon and the cell below now
/// pins the bodies RUNNING, with that row's own matrix in
/// `e2e_nested_fixed_array_runs_its_element_drop_bodies`.
/// `Array[Vec[D], N]` was never that shape and is correct here.
#[test]
fn e2e_optres_vec_payload_runs_element_bodies_outside_the_discard() {
    const HDR: &str = "struct D { a: String, b: i64 }\n\
                           impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.b}\") } }\n\
                           fn pay() -> String { return \"heap\"; }\n\
                           fn mkd(n: i64) -> D { return D { a: pay(), b: n }; }\n";
    for (label, body, want) in [
            (
                "bound Option[Vec[D]]",
                "let o: Option[Vec[D]] = Option.Some([mkd(1), mkd(2)]);",
                "dD1\ndD2\nmid\n",
            ),
            (
                "twin: bound Option[Array[D, 2]]",
                "let o: Option[Array[D, 2]] = Option.Some([mkd(1), mkd(2)]);",
                "dD1\ndD2\nmid\n",
            ),
            (
                "plain move of a Vec payload",
                "let o: Option[Vec[D]] = Option.Some([mkd(1)]);\nlet w = o;",
                "dD1\nmid\n",
            ),
            (
                "twin: plain move of an Array payload",
                "let o: Option[Array[D, 1]] = Option.Some([mkd(1)]);\nlet w = o;",
                "dD1\nmid\n",
            ),
            (
                "bound Result[Vec[D], i64] Ok",
                "let o: Result[Vec[D], i64] = Result.Ok([mkd(1), mkd(2)]);",
                "dD1\ndD2\nmid\n",
            ),
            (
                "consuming match arm, Vec payload",
                "let o: Option[Vec[D]] = Option.Some([mkd(1), mkd(2)]);\n                 match o { Option.Some(v) => { println(f\"n={v.len()}\"); } Option.None => { println(\"none\"); } }",
                "n=2\ndD1\ndD2\nmid\n",
            ),
            (
                "if let, Vec payload",
                "let o: Option[Vec[D]] = Option.Some([mkd(1), mkd(2)]);\n                 if let Option.Some(v) = o { println(f\"n={v.len()}\"); }",
                "n=2\ndD1\ndD2\nmid\n",
            ),
            (
                "nested Vec payload, bound",
                "let o: Option[Vec[Vec[D]]] = Option.Some([[mkd(1)], [mkd(2)]]);",
                "dD1\ndD2\nmid\n",
            ),
            (
                "Array of Vec payload, bound",
                "let o: Option[Array[Vec[D], 2]] = Option.Some([[mkd(1)], [mkd(2)]]);",
                "dD1\ndD2\nmid\n",
            ),
            // The two things that moved with the lift. A double here is the
            // regression each guards against.
            (
                "the discard keeps exactly one round",
                "let o: Option[Vec[D]] = Option.Some([mkd(1), mkd(2)]);\nlet _ = o;",
                "dD1\ndD2\nmid\n",
            ),
            (
                "a mixed Result[Vec[D], D] runs the live arm once",
                "let o: Result[Vec[D], D] = Result.Ok([mkd(1), mkd(2)]);",
                "dD1\ndD2\nmid\n",
            ),
            (
                "the mixed envelope's Err arm is unaffected",
                "let o: Result[Vec[D], D] = Result.Err(mkd(9));",
                "dD9\nmid\n",
            ),
            // Controls.
            (
                // B-2026-09-14-15 moved this cell: it pinned the agreed
                // SILENCE this row deliberately stopped short of, and that
                // row gave codegen's array element walker its nesting arm and
                // took the interpreter's matching stop off in one commit.
                "a nested FIXED array runs its innermost elements' bodies",
                "let o: Option[Array[Array[D, 1], 2]] = Option.Some([[mkd(1)], [mkd(2)]]);",
                "dD1\ndD2\nmid\n",
            ),
            (
                "control: a Vec of non-Drop elements",
                "let o: Option[Vec[i64]] = Option.Some([1, 2]);",
                "mid\n",
            ),
            (
                "control: None runs nothing",
                "let o: Option[Vec[D]] = Option.None;",
                "mid\n",
            ),
            (
                "control: a scalar payload is unchanged",
                "let o: Option[D] = Option.Some(mkd(1));",
                "dD1\nmid\n",
            ),
        ] {
            let src = format!("{HDR}fn main() {{\n{body}\nprintln(\"mid\");\n}}\n");
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
            if let Some(aot) = run_program(&src) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

/// B-2026-09-07-30 — a projection off an RC-FALLBACK-PROMOTED local whose
/// destination is a `Vec.push` ARGUMENT or an EXISTING BINDING.
///
/// B-2026-09-07-23 gave the `let` binding and the struct-literal field an
/// independent buffer; these two destinations kept taking the box's alias,
/// because every disarm reaches the source field by GEP-ing the binding's
/// slot and a promoted slot holds a `{i64 rc, T}` box HANDLE.
///
/// THE STDOUT ASSERTION IS THE ONE THAT FAILS ON THE PARENT for the `push`
/// cells: one buffer acquires four owners, so the program aborts with
/// `free(): double free detected in tcache 2` BEFORE printing and stdout is
/// empty. The `assign` cells are the opposite — under the default auto-par
/// build they print correctly and merely strand the box, so only the
/// `memory_sanitizer` twin (which builds with fan-out OFF) catches those.
/// Both directions are needed; neither suite covers this alone.
///
/// The VALUES rule out the fix that looks equivalent. Zeroing the box's own
/// field would neutralize the source, but the box is the surviving owner
/// exactly because the binding is read again — cell 5 reads `t.a` on every
/// trip and would print 3 rather than 117 under that fix, and cell 6 reads
/// two elements back and would see one shared buffer.
#[test]
fn e2e_rc_boxed_projection_push_and_assign_destinations_copy() {
    // The payload must be HEAP-allocated: a string LITERAL does not
    // allocate, so a literal-payload fixture gives the box no buffer for a
    // second owner to free and pins nothing.
    const PRE: &str = r#"struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
"#;
    // (source tail, expected stdout, cell name)
    let cells: [(&str, &str, &str); 9] = [
        // 1 — the row's `push` cell, three trips.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut v: Vec[String] = Vec.new(); let mut i = 0i64;
  while i < 3i64 { v.push(t.a); i = i + 1; }
  return v.len(); }
fn main() { println(go()); }
"#,
            "3",
            "push_three_trips",
        ),
        // 2 — ONE trip, the smallest dirty push cell.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut v: Vec[String] = Vec.new(); let mut i = 0i64;
  while i < 1i64 { v.push(t.a); i = i + 1; }
  return v.len(); }
fn main() { println(go()); }
"#,
            "1",
            "push_one_trip",
        ),
        // 3 — FIVE trips: six owners on the parent.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut v: Vec[String] = Vec.new(); let mut i = 0i64;
  while i < 5i64 { v.push(t.a); i = i + 1; }
  return v.len(); }
fn main() { println(go()); }
"#,
            "5",
            "push_five_trips",
        ),
        // 4 — the `assign` destination, which also frees its own displaced
        // value each trip.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut s = payload(); let mut i = 0i64;
  while i < 3i64 { s = t.a; i = i + 1; }
  return s.len(); }
fn main() { println(go()); }
"#,
            "38",
            "assign_three_trips",
        ),
        // 5 — the LIVE-VALUE oracle: `t.a` must still read 38 on every
        // trip. 3 x 38 + 3 = 117.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut v: Vec[String] = Vec.new(); let mut i = 0i64; let mut n = 0i64;
  while i < 3i64 { v.push(t.a); n = n + t.a.len(); i = i + 1; }
  return n + v.len(); }
fn main() { println(go()); }
"#,
            "117",
            "push_source_stays_live",
        ),
        // 6 — the elements must be independent buffers, not three aliases.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut v: Vec[String] = Vec.new(); let mut i = 0i64;
  while i < 3i64 { v.push(t.a); i = i + 1; }
  return v[0].len() + v[2].len(); }
fn main() { println(go()); }
"#,
            "76",
            "push_elements_independent",
        ),
        // 7 — the assign destination read back on every trip.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut s = payload(); let mut i = 0i64; let mut n = 0i64;
  while i < 3i64 { s = t.a; n = n + s.len(); i = i + 1; }
  return n; }
fn main() { println(go()); }
"#,
            "114",
            "assign_destination_read_each_trip",
        ),
        // 8 — CONTROL: the `let` destination B-2026-09-07-23 fixed. It must
        // keep taking exactly ONE copy — a second one leaks 38 B per trip,
        // which is what routing this fix through the shared argument disarm
        // measured.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut i = 0i64; let mut n = 0i64;
  while i < 3i64 { let s = t.a; n = n + s.len(); i = i + 1; }
  return n; }
fn main() { println(go()); }
"#,
            "114",
            "control_let_destination_unchanged",
        ),
        // 9 — CONTROL: no loop, so no promotion; the destination
        // legitimately takes the buffer.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut v: Vec[String] = Vec.new();
  v.push(t.a);
  return v.len(); }
fn main() { println(go()); }
"#,
            "1",
            "control_no_promotion",
        ),
    ];
    for (tail, want, name) in cells {
        let Some(cap) = run_program_capturing(&format!("{PRE}{tail}")) else {
            return;
        };
        assert_eq!(cap.stdout.trim(), want, "cell {name}: stdout");
        assert!(
            cap.status.success(),
            "cell {name}: exited {:?}; stderr={:?}",
            cap.status,
            cap.stderr
        );
    }
}

/// B-2026-09-20-63 — a constructor used DIRECTLY as a `match` scrutinee is
/// a temporary bound under no name, and the two registrations that would
/// give its instantiated payload an owner were both read off the
/// DECLARATION: `field_drop_kinds`, built by classifying each variant's
/// declared payload `TypeExpr`, is all-`None` for a payload spelled `T`,
/// and `drop_method_keys` is keyed by enum name and never looks at the
/// payload at all. So `materialize_freshtemp_enum_scrutinee` declined
/// outright: no memory owner for the heap-boxed payload and no bodies
/// walker for its elements. The instantiation was available the whole time
/// — `enum_inst_type_exprs` is keyed on the constructor's own SPAN, not on
/// a binding name, which is the table the `let` site already reads.
///
/// THE LEAK HALF CLOSES FOR BOTH CONTAINERS; the BODIES half closes for
/// `Array` only, and the split is not arbitrary. A `Vec` payload handed to
/// an arm's binding becomes that binding's value (it is what
/// `boxed_payload_interior_taken_by_arm` names), so the husk must neither
/// free its buffer — doing so is a double free, measured — nor walk it:
/// walking a moved-out payload printed two garbage ids with an invalid read
/// under valgrind. The bodies such a binding owes are its own to run, and an
/// erased payload does not run them today. An `Array` payload has no such
/// second owner, so the husk is the sole channel and arming it is correct.
///
/// AN ARM THAT DISCARDS ITS PAYLOAD IS LEFT EXACTLY AS IT WAS, deliberately.
/// `match Slot.S(a) { Slot.S(_) => .. }` runs no element body on ANY surface
/// — `--interp` included — and the DECLARED spelling `enum Ev { V(Vec[R]), N }`
/// behaves the same way, so it is an agreed gap across four surfaces and two
/// spellings rather than a divergence. Arming the walker there would make
/// this one backend right and leave the other three wrong, which is strictly
/// worse than the agreed answer. The memory half still runs for such an arm,
/// so it stops leaking without changing what it prints.
/// B-2026-09-21-11 — the half B-2026-09-20-63 left open, and it closes at a
/// different site than that one.
///
/// When a generic enum's payload instantiates to a `Vec`, an arm's binding
/// takes the buffer over — that is exactly the population
/// `boxed_payload_interior_taken_by_arm` names — so the scrutinee husk
/// cannot be the channel for the elements' `Drop` bodies: it fires at the
/// merge block, after that binding has already freed the buffer, and
/// walking it there read freed memory. B-2026-09-20-63 therefore stood the
/// husk down and left the bodies owned by nobody.
///
/// They belong to the BINDING, where they fire at the arm's end, ahead of
/// its own buffer free. `register_arm_container_payload_elem_bodies` is the
/// registrar for exactly that, and its `Option`/`Result`-only gate exists to
/// stop a SECOND walker reaching one buffer. In this position there is no
/// first one, which is what the husk recorded when it declined.
///
/// THE REGISTRATION HAD TO MOVE, not just widen. The cleanup frame drains
/// LIFO and the buffer free is queued after the bind, so a bodies walk
/// registered where it is decided fires LAST and reads the released buffer
/// — measured as two garbage ids and an invalid read of size 8. Deferring
/// it past that queueing is what puts the two in the order both other
/// backends already use. The call could only ever return early before this
/// row, so moving it changes nothing that was reaching it.
#[test]
fn e2e_arm_binding_owns_its_instantiated_vec_payload_elem_bodies() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   fn mkr(i: i64) -> R { return R { id: i }; }\n\
                   struct W { v: R }\n\
                   impl Drop for W { fn drop(mut ref self) { println(\"dW\") } }\n\
                   enum Slot[T] { S(T), N }\n";
    for (label, stmts, want) in [
            (
                "THE FIX: Vec payload, FRESH CTOR TEMP scrutinee, read-only arm",
                "let a: Vec[R] = [mkr(1), mkr(2)];\n\
                 match Slot.S(a) { Slot.S(v) => { println(f\"x{v[0].id}\") } Slot.N => { println(\"no\") } }",
                "x1\ndR1\ndR2\nend\n",
            ),
            (
                "THE FIX with a GUARD and two binding arms — the husk fires once at \
                 the merge block, so an arm-sensitivity fault is invisible to a \
                 single-arm grid",
                "let a: Vec[R] = [mkr(1), mkr(2)];\n\
                 match Slot.S(a) {\n\
                   Slot.S(v) if v[0].id > 99 => { println(f\"big{v[0].id}\") }\n\
                   Slot.S(w) => { println(f\"x{w[1].id}\") }\n\
                   Slot.N => { println(\"no\") }\n\
                 }",
                "x2\ndR1\ndR2\nend\n",
            ),
            (
                "THE FIX with an element carrying its OWN body over a Drop-bearing \
                 field — both run, in the order the element walk uses",
                "let a: Vec[W] = [W { v: mkr(1) }, W { v: mkr(2) }];\n\
                 match Slot.S(a) { Slot.S(v) => { println(\"x\") } Slot.N => { println(\"no\") } }",
                "x\ndW\ndR1\ndW\ndR2\nend\n",
            ),
            (
                "control: a SCALAR element owes no body at all, and the registrar \
                 must still leave the buffer free alone",
                "let a: Vec[i64] = [1, 2];\n\
                 match Slot.S(a) { Slot.S(v) => { println(f\"x{v[0]}\") } Slot.N => { println(\"no\") } }",
                "x1\nend\n",
            ),
            (
                "control: a CONSUMING arm was already correct — the binding's new \
                 home owns the elements and this must not add a second walker",
                "let a: Vec[R] = [mkr(1), mkr(2)];\n\
                 match Slot.S(a) { Slot.S(v) => { let z = v; println(f\"x{z[0].id}\") } Slot.N => { println(\"no\") } }",
                "x1\ndR1\ndR2\nend\n",
            ),
            (
                "control: the ctor BOUND FIRST — B-2026-09-20-62's cell, where the \
                 husk never stands down and this registrar must stay asleep",
                "let a: Vec[R] = [mkr(1), mkr(2)];\n\
                 let s = Slot.S(a);\n\
                 match s { Slot.S(v) => { println(f\"x{v[0].id}\") } Slot.N => { println(\"no\") } }",
                "x1\ndR1\ndR2\nend\n",
            ),
            (
                "AGREED GAP, PINNED (B-2026-09-21-12): one binding arm beside one \
                 that DISCARDS. The husk's gate is a union over every arm, so it \
                 stands down for the whole construct and nothing here re-arms it",
                "let a: Vec[R] = [mkr(1), mkr(2)];\n\
                 match Slot.S(a) {\n\
                   Slot.S(v) if v[0].id > 99 => { println(\"big\") }\n\
                   Slot.S(_) => { println(\"x\") }\n\
                   Slot.N => { println(\"no\") }\n\
                 }",
                "x\nend\n",
            ),
            (
                "AGREED GAP, PINNED: an `Option[R]` payload is in the same \
                 arm-owned population and silent on all four surfaces, so it is an \
                 agreement rather than a divergence and stays as it is",
                "let a: Option[R] = Option.Some(mkr(3));\n\
                 match Slot.S(a) { Slot.S(v) => { println(\"x\") } Slot.N => { println(\"no\") } }",
                "x\nend\n",
            ),
        ] {
            let src = format!("{hdr}fn main() {{\n{stmts}\nprintln(\"end\");\n}}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
        }
}

/// B-2026-07-30-11 (Vec leg) — a `Vec[T]`'s ELEMENTS run their user
/// `impl Drop` bodies when the Vec dies.
///
/// A user body used to run only for a value reachable from a direct binding
/// through STRUCT FIELDS. `Vec[Res]` freed each element's buffers via the
/// scope-exit drain and ran no body, so a resource held in an element was
/// held for the program's lifetime — silently, and on both backends.
///
/// Three properties, and the second is the one the first attempt got wrong:
///  * elements fire in FORWARD order (`dA1` then `dA2`), matching the
///    memory drain and the interpreter — struct fields go in reverse, and
///    container elements deliberately do not;
///  * they fire at the binding's NLL live-range end, not scope exit. The
///    bodies ride the `UserDrop` channel for exactly this reason; folded
///    into the scope-exit drain instead they printed `100 1 2` against the
///    interpreter's `1 2 100`;
///  * `Vec[W]` where only `W`'s FIELD is Drop composes — B-2026-07-29-39's
///    field walk did not reach through a container, so this went silent one
///    Vec deep.
///
/// The `pop()` case is the disarm check: the element leaves through the
/// returned `Option`, and the Vec must not also drop it.
///
/// Twinned with `tests/interpreter.rs`'s
/// `test_vec_elements_run_user_drop_bodies` on identical source and
/// expected output — that pair IS the parity contract.
#[test]
fn e2e_vec_elements_run_user_drop_bodies() {
    let Some(out) = run_program(
            "struct A { t: i64 }\n\
             impl Drop for A { fn drop(mut ref self) { println(f\"dA{self.t}\"); } }\n\
             struct W { a: A }\n\
             fn main() {\n\
             \x20   { let v: Vec[A] = [A { t: 1 }, A { t: 2 }]; println(f\"{v.len()}\"); }\n\
             \x20   { let w: Vec[W] = [W { a: A { t: 3 } }]; println(f\"{w.len()}\"); }\n\
             \x20   { let mut p: Vec[A] = [A { t: 4 }]; let g = p.pop(); println(f\"{p.len()}\"); }\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    // `dA4` fires exactly ONCE, via the popped Option binding's payload
    // walk (the Option/Result leg of this entry, landed) — with the LIVE
    // element data, not zeroes. What the pop case pins is that the Vec
    // does not drop it a second time.
    assert_eq!(out, "2\ndA1\ndA2\n1\ndA3\ndA4\n0\nend\n");
}

#[test]
fn test_e2e_rebound_vec_shared_param_keeps_every_element_alive() {
    // B-2026-09-07-56 — value half. The copy made for `let mut work = xs`
    // must co-own the elements, so reading them back through EITHER
    // container gives the same answer on every pass.
    let src = r#"
shared struct Node { val: i64 }

fn probe(xs: Vec[Node]) -> i64 {
    let mut work = xs;
    work[0].val + work[1].val
}

fn main() {
    let a = Node { val: 7 };
    let b = Node { val: 9 };
    let mut v: Vec[Node] = Vec.new();
    v.push(a);
    v.push(b);
    println(probe(v));
    println(probe(v));
    println(v[0].val);
}
"#;
    assert_eq!(run_program(src), Some("16\n16\n7\n".to_string()));
}

#[test]
fn test_e2e_owned_struct_param_vec_shared_field_move_reads_back() {
    // CONTROL — passes with and without the fix. A `Vec[shared T]` field
    // moved out of a by-value struct param is balanced by the entry-copy
    // machinery and never reaches the copy helper's element chain; this
    // pins that the new retain arm leaves it alone.
    let src = r#"
shared struct Node { val: i64 }
struct Bag { kids: Vec[Node] }

fn probe(p: Bag) -> i64 {
    let ks = p.kids;
    ks[0].val + ks[1].val
}

fn main() {
    let a = Node { val: 7 };
    let b = Node { val: 9 };
    let mut v: Vec[Node] = Vec.new();
    v.push(a);
    v.push(b);
    let bag = Bag { kids: v };
    println(probe(bag));
}
"#;
    assert_eq!(run_program(src), Some("16\n".to_string()));
}

#[test]
fn test_e2e_rebound_vec_weak_param_reads_back_unchanged() {
    // B-2026-09-08-1 — the SEMANTICS half, and worth being explicit about
    // what it does and does not guard. Unlike B-2026-09-07-56 one tier up,
    // this defect is NOT a miscompile: measured pre-fix, this program
    // prints exactly the same four lines while leaking 24 bytes. So this
    // test is not the regression guard — `tests/memory_sanitizer.rs` is.
    //
    // What it pins is the direction the FIX could go wrong. The correction
    // adds a weak-count retain per element, and a weak retain must not
    // change what the slot observes: the referent is alive throughout, so
    // every read still upgrades, and repeated passes must not drift.
    let src = r#"
shared struct N { v: i64 }

fn probe(xs: Vec[weak N]) -> i64 {
    let work = xs;
    match work[0] { Some(x) => { x.v } None => { 0 - 1 } }
}

fn main() {
    let a: N = N { v: 7 };
    let mut w: Vec[weak N] = Vec.new();
    w.push(a);
    println(probe(w));
    println(probe(w));
    match w[0] { Some(x) => { println(x.v) } None => { println(0 - 1) } }
    println(a.v);
}
"#;
    assert_eq!(run_program(src), Some("7\n7\n7\n7\n".to_string()));
}

#[test]
fn test_e2e_rebound_vec_weak_param_dead_referent_still_reads_none() {
    // The other direction, and the one a retain-based fix is most likely to
    // break: a weak handle must NOT keep its referent alive. `build` drops
    // the only strong binding, so the upgrade must fail even though the
    // copy now holds an extra weak count — a weak count keeps the CONTROL
    // BLOCK addressable, never the payload.
    //
    // This shape is also where the defect stops being a mere leak: pre-fix
    // it reported `Invalid read of size 8` under valgrind, because the
    // first of the two drops released the block while the second still
    // held a handle to it. The ASAN sibling pins that; this pins that the
    // value stayed `None` through the correction.
    let src = r#"
shared struct N { v: i64 }

fn probe(xs: Vec[weak N]) -> i64 {
    let work = xs;
    match work[0] { Some(x) => { x.v } None => { 0 - 1 } }
}

fn build() -> Vec[weak N] {
    let a: N = N { v: 7 };
    let mut w: Vec[weak N] = Vec.new();
    w.push(a);
    w
}

fn main() {
    let dead = build();
    println(probe(dead));
}
"#;
    assert_eq!(run_program(src), Some("-1\n".to_string()));
}

#[test]
fn test_e2e_nested_vec_weak_read_is_stable_and_still_expires() {
    // B-2026-09-08-7 — the SEMANTICS half, and as with B-2026-09-08-1 it is
    // NOT the regression guard: every fixture in that row printed the right
    // values before and after the fix, which is exactly why the family
    // survived so long. `tests/memory_sanitizer.rs` is the guard.
    //
    // What this pins is the direction the fix could go wrong. Two of the
    // three gaps added COUNTS -- a per-slot weak drain and a balancing
    // acquire on a nested-index read -- and a weak count must not change
    // what a slot observes. So: reading the same nested slot twice must
    // give the same answer (the acquire is balanced, not accumulating), and
    // a container whose strong owner died with the frame that built it must
    // still read `None` (a weak count keeps the CONTROL BLOCK addressable,
    // never the payload).
    let src = r#"
shared struct N { v: i64 }

fn build() -> Vec[Vec[weak N]] {
    let a: N = N { v: 7 };
    let mut inner: Vec[weak N] = Vec.new();
    inner.push(a);
    let mut outer: Vec[Vec[weak N]] = Vec.new();
    outer.push(inner);
    outer
}

fn main() {
    let a: N = N { v: 7 };
    let mut inner: Vec[weak N] = Vec.new();
    inner.push(a);
    let mut outer: Vec[Vec[weak N]] = Vec.new();
    outer.push(inner);
    match outer[0][0] { Some(x) => { println(x.v) } None => { println(0 - 1) } }
    match outer[0][0] { Some(y) => { println(y.v) } None => { println(0 - 1) } }
    let dead = build();
    match dead[0][0] { Some(z) => { println(z.v) } None => { println(0 - 1) } }
    println(a.v);
}
"#;
    assert_eq!(run_program(src), Some("7\n7\n-1\n7\n".to_string()));
}

/// B-2026-08-31-24 — a `Vec` whose element type is OVER-ALIGNED (a
/// `Vector[T, N]` wider than 16 bytes) put its element buffer on plain
/// malloc memory, which guarantees only 16-byte alignment, and then read
/// and wrote the elements at the vector's NATURAL alignment. On x86-64
/// LLVM selects `vmovaps` for a 32-byte-aligned `<4 x i64>` access, and
/// `vmovaps` faults on a 16-aligned address — so the program SIGSEGV'd.
///
/// The IR is the assertion because it is the mechanism, and because the
/// crash itself is alignment-of-the-day: whether a given malloc result
/// happens to land on 32 is deterministic per binary but arbitrary across
/// shapes, which is why two of the twelve program shapes in the E2E twin
/// below ran clean even before the fix. The symbol either appears or it
/// does not.
///
/// Fixing this at the ~10 ALLOCATION sites rather than at the 229 element
/// ACCESS sites is what keeps it one change: de-tuning every access to
/// `align 1` would also give up the aligned-move selection that is the
/// entire point of a SIMD element type.
#[test]
fn test_ir_over_aligned_vec_element_buffer_uses_the_aligned_allocator() {
    // Every Vec buffer-producing shape the sweep found: the literal
    // (malloc), the push grow, `reserve`, `insert`, and the STABILIZED
    // push loop in `reduce.rs`, whose bulk reserve is a separate emission
    // path from the ordinary push grow and was the last one found.
    for (label, src) in [
        (
            "literal",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let v: Vec[Vector[i64, 4]] = [a, a];\n\
                     let g = ref v[1];\n\
                     println(g.reduce_sum());\n\
                 }",
        ),
        (
            "push-grow",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut v: Vec[Vector[i64, 4]] = [a];\n\
                     v.push(a);\n\
                     v.push(a);\n\
                     println(v.len());\n\
                 }",
        ),
        (
            "stabilized-push-loop",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut v: Vec[Vector[i64, 4]] = [];\n\
                     for i in 0..9 { v.push(a); }\n\
                     println(v.len());\n\
                 }",
        ),
        (
            "reserve",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut v: Vec[Vector[i64, 4]] = [];\n\
                     v.reserve(16);\n\
                     v.push(a);\n\
                     println(v.len());\n\
                 }",
        ),
        (
            "insert",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut v: Vec[Vector[i64, 4]] = [a];\n\
                     v.insert(0, a);\n\
                     println(v.len());\n\
                 }",
        ),
        (
            "struct-with-vector-field",
            "struct H { v: Vector[i64, 4] }\n\
                 fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut hs: Vec[H] = [];\n\
                     for i in 0..9 { hs.push(H { v: a }); }\n\
                     println(hs.len());\n\
                 }",
        ),
    ] {
        let ir = ir_for(src);
        assert!(
            ir.contains("karac_alloc_aligned_or_panic")
                || ir.contains("karac_realloc_aligned_or_panic"),
            "case {label}: the element buffer for an over-aligned element \
                 type is still allocated through the plain entry point, so it \
                 carries only malloc's 16-byte guarantee while the element \
                 accesses are emitted at the natural alignment:\n{ir}"
        );
    }

    // B-2026-09-01-46 — the CONSTRUCTOR shapes the original sweep missed,
    // pinned by the NAME of the allocation rather than by the mere presence
    // of the symbol.
    //
    // The weaker assertion above cannot see these. Every case up there
    // builds its Vec as a LITERAL (`[a]` / `[]`) or calls `reserve`, and a
    // `push` on any of them emits a conditional GROW path that calls
    // `karac_realloc_aligned_or_panic` — so "the IR mentions an aligned
    // entry point somewhere" is satisfied by the grow path even when the
    // CONSTRUCTOR's own buffer is still a plain `malloc`. That is exactly
    // the state `Vec.new()` was in when it segfaulted on x86-64 CI while
    // the literal spelling of the same program ran clean.
    //
    // `Vec.new()` appears twice, with two loop forms, because it allocates
    // nothing by itself: the presize pass rewrites `Vec.new()` + a
    // stabilized push loop into `Vec.with_capacity(n)`, so `%with_cap.buf`
    // is the buffer the program actually runs on.
    for (label, value_name, src) in [
        (
            "vec-new-while-push",
            "with_cap.buf",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut v: Vec[Vector[i64, 4]] = Vec.new();\n\
                     let mut i = 0;\n\
                     while i < 40 { v.push(a); i = i + 1; }\n\
                     println(v.len());\n\
                 }",
        ),
        (
            "vec-new-for-push",
            "with_cap.buf",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut v: Vec[Vector[i64, 4]] = Vec.new();\n\
                     for i in 0..40 { v.push(a); }\n\
                     println(v.len());\n\
                 }",
        ),
        (
            "with-capacity",
            "with_cap.buf",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut v: Vec[Vector[i64, 4]] = Vec.with_capacity(40);\n\
                     v.push(a);\n\
                     println(v.len());\n\
                 }",
        ),
        (
            "filled",
            "filled.buf",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let v: Vec[Vector[i64, 4]] = Vec.filled(40, a);\n\
                     println(v.len());\n\
                 }",
        ),
    ] {
        let ir = ir_for(src);
        let want = format!("%{value_name} = call ptr @karac_alloc_aligned_or_panic");
        let plain = format!("%{value_name} = call ptr @karac_alloc_or_panic");
        assert!(
            ir.contains(&want),
            "case {label}: the Vec's own element buffer `%{value_name}` must \
                 come from the aligned allocator; it is {}.\n{ir}",
            if ir.contains(&plain) {
                "still the plain one, so it carries only malloc's 16-byte \
                     guarantee while every element access is emitted at 32"
            } else {
                "not allocated under that name at all — if the emission site \
                     was renamed, update `value_name` rather than deleting the case"
            }
        );
    }

    // Controls: an element type at or below malloc's 16-byte guarantee
    // must NOT pay for the aligned path. `Vector[i64, 2]` is exactly 16 —
    // the boundary, and the case that proves the predicate is `> 16` and
    // not `>= 16`.
    for (label, src) in [
        (
            "control-i64",
            "fn main() {\n\
                     let mut v: Vec[i64] = [];\n\
                     for i in 0..9 { v.push(i); }\n\
                     println(v.len());\n\
                 }",
        ),
        (
            "control-v128-boundary",
            "fn main() {\n\
                     let a: Vector[i64, 2] = Vector[i64, 2](3, 4);\n\
                     let mut v: Vec[Vector[i64, 2]] = [];\n\
                     for i in 0..9 { v.push(a); }\n\
                     println(v.len());\n\
                 }",
        ),
    ] {
        let ir = ir_for(src);
        assert!(
            !ir.contains("_aligned_or_panic"),
            "case {label}: an element type malloc already satisfies was \
                 routed to the aligned allocator, which costs a `posix_memalign` \
                 for nothing:\n{ir}"
        );
    }
}

/// B-2026-08-31-24's E2E twin — the interpreter is the oracle, because it
/// stores these elements in boxed `Value`s and never had the problem.
///
/// **This test is a correctness oracle, not the crash guard — the IR test
/// above is the guard.** Whether a given buffer actually faults is
/// alignment-of-the-day: an under-aligned buffer only crashes when the
/// malloc result it happens to get is 16-but-not-32. Under `karac build`,
/// eight of these shapes SIGSEGV'd pre-fix (40/40 runs each) and two —
/// `literal-read`, `push-onto-literal` — ran clean (also 40/40), their
/// buffers having landed on 32 by luck. Inside THIS harness, which
/// compiles the same source without the concurrency analysis and so emits
/// a slightly different allocation sequence, ALL of them ran clean pre-fix,
/// `heap-offset-walk` included — and that one crashes 100% of the time
/// under `karac build`, which is why it is here even though this harness
/// cannot make it fire. Do not read a green run of this test as evidence
/// that the buffers are aligned; read the IR test for that.
#[test]
fn e2e_over_aligned_vec_elements_are_the_interpreter_oracle() {
    for (label, src, want) in [
        (
            "literal-read",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let b: Vector[i64, 4] = Vector[i64, 4](5, 6, 7, 8);\n\
                     let v: Vec[Vector[i64, 4]] = [a, b];\n\
                     let g = ref v[0];\n\
                     println(g.reduce_sum());\n\
                     let h = ref v[1];\n\
                     println(h.reduce_sum());\n\
                 }",
            "10\n26\n",
        ),
        (
            "for-in",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let b: Vector[i64, 4] = Vector[i64, 4](5, 6, 7, 8);\n\
                     let v: Vec[Vector[i64, 4]] = [a, b];\n\
                     for e in v { println(e.reduce_sum()); }\n\
                 }",
            "10\n26\n",
        ),
        (
            "push-growth",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut v: Vec[Vector[i64, 4]] = [];\n\
                     for i in 0..9 { v.push(a); }\n\
                     println(v.len());\n\
                     let g = ref v[8];\n\
                     println(g.reduce_sum());\n\
                 }",
            "9\n10\n",
        ),
        (
            "push-onto-literal",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let b: Vector[i64, 4] = Vector[i64, 4](5, 6, 7, 8);\n\
                     let mut v: Vec[Vector[i64, 4]] = [a];\n\
                     v.push(b);\n\
                     v.push(a);\n\
                     let g = ref v[2];\n\
                     println(g.reduce_sum());\n\
                     println(v.len());\n\
                 }",
            "10\n3\n",
        ),
        (
            "i32x8",
            "fn main() {\n\
                     let a: Vector[i32, 8] = Vector[i32, 8](1, 2, 3, 4, 5, 6, 7, 8);\n\
                     let mut v: Vec[Vector[i32, 8]] = [];\n\
                     for i in 0..5 { v.push(a); }\n\
                     let g = ref v[4];\n\
                     println(g.reduce_sum());\n\
                 }",
            "36\n",
        ),
        (
            "f64x4",
            "fn main() {\n\
                     let a: Vector[f64, 4] = Vector[f64, 4](1.5, 2.5, 3.5, 4.5);\n\
                     let mut v: Vec[Vector[f64, 4]] = [];\n\
                     for i in 0..7 { v.push(a); }\n\
                     let g = ref v[6];\n\
                     println(g.reduce_sum());\n\
                 }",
            "12\n",
        ),
        (
            "reserve",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut v: Vec[Vector[i64, 4]] = [];\n\
                     v.reserve(16);\n\
                     v.push(a);\n\
                     let g = ref v[0];\n\
                     println(g.reduce_sum());\n\
                 }",
            "10\n",
        ),
        (
            "insert",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let b: Vector[i64, 4] = Vector[i64, 4](5, 6, 7, 8);\n\
                     let mut v: Vec[Vector[i64, 4]] = [a];\n\
                     v.insert(0, b);\n\
                     let g = ref v[0];\n\
                     println(g.reduce_sum());\n\
                     let h = ref v[1];\n\
                     println(h.reduce_sum());\n\
                 }",
            "26\n10\n",
        ),
        (
            "nested-vec",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut inner: Vec[Vector[i64, 4]] = [];\n\
                     for i in 0..5 { inner.push(a); }\n\
                     let g = ref inner[4];\n\
                     println(g.reduce_sum());\n\
                     println(inner.len());\n\
                 }",
            "10\n5\n",
        ),
        (
            // The over-alignment travels through a STRUCT: `H`'s own ABI
            // alignment is the vector field's, so the Vec-of-H buffer needs
            // the same routing even though no element is itself a vector.
            "struct-field",
            "struct H { v: Vector[i64, 4] }\n\
                 fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut hs: Vec[H] = [];\n\
                     for i in 0..9 { hs.push(H { v: a }); }\n\
                     println(hs.len());\n\
                     let g = ref hs[8];\n\
                     println(g.v.reduce_sum());\n\
                 }",
            "9\n10\n",
        ),
        // Controls — the two element shapes malloc already satisfies, and
        // the two container shapes that never used this buffer at all.
        (
            "control-vec-i64",
            "fn main() {\n\
                     let mut v: Vec[i64] = [];\n\
                     for i in 0..9 { v.push(i); }\n\
                     println(v.len());\n\
                     println(v[8]);\n\
                 }",
            "9\n8\n",
        ),
        (
            "control-v128",
            "fn main() {\n\
                     let a: Vector[i64, 2] = Vector[i64, 2](3, 4);\n\
                     let mut v: Vec[Vector[i64, 2]] = [];\n\
                     for i in 0..9 { v.push(a); }\n\
                     let g = ref v[8];\n\
                     println(g.reduce_sum());\n\
                 }",
            "7\n",
        ),
        (
            // A fixed-size `Array` is an alloca, which LLVM aligns to the
            // element's natural alignment on its own — never affected.
            "control-array",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let arr: Array[Vector[i64, 4], 3] = Array[a, a, a];\n\
                     for e in arr { println(e.reduce_sum()); }\n\
                 }",
            "10\n10\n10\n",
        ),
        (
            // Each iteration allocates a `Vec[i64]` of a different
            // length just before the vector buffer, so the heap offset
            // walks the residues mod 32 and some iteration necessarily
            // lands on a 16-but-not-32 address. Under `karac build` that
            // makes the pre-fix fault deterministic (rc=139 every run)
            // where the natural shapes are a lottery; inside this harness
            // it ran clean pre-fix like the rest. Kept because it is the
            // shape that exercises the most distinct buffer offsets.
            "heap-offset-walk",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut t: i64 = 0;\n\
                     for k in 0..24 {\n\
                         let mut pad: Vec[i64] = [];\n\
                         for j in 0..k { pad.push(j); }\n\
                         let mut v: Vec[Vector[i64, 4]] = [];\n\
                         v.push(a);\n\
                         let g = ref v[0];\n\
                         t = t + g.reduce_sum() + pad.len();\n\
                     }\n\
                     println(t);\n\
                 }",
            "516\n",
        ),
        (
            // Map values live in the runtime's own allocation, not this
            // buffer; measured clean at 200 entries (many rehashes).
            "control-map-value",
            "fn main() {\n\
                     let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                     let mut m: Map[i64, Vector[i64, 4]] = Map.new();\n\
                     for i in 0..9 { m.insert(i, a); }\n\
                     println(m.len());\n\
                     match m.get(7) {\n\
                         Some(g) => { println(g.reduce_sum()); }\n\
                         None => { println(-1); }\n\
                     }\n\
                 }",
            "9\n10\n",
        ),
    ] {
        assert_eq!(run_program(src).as_deref(), Some(want), "case {label}");
    }
}

#[test]
fn test_e2e_derive_eq_struct_with_vec_field_compares_contents() {
    // B-2026-08-12-5 — `#[derive(Eq)]` equality over a struct with a `Vec`
    // field compared the WRONG BYTES. `compile_struct_eq` recurses per
    // field through `compile_binop`, which dispatches on LLVM SHAPE, and a
    // `Vec` field has the identical `{ptr, len, cap}` layout as a `String`
    // — so Vec fields were routed to the String byte-compare, which reads
    // `len` bytes. For a String `len` IS the byte count; for a Vec it is
    // the ELEMENT count, so the compare looked at the first `len` bytes of
    // the element buffer and ignored everything past them.
    //
    // Both directions were wrong, and the FALSE POSITIVE is the dangerous
    // one — an equality that wrongly matches silently merges distinct
    // values in a dedup or returns a wrong cache hit:
    //   * `Vec[i64]`: `[7] == [263]` was TRUE (263 = 0x107, same low
    //     byte), and `[1,2] == [1,3]` was TRUE (the two bytes read are
    //     element 0's low bytes, both identical).
    //   * `Vec[String]`: equal contents compared UNEQUAL, since the bytes
    //     read are the low bytes of two distinct heap pointers.
    //
    // Fixed by routing a Vec-carrying struct to `emit_eq_fn_for_struct`,
    // the TYPE-directed comparator that already existed for `Set`/`Map`
    // keys (B-2026-06-20-15 fixed this same bug in the hashing path); the
    // `==` operator had never picked it up. Every line below is asserted
    // against `karac run --interp`, which was correct throughout.
    let out = run_program(
        r#"
#[derive(Eq)]
struct I { v: Vec[i64] }
#[derive(Eq)]
struct S { v: Vec[String] }
#[derive(Eq)]
struct P { name: String, n: i64 }
fn one(a: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(a); v }
fn two(a: i64, b: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(a); v.push(b); v }
fn strs(a: String) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(a); v }
fn main() {
    // False positives: differ only above the first byte / in a later element.
    println(I { v: one(7) } == I { v: one(263) });
    println(I { v: two(1, 2) } == I { v: two(1, 3) });
    // Genuine equality and genuine difference still work.
    println(I { v: one(7) } == I { v: one(7) });
    println(I { v: one(7) } == I { v: one(8) });
    // Length difference.
    println(I { v: one(1) } == I { v: two(1, 2) });
    // False negative: equal String contents in distinct allocations.
    println(S { v: strs("abcd") } == S { v: strs("abcd") });
    println(S { v: strs("abcd") } == S { v: strs("zzzz") });
    // `!=` must be the exact negation.
    println(I { v: two(1, 2) } != I { v: two(1, 3) });
    // A struct with NO Vec field keeps the inline field walk — control.
    println(P { name: "abcd", n: 1 } == P { name: "abcd", n: 1 });
    println(P { name: "abcd", n: 1 } == P { name: "abcd", n: 2 });
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "false
false
true
false
false
true
false
true
true
false
"
        );
    }
}

#[test]
fn test_e2e_field_read_option_shared_push_correct() {
    // B-2026-07-12-4 correctness pin (the ASAN/leak gate lives in
    // tests/memory_sanitizer.rs). Pushing a FIELD-READ `Option[shared]`
    // (`stack.push(n.left)` / `n.right`) onto a `Vec[Option[shared]]`, then
    // reading the pushed nodes back, must yield the correct values — the
    // co-ownership retain fix must not disturb the stored handles. Non-ASAN
    // so it runs everywhere as a value regression guard.
    let out = run_program(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn main() {
    let root = Some(Node {
        val: 1,
        left: Some(Node { val: 2, left: None, right: None }),
        right: Some(Node { val: 3, left: None, right: None }),
    });
    let mut stack: Vec[Option[Node]] = Vec.new();
    match root {
        None => {}
        Some(n) => { stack.push(n.left); stack.push(n.right); }
    }
    let mut sum: i64 = 0;
    for item in stack {
        match item { None => {} Some(node) => { sum = sum + node.val; } }
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5");
    }
}

#[test]
fn test_e2e_return_struct_field_vec_element_is_cloned() {
    // B-2026-07-27-1: `return <struct>.<vecfield>[i];` handed back the
    // container's OWN element buffer — the caller freed it and the struct's
    // per-element drop freed it again (SIGABRT under JIT and AOT, clean
    // under `--interp`). The TAIL spelling of the same read was already
    // correct via `compile_tail_final_expr`'s field-rooted clone arm; the
    // explicit-`return` arm never reached that helper. Both now share
    // `compile_field_rooted_index_return`. Asserts the value is right AND
    // that the source container survives every read intact — a
    // move-instead-of-clone would leave `s.xs[0]` dangling.
    if let Some(out) = run_program(
        "struct S { xs: Vec[String] }\n\
             impl S {\n\
             \x20   fn get(ref self, i: i64) -> String { return self.xs[i]; }\n\
             \x20   fn tail(ref self, i: i64) -> String { self.xs[i] }\n\
             }\n\
             fn free_get(s: ref S, i: i64) -> String { return s.xs[i]; }\n\
             fn main() {\n\
             \x20   let mut s = S { xs: Vec.new() };\n\
             \x20   s.xs.push(\"a\".to_string());\n\
             \x20   s.xs.push(\"b\".to_string());\n\
             \x20   println(s.get(0));\n\
             \x20   println(free_get(s, 1));\n\
             \x20   println(s.tail(0));\n\
             \x20   println(s.get(0));\n\
             \x20   println(s.xs[0]);\n\
             \x20   println(s.xs.len());\n\
             }",
    ) {
        assert_eq!(out, "a\nb\na\na\na\n2\n");
    }
}

#[test]
fn test_ir_return_struct_field_vec_element_emits_clone() {
    // B-2026-07-27-1 structural guard: the explicit-`return` lowering of a
    // field-rooted heap element index must emit the element deep-clone, and
    // must match what the TAIL spelling emits. Pinning the shape keeps the
    // two return forms from silently diverging again — an output-only
    // assertion can pass on a build where the missing clone happens not to
    // abort. A `Vec[i64]` (Copy) field must still emit NO clone.
    let ret_ir = ir_for(
        "struct S { xs: Vec[String] }\n\
             impl S { fn get(ref self, i: i64) -> String { return self.xs[i]; } }\n\
             fn main() { let mut s = S { xs: Vec.new() }; s.xs.push(\"a\"); println(s.get(0)); }",
    );
    assert!(
        ret_ir.contains("karac_clone_String"),
        "an explicit `return self.xs[i];` must deep-clone the element:\n{ret_ir}"
    );
    let copy_ir = ir_for(
        "struct S { ns: Vec[i64] }\n\
             impl S { fn get(ref self, i: i64) -> i64 { return self.ns[i]; } }\n\
             fn main() { let mut s = S { ns: Vec.new() }; s.ns.push(7); println(s.get(0)); }",
    );
    assert!(
        !copy_ir.contains("karac_clone_String"),
        "a Copy (`Vec[i64]`) field element must NOT be cloned:\n{copy_ir}"
    );
}

/// Iterating a SHARED-borrowed nested `Vec[Vec[i64]]` binds the inner
/// element BY VALUE (`i64`, not `ref i64`), so it is usable in arithmetic.
/// Before the fix `karac build` HARD-errored on `total + x` ("expected
/// 'i64', found 'ref i64'") while `karac run` warned-and-proceeded — a
/// run/build divergence. (B-2026-06-30-4.) This locks the build side:
/// `for x in row` over a `ref Vec[i64]` row compiles and computes the same
/// `1+2+3+4 = 10` the interpreter does.
#[test]
fn for_loop_borrowed_nested_vec_scalar_autoderefs() {
    let src = "fn cell_sum(g: ref Vec[Vec[i64]]) -> i64 {\n\
                   \x20   let mut total = 0i64;\n\
                   \x20   for row in g { for x in row { total = total + x; } }\n\
                   \x20   total\n\
                   }\n\
                   fn main() {\n\
                   \x20   let mut grid: Vec[Vec[i64]] = Vec.new();\n\
                   \x20   let mut r0: Vec[i64] = Vec.new(); r0.push(1i64); r0.push(2i64);\n\
                   \x20   let mut r1: Vec[i64] = Vec.new(); r1.push(3i64); r1.push(4i64);\n\
                   \x20   grid.push(r0); grid.push(r1);\n\
                   \x20   println(f\"{cell_sum(grid)}\");\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("10\n"));
}

/// Iterating a `mut ref Vec[i64]` binds the Copy scalar element BY VALUE
/// (`i64`, not `mut ref i64`) — bare `for` borrows via `.iter()` regardless
/// of the collection's borrow form (design.md line 2739). Before the fix
/// `karac build` HARD-errored on `x * 2` ("arithmetic operator requires
/// numeric type, found 'mut ref i64'") while `karac run` warned-and-
/// proceeded — a run/build divergence (the mutable-borrow sibling of
/// B-2026-06-30-4). (B-2026-06-30-6.) This locks the build side: the loop
/// mutates only the loop local (bare `for` is read-only — in-place element
/// mutation is `.iter_mut()`'s job), so the Vec is unchanged and the
/// program prints `3`/`4`, exactly matching `karac run`.
#[test]
fn for_loop_mut_ref_vec_scalar_element_is_read_only() {
    let src = "fn double_all(xs: mut ref Vec[i64]) {\n\
                   \x20   for x in xs { x = x * 2; }\n\
                   }\n\
                   fn main() {\n\
                   \x20   let mut v: Vec[i64] = Vec.new(); v.push(3i64); v.push(4i64);\n\
                   \x20   double_all(mut v);\n\
                   \x20   println(v[0]); println(v[1]);\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("3\n4\n"));
}

/// Reassign-and-use of the by-value loop local computes correctly on the
/// build side too: `x = x * 2` scales the local, `total + x` reads it, so
/// `(3*2)+(4*2) = 14` — matching `karac run`. (B-2026-06-30-6.)
#[test]
fn for_loop_mut_ref_vec_scalar_reassign_and_use() {
    let src = "fn sum_scaled(xs: mut ref Vec[i64]) -> i64 {\n\
                   \x20   let mut total = 0i64;\n\
                   \x20   for x in xs { x = x * 2; total = total + x; }\n\
                   \x20   total\n\
                   }\n\
                   fn main() {\n\
                   \x20   let mut v: Vec[i64] = Vec.new(); v.push(3i64); v.push(4i64);\n\
                   \x20   println(f\"{sum_scaled(mut v)}\");\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("14\n"));
}

// ── Index-store of a heap-owning Vec element (B-2026-06-19-7) ──

/// `out[j] = nb` where `out: Vec[Vec[i64]]` moves a tracked `Vec` binding
/// into an owning element slot. Before the fix the AOT binary SIGTRAPped
/// (exit 133): the store neither dropped the old element (leak) nor
/// suppressed the moved source's cleanup, so the source binding and the
/// container both freed the buffer (double-free). The interpreter was always
/// correct. This build+run test would crash pre-fix.
/// `Vec[T].truncate(n)` drops the [n, len) tail and sets len = n (buffer
/// kept). Must match the interpreter oracle (`test_vec_truncate`): pod
/// (`i64`) shorten / no-op (`n >= len`) / to-zero, plus a heap (`String`)
/// element case with a re-push after. Heap-drop leak-safety under auto-par
/// is covered by `tests/memory_sanitizer.rs::asan_vec_truncate_heap_no_leak`.
#[test]
fn e2e_vec_truncate() {
    if let Some(out) = run_program(
            "fn main() {\n\
                 let mut a: Vec[i64] = Vec.new();\n\
                 a.push(1); a.push(2); a.push(3); a.push(4);\n\
                 a.truncate(2);\n\
                 println(a.len()); println(a[0]); println(a[1]);\n\
                 a.truncate(5); println(a.len());\n\
                 a.truncate(0); println(a.len());\n\
                 let mut s: Vec[String] = Vec.new();\n\
                 s.push(\"aa\".to_string()); s.push(\"bb\".to_string()); s.push(\"cc\".to_string());\n\
                 s.truncate(1);\n\
                 println(s.len()); println(s[0]);\n\
                 s.push(\"dd\".to_string());\n\
                 println(s.len()); println(s[1]);\n\
             }",
        ) {
            assert_eq!(out, "2\n1\n2\n2\n0\n1\naa\n2\ndd\n");
        }
}

/// `Vec[T].swap_remove(i) -> T` — return element `i`, move the last element
/// into slot `i` (order not preserved), `len--`. Must match the interpreter
/// oracle (`test_vec_swap_remove`): middle index (pod) + heap (`String`)
/// index-0. Heap move-safety (no leak / no double-free) in
/// `tests/memory_sanitizer.rs::asan_vec_swap_remove_heap_no_leak`.
#[test]
fn e2e_vec_swap_remove() {
    if let Some(out) = run_program(
            "fn main() {\n\
                 let mut a: Vec[i64] = Vec.new();\n\
                 a.push(10); a.push(20); a.push(30); a.push(40);\n\
                 let x = a.swap_remove(1);\n\
                 println(x); println(a.len()); println(a[0]); println(a[1]); println(a[2]);\n\
                 let mut s: Vec[String] = Vec.new();\n\
                 s.push(\"aa\".to_string()); s.push(\"bb\".to_string()); s.push(\"cc\".to_string());\n\
                 let z = s.swap_remove(0);\n\
                 println(z); println(s.len()); println(s[0]); println(s[1]);\n\
             }",
        ) {
            assert_eq!(out, "20\n3\n10\n40\n30\naa\n2\ncc\nbb\n");
        }
}

/// B-2026-07-11-35 (push leg): a GENERIC container built via a generic
/// constructor (`Box.new()`) and filled through a generic method
/// (`fn add(mut ref self, x: T) { self.xs.push(x) }`) with NON-COPY (String)
/// elements. The mono param prologue registered `x: T` off the bare `T`
/// (never entering `owned_vecstr_params`), so the retaining `self.xs.push(x)`
/// MOVED the caller's buffer instead of deep-copying it — every method-pushed
/// element read back as garbage from the caller (interp/oracle stayed
/// correct; only codegen — JIT + native — corrupted). The prologue now
/// resolves the param to its concrete monomorph type before registration.
/// Also pins the coexistence of `Box[String]` and `Box[i64]` in one program:
/// the per-monomorph struct-drop synthesis gives each a distinct
/// `__karac_drop_struct_Box$*` so the String drain never runs over the i64
/// Vec (which would `free` each i64 as a bogus `{ptr,len,cap}`). The leak is
/// pinned in `tests/memory_sanitizer.rs::asan_generic_container_method_push_no_leak`.
#[test]
fn e2e_generic_container_method_push_noncopy_element() {
    if let Some(out) = run_program(
        "struct Box[T] { xs: Vec[T] }\n\
             impl[T] Box[T] {\n\
             \x20   fn new() -> Box[T] { Box { xs: Vec.new() } }\n\
             \x20   fn add(mut ref self, x: T) { self.xs.push(x); }\n\
             \x20   fn at(ref self, i: i64) -> T { self.xs[i] }\n\
             \x20   fn size(ref self) -> i64 { self.xs.len() }\n\
             }\n\
             fn main() {\n\
             \x20   let mut s: Box[String] = Box.new();\n\
             \x20   s.add(f\"alpha\"); s.add(f\"beta\"); s.add(f\"gamma\");\n\
             \x20   let mut n: Box[i64] = Box.new();\n\
             \x20   n.add(100); n.add(200);\n\
             \x20   println(s.at(0)); println(s.at(1)); println(s.at(2));\n\
             \x20   println(f\"{n.at(0)}\"); println(f\"{n.at(1)}\");\n\
             \x20   println(f\"{s.size()} {n.size()}\");\n\
             }",
    ) {
        assert_eq!(out, "alpha\nbeta\ngamma\n100\n200\n3 2\n");
    }
}

/// `Vec[u8].as_ptr()` lowers to a load of the `{ptr,len,cap}` header's
/// data field (field 0) — the heap-buffer FFI handoff a `host fn` blit
/// consumes (added for the Fathom framebuffer; previously only `Array`/
/// `CStr` had `as_ptr`). IR-level: the call site GEPs field 0 and loads a
/// `ptr`, and the result feeds the (raw-pointer) `sink` call.
#[test]
fn test_vec_as_ptr_loads_data_field() {
    let src = r#"
            fn driver() -> i64 {
                let mut v: Vec[u8] = Vec.new();
                v.push(7);
                let _p = v.as_ptr();
                v.len()
            }
        "#;
    let ir = ir_for(src);
    let body = function_body(&ir, "driver").expect("driver fn must lower");
    assert!(
        body.contains("vec.asptr"),
        "Vec.as_ptr() should emit the data-field load (vec.asptr); body:\n{body}"
    );
}

#[test]
fn e2e_refinement_try_from_vec_no_double_free() {
    // `Refined.try_from(v)` consumes `v`: on the Ok path the heap buffer
    // lives in the `Ok` payload, so the source binding must not free it
    // again. The suppression is emitted branch-locally in the Ok block (the
    // Err path still frees the discarded value). Pre-fix this double-freed a
    // `Vec[String]` at cleanup — the Weave `NonEmpty.try_from(enriched)`.
    if let Some(out) = run_program(
        "pub type NonEmptyV = Vec[String] where self.len() > 0;\n\
             fn main() {\n\
                 let mut v: Vec[String] = Vec.new();\n\
                 v.push(\"a\".to_string());\n\
                 v.push(\"b\".to_string());\n\
                 match NonEmptyV.try_from(v) {\n\
                     Ok(rows) => println(f\"got {rows.len()} rows\"),\n\
                     Err(_)   => println(\"empty\"),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "got 2 rows\n");
    }
}

#[test]
fn e2e_plain_struct_field_write_through_vec_element() {
    if let Some(out) = run_program(
        "struct Item { v: i64 }\n\
             fn main() {\n\
                 let mut items = Vec.new();\n\
                 items.push(Item { v: 10 });\n\
                 items.push(Item { v: 20 });\n\
                 items[1].v = 99;\n\
                 println(items[0].v);\n\
                 println(items[1].v);\n\
             }",
    ) {
        assert_eq!(out, "10\n99\n");
    }
}

// ── Binary-size floor: Vec must not re-anchor the heavy runtime cluster ──

/// B-2026-06-11-8 regression guard. A `Vec`-using compute binary must
/// dead-strip to the lean floor (~33 KB), NOT drag in the ~250 KB
/// std-IO/fmt heavy-runtime cluster. The regression: `karac_alloc_or_panic`
/// (on every `Vec.push`/`filled`/`with_capacity` path, and force-kept) used
/// `std::io::stderr()` on its OOM diagnostic, which anchored that cluster
/// onto every Vec/String binary (33 KB → 285 KB). Fixed by routing fatal
/// paths through the `fatal` module's raw `write(2)` + heapless formatter.
/// Threshold 150 KB clears both the lean (~33 KB) and full (~81 KB) floors
/// while staying far below the 285 KB heavy floor the bug produced.
#[test]
fn e2e_vec_binary_stays_lean_no_heavy_runtime_floor() {
    use karac::codegen::{compile_to_object_with_options, link_executable};
    let src = "fn main() {\n\
                   let mut v: Vec[i64] = Vec.filled(8, 0);\n\
                   v[0] = 42;\n\
                   println(f\"{v[0]}\");\n\
                   }";
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "size-regression source must parse"
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

    let id = std::process::id();
    let obj_path = format!("/tmp/karac_size_{}.o", id);
    let exe_path = format!("/tmp/karac_size_{}", id);
    if compile_to_object_with_options(
        &parsed.program,
        &obj_path,
        Some(&ownership),
        None,
        None,
        None,
    )
    .is_err()
    {
        panic!("codegen failed for Vec binary-size regression program");
    }
    // Soft-skip when the runtime archive / linker is unavailable (same
    // policy as the run_program harness), so the test doesn't fail
    // vacuously in environments without libkarac_runtime.a.
    if link_executable(&obj_path, &exe_path).is_err() {
        let _ = std::fs::remove_file(&obj_path);
        return;
    }
    let bytes = std::fs::metadata(&exe_path).map(|m| m.len()).unwrap_or(0);
    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&exe_path);
    // The build + link above run on EVERY platform (so a codegen/link
    // regression still fails here). The byte-floor itself is calibrated on
    // macOS arm64 — the project's binary-size discipline platform — and an
    // ELF/x86_64 baseline is materially larger (measured ~328 KB on the
    // Linux CI runner for the same program), so only enforce the floor
    // there; elsewhere just assert the binary built.
    if cfg!(target_os = "macos") {
        assert!(
            bytes > 0 && bytes < 150_000,
            "Vec compute binary is {bytes} B — expected < 150 KB (lean floor \
                 ~33 KB). A jump toward ~285 KB means a force-kept hot-path runtime \
                 symbol re-anchored the std-IO heavy cluster (B-2026-06-11-8)."
        );
    } else {
        assert!(bytes > 0, "Vec compute binary failed to build/link");
    }
}

#[test]
fn test_e2e_ref_enum_vec_struct_payload_field_access() {
    // Regression for B-2026-07-11-6: a `Vec[struct]` bound as an enum payload
    // lost its element TypeExpr, so `for e in entries { e.field }` bound the
    // element without a struct type and the field read garbage (`i64 0`). The
    // value-source payload binding registered `vec_elem_types` (the LLVM elem
    // type — enough for a `Vec[i64]` payload to iterate) but not
    // `var_elem_type_exprs`, which struct-field GEP needs. Now it records
    // both, matching a plain `let es: Vec[Ent]` local. Surfaced by the
    // `examples/json.kara` dogfood's `Obj(entries) => for e in entries {…}`.
    if let Some(out) = run_program(
            "enum J { N, Obj(Vec[Ent]) }\n\
             struct Ent { key: String, n: i64 }\n\
             fn dump(v: ref J, out: mut ref String) {\n\
                 match v {\n\
                     N => { out.push_str(\"n\"); }\n\
                     Obj(entries) => { for e in entries { out.push_str(e.key); out.push_str(f\"{e.n}\"); } }\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut es: Vec[Ent] = Vec.new();\n\
                 es.push(Ent { key: \"a\", n: 1 });\n\
                 es.push(Ent { key: \"b\", n: 2 });\n\
                 let mut o: String = \"\";\n\
                 dump(J.Obj(es), mut o);\n\
                 println(o);\n\
             }",
        ) {
            assert_eq!(out, "a1b2\n");
        }
}

#[test]
fn test_e2e_vec_get_first_last_unwrap_scalar() {
    // B-2026-07-14-16 (scalar leg): `v.get(i).unwrap()` / `.first().unwrap()`
    // / `.last().unwrap()` on a scalar `Vec[i64]` crashed under JIT/native —
    // `get` packs the element VALUE into the `Option[ref T]` payload, but the
    // unwrap reconstructed at `ref T` (→ `ptr`) and `inttoptr`'d the value,
    // producing a bogus pointer that faulted on use (the interpreter was
    // correct). The unwrap now peels a `ref`/`mut ref` to a SCALAR pointee so
    // the payload is rebuilt as the value. Must print the actual elements.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(10); v.push(20); v.push(30);\n\
                 println(v.get(0).unwrap());\n\
                 println(v.get(2).unwrap());\n\
                 println(v.first().unwrap());\n\
                 println(v.last().unwrap());\n\
             }",
    ) {
        assert_eq!(out, "10\n30\n10\n30\n");
    }
}

#[test]
fn test_e2e_vec_get_first_last_unwrap_heap_element() {
    // B-2026-07-14-11: `let row = g.get(i).unwrap()` / `.first()`/`.last()`
    // on a `Vec[Vec[T]]` (or `Vec[String]`) failed codegen dispatch LOUD
    // ("no handler for method 'len' on variable 'row'"): `Vec.get` returns
    // `Option[ref elem]` and the `ref` was neither peeled at reconstruction
    // (so the 3-word `{ptr,len,cap}` collapsed to its data pointer) nor at
    // binding registration (so `row` never landed in `vec_elem_types`). The
    // fix reconstructs at the element VALUE shape and registers the peeled
    // element type, treating the alias as borrow-elided (no double-free).
    // Interpreter is the oracle.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let g: Vec[Vec[i64]] = [[1, 2, 3], [4, 5, 6], [7, 8]];\n\
                 let row = g.get(1).unwrap();\n\
                 println(row.len());\n\
                 println(row.get(2).unwrap());\n\
                 let f = g.first().unwrap();\n\
                 println(f.len());\n\
                 let l = g.last().unwrap();\n\
                 println(l.len());\n\
                 let words: Vec[String] = [\"hello\", \"world\"];\n\
                 let w = words.get(1).unwrap();\n\
                 println(w.len());\n\
                 println(w);\n\
             }",
    ) {
        assert_eq!(out, "3\n6\n3\n2\n5\nworld\n");
    }
}

#[test]
fn test_e2e_vec_get_first_last_unwrap_struct_element() {
    // B-2026-07-14-16 (struct leg): `let p = v.get(i).unwrap()` /
    // `.first()`/`.last()` on a `Vec[Struct]` reconstructed at `ref Struct`
    // → `ptr` and `inttoptr`'d the packed VALUE, so every field read came
    // back 0/garbage (the crash-free but silent-wrong leg; interp was the
    // oracle). Vec.get PACKS the element value, so the unwrap now peels the
    // `ref` to the struct value shape for a value-packing accessor. Covers a
    // pure-value struct (inline, ≤3 words), a heap-field struct (the fields
    // alias the container's buffers — borrow-elided, no double-free), and a
    // boxed struct (>3 words). Must match the interpreter.
    if let Some(out) = run_program(
        "struct Point { x: i64, y: i64 }\n\
             struct Big { a: i64, b: i64, c: i64, d: i64, e: i64 }\n\
             fn main() {\n\
                 let pts: Vec[Point] = [Point { x: 1, y: 2 }, Point { x: 3, y: 4 }];\n\
                 let p = pts.get(1).unwrap();\n\
                 println(p.x);\n\
                 println(p.y);\n\
                 let f = pts.first().unwrap();\n\
                 println(f.x);\n\
                 let big: Vec[Big] = [Big { a: 1, b: 2, c: 3, d: 4, e: 5 }];\n\
                 let b = big.last().unwrap();\n\
                 println(b.a);\n\
                 println(b.e);\n\
             }",
    ) {
        assert_eq!(out, "3\n4\n1\n1\n5\n");
    }
}

#[test]
fn test_e2e_vec_element_field_move_by_assignment_and_its_controls() {
    // B-2026-08-11-25 — the value half. `out = stats[0].region` moves a
    // heap field out of a struct held as a Vec element into an EXISTING
    // binding; the field was never cap-zeroed in its owner, so the owner's
    // drop freed it too and the binary aborted with a double free BEFORE
    // flushing stdout — so the program looked like it produced no output at
    // all, which reads as a startup failure.
    //
    // The four ABORTING shapes from the row (String field, `Vec[i64]`
    // field, an unrelated earlier read of the same field, and a source
    // built by `+` rather than `push_str`) and the four CONTROLS that were
    // already clean and that an over-broad fix would regress: the `let`
    // form, a plain struct binding, a tuple element, and a Vec with no
    // struct at all.
    //
    // Payloads are built at RUN TIME throughout. A `String` literal field
    // is static, so a stray second free lands on a non-heap pointer and
    // passes silently — the row records two early controls that looked
    // clean for exactly that reason. The ASAN sibling in
    // tests/memory_sanitizer.rs is what proves the repair is not a leak.
    assert_eq!(
        run_program(
            "struct R { region: String }\n\
                 struct V { xs: Vec[i64] }\n\
                 fn main() {\n\
                     let mut s1 = String.new(); s1.push_str(\"north\");\n\
                     let mut a1: Vec[R] = Vec.new(); a1.push(R { region: s1 });\n\
                     let mut o1 = String.new(); o1 = a1[0].region;\n\
                     let mut iv: Vec[i64] = Vec.new(); iv.push(7);\n\
                     let mut a2: Vec[V] = Vec.new(); a2.push(V { xs: iv });\n\
                     let mut o2: Vec[i64] = Vec.new(); o2 = a2[0].xs;\n\
                     let mut s3 = String.new(); s3.push_str(\"east\");\n\
                     let mut a3: Vec[R] = Vec.new(); a3.push(R { region: s3 });\n\
                     let pre = a3[0].region.len();\n\
                     let mut o3 = String.new(); o3 = a3[0].region;\n\
                     let c1 = \"so\".to_string(); let c2 = \"uth\".to_string();\n\
                     let mut a4: Vec[R] = Vec.new(); a4.push(R { region: c1 + c2 });\n\
                     let mut o4 = String.new(); o4 = a4[0].region;\n\
                     let mut s5 = String.new(); s5.push_str(\"west\");\n\
                     let mut a5: Vec[R] = Vec.new(); a5.push(R { region: s5 });\n\
                     let o5 = a5[0].region;\n\
                     let mut s6 = String.new(); s6.push_str(\"up\");\n\
                     let h6 = R { region: s6 };\n\
                     let mut o6 = String.new(); o6 = h6.region;\n\
                     let mut s7 = String.new(); s7.push_str(\"down\");\n\
                     let t7 = (R { region: s7 }, 1);\n\
                     let mut o7 = String.new(); o7 = t7.0.region;\n\
                     let mut s8 = String.new(); s8.push_str(\"flat\");\n\
                     let mut a8: Vec[String] = Vec.new(); a8.push(s8);\n\
                     let mut o8 = String.new(); o8 = a8[0].clone();\n\
                     println(f\"{o1} {o2.len()} {pre} {o3} {o4} {o5} {o6} {o7} {o8}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("north 1 4 east south west up down flat\n"),
    );
}

#[test]
fn test_e2e_field_rooted_vec_elem_read_into_enum_payload_values() {
    // B-2026-08-13-11's VALUE side — the row's own undo/redo repro, with the
    // payloads PRINTED rather than counted.
    //
    // The memory claim is the asan twin's; what this pins is that the round
    // trip preserves the text. `beta` comes out of the Vec into the returned
    // `Cmd.Insert`, goes back INTO the Vec on the next call, and comes out
    // again into the next returned command — so a clone emitted at the wrong
    // point prints an empty or garbled payload here rather than merely
    // double-freeing. `d.lines[0]` last says the element the round trip did
    // NOT touch is still intact.
    //
    // Payloads are built with `String.new()` + `push_str` on purpose: a
    // string LITERAL is static, so the second free lands on a non-heap
    // pointer and the whole bug disappears — the measurement hazard the row
    // recorded after its first reduction attempt looked clean.
    assert_eq!(
            run_program(
                "enum Cmd { Insert(i64, String), Delete(i64, String) }\n\
                 struct Doc { lines: Vec[String] }\n\
                 fn apply(d: mut ref Doc, c: Cmd) -> Cmd {\n\
                     match c {\n\
                         Insert(at, text) => {\n\
                             d.lines.insert(at as usize, text);\n\
                             Cmd.Delete(at, d.lines[at as usize].clone())\n\
                         }\n\
                         Delete(at, _text) => {\n\
                             let gone = d.lines[at as usize].clone();\n\
                             d.lines.remove(at as usize);\n\
                             Cmd.Insert(at, gone)\n\
                         }\n\
                     }\n\
                 }\n\
                 fn main() {\n\
                     let mut l: Vec[String] = Vec.new();\n\
                     let mut a = String.new(); a.push_str(\"alpha\");\n\
                     let mut b = String.new(); b.push_str(\"beta\");\n\
                     l.push(a); l.push(b);\n\
                     let mut d = Doc { lines: l };\n\
                     let inv = apply(mut d, Cmd.Delete(1, \"beta\"));\n\
                     match inv { Insert(i, t) => { println(t); } Delete(i, t) => { println(t); } }\n\
                     let i2 = apply(mut d, inv);\n\
                     match i2 { Insert(i, t) => { println(t); } Delete(i, t) => { println(t); } }\n\
                     println(d.lines.len());\n\
                     println(d.lines[0]);\n\
                 }"
            )
            .as_deref(),
            Some("beta\nbeta\n2\nalpha\n"),
        );
}

/// B-2026-08-13-20 — a Vec whose `cap` has been zeroed must not walk its
/// ELEMENTS either, not just skip the buffer free.
///
/// `cap == 0` is codegen's "this slot no longer owns its buffer" marker:
/// every move suppression disarms a source by zeroing cap and leaving
/// `{ptr, len}` alone. The emitted `karac_drop_Vec_<E>` guarded only the
/// free on it and walked `0..len` regardless, so a disarmed slot ran the
/// element drop over memory its new owner had already released.
///
/// The element type is what made it visible: `Vec[i64]` has a no-op
/// element drop, so the unguarded walk did nothing and the shape looked
/// fine. `Vec[String]` frees each element, and the second pass over the
/// same pointers SEGFAULTED in libc `free` — with `karac check` clean and
/// the interpreter printing the right answer, so `--interp` hid it
/// completely and only a build found it.
///
/// Fixed in the emitted drop fn rather than by also zeroing `len` at the
/// move site: that closes the class for every suppression path at once,
/// and leaves `len` readable, which is what the defensive-copy rows
/// (B-2026-08-13-14 / -19) rely on to keep a moved-from source's contents
/// observable instead of empty. The inline `FreeVecBuffer` cleanup had
/// always guarded both halves this way — the two implementations of one
/// operation simply disagreed.
#[test]
fn test_e2e_disarmed_vec_does_not_walk_its_elements() {
    assert_eq!(
        run_program(
            "struct A { mut lines: Vec[String] }\n\
                 fn main() {\n\
                     let mut v: Vec[String] = Vec.new();\n\
                     v.push(f\"beta\");\n\
                     let t: (Vec[String], i64) = (v, 2);\n\
                     let r = t.0;\n\
                     println(r.len());\n\
                     let mut v2: Vec[String] = Vec.new();\n\
                     v2.push(f\"one\");\n\
                     v2.push(f\"two\");\n\
                     let t2: (Vec[String], i64) = (v2, 3);\n\
                     let mut r2 = t2.0;\n\
                     r2.push(f\"three\");\n\
                     let chk = t2.0;\n\
                     println(f\"{r2.len()} {chk.len()} {chk[0]}\");\n\
                     let mut iv: Vec[i64] = Vec.new();\n\
                     iv.push(1);\n\
                     let mut vv: Vec[Vec[i64]] = Vec.new();\n\
                     vv.push(iv);\n\
                     let t3: (Vec[Vec[i64]], i64) = (vv, 4);\n\
                     let r3 = t3.0;\n\
                     println(f\"{r3.len()} {r3[0].len()}\");\n\
                     let mut v4: Vec[String] = Vec.new();\n\
                     v4.push(f\"x\");\n\
                     let a = A { lines: v4 };\n\
                     let b = a;\n\
                     println(b.lines.len());\n\
                 }"
        )
        .as_deref(),
        Some("1\n3 2 one\n1 1\n1\n"),
    );
}

#[test]
fn test_e2e_nested_vec_elem_field_read_is_a_copy() {
    // B-2026-08-13-4's VALUE side, and the reason the fix is a CLONE rather
    // than a move: after `let bound = ds[0].inner.word`, the ELEMENT still
    // has its string. That is what `karac check` accepts and what the
    // interpreter does, so cap-zeroing the element instead — the cheaper
    // fix — would have turned a double free into a silent use-after-free.
    // Every leg reads the element back after consuming the field, which is
    // the assertion that rules that shortcut out.
    //
    // `ds[0].inner.n` is read after the four string consumptions: the
    // scalar sibling of the cloned field must be untouched, which a clone
    // emitted at the wrong offset would corrupt.
    assert_eq!(
        run_program(
            "struct Pair { word: String, n: i64 }\n\
                 struct Deep { inner: Pair, tag: i64 }\n\
                 struct Outer { mid: Deep, label: String }\n\
                 fn main() {\n\
                     let k = 1;\n\
                     let mut ds: Vec[Deep] = Vec.new();\n\
                     ds.push(Deep { inner: Pair { word: f\"a{k}\", n: 7 }, tag: 8 });\n\
                     let bound = ds[0].inner.word;\n\
                     println(bound);\n\
                     println(ds[0].inner.word);\n\
                     let mut out: Vec[String] = Vec.new();\n\
                     out.push(ds[0].inner.word);\n\
                     println(out[0]);\n\
                     println(ds[0].inner.word);\n\
                     println(ds[0].inner.n);\n\
                     let mut xs: Vec[Outer] = Vec.new();\n\
                     xs.push(Outer {\n\
                         mid: Deep { inner: Pair { word: f\"b{k}\", n: 9 }, tag: 10 },\n\
                         label: f\"L{k}\",\n\
                     });\n\
                     let deep = xs[0].mid.inner.word;\n\
                     println(deep);\n\
                     println(xs[0].mid.inner.word);\n\
                 }"
        )
        .as_deref(),
        Some("a1\na1\na1\na1\n7\nb1\nb1\n"),
    );
}

#[test]
fn test_e2e_vec_sort_by_partition_path_low_cardinality() {
    // B-2026-08-11-10 § Direction 7 — above the entry probe's length floor,
    // a low-cardinality `sort_by` leaves the merge entirely and takes the
    // full-array stable partition. Strict `assert_eq!` rather than the
    // tolerant `if let Some(out)` form: a stale runtime archive must fail
    // this loudly instead of asserting nothing (CLAUDE.md).
    //
    // Three properties in one program, because the partition can violate
    // each independently:
    //   - 6000 elements over 5 keys must come out SORTED (inv == 0),
    //   - and STABLE (the second field is the original index, so any
    //     reordering of equal keys shows up as ord != expected), which is
    //     what the count-then-scatter shape exists to preserve;
    //   - a run of 6000 records that ALL compare equal must survive the
    //     all-equal early exit untouched — that branch returns a range
    //     declaring it sorted without writing it, so only stability can
    //     catch it having moved anything.
    //
    // 6000 straddles the geometry deliberately: > 4096 so the probe fires,
    // with halves < 4096 so the leaf hands back to the merge and the
    // ping-pong parity copy-back is exercised too.
    assert_eq!(
        run_program(
            "struct R { key: i64, ord: i64 }\n\
                 fn main() {\n\
                     let mut v: Vec[R] = Vec.new();\n\
                     let mut seed: i64 = 12345;\n\
                     let mut i: i64 = 0;\n\
                     while i < 6000 {\n\
                         seed = (seed * 1103515245 + 12345) % 2147483648;\n\
                         v.push(R { key: seed % 5, ord: i });\n\
                         i = i + 1;\n\
                     }\n\
                     v.sort_by(|a, b| a.key.cmp(b.key));\n\
                     let mut inv: i64 = 0;\n\
                     let mut unstable: i64 = 0;\n\
                     let mut j: i64 = 1;\n\
                     while j < 6000 {\n\
                         if v[j].key < v[j - 1].key { inv = inv + 1; }\n\
                         if v[j].key == v[j - 1].key {\n\
                             if v[j].ord < v[j - 1].ord { unstable = unstable + 1; }\n\
                         }\n\
                         j = j + 1;\n\
                     }\n\
                     let mut w: Vec[R] = Vec.new();\n\
                     let mut m: i64 = 0;\n\
                     while m < 6000 { w.push(R { key: 3, ord: m }); m = m + 1; }\n\
                     w.sort_by(|a, b| a.key.cmp(b.key));\n\
                     let mut moved: i64 = 0;\n\
                     let mut q: i64 = 0;\n\
                     while q < 6000 { if w[q].ord != q { moved = moved + 1; } q = q + 1; }\n\
                     println(f\"{inv} {unstable} {moved}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("0 0 0\n"),
    );
}

#[test]
fn test_e2e_vec_sort_by_partition_insertion_leaf_is_stable() {
    // B-2026-08-16-3 — the partition's leaf sorter. A range that stops
    // partitioning because it is SHORT is now sorted by a dedicated
    // insertion sort instead of being handed back to the merge, so every
    // element on this path passes through code the older partition tests
    // never reached.
    //
    // This drives the UNSTRUCTURED arm, which the low-cardinality test
    // above does not: 9000 keys drawn over a wide range, so the probe
    // admits on orderedness rather than cardinality, the per-range tie gate
    // is off, and the recursion descends the full log2(n/64) to leaves that
    // are all insertion-sorted.
    //
    // Stability is the property at risk and the reason for the `ord` field:
    // an insertion sort is stable only if an element shifts past strictly
    // greater elements, and an `>=` there would be just as sorted and
    // silently reorder equal keys. `key` is deliberately `% 300` so there
    // are ~30 duplicates of each key spread across many different leaves —
    // with distinct keys the stability assertion would be vacuous.
    //
    // Strict `assert_eq!` rather than the tolerant `if let Some(out)` form:
    // a stale runtime archive must fail this loudly (CLAUDE.md).
    assert_eq!(
        run_program(
            "struct R { key: i64, ord: i64 }\n\
                 fn main() {\n\
                     let mut v: Vec[R] = Vec.new();\n\
                     let mut seed: i64 = 987654321;\n\
                     let mut i: i64 = 0;\n\
                     while i < 9000 {\n\
                         seed = (seed * 1103515245 + 12345) % 2147483648;\n\
                         v.push(R { key: (seed / 65536) % 300, ord: i });\n\
                         i = i + 1;\n\
                     }\n\
                     v.sort_by(|a, b| a.key.cmp(b.key));\n\
                     let mut inv: i64 = 0;\n\
                     let mut unstable: i64 = 0;\n\
                     let mut dups: i64 = 0;\n\
                     let mut j: i64 = 1;\n\
                     while j < 9000 {\n\
                         if v[j].key < v[j - 1].key { inv = inv + 1; }\n\
                         if v[j].key == v[j - 1].key {\n\
                             dups = dups + 1;\n\
                             if v[j].ord < v[j - 1].ord { unstable = unstable + 1; }\n\
                         }\n\
                         j = j + 1;\n\
                     }\n\
                     let mut sum: i64 = 0;\n\
                     let mut q: i64 = 0;\n\
                     while q < 9000 { sum = sum + v[q].ord; q = q + 1; }\n\
                     println(f\"{inv} {unstable} {dups > 8000} {sum}\");\n\
                 }\n"
        )
        .as_deref(),
        // inv/unstable zero; `dups > 8000` guards the stability check from
        // going vacuous if the key range ever changes; the ord sum is
        // 0+1+..+8999 and catches an element dropped or duplicated by the
        // ping-pong, which sortedness alone would not.
        Some("0 0 true 40495500\n"),
    );
}

#[test]
fn test_e2e_vec_get_unwrap_struct_element_loop() {
    // B-2026-07-14-16 (struct leg) regression: reading many `Vec[Struct]`
    // elements via `v.get(j).unwrap()` in a loop. An earlier version of the
    // struct borrow-elision probed a drop-fn SYNTHESISER as a predicate,
    // which moved the LLVM builder's insert point mid-let-codegen and
    // produced a layout-sensitive CRASH here (empty output) that valgrind
    // masked. The predicate is now pure. Sum must be exact.
    if let Some(out) = run_program(
        "struct Score { v: i64 }\n\
             fn main() {\n\
                 let mut v: Vec[Score] = Vec.new();\n\
                 let mut i: i64 = 0;\n\
                 while i < 100 {\n\
                     v.push(Score { v: (i * 37 + 5) % 100 });\n\
                     i = i + 1;\n\
                 }\n\
                 v.sort_by(|a, b| a.v.cmp(b.v));\n\
                 let mut bad: i64 = 0;\n\
                 let mut prev: i64 = -1;\n\
                 let mut j: i64 = 0;\n\
                 while j < 100 {\n\
                     let s = v.get(j).unwrap();\n\
                     if s.v < prev { bad = bad + 1; }\n\
                     prev = s.v;\n\
                     j = j + 1;\n\
                 }\n\
                 println(bad);\n\
             }",
    ) {
        assert_eq!(out, "0\n");
    }
}

#[test]
fn test_e2e_option_take_get_or_insert() {
    // B-2026-07-14-6 (mutating combinators): `take()` yields the receiver's
    // current value and leaves `None` in its slot (a second take yields
    // None); `get_or_insert(v)` fills a `None` with `Some(v)` and yields the
    // now-present payload BY VALUE. Both mutate the receiver's slot and are
    // seeded receiver-mutating in the effectchecker so the auto-par gate
    // serializes them. Must match the interpreter oracle.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut o: Option[i64] = Some(5);\n\
                 let t = o.take();\n\
                 println(t.unwrap_or(0));\n\
                 println(o.unwrap_or(0 - 1));\n\
                 let mut p: Option[i64] = Some(9);\n\
                 let a = p.take();\n\
                 let b = p.take();\n\
                 println(a.unwrap_or(0));\n\
                 println(b.unwrap_or(0 - 3));\n\
                 let mut s: Option[i64] = Some(5);\n\
                 println(s.get_or_insert(9));\n\
                 println(s.unwrap_or(0 - 1));\n\
                 let mut n: Option[i64] = None;\n\
                 println(n.get_or_insert(9));\n\
                 println(n.unwrap_or(0 - 1));\n\
             }",
    ) {
        assert_eq!(out, "5\n-1\n9\n-3\n5\n5\n9\n9\n");
    }
}

/// B-2026-08-09-9 — a user enum with a `Vec[String]` payload, consumed by a
/// match arm over a LIVE local. Split out of leg 3 as an `#[ignore]`d gap
/// and now closed; this is that test, un-ignored.
///
/// The clone leg was gated OFF this shape because its duplicator
/// (`deep_copy_enum_heap_payload_in_place`) copies the outer buffer only,
/// leaving the element `String`s shared — measured then as two invalid
/// reads plus two invalid frees, i.e. correct output but memory-unsafe.
///
/// The premise behind that outer-only copy was that it mirrored "the enum
/// drop's outer-only payload free". Half true, and the missing half is the
/// bug: `emit_enum_drop_switch`'s `VecOrString` arm does free only the outer
/// buffer, but it is not the whole owner — the ELEMENTS are drained by the
/// separate per-binding container-elem-bodies channel. So the copy was a
/// level shallower than the thing it had to be independent of.
///
/// Element depth had to become the CALLER's choice rather than the default:
/// making it unconditional leaked 1990 bytes across 300 allocations in
/// `asan_match_bound_struct_variant_vec_field_reborrow_no_double_free`,
/// because most callers produce a copy that is dropped by the enum's own
/// `EnumDrop` — outer-only — so the deep elements had no owner. The
/// live-local clone qualifies precisely because it is produced ONLY when an
/// arm's payload escapes into a consumer, and that consumer drains them.
///
/// Case 3 is the reason the gate survives in widened form rather than being
/// deleted: a `NestedStruct` payload can carry a user `impl Drop`, and
/// duplicating it runs that body TWICE — not a memory bug at all, but a
/// visible one (`e2e_own_drop_enum_reassign_sequencing` grew a second
/// `drop 5 l5`). Independence and observability are separate hazards.
#[test]
fn test_e2e_consuming_match_over_live_enum_vec_payload_keeps_the_source() {
    // 1. THE BUG: `Vec[String]` payload, arm consumes it via `for`, source
    // read again afterwards. Element buffers must be independent.
    assert_eq!(
        run_program(
            "enum E { A(Vec[String]), B }\n\
                 fn main() {\n\
                     let e: E = E.A([f\"aa\", f\"bb\"]);\n\
                     match e { E.A(v) => { for s in v { println(s); } } E.B => {} }\n\
                     match e { E.A(v) => { for s in v { println(s); } } E.B => {} }\n\
                 }"
        )
        .as_deref(),
        Some("aa\nbb\naa\nbb\n")
    );
    // 2. The scalar-element sibling, which the pre-widening gate already
    // allowed — kept so a regression cannot quietly narrow back to it.
    assert_eq!(
        run_program(
            "enum E { A(Vec[i64]), B }\n\
                 fn main() {\n\
                     let e: E = E.A([11, 22]);\n\
                     match e { E.A(v) => { for s in v { println(s); } } E.B => {} }\n\
                     match e { E.A(v) => { for s in v { println(s); } } E.B => {} }\n\
                 }"
        )
        .as_deref(),
        Some("11\n22\n11\n22\n")
    );
    // 3. CONTROL — a payload struct with a user `Drop`. The clone must NOT
    // fire: duplicating it would run the body twice, which is observable
    // even though it is memory-safe. One `drop`, not two — and it lands at
    // the arm's NLL end rather than at scope exit, which is the existing
    // sequencing this control must not perturb either.
    assert_eq!(
        run_program(
            "struct R { id: i64 }\n\
                 impl Drop for R {\n\
                 \x20   fn drop(mut ref self) { println(f\"drop {self.id}\") }\n\
                 }\n\
                 enum E { A(R), B }\n\
                 fn main() {\n\
                     let e: E = E.A(R { id: 7 });\n\
                     match e { E.A(v) => { println(f\"got {v.id}\"); } E.B => {} }\n\
                     println(f\"end\");\n\
                 }"
        )
        .as_deref(),
        Some("got 7\ndrop 7\nend\n")
    );
}

/// B-2026-08-09-12 — the `<refparam>.field` ref-chain ENUM clone leg, the
/// sibling that shares B-2026-08-09-9's payload duplicator.
///
/// `clone_escaping_borrowed_ref_chain_enum` (B-2026-07-21-5/-6) exists so a
/// consuming arm over a borrowed enum gets an INDEPENDENT payload instead of
/// aliasing the caller's storage. With the outer-only copy it was
/// independent only one level deep: a `Vec[String]` payload came back with
/// the element buffers still shared, so the arm's `for` loop freed strings
/// the caller still owned — one invalid read and one invalid free per call,
/// under correct-looking stdout.
///
/// Gating the leg off this shape was NOT an option, which is what makes the
/// element-deep copy the only route: falling back to the un-cloned path
/// reintroduces B-2026-07-21-5/-6, the aliasing this leg was written to
/// prevent. The live-local sibling could afford a gate; this one cannot.
///
/// Case 2 is the read-only counterpart, which must stay a zero-cost alias —
/// no clone, no copy, the caller keeps its payload.
#[test]
fn test_e2e_ref_chain_enum_vec_payload_clone_is_element_deep() {
    // 1. THE BUG: consumed through a `ref` param, called twice so the second
    // call reads what the first would have freed.
    assert_eq!(
        run_program(
            "enum E { A(Vec[String]), B }\n\
                 struct S { e: E }\n\
                 fn take(s: ref S) {\n\
                     match s.e { E.A(x) => { for t in x { println(t); } } E.B => {} }\n\
                 }\n\
                 fn main() {\n\
                     let s: S = S { e: E.A([f\"aa\", f\"bb\"]) };\n\
                     take(s);\n\
                     take(s);\n\
                 }"
        )
        .as_deref(),
        Some("aa\nbb\naa\nbb\n")
    );
    // 2. CONTROL — read-only through the borrow: no clone at all.
    assert_eq!(
        run_program(
            "enum E { A(Vec[String]), B }\n\
                 struct S { e: E }\n\
                 fn peek(s: ref S) {\n\
                     match s.e { E.A(x) => { println(x[0]); } E.B => {} }\n\
                 }\n\
                 fn main() {\n\
                     let s: S = S { e: E.A([f\"aa\", f\"bb\"]) };\n\
                     peek(s);\n\
                     peek(s);\n\
                 }"
        )
        .as_deref(),
        Some("aa\naa\n")
    );
}

#[test]
fn e2e_try_push_fallible_codegen() {
    // phase-8-stdlib-floor item 8: `Vec.try_push` lowers to real fallible
    // allocation (grow via `karac_alloc_fallible`, null → `Err(AllocError)`)
    // and returns `Result[(), AllocError]`. The host never OOMs, so every
    // push is `Ok`; verify the result is matchable AND the element is
    // actually stored (the grow path runs: Vec.new starts cap 0, so the
    // first push grows to cap 4).
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 match v.try_push(10_i64) {\n\
                     Ok(_) => println(\"ok1\"),\n\
                     Err(_) => println(\"err1\"),\n\
                 }\n\
                 let _ = v.try_push(20_i64);\n\
                 let _ = v.try_push(30_i64);\n\
                 println(v.len());\n\
                 println(v[0]);\n\
                 println(v[2]);\n\
             }",
    ) {
        assert_eq!(out, "ok1\n3\n10\n30\n");
    }
}

#[test]
fn e2e_try_push_question_propagation_codegen() {
    // `try_push` composes with `?` and `Ok(())` (Result[(), AllocError]).
    if let Some(out) = run_program(
        "fn fill(v: mut ref Vec[i64]) -> Result[(), AllocError] {\n\
                 v.try_push(1_i64)?;\n\
                 v.try_push(2_i64)?;\n\
                 Ok(())\n\
             }\n\
             fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 match fill(mut v) {\n\
                     Ok(_) => println(v.len()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "2\n");
    }
}

#[test]
fn e2e_vec_contains_scalar_codegen() {
    // B-2026-06-10-1: `Vec.contains(x)` lowers to a linear element scan
    // (element `==` via `compile_binop`). Scalar element type.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut xs: Vec[i64] = Vec.new();\n\
                 xs.push(10);\n\
                 xs.push(20);\n\
                 xs.push(30);\n\
                 println(xs.contains(20));\n\
                 println(xs.contains(25));\n\
             }",
    ) {
        assert_eq!(out, "true\nfalse\n");
    }
}

/// `Vec.binary_search(x) -> Option[i64]` codegen, mirroring the interpreter
/// (Rust's branchless `binary_search_by`). Covers found / not-found / empty,
/// unsigned (u8) and String element types, the Slice receiver, and — the
/// case that pins the exact algorithm — DUPLICATE keys (textbook
/// return-on-first-equal would pick a different index than std does).
#[test]
fn e2e_vec_binary_search_codegen() {
    if let Some(out) = run_program(
        "fn print_opt(o: Option[i64]) {\n\
                 match o { Some(i) => println(i), None => println(-1i64) }\n\
             }\n\
             fn main() {\n\
                 let v: Vec[i64] = vec![1, 3, 5, 7, 9, 11];\n\
                 print_opt(v.binary_search(7));\n\
                 print_opt(v.binary_search(1));\n\
                 print_opt(v.binary_search(11));\n\
                 print_opt(v.binary_search(4));\n\
                 print_opt(v.binary_search(12));\n\
                 let e: Vec[i64] = Vec.new();\n\
                 print_opt(e.binary_search(5));\n\
                 let u: Vec[u8] = vec![10u8, 50u8, 200u8, 250u8];\n\
                 print_opt(u.binary_search(200u8));\n\
                 print_opt(u.binary_search(99u8));\n\
                 let s: Vec[String] = vec![\"apple\", \"banana\", \"cherry\"];\n\
                 print_opt(s.binary_search(\"cherry\"));\n\
                 print_opt(s.binary_search(\"fig\"));\n\
                 let dup: Vec[i64] = vec![1, 2, 2, 2, 2, 3, 4];\n\
                 print_opt(dup.binary_search(2));\n\
                 let sl: Slice[i64] = v.as_slice();\n\
                 print_opt(sl.binary_search(9));\n\
             }",
    ) {
        // 7→3 1→0 11→5 miss miss empty→miss | u8 200→2 99→miss | cherry→2
        // fig→miss | dup 2→4 (std's index) | slice 9→4
        assert_eq!(out, "3\n0\n5\n-1\n-1\n-1\n2\n-1\n2\n-1\n4\n4\n");
    }
}

#[test]
fn e2e_try_push_front_fallible_codegen() {
    // `VecDeque.try_push_front` — fallible shift-insert at index 0.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut q: VecDeque[i64] = VecDeque.new();\n\
                 let _ = q.try_push_back(2_i64);\n\
                 match q.try_push_front(1_i64) {\n\
                     Ok(_) => println(\"ok\"),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
                 println(q.len());\n\
                 println(q[0]);\n\
                 println(q[1]);\n\
             }",
    ) {
        assert_eq!(out, "ok\n2\n1\n2\n");
    }
}

#[test]
fn e2e_try_with_capacity_match_codegen() {
    // phase-8-stdlib-floor item 8: `Vec.try_with_capacity` — fallible
    // `with_capacity` returning `Result[Vec[T], AllocError]`. Match form
    // binds the `Result` directly; the empty `{cap=n, len=0}` Vec is
    // wrapped in `Result.Ok(_)` and round-trips through match-extraction.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let r: Result[Vec[i64], AllocError] = Vec.try_with_capacity(4);\n\
                 match r {\n\
                     Ok(v) => {\n\
                         let mut v3 = v;\n\
                         v3.push(7_i64);\n\
                         println(\"ok\"); println(v3.len()); println(v3[0]);\n\
                     }\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "ok\n1\n7\n");
    }
}

#[test]
fn e2e_try_with_capacity_question_codegen() {
    // `?`-form: `let mut v: Vec[i64] = Vec.try_with_capacity(n)?` pre-sizes
    // the Vec, pushes fill the reserved slots. Exercises both the
    // element-type recovery through the `Result` wrapper AND the multi-word
    // `?` Ok-payload reconstruction (the `Vec` unwrapped by `?`).
    if let Some(out) = run_program(
        "fn build() -> Result[i64, AllocError] {\n\
                 let mut v: Vec[i64] = Vec.try_with_capacity(3)?;\n\
                 v.push(10_i64);\n\
                 v.push(20_i64);\n\
                 v.push(30_i64);\n\
                 Ok(v[0] + v[1] + v[2])\n\
             }\n\
             fn main() {\n\
                 match build() { Ok(s) => println(s), Err(_) => println(\"err\") }\n\
             }",
    ) {
        assert_eq!(out, "60\n");
    }
}

#[test]
fn e2e_try_clone_vec_scalar_codegen() {
    // phase-8-stdlib-floor item 8: `Vec[i64].try_clone()` — fallible deep
    // clone returning `Result[Vec[T], AllocError]`. Scalar element → single
    // fallible buffer alloc + memcpy-equivalent per-element copy. The clone
    // is independent: mutating the source after cloning must not touch the
    // clone (and vice-versa).
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut src: Vec[i64] = Vec.new();\n\
                 src.push(10_i64);\n\
                 src.push(20_i64);\n\
                 match src.try_clone() {\n\
                     Ok(c) => {\n\
                         let mut c2 = c;\n\
                         src.push(30_i64);\n\
                         c2.push(99_i64);\n\
                         println(src.len()); println(c2.len());\n\
                         println(c2[0]); println(c2[1]); println(c2[2]);\n\
                     }\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "3\n3\n10\n20\n99\n");
    }
}

#[test]
fn e2e_try_clone_vec_tuple_scalar_codegen() {
    // `Vec[(i64, i64)].try_clone()` — element is a non-heap tuple,
    // exercising the tuple fallible-clone fn (per-field recursion) nested
    // inside the Vec fallible-clone loop. (The tuple-WITH-heap-element
    // variant `Vec[(i64, String)]` is pre-existing-broken even under the
    // panicking `.clone()` — bugs.md B-2026-06-10-5 — so it is not covered.)
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut src: Vec[(i64, i64)] = Vec.new();\n\
                 src.push((1_i64, 2_i64));\n\
                 src.push((3_i64, 4_i64));\n\
                 match src.try_clone() {\n\
                     Ok(c) => {\n\
                         println(c.len());\n\
                         println(c[0].0); println(c[0].1);\n\
                         println(c[1].0); println(c[1].1);\n\
                     }\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "2\n1\n2\n3\n4\n");
    }
}

#[test]
fn test_e2e_vec_macro_literal() {
    // `vec![a, b, c]` desugars to the same PrefixCollectionLiteral node
    // codegen already lowers for `Vec[a, b, c]` — codegen never sees a
    // `vec!` node. Pins the parser desugaring through the AOT backend
    // (skips vacuously when the runtime archive is absent). The repeat
    // form `vec![v; n]` / `Vec[v; n]` is covered by
    // `test_e2e_vec_repeat_literal` below.
    let Some(output) = run_program(
        "fn main() {\n\
                 let v = vec![10, 20, 30];\n\
                 let mut total = 0;\n\
                 for x in v { total = total + x; }\n\
                 println(total);\n\
                 println(v.len());\n\
             }",
    ) else {
        return;
    };
    assert_eq!(output, "60\n3\n");
}

#[test]
fn test_e2e_vec_insert() {
    // `Vec[T].insert(idx, value)` — grow if full, memmove the [idx..len]
    // tail right by one, store at idx, len++. `idx == len` appends. Covers
    // front/middle/end inserts on a scalar Vec, and a heap (String) element
    // whose buffer MOVES into the container (the `insert` arm carries push's
    // ownership-suppression set — a leak-clean churn is
    // tests/memory_sanitizer.rs::asan_vec_insert_heap_no_double_free).
    // Interpreter parity: tests/interpreter.rs::test_vec_insert_scalar_and_heap.
    let Some(output) = run_program(
        "fn main() {\n\
                 let mut v: Vec[i64] = [1, 2, 4, 5];\n\
                 v.insert(2, 3i64);\n\
                 v.insert(0, 0i64);\n\
                 v.insert(6, 6i64);\n\
                 let mut i = 0;\n\
                 while i < v.len() { print(f\"{v[i]}\"); i = i + 1; }\n\
                 println(\"\");\n\
                 let mut s: Vec[String] = Vec.new();\n\
                 s.push(f\"a\");\n\
                 s.push(f\"c\");\n\
                 let mid = f\"b-{1}\";\n\
                 s.insert(1, mid);\n\
                 println(s[1]);\n\
             }",
    ) else {
        return;
    };
    assert_eq!(output, "0123456\nb-1\n");
}

#[test]
fn test_e2e_vec_repeat_literal() {
    // `Vec[v; n]` / `vec![v; n]` build a heap Vec[T] of n copies of v via
    // the shared `build_vec_filled` (malloc + runtime fill loop) — the same
    // path as `Vec.filled(n, v)`. Covers: literal count, runtime count, the
    // `vec!` macro spelling, push-after-fill (cap==len ⇒ first push grows),
    // and indexing the filled element. This is the codegen tail of the
    // phase-4-interpreter.md `Vec[v; n]` repeat-literal item.
    let Some(output) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = Vec[7; 4];\n\
                 let mut sum = 0;\n\
                 for x in v { sum = sum + x; }\n\
                 println(sum);\n\
                 println(v.len());\n\
                 let m = vec![3; 5];\n\
                 println(m.len());\n\
                 let n = 6;\n\
                 let r: Vec[i64] = Vec[2; n];\n\
                 println(r.len());\n\
                 let mut g: Vec[i64] = Vec[0; 3];\n\
                 g.push(99);\n\
                 println(g.len());\n\
                 println(g[3]);\n\
             }",
    ) else {
        return;
    };
    assert_eq!(output, "28\n4\n5\n6\n4\n99\n");
}

#[test]
fn test_e2e_vec_get_first_option_ref_t_reread() {
    // Vec/Slice `get`/`first`/`last` return `Option[ref T]`
    // (B-2026-06-07-5 Option[ref T] slice). The `Some(x)` binding is an
    // immutable borrow → Copy, so it is freely RE-READABLE
    // (`println(w); println(w.len())` — no use-after-move, which the
    // pre-flip owned `Option[T]` accessor rejected). Exercises a scalar
    // `get`, a `String` `first` (the full 3-word value reconstructed from
    // the peeled `ref` binding type), and an out-of-bounds `None`.
    let out = run_program(
        r#"
fn main() {
    let v = [10, 20, 30];
    match v.get(1) {
        Some(x) => println(x),
        None => println("none"),
    };
    let words = ["alpha", "beta", "gamma"];
    match words.first() {
        Some(w) => { println(w); println(w.len()); }
        None => println("empty"),
    };
    match v.get(99) {
        Some(x) => println(x),
        None => println("oob"),
    };
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["20", "alpha", "5", "oob"]);
    }
}

#[test]
fn test_e2e_vec_tuple_heap_element_push_clone() {
    // B-2026-06-10-5: `Vec[(i64, String)]` — pushing tuples with heap
    // String fields, reading them back, and `.clone()`-ing the Vec. This
    // pins OUTPUT correctness for the push-read and deep-clone shapes (the
    // memory-safety side — no UAF/double-free/leak — is pinned by
    // `tests/memory_sanitizer.rs::asan_vec_tuple_heap_element_*` + the O0
    // `leaks` check). Before the fix the inline f-string field was freed
    // right after the push, so a read printed garbage / crashed.
    let out = run_program(
        r#"
fn main() {
    let mut src: Vec[(i64, String)] = Vec.new();
    src.push((1i64, f"p{1}"));
    src.push((2i64, f"q{2}"));
    let c = src.clone();
    println(c[0].1);
    println(c[1].1);
    println(src[0].1);
    println(src.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["p1", "q2", "p1", "2"]);
    }
}

/// B-2026-06-21-3: the same, un-annotated, from a `Vec[Fn(..)]` element.
#[test]
fn fn_value_vec_element_unannotated_extraction() {
    let out = run_program(
        "fn doubler(n: i64) -> i64 { n * 2i64 }\n\
             fn main() {\n\
                 let mut v: Vec[Fn(i64) -> i64] = Vec.new();\n\
                 v.push(doubler);\n\
                 let g = v[0];\n\
                 println(f\"{g(21i64)}\");\n\
             }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// B-2026-06-21-2: a fn value stored in a `Vec[Fn(...)]` element, extracted
/// with an explicit annotation and called.
#[test]
fn fn_value_vec_element_annotated_extraction() {
    let out = run_program(
        "fn doubler(n: i64) -> i64 { n * 2i64 }\n\
             fn main() {\n\
                 let mut v: Vec[Fn(i64) -> i64] = Vec.new();\n\
                 v.push(doubler);\n\
                 let g: Fn(i64) -> i64 = v[0];\n\
                 println(f\"{g(21i64)}\");\n\
             }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

#[test]
fn e2e_fn_value_with_a_ref_vec_param_builds_and_reads() {
    // B-2026-08-05-15: `let f = g` where `g` takes `ref Vec[u8]`. The
    // indirect call used to push the `{ptr, i64, i64}` Vec triple into the
    // signature's `ptr` slot, which LLVM's verifier rejects — so this would
    // not BUILD, while `karac check` passed and the interpreter ran it.
    let out = run_program(
        "fn head(v: ref Vec[u8], i: i64) -> i64 {\n\
                 return v[i] as i64;\n\
             }\n\
             fn main() {\n\
                 let mut v: Vec[u8] = Vec.new();\n\
                 let mut k = 0i64;\n\
                 while k < 6i64 { v.push((k * 3i64) as u8); k = k + 1i64; }\n\
                 let f = head;\n\
                 let mut acc = 0i64;\n\
                 let mut i = 0i64;\n\
                 while i < 6i64 { acc = acc + f(v, i); i = i + 1i64; }\n\
                 println(acc);\n\
             }",
    );
    assert_eq!(out, Some("45\n".to_string()));
}

#[test]
fn e2e_fn_value_with_a_mut_ref_vec_param_mutates_the_callers_vec() {
    // The `mut ref` leg: the callee must receive the caller's buffer, not a
    // copy of its header, or the pushes land nowhere observable.
    let out = run_program(
        "fn bump(v: mut ref Vec[i64], x: i64) { v.push(x); }\n\
             fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 let f = bump;\n\
                 let mut i = 0i64;\n\
                 while i < 5i64 { f(mut v, i * 2i64); i = i + 1i64; }\n\
                 let mut s = 0i64;\n\
                 let mut j = 0i64;\n\
                 while j < v.len() { s = s + v[j]; j = j + 1i64; }\n\
                 println(s);\n\
             }",
    );
    assert_eq!(out, Some("20\n".to_string()));
}

#[test]
fn test_e2e_bug7_shared_struct_inserted_and_returned_mut_ref_map() {
    // Repro A from the bug report — mut-ref-Map shape.  The helper
    // inserts `n` into a caller-owned `Map[i64, SharedStruct]` then
    // returns `n` itself.  Before the fix this printed `2` (silent
    // data corruption); after the fix it prints 42.
    let out = run_program(
        r#"
shared struct Node { val: i64 }
fn helper(visited: mut ref Map[i64, Node]) -> Node {
    let n = Node { val: 42 };
    let _ = visited.insert(1_i64, n);
    n
}
fn main() {
    let mut m: Map[i64, Node] = Map.new();
    let r = helper(mut m);
    println(r.val);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_bug7_shared_struct_inserted_and_returned_owned_map() {
    // Repro B from the bug report — helper-owned-Map shape.  The
    // helper allocates its own `Map[i64, SharedStruct]`, inserts
    // `n`, then returns `n`.  Before the fix this hung at high CPU
    // (the freed `n` allocation gets reused as part of the map's
    // bucket array, and the caller's `rc_inc` loops against that
    // memory); after the fix it prints 42 promptly.
    let out = run_program(
        r#"
shared struct Node { val: i64 }
fn helper() -> Node {
    let mut visited: Map[i64, Node] = Map.new();
    let n = Node { val: 42 };
    let _ = visited.insert(1_i64, n);
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
fn test_e2e_bug7_vec_shared_struct_push_and_return() {
    // Sibling case to Repro A/B: `Vec[SharedStruct]` rather than
    // `Map[K, SharedStruct]`.  The Vec.push site already calls the
    // shared cleanup-suppression helper, so the same fix carries
    // through — return value reads 42, not garbage.
    let out = run_program(
        r#"
shared struct Node { val: i64 }
fn helper() -> Node {
    let mut v: Vec[Node] = Vec.new();
    let n = Node { val: 42 };
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
fn test_e2e_shared_list_build_remove_repeat() {
    // Regression for the `shared struct` RC inc/dec discipline bugs
    // (2026-05-30): (1) the tail-cursor list builder (`let mut tail =
    // head; … tail.next = Some(node); tail = node;`) double-inc'd the
    // head (let-copy receive-inc + move-suppression transfer-inc) → the
    // whole chain leaked (RSS ∝ iterations); (2) `remove_nth_from_end`
    // returning `dummy.next` (which aliases the `head` param) failed to
    // transfer a ref to the caller's binding because the `Option[shared]`
    // struct-field capture (`ListNode { val: 0, next: head }`) and the
    // `Option[shared]` Identifier-tail return weren't inc-counted → the
    // caller's binding shared the source's single ref and the second
    // scope-exit dec drove it negative (double-free; crash on the 2nd
    // call, masked at O2).
    //
    // The loop builds a fresh 8-node list and removes a rotating
    // position every iteration — exercising build + in-place splice +
    // drop K times. Pre-fix this leaked unboundedly and/or SIGBUS'd on
    // the 2nd iteration; the deterministic sink confirms correctness
    // across repeated calls. Sink: head value of each result summed.
    // Removing position n (1..=8) of [1..8] keeps the head (value 1)
    // except when n == 8 (head removed → new head value 2). Over K=64
    // iters, n == 8 on 1/8 → 8 of them: 56*1 + 8*2 = 72.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn from_array(arr: Slice[i64]) -> Option[ListNode] {
    let n = arr.len();
    if n == 0 { return None; }
    let head = ListNode { val: arr[0], next: None };
    let mut tail = head;
    for i in 1..n {
        let node = ListNode { val: arr[i], next: None };
        tail.next = Some(node);
        tail = node;
    }
    Some(head)
}
fn remove_nth_from_end(head: Option[ListNode], n: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: head };
    let mut fast = head;
    let mut i = 0i64;
    while i < n {
        if let Some(node) = fast { fast = node.next; }
        i = i + 1i64;
    }
    let mut slow = dummy;
    loop {
        match fast {
            Some(node) => { fast = node.next; if let Some(s) = slow.next { slow = s; } }
            None => break,
        }
    }
    if let Some(target) = slow.next { slow.next = target.next; }
    dummy.next
}
fn head_val(list: Option[ListNode]) -> i64 {
    match list { Some(node) => node.val, None => 0i64 }
}
fn main() {
    let data: Array[i64, 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 64i64 {
        let list = from_array(data);
        let n: i64 = (k % 8i64) + 1i64;
        let out = remove_nth_from_end(list, n);
        sum = sum + head_val(out);
        k = k + 1i64;
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "72");
    }
}

#[test]
fn test_e2e_vec_method_on_ref_returning_call_receiver() {
    // B-2026-07-29-12: a Vec method whose RECEIVER is a borrow-returning
    // user accessor — `h.view().is_empty()` where `view() -> ref Vec[i64]`.
    // The receiver classifier is identifier-keyed, so a call-result
    // receiver had no name to look up and the build died with "no handler
    // for method 'is_empty' on non-identifier receiver". Binding first
    // (`let v = h.view(); v.is_empty()`) always worked, so the two
    // spellings disagreed — the interpreter ran both.
    //
    // The `.to_string()` variant is included because it did NOT fail
    // loudly: it built and printed an EMPTY line instead of `true`, a
    // silent wrong answer that the loud f-string form masked.
    let output = run_program(
        "struct H { items: Vec[i64] }\n\
             impl H {\n\
                 fn view(ref self) -> ref Vec[i64] { self.items }\n\
             }\n\
             fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(3);\n\
                 v.push(9);\n\
                 let h = H { items: v };\n\
                 println(h.view().is_empty().to_string());\n\
                 println(f\"{h.view().is_empty()}\");\n\
                 println(h.view().len().to_string());\n\
                 println(h.view().contains(9).to_string());\n\
                 let e = H { items: Vec.new() };\n\
                 println(e.view().is_empty().to_string());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "false\nfalse\n2\ntrue\ntrue\n");
}

/// B-2026-08-25-3 — THE STRUCTURAL GUARD for
/// `vec_mutation_methods_bounds_check_out_of_range_index`.
///
/// That test proves the check FIRED on one execution. It cannot distinguish
/// "the check is present" from "the check is absent but this run happened
/// not to expose it" — which is exactly why an intermittent green on it was
/// alarming enough to open B-2026-08-25-3 at high priority: a guard that
/// sometimes passes lets CI go green while a heap-corruption bug is live.
/// Sampling cannot fix that property (30,000 clean compile+runs on arm64
/// bounded the rate and settled nothing). Asserting on the EMITTED CODE can.
///
/// This asserts the panic path for each of the three methods survives into
/// the object file, i.e. past BOTH codegen and the LLVM pipeline — an
/// IR-only assertion would miss a backend that deleted the branch later.
///
/// THE INDEX MUST BE OPAQUE, and this is the whole subtlety. LLVM removing
/// the check when it can PROVE the index in range is CORRECT optimization,
/// not a bug, so a literal index would assert against legitimate behaviour.
/// `env.args().len()` is the codebase's opacity idiom (a stable 1 at run
/// time, unknowable at compile time). Measured while writing this: with a
/// literal `0` all three messages correctly VANISH from the binary.
///
/// ONE PROGRAM PER METHOD, also load-bearing. Chaining them lets the
/// compiler carry a range fact across calls — a combined program had
/// `remove`'s check correctly folded away, because a preceding successful
/// `insert(i, _)` proves `i <= len` and so `i < len+1`.
///
/// Runs on the OBJECT, so it needs no linker and no runtime archive: unlike
/// the E2E guard it has no soft-skip path and cannot report a vacuous green.
#[test]
fn vec_mutation_bounds_checks_survive_into_emitted_object() {
    use karac::codegen::compile_to_object_with_options;

    // (method, call form, panic message)
    let cases = [
        (
            "insert",
            "v.insert(i, 9); println(v.len());",
            "Vec.insert index out of bounds",
        ),
        (
            "remove",
            "let x = v.remove(i); println(x);",
            "Vec.remove index out of bounds",
        ),
        (
            "swap_remove",
            "let x = v.swap_remove(i); println(x);",
            "Vec.swap_remove index out of bounds",
        ),
    ];

    let emit = |body: &str, tag: &str| -> Vec<u8> {
        let src = format!(
            "fn main() {{\n\
                 \x20   let mut v: Vec[i64] = Vec.new();\n\
                 \x20   v.push(11);\n\
                 \x20   v.push(22);\n\
                 \x20   let i: i64 = env.args().len() as i64;\n\
                 \x20   {body}\n\
                 }}"
        );
        let mut parsed = karac::parse(&src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        karac::prepare_for_resolve(&mut parsed.program);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        super::common::assert_check_clean(&resolved, &typed, &src);
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        let obj = format!("/tmp/karac_boundsguard_{}_{}.o", std::process::id(), tag);
        let _ = std::fs::remove_file(&obj);
        compile_to_object_with_options(&parsed.program, &obj, Some(&ownership), None, None, None)
            .unwrap_or_else(|e| panic!("codegen failed for {tag}: {e}"));
        let bytes = std::fs::read(&obj)
            .unwrap_or_else(|e| panic!("emitted object for {tag} unreadable: {e}"));
        let _ = std::fs::remove_file(&obj);
        assert!(!bytes.is_empty(), "emitted object for {tag} was empty");
        bytes
    };

    let contains = |hay: &[u8], needle: &str| -> bool {
        hay.windows(needle.len()).any(|w| w == needle.as_bytes())
    };

    // ANTI-VACUITY: a program that never calls these methods must carry
    // none of the three messages. Without this, a match on some unrelated
    // always-present string would make every assertion below trivially true.
    let baseline = emit("println(v.len());", "baseline");
    for (method, _, msg) in cases {
        assert!(
            !contains(&baseline, msg),
            "anti-vacuity failed: {msg:?} appears in an object whose program \
                 never calls Vec.{method} — the search is matching something \
                 unrelated, so the assertions below prove nothing"
        );
    }

    for (method, call, msg) in cases {
        let bytes = emit(call, method);
        assert!(
            contains(&bytes, msg),
            "Vec.{method}'s bounds check did NOT survive into the emitted \
                 object: {msg:?} is absent. The index is `env.args().len()`, \
                 which is opaque at compile time, so the check cannot be \
                 legitimately optimised away — its absence means an \
                 out-of-range {method} would corrupt the heap (B-2026-08-24-15)."
        );
    }
}

/// B-2026-08-25-3 — the RUNTIME half of the bounds check, which neither
/// sibling guard actually reaches.
///
/// `vec_mutation_methods_bounds_check_out_of_range_index` uses LITERAL
/// indices on a `Vec` whose length is provable, so LLVM settles the
/// comparison at COMPILE time. Measured: `v.insert(7i64, …)` emits an
/// unconditional panic and DELETES the `println("survived")` fall-through —
/// the string is absent from the binary — while `v.insert(1i64, …)` drops
/// the panic message entirely. Its `!stdout.contains("survived")` assertion
/// is therefore discharged by dead-code elimination rather than by a check
/// firing. This surfaced as two different out-of-range literals compiling to
/// BYTE-IDENTICAL binaries (`insert(7)` and `insert(3)`, same SHA-256).
///
/// `vec_mutation_bounds_checks_survive_into_emitted_object` keeps the index
/// opaque, but asserts only that the message is PRESENT in the object, and
/// its index (`env.args().len()`, a stable 1) is IN RANGE — so it never
/// fires either.
///
/// So nothing covered the case real code actually hits: an index computed at
/// run time that turns out to be out of range. `env.args().len() + 6` is
/// opaque to the optimiser and equals 7 against a length of 2, so the check
/// must survive to the binary AND fire when it runs.
#[test]
fn vec_mutation_bounds_check_fires_for_a_runtime_opaque_index() {
    let cases = [
        ("v.insert(i, 9i64);", "Vec.insert index out of bounds"),
        (
            "let x = v.remove(i); println(x);",
            "Vec.remove index out of bounds",
        ),
        (
            "let x = v.swap_remove(i); println(x);",
            "Vec.swap_remove index out of bounds",
        ),
    ];

    for (call, want) in cases {
        let src = format!(
            "fn main() {{\n\
                 \x20   let mut v: Vec[i64] = Vec.new();\n\
                 \x20   v.push(11i64);\n\
                 \x20   v.push(22i64);\n\
                 \x20   let i: i64 = (env.args().len() as i64) + 6i64;\n\
                 \x20   {call}\n\
                 \x20   println(\"survived\");\n\
                 }}"
        );
        let run = match run_program_capturing(&src) {
            Some(r) => r,
            None => return, // no runtime archive / linker — harness skip
        };
        assert!(
            run.stderr.contains(want),
            "a RUN-TIME out-of-range index must panic with {want:?}; got \
                 stdout={:?} stderr={:?}",
            run.stdout,
            run.stderr
        );
        assert!(
            !run.stdout.contains("survived"),
            "{call} fell through to the next statement; got stdout={:?}",
            run.stdout
        );
        assert_eq!(
            run.status.code(),
            Some(101),
            "{call} must exit via panic (101), not abort or success; \
                 stderr={:?}",
            run.stderr
        );
    }
}

/// B-2026-08-25-20 — the `Vec` capacity family, twinned against the
/// interpreter rather than pinned to a string.
///
/// TWINNING IS THE POINT HERE, and it is also why every capacity assertion
/// inside the program is a `>=`. `capacity()` is a lower-bound query, not
/// an allocator promise: the interpreter grows a host `Vec<Value>` on
/// Rust's policy and codegen grows on `max(4, cap * 2)`, so the two
/// legitimately report different numbers. What both owe is the CONTRACT —
/// `capacity() >= len()`, and `>= len() + n` after `reserve(n)` — plus
/// identical observable element content, which is what this compares.
#[test]
fn e2e_vec_capacity_family_agrees_with_the_interpreter() {
    let cases: &[(&str, &str)] = &[
        (
            "reserve keeps len and grows cap",
            "let mut v: Vec[i64] = Vec.new();\n\
                 v.reserve(100);\n\
                 println(f\"{v.len()} {v.capacity() >= 100}\");\n\
                 let mut i = 0;\n\
                 while i < 100 { v.push(i); i = i + 1; }\n\
                 println(f\"{v.len()} {v[0]} {v[99]}\");",
        ),
        (
            "reserve_exact and non-positive no-ops",
            "let mut v: Vec[i64] = Vec.new();\n\
                 v.push(1);\n\
                 v.reserve_exact(7);\n\
                 println(f\"{v.capacity() >= 8} {v.len()}\");\n\
                 v.reserve(-5);\n\
                 v.reserve(0);\n\
                 println(f\"{v.len()} {v[0]}\");",
        ),
        (
            "resize grows with copies and shrinks like truncate",
            "let mut v: Vec[i64] = Vec.new();\n\
                 v.push(1); v.push(2); v.push(3);\n\
                 v.resize(5, 9);\n\
                 println(f\"{v.len()} {v[3]} {v[4]}\");\n\
                 v.resize(2, 0);\n\
                 println(f\"{v.len()} {v[0]} {v[1]}\");\n\
                 v.resize(-3, 0);\n\
                 println(v.len());",
        ),
        (
            "resize deep-copies a heap fill value",
            "let mut v: Vec[String] = Vec.new();\n\
                 v.push(\"a\"); v.push(\"b\");\n\
                 v.resize(5, \"z\");\n\
                 println(f\"{v.len()} {v[2]} {v[3]} {v[4]}\");\n\
                 v.resize(1, \"q\");\n\
                 println(f\"{v.len()} {v[0]}\");",
        ),
        (
            "append moves an owned source",
            "let mut a: Vec[i64] = Vec.new();\n\
                 a.push(10); a.push(20);\n\
                 let mut b: Vec[i64] = Vec.new();\n\
                 b.push(30); b.push(40);\n\
                 a.append(b);\n\
                 println(f\"{a.len()} {a[0]} {a[3]}\");",
        ),
        (
            "append of heap elements, including into an empty vec",
            "let mut p: Vec[String] = Vec.new();\n\
                 p.push(\"one\"); p.push(\"two\");\n\
                 let mut q: Vec[String] = Vec.new();\n\
                 q.push(\"three\");\n\
                 p.append(q);\n\
                 println(f\"{p.len()} {p[0]} {p[2]}\");\n\
                 let mut e: Vec[String] = Vec.new();\n\
                 let mut f2: Vec[String] = Vec.new();\n\
                 f2.push(\"solo\");\n\
                 e.append(f2);\n\
                 println(f\"{e.len()} {e[0]}\");",
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
            assert_eq!(
                aot, expected,
                "{label}: AOT and the interpreter disagree on the Vec \
                     capacity family"
            );
        }
    }
}

/// `try_resize` / `try_append` end to end. Both are all-or-nothing: the
/// fallible allocation happens before anything is mutated, so an `Err`
/// leaves the receiver's buffer, length and contents untouched — and for
/// `try_append`, leaves the SOURCE still owning its elements, which is why
/// its cleanup is disarmed only on the success path.
#[test]
fn e2e_vec_try_resize_and_try_append_agree_with_the_interpreter() {
    let src = "fn build() -> Result[i64, AllocError] {\n\
                       let mut v: Vec[i64] = Vec.new();\n\
                       v.push(1); v.push(2); v.push(3);\n\
                       v.try_resize(6, 7)?;\n\
                       println(f\"grow {v.len()} {v[5]}\");\n\
                       v.try_resize(2, 0)?;\n\
                       println(f\"shrink {v.len()}\");\n\
                       let mut a: Vec[i64] = Vec.new();\n\
                       a.push(10);\n\
                       let mut c: Vec[i64] = Vec.new();\n\
                       c.push(30);\n\
                       a.try_append(c)?;\n\
                       println(f\"append {a.len()} {a[1]}\");\n\
                       let mut p: Vec[String] = Vec.new();\n\
                       p.push(\"one\");\n\
                       p.try_resize(3, \"pad\")?;\n\
                       let mut q: Vec[String] = Vec.new();\n\
                       q.push(\"tail\");\n\
                       p.try_append(q)?;\n\
                       println(f\"heap {p.len()} {p[2]} {p[3]}\");\n\
                       return Ok(p.len());\n\
                   }\n\
                   fn main() {\n\
                       match build() {\n\
                           Ok(n) => { println(f\"ok {n}\"); }\n\
                           Err(e) => { println(\"alloc failed\"); }\n\
                       }\n\
                   }";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errored: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    assert_eq!(
        expected,
        "grow 6 7\nshrink 2\nappend 2 30\nheap 4 pad tail\nok 4\n"
    );
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, expected, "AOT and the interpreter disagree");
    }
}

/// The `try_*` half. The row's headline was three missing `try_*` methods;
/// the actual missing half was the PANICKING base each one derives from
/// (`fallible_alloc.rs` recurses into the base for argument validation and
/// return-type synthesis), so this is what proves the derivation closed.
#[test]
fn e2e_vec_try_reserve_companions_agree_with_the_interpreter() {
    let src = "fn build() -> Result[bool, AllocError] {\n\
                       let mut v: Vec[i64] = Vec.new();\n\
                       v.try_reserve(64)?;\n\
                       let mut w: Vec[i64] = Vec.new();\n\
                       w.try_reserve_exact(9)?;\n\
                       v.try_reserve(0)?;\n\
                       return Ok(v.capacity() >= 64 and w.capacity() >= 9);\n\
                   }\n\
                   fn main() {\n\
                       match build() {\n\
                           Ok(n) => { println(f\"ok {n}\"); }\n\
                           Err(e) => { println(\"alloc failed\"); }\n\
                       }\n\
                   }";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errored: {interp_errs:?}"
    );
    assert_eq!(interp_out.join(""), "ok true\n");
    if let Some(aot) = run_program(src) {
        assert_eq!(
            aot, "ok true\n",
            "the fallible reserve companions must lower to real \
                 `karac_alloc_fallible` + `Result`, not diverge from the \
                 interpreter"
        );
    }
}

// ── Vec[T] growable arrays ────────────────────────────────────

#[test]
fn test_ir_vec_param_type() {
    let ir = ir_for("fn take(v: Vec[i64]) { }");
    // Vec[T] lowers to { ptr, i64, i64 }.
    assert!(
        ir.contains("{ ptr, i64, i64 }"),
        "expected {{ ptr, i64, i64 }} struct for Vec param, got:\n{}",
        ir
    );
}

#[test]
fn test_ir_vec_new() {
    let ir = ir_for("fn main() { let v: Vec[i64] = Vec.new(); }");
    // Vec::new() produces { null, 0, 0 } stored into an alloca.
    assert!(
        ir.contains("{ ptr, i64, i64 }"),
        "expected Vec struct type, got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_vec_push_len() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    v.push(30);
    println(v.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn test_e2e_vec_push_many() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 10 {
        v.push(i);
        i = i + 1;
    }
    println(v.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10");
    }
}

#[test]
fn test_e2e_vec_index() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(100);
    v.push(200);
    v.push(300);
    println(v[0]);
    println(v[1]);
    println(v[2]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["100", "200", "300"]);
    }
}

// Regression test for the plain-struct `v[i].field` codegen gap
// surfaced during the SoA work (2026-05-29). `compile_field_access`'s
// Index-receiver branch handled shared structs and (then-new) SoA
// vars, but not plain owned `Vec[Struct]` — for non-first fields it
// fell through to the generic struct-field path which returns the
// `i64 0` placeholder, so `entities[i].y` silently produced 0 even
// though the corresponding `let e = entities[i]; e.y` worked. Pins
// the fix in `src/codegen/expr_ops.rs` so the gap can't reopen.
#[test]
fn test_e2e_vec_struct_indexed_field_access() {
    let out = run_program(
        r#"
struct Entity { x: i64, y: i64, vx: i64, vy: i64 }
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    let mut i: i64 = 0;
    while i < 6 {
        entities.push(Entity { x: i, y: i * 10, vx: i * 100, vy: i * 1000 });
        i = i + 1;
    }
    println(entities[0].x);
    println(entities[4].y);
    println(entities[5].vx);
    println(entities[5].vy);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "40", "500", "5000"]);
    }
}

#[test]
fn test_e2e_vec_pop_returns_option() {
    // `Vec.pop` now returns `Option[T]` per design.md (was raw
    // element pre-2026-05-10). Match destructure unwraps Some,
    // None on empty. Previous test asserted raw shape; the
    // semantic upgrade aligns codegen with the spec + interpreter.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    match v.pop() {
        Some(x) => println(x),
        None => println(0),
    }
    println(v.len());
    match v.pop() {
        Some(x) => println(x),
        None => println(0),
    }
    match v.pop() {
        Some(_) => println(99),
        None => println(0),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["20", "1", "10", "0"]);
    }
}

#[test]
fn test_e2e_vec_clear_and_extend() {
    // `Vec[T].clear()` empties the Vec (drop-all-elements + reset header)
    // and it stays usable; `extend(other)` appends clones. Matches the
    // interpreter oracle (`test_vec_clear_and_extend`); the heap-element
    // leak-freedom is gated in `tests/memory_sanitizer.rs`.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.clear();
    println(v.len());
    v.push(9);
    let mut w: Vec[i64] = Vec.new();
    w.push(7);
    w.push(8);
    v.extend(w);
    println(v.len());
    println(v[0]);
    println(v[1]);
    println(v[2]);

    let mut s: Vec[String] = Vec.new();
    s.push("alpha");
    s.push("beta");
    s.clear();
    println(s.len());
    s.push("gamma");
    println(s[0]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "0\n3\n9\n7\n8\n0\ngamma\n");
    }
}

#[test]
fn test_e2e_vec_deque_pop_front_returns_option_with_tuple_payload() {
    // The LeetCode 3629 kata's blocking shape: VecDeque[(i64, i64)]
    // BFS frontier with `pop_front()` returning `Option[(i64,i64)]`.
    // Multi-word Option payload via the bumped layout +
    // `coerce_to_payload_words(val, 3)` construction; destructure
    // uses the direct-pattern form `Some((i, d))` which routes
    // through the existing tuple-payload reconstruction machinery.
    let out = run_program(
        r#"
fn main() {
    let mut q: VecDeque[(i64, i64)] = VecDeque.new();
    q.push_back((0, 0));
    let mut sum = 0i64;
    loop {
        match q.pop_front() {
            None => { break; },
            Some((i, d)) => {
                sum = sum + i + d;
                if i < 3 {
                    q.push_back((i + 1, d + 1));
                }
            },
        }
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        // BFS: pop (0,0) sum+=0; push (1,1) → pop sum+=2 (2);
        // push (2,2) → pop sum+=4 (6); push (3,3) → pop sum+=6 (12);
        // i==3 no push; empty → break. Total 12.
        assert_eq!(out.trim(), "12");
    }
}

#[test]
fn test_e2e_function_returning_vec_of_vec_no_double_free() {
    // Move-aware scope-exit cleanup: when a function returns a
    // tracked Vec / String binding via the tail expression, the
    // let-site's `track_vec_var` cleanup is suppressed (by
    // zeroing the source's `cap` field) so the caller's
    // `f.data` isn't pointing at a freed buffer. Without this,
    // `Vec[Vec[i64]]` returns SIGSEGV at the first inner indexed
    // access (the inner Vec's data pointer GEPs through a freed
    // outer slot).
    let out = run_program(
        r#"
fn make_grid(n: i64) -> Vec[Vec[i64]] {
    let mut g: Vec[Vec[i64]] = Vec.filled(n, Vec.new());
    g[0].push(99);
    g[0].push(11);
    g[2].push(42);
    g
}
fn main() {
    let f: Vec[Vec[i64]] = make_grid(3);
    println(f.len());
    println(f[0].len());
    println(f[0][0]);
    println(f[0][1]);
    println(f[2][0]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n2\n99\n11\n42");
    }
}

#[test]
fn test_e2e_nested_indexed_read_vec_of_vec() {
    // `grid[0][0]` — nested indexed read on `Vec[Vec[i64]]`.
    // Codegen synthesizes a fresh identifier for the inner
    // `grid[0]` (pointing into grid's storage) and re-dispatches
    // the outer index through the existing identifier-keyed path.
    let out = run_program(
        r#"
fn main() {
    let mut grid: Vec[Vec[i64]] = Vec.filled(3, Vec.new());
    grid[0].push(99);
    grid[0].push(11);
    grid[2].push(42);
    let v = grid[0][0];
    let w = grid[0][1];
    let x = grid[2][0];
    println(v);
    println(w);
    println(x);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99\n11\n42");
    }
}

#[test]
fn test_e2e_match_bound_payload_vec_field_copy_over_ref_vec() {
    // B-2026-07-17-20: `for it in items { match it { Fu(f) => { let ps =
    // f.params; … } } }` over `items: ref Vec[It]` — copying a Vec field out
    // of a match-bound struct payload that aliases a borrowed ref-Vec enum
    // element. Pre-fix this AOT-crashed with `free(): double free` (the copy
    // shallow-aliased the container's buffer); now the field deep-copies.
    // Locks run == build parity on the exact repro.
    let out = run_program(
        r#"
struct P { n: i64 }
struct F { name: String, params: Vec[P] }
enum It { Fu(F), Other }
fn collect(items: ref Vec[It]) -> i64 {
    let mut total = 0;
    for it in items {
        match it {
            It.Fu(f) => {
                let ps = f.params;
                for p in ps { total = total + p.n; }
            }
            It.Other => {}
        }
    }
    total
}
fn main() {
    let mut items: Vec[It] = Vec.new();
    let mut ps1: Vec[P] = Vec.new();
    ps1.push(P { n: 1 });
    ps1.push(P { n: 2 });
    items.push(It.Fu(F { name: "a", params: ps1 }));
    items.push(It.Other);
    println(collect(items).to_string());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn test_e2e_match_bound_struct_variant_vec_field_reborrow_over_ref_vec() {
    // B-2026-07-18-4: a STRUCT-VARIANT enum payload's Vec field bound
    // DIRECTLY (`match it { Fu { params } => … }`, not through a struct
    // payload `f`), then whole-moved into a local (`let ps = params`), over
    // `items: ref Vec[It]`. `params` is typed `ref Vec[P]` (a borrow of the
    // container-owned buffer), so `ps` is a re-borrow. Pre-fix the binding
    // recorded none of the Vec dispatch tables — `ps.len()` build-failed and
    // `for p in ps` compiled to an EMPTY Vec (AOT sum 0 vs interp 9). The
    // typechecker now peels the borrow to register `ps`'s element type, and
    // codegen binds `ps` as an alias (no scope-exit free) so the container
    // stays the sole owner. Locks run == build parity and no double-free.
    let out = run_program(
        r#"
struct P { n: i64 }
enum It { Fu { params: Vec[P] }, Other }
fn collect(items: ref Vec[It]) -> i64 {
    let mut total = 0;
    for it in items {
        match it {
            It.Fu { params } => {
                let ps = params;
                for p in ps { total = total + p.n; }
            }
            It.Other => {}
        }
    }
    total
}
fn main() {
    let mut items: Vec[It] = Vec.new();
    let mut ps: Vec[P] = Vec.new();
    ps.push(P { n: 4 });
    ps.push(P { n: 5 });
    items.push(It.Fu { params: ps });
    items.push(It.Other);
    println(collect(items).to_string());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "9");
    }
}

#[test]
fn test_e2e_vec_deque_pop_back_returns_option() {
    // Sibling: pop_back on a primitive-element VecDeque returns
    // Option[i64] — same multi-word path, but the value only
    // populates w0 (w1/w2 padded with zeros).
    let out = run_program(
        r#"
fn main() {
    let mut q: VecDeque[i64] = VecDeque.new();
    q.push_back(1);
    q.push_back(2);
    q.push_back(3);
    match q.pop_back() {
        Some(x) => println(x),
        None => println(0),
    }
    match q.pop_back() {
        Some(x) => println(x),
        None => println(0),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n2");
    }
}

#[test]
fn test_e2e_vec_for_loop() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    let mut sum = 0;
    for x in v {
        sum = sum + x;
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "6");
    }
}

#[test]
fn test_ir_vec_filled_zero_uses_calloc_no_fill_loop() {
    // B-2026-07-08-7: a statically-zero fill of a trivially-copyable element
    // lowers to the `calloc`-backed `karac_alloc_zeroed_or_panic(count,size)`
    // — one lazily-zeroed allocation, matching rust's `vec![0; n]` — and
    // emits NO runtime fill loop (no `filled.body` block, no per-slot store).
    let ir = ir_for("fn main() { let v: Vec[i64] = Vec.filled(8, 0); println(v[0]); }");
    assert!(
        ir.contains("karac_alloc_zeroed_or_panic"),
        "zero fill should route through the calloc wrapper:\n{ir}"
    );
    assert!(
        !ir.contains("filled.body"),
        "zero fill must skip the runtime fill loop entirely:\n{ir}"
    );
}

#[test]
fn test_ir_vec_filled_empty_aggregate_uses_calloc() {
    // B-2026-08-01-32: an EMPTY-collection fill value (`Vec.filled(n,
    // Vec.new())` — the adjacency-list / bucket-table shape) is a
    // statically all-zero aggregate ({null,0,0}), and cloning an
    // all-zero collection handle is the identity — so it takes the same
    // calloc fast path as a scalar zero instead of store-looping (and
    // pre-fix per-element cloning) 24 bytes per slot (~10-100x on a
    // 1e6-element fill).
    let ir = ir_for(
        "fn main() { let g: Vec[Vec[i64]] = Vec.filled(8, Vec.new()); println(g[0].len()); }",
    );
    assert!(
        ir.contains("karac_alloc_zeroed_or_panic"),
        "empty-aggregate fill should route through the calloc wrapper:\n{ir}"
    );
    assert!(
        !ir.contains("filled.body"),
        "empty-aggregate fill must skip the runtime fill loop entirely:\n{ir}"
    );
}

#[test]
fn test_e2e_vec_filled_empty_vec_elements_independent() {
    // The calloc-backed empty-aggregate fill must produce INDEPENDENT
    // elements: pushing into one slot must not alias another, String
    // elements read back empty, and the scope-exit drops balance (the
    // ASAN suite pins the memory side).
    let Some(out) = run_program(
        r#"
fn main() {
    let mut grid: Vec[Vec[i64]] = Vec.filled(4, Vec.new());
    grid[0].push(11);
    grid[0].push(12);
    grid[2].push(33);
    println(f"g0 {grid[0].len()} {grid[0][0]} {grid[0][1]}");
    println(f"g1 {grid[1].len()} g2 {grid[2].len()} {grid[2][0]} g3 {grid[3].len()}");
    let names: Vec[String] = Vec.filled(3, String.new());
    println(f"s {names.len()} {names[0].len()}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "g0 2 11 12\ng1 0 g2 1 33 g3 0\ns 3 0\nend\n");
}

#[test]
fn test_ir_vec_filled_nonzero_keeps_fill_loop() {
    // Regression guard: the calloc fast path must fire ONLY for a
    // compile-time zero. A non-zero fill still takes the malloc + runtime
    // fill loop (LLVM memsets it downstream) — no zeroed-alloc symbol.
    let ir = ir_for("fn main() { let v: Vec[i64] = Vec.filled(8, 7); println(v[0]); }");
    assert!(
        !ir.contains("karac_alloc_zeroed_or_panic"),
        "non-zero fill must NOT use the calloc wrapper:\n{ir}"
    );
    assert!(
        ir.contains("filled.body"),
        "non-zero fill must keep the runtime fill loop:\n{ir}"
    );
}

#[test]
fn test_e2e_vec_filled_zero_all_zeroed() {
    // The calloc fast path must produce the same observable result as the
    // fill loop it replaces: every slot reads back zero, len == n.
    let out = run_program(
        r#"
fn main() {
    let v: Vec[i64] = Vec.filled(5, 0);
    println(v.len());
    let mut i = 0i64;
    let mut sum = 0i64;
    while i < 5 {
        sum = sum + v[i];
        i = i + 1;
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5\n0");
    }
}

#[test]
fn test_e2e_vec_filled_bool_with_indexed_write() {
    // Kata's `Vec.filled(n, false)` shape — followed by indexed
    // writes flipping selected slots true.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[bool] = Vec.filled(4, false);
    v[2] = true;
    let mut i = 0i64;
    while i < 4 {
        println(v[i]);
        i = i + 1;
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "false\nfalse\ntrue\nfalse");
    }
}

#[test]
fn test_e2e_vec_filled_nested_vec_independent_after_push() {
    // The kata's sieve init shape: `Vec.filled(n, Vec.new())`.
    // The per-slot bit-copy of the `Vec.new()` aggregate stores
    // `{null, 0, 0}` into each slot — pointers all start at
    // null, so the first `grid[i].push(...)` allocates a fresh
    // buffer per row (no aliasing). Equivalent to the
    // interpreter's deep-clone fix at `beb7310`, but achieved
    // structurally rather than via a clone helper because
    // empty Vec storage has no data pointer to alias.
    let out = run_program(
        r#"
fn main() {
    let mut grid: Vec[Vec[i64]] = Vec.filled(3, Vec.new());
    grid[0].push(99);
    println(grid[0].len());
    println(grid[1].len());
    println(grid[2].len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1\n0\n0");
    }
}

#[test]
fn test_e2e_vec_with_capacity_len_zero_then_push_fills() {
    // `Vec.with_capacity(N)` malloc's the buffer but reports
    // `len == 0`. Subsequent push N times fills it; observable
    // behavior matches `Vec.new()` plus reserve. The realloc-
    // free guarantee is a perf property and isn't directly
    // testable from kara code without IR inspection — this test
    // covers the value-level contract.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.with_capacity(5);
    println(v.len());
    let mut i = 0i64;
    while i < 5 {
        v.push(i * 10);
        i = i + 1;
    }
    println(v.len());
    println(v[0]);
    println(v[4]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0\n5\n0\n40");
    }
}

#[test]
fn test_e2e_vec_with_capacity_zero_push_grows() {
    // Degenerate `with_capacity(0)` — same shape as `Vec.new()`.
    // Push grows from there.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.with_capacity(0);
    println(v.len());
    v.push(42);
    println(v.len());
    println(v[0]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0\n1\n42");
    }
}

#[test]
fn test_e2e_vec_with_capacity_untyped_let_infers_from_push() {
    // No `: Vec[T]` annotation on the let — element type comes
    // from the downstream push via the typechecker arm in
    // expr_call.rs that returns `Vec[?T]` for
    // `Vec.with_capacity(n)`. Pre-fix this errored at codegen
    // with "element type unknown".
    let out = run_program(
        r#"
fn main() {
    let mut v = Vec.with_capacity(5);
    v.push(7);
    v.push(11);
    v.push(13);
    println(v.len());
    println(v[0]);
    println(v[2]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n7\n13");
    }
}

#[test]
fn test_e2e_vec_with_capacity_exceeds_grows_correctly() {
    // Push N+1 elements into a `with_capacity(N)` Vec — the
    // (N+1)-th push must trigger a grow and the final state
    // must be correct (no data corruption from the grow path
    // copying out of the malloc'd buffer).
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.with_capacity(2);
    v.push(10);
    v.push(20);
    v.push(30);
    println(v.len());
    println(v[0]);
    println(v[2]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n10\n30");
    }
}

#[test]
fn test_e2e_vec_deque_push_back_len_is_empty() {
    // VecDeque codegen v1 surface: `new` + `push_back` + `len` +
    // `is_empty` mirror Vec's `{ptr, len, cap}` layout exactly.
    let out = run_program(
        r#"
fn main() {
    let mut q: VecDeque[i64] = VecDeque.new();
    q.push_back(1);
    q.push_back(2);
    q.push_back(3);
    println(q.len());
    println(q.is_empty());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\nfalse");
    }
}

#[test]
fn test_e2e_vec_deque_pop_back_alias_of_pop_returns_option() {
    // `pop_back` shares the `pop` arm — both return `Option[T]`
    // after the 2026-05-10 Option-wrap upgrade. Match unwraps
    // Some; None on empty.
    let out = run_program(
        r#"
fn main() {
    let mut q: VecDeque[i64] = VecDeque.new();
    q.push_back(1);
    q.push_back(2);
    q.push_back(3);
    match q.pop_back() {
        Some(x) => println(x),
        None => println(0),
    }
    println(q.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n2");
    }
}

// ── Vec[T] indexed write (Slice Vb) ───────────────────────────

#[test]
fn test_e2e_vec_indexed_write_basic() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    v.push(30);
    v[1] = 99;
    println(v[0]);
    println(v[1]);
    println(v[2]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "99", "30"]);
    }
}

#[test]
fn test_e2e_vec_indexed_write_oob_panics() {
    let captured = run_program_capturing(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v[5] = 99;
    println(42);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("panic: vec index out of bounds"),
            "expected vec OOB panic, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("42"),
            "code after panicking index store should not run"
        );
    }
}

/// B-2026-08-14-32 — an index read into a heap-owning container, used as the
/// value of an `if` / `match` ARM, is cloned for the binding instead of
/// aliasing the container.
///
/// The `let` path already defended the DIRECT form (`let w = v[1]`) by
/// calling `clone_owned_vec_index_element` on its RHS, and elides that clone
/// when a pre-pass proves the read is a non-escaping borrow. Both halves
/// match `ExprKind::Index` at the TOP LEVEL, so the same read one level down
/// inside a branch got neither: no clone, and an owned scope cleanup anyway.
/// The binding and the container then freed one buffer, and
/// `let w = if c { v[1] } else { "x" }` aborted with
/// `free(): double free detected in tcache 2` before the next statement ran.
/// `--interp` was correct throughout, so this was compiled-only — hence the
/// interpreter twin of the same name.
///
/// Every arm shape that crashed is here: an index against a literal, against
/// another index, against an owned-returning CALL (the arm that must NOT be
/// cloned), a `match` arm, a nested if-expression, and a `Vec[Vec[i64]]` to
/// show it was never String-specific. The discarded `if` STATEMENT is the
/// leak edge — nothing owns its value, so its arms must not be cloned there.
/// The last line re-reads every element AFTER the bindings, which is what
/// proves the container still owns intact buffers rather than freed ones.
#[test]
fn test_e2e_if_arm_vec_element_binding_is_cloned() {
    assert_eq!(
            run_program(
                "fn fresh() -> String { \"zz\" }\n\
                 fn main() {\n\
                     let mut v: Vec[String] = Vec.new();\n\
                     v.push(\"aa\"); v.push(\"bb\"); v.push(\"cc\");\n\
                     let mut n: Vec[Vec[i64]] = Vec.new();\n\
                     n.push([1, 2]); n.push([3, 4]);\n\
                     let a = if v.len() > 1 { v[1] } else { \"x\" };\n\
                     let b = if v.len() > 1 { v[1] } else { v[0] };\n\
                     let c = if v.len() > 9 { v[0] } else { fresh() };\n\
                     let d = match v.len() > 1 { true => v[2], false => \"x\" };\n\
                     let e = if v.len() > 2 { if v.len() > 9 { v[0] } else { v[1] } } else { \"x\" };\n\
                     let g = if n.len() > 1 { n[1] } else { n[0] };\n\
                     if v.len() > 0 { v[0] } else { v[1] };\n\
                     println(f\"{a} {b} {c} {d} {e} {g[0]}\");\n\
                     println(f\"{v[0]} {v[1]} {v[2]} {n[1][1]}\");\n\
                 }"
            )
            .as_deref(),
            Some("bb bb zz cc bb 3\naa bb cc 4\n"),
        );
}

#[test]
fn test_e2e_panic_location_vec_oob() {
    let captured = run_program_capturing_with_filename(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v[5] = 99;
    println(42);
}
"#,
        "oob_demo.kara",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("panic at "),
            "expected rich `panic at` prefix, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            c.stderr.contains("oob_demo.kara:"),
            "expected source filename in panic location, got stderr={:?}",
            c.stderr
        );
        assert!(
            c.stderr.contains(" in main:"),
            "expected enclosing function name in panic location, got stderr={:?}",
            c.stderr
        );
        assert!(
            c.stderr.contains("vec index out of bounds"),
            "expected the panic message, got stderr={:?}",
            c.stderr
        );
        assert!(
            !c.stdout.contains("42"),
            "code after the panicking store must not run"
        );
    }
}

#[test]
fn test_e2e_vec_indexed_write_after_push() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(0);
    v.push(0);
    v[0] = 7;
    v[1] = 8;
    println(v[0] + v[1]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "15");
    }
}

#[test]
fn test_e2e_vec_indexed_write_through_mut_ref_param() {
    let out = run_program(
        r#"
fn set_at(v: mut ref Vec[i64], i: i64, x: i64) {
    v[i] = x;
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    set_at(mut v, 1_i64, 99_i64);
    println(v[0]);
    println(v[1]);
    println(v[2]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "99", "3"]);
    }
}

// ── Vec/Slice/Array indexed-receiver method dispatch (Slice Vc) ──

#[test]
fn test_e2e_indexed_receiver_inner_vec_len() {
    let out = run_program(
        r#"
fn main() {
    let mut outer: Vec[Vec[i64]] = Vec.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(1);
    a.push(2);
    a.push(3);
    outer.push(a);
    let mut b: Vec[i64] = Vec.new();
    b.push(10);
    outer.push(b);
    println(outer[0].len());
    println(outer[1].len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "1"]);
    }
}

#[test]
fn test_e2e_indexed_receiver_inner_vec_is_empty() {
    let out = run_program(
        r#"
fn main() {
    let mut outer: Vec[Vec[i64]] = Vec.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(7);
    outer.push(a);
    let b: Vec[i64] = Vec.new();
    outer.push(b);
    println(outer[0].is_empty());
    println(outer[1].is_empty());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["false", "true"]);
    }
}

#[test]
fn test_e2e_indexed_receiver_inner_vec_push() {
    // Headline regression gate — closes the LeetCode 3629 kata's primary
    // blocker (`factors[j].push(i)`). The push must write back through
    // the elem pointer aliasing the outer storage so subsequent reads of
    // `outer[0]` observe the new element.  We verify via len() and
    // for-loop element iteration since chained-index reads `outer[i][j]`
    // are out of scope for v1.
    let out = run_program(
        r#"
fn main() {
    let mut outer: Vec[Vec[i64]] = Vec.new();
    let a: Vec[i64] = Vec.new();
    outer.push(a);
    let b: Vec[i64] = Vec.new();
    outer.push(b);
    outer[0].push(42);
    outer[0].push(43);
    outer[1].push(99);
    println(outer[0].len());
    println(outer[1].len());
    let mut acc: i64 = 0;
    for inner in outer {
        for x in inner { acc = acc + x; }
    }
    println(acc);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // 42 + 43 + 99 = 184
        assert_eq!(lines, vec!["2", "1", "184"]);
    }
}

#[test]
fn test_e2e_field_receiver_plain_struct_vec_push() {
    // Slice FR (sibling to MR): `outer.field.method(...)` on a plain
    // struct must GEP into the slot's field, not extract-value.  The
    // push must write back through the field pointer aliasing the
    // parent slot so a subsequent `.len()` on the same field reads
    // the new count. Iteration over a FieldAccess source (`for x in
    // h.nums`) is a separate codegen path and out of scope for this
    // test.
    let out = run_program(
        r#"
struct Holder { nums: Vec[i64] }
fn main() {
    let mut h = Holder { nums: Vec.new() };
    h.nums.push(10);
    h.nums.push(20);
    h.nums.push(30);
    println(h.nums.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3"]);
    }
}

#[test]
fn test_e2e_field_receiver_shared_struct_vec_push() {
    // Headline regression gate — closes the LeetCode 133 kata's primary
    // codegen blocker (`curr_clone.neighbors.push(...)`) on a shared
    // struct.  The push must persist through the RC heap GEP so a
    // subsequent `.len()` on the same field returns the new count.
    let out = run_program(
        r#"
shared struct Bag { tag: i64, mut items: Vec[i64] }
fn main() {
    let b = Bag { tag: 7, items: Vec.new() };
    b.items.push(11);
    b.items.push(22);
    println(b.tag);
    println(b.items.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["7", "2"]);
    }
}

#[test]
fn test_e2e_nested_receiver_push_shared_struct_element() {
    // B-2026-07-11-11 sibling: the indexed element is a `shared struct`, so
    // the element slot holds an RC handle that must be loaded before the
    // field GEP. The push must persist through the heap GEP so a later read
    // sees it.
    let out = run_program(
        r#"
shared struct Node { mut kids: Vec[i64] }
struct Tree { nodes: Vec[Node] }
fn main() {
    let mut t = Tree { nodes: Vec.new() };
    t.nodes.push(Node { kids: Vec.new() });
    t.nodes[0].kids.push(7);
    t.nodes[0].kids.push(8);
    println(t.nodes[0].kids.len());
    println(t.nodes[0].kids[1]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "8"]);
    }
}

#[test]
fn test_e2e_field_receiver_indexed_inner_vec_push() {
    // Slice FR follow-up (2026-05-16): `outer[i].field.method(...)`
    // chained Index→FieldAccess→method dispatch. The inner Index
    // lowers to an element pointer via the same per-container
    // helper the MR-slice indexed-receiver arm uses; the field GEP
    // then hangs off the element pointer.  Closes the LeetCode 133
    // kata's inner-loop `nodes[i as u64].neighbors.push(nodes[j as
    // u64])` shape (which the bench `clone_bfs.kara` workload
    // depends on at construction time).
    let out = run_program(
        r#"
shared struct Node { val: i64, mut neighbors: Vec[i64] }
fn main() {
    let mut nodes: Vec[Node] = Vec.new();
    nodes.push(Node { val: 1, neighbors: Vec.new() });
    nodes.push(Node { val: 2, neighbors: Vec.new() });
    nodes[0_u64].neighbors.push(99);
    nodes[0_u64].neighbors.push(88);
    nodes[1_u64].neighbors.push(77);
    println(nodes[0_u64].neighbors.len());
    println(nodes[1_u64].neighbors.len());
    println(nodes[0_u64].val);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "1", "1"]);
    }
}

#[test]
fn test_ir_discarded_vec_temp_emits_free() {
    // General owned-temp tracking, slice 1
    // (docs/spikes/general-owned-temp-tracking.md): a fresh-owned
    // Vec/String produced in statement position (`make_vec();`) is a
    // discarded temporary with no binding to drop it. The owned-temp
    // chokepoint materializes it into an `__owned_tmp` slot and queues a
    // `FreeVecBuffer` that drains at the `;`. Archive-independent — proves
    // the leak is closed at the IR level (macOS ASAN has no LeakSanitizer,
    // so the leak direction can't be caught at runtime here). Regression
    // against the prior behaviour where the buffer leaked silently.
    let src = r#"
fn make_vec() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    return v;
}

fn main() {
    make_vec();
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__owned_tmp"),
        "expected discarded Vec temp materialized into __owned_tmp slot; got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.free"),
        "expected a FreeVecBuffer drain (cleanup.free block) for the discarded temp; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_scope_exit_vec_free_routes_through_free_buf() {
    // Large-buffer recycling (phase-10 allocator buffer recycling): the
    // scope-exit `FreeVecBuffer` drain releases a Vec's data buffer
    // through `karac_free_buf(data, bytes_hint)` — the runtime recycling
    // cache's entry point — not bare libc `free`. The hint is
    // `cap * elem_size` (the `freebuf.bytes` mul), so sub-MB frees
    // short-circuit to libc without a cache lock or allocator query.
    let src = r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(7_i64);
    println(f"{v.len()}");
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("karac_free_buf"),
        "expected the scope-exit Vec buffer free to route through karac_free_buf; got:\n{ir}"
    );
    assert!(
        ir.contains("freebuf.bytes"),
        "expected a cap * elem_size bytes hint (freebuf.bytes mul) at the free site; got:\n{ir}"
    );
}

#[test]
fn test_ir_vec_reassign_overwrite_free_routes_through_free_buf() {
    // The eager overwrite-free on `g = build(2)` (the swap-chain shape —
    // `grid = next` in the LBM katas) must also route through
    // `karac_free_buf`: this is the free that recycles a ping-ponged
    // grid buffer. In this program every heap release is a Vec data
    // buffer, so no bare `call void @free(` may remain at all — the
    // strongest form of "all Vec-buffer frees are recycling-aware".
    let src = r#"
fn build(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(n);
    return v;
}

fn main() {
    let mut g: Vec[i64] = build(1);
    g = build(2);
    println(f"{g[0]}");
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("ov.free"),
        "expected the reassignment to emit the overwrite-free block; got:\n{ir}"
    );
    assert!(
        ir.contains("karac_free_buf"),
        "expected the overwrite free to route through karac_free_buf; got:\n{ir}"
    );
    assert!(
        !ir.contains("call void @free("),
        "no bare libc free may remain for a program whose only heap is Vec buffers; got:\n{ir}"
    );
}

#[test]
fn test_e2e_big_vec_loop_recycles_transparently() {
    // 6 iterations × 2 MiB fresh Vec[i64]: iteration 1's buffer parks in
    // the runtime recycling cache at scope exit and later iterations'
    // exact-capacity reserve takes it back (same pages). Recycling must
    // be observationally invisible — byte-exact values, exact len — or
    // the cache is serving wrong-sized/aliased memory. (The perf claim
    // is benched, not unit-tested.)
    let src = r#"
fn main() {
    let mut total: i64 = 0;
    let mut it: i64 = 0;
    while it < 6 {
        let mut v: Vec[i64] = Vec.new();
        let mut i: i64 = 0;
        while i < 262144 {
            v.push(i + it);
            i = i + 1;
        }
        total = total + v[0] + v[262143] + v.len();
        it = it + 1;
    }
    println(f"{total}");
}
"#;
    // Σ v[0] = Σ it = 15; Σ v[262143] = 6·262143 + 15; Σ len = 6·262144.
    assert_eq!(run_program(src).as_deref(), Some("3145752\n"));
}

#[test]
fn test_ir_discarded_nested_vec_elem_freed() {
    // General owned-temp tracking, slice 2: a discarded `Vec[String]`
    // temp's *outer* buffer was already freed by slice 1, but its inner
    // String element buffers leaked because slice 1 passed `elem_ty:
    // None`. The hint table now supplies the element `TypeExpr`, so
    // `materialize_owned_temp` derives the element LLVM type via
    // `extract_vec_elem_type` and threads it to `track_vec_var` — the
    // `FreeVecBuffer` cleanup then emits its recursive per-element free
    // loop (`cleanup.drop.cond` block). Its presence proves the nested
    // leak is closed; a primitive-element discard (slice 1) never emits it.
    let src = r#"
fn make_vv() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("a");
    return v;
}

fn main() {
    make_vv();
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__owned_tmp"),
        "expected discarded Vec[String] temp materialized into __owned_tmp slot; got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.drop.cond"),
        "expected the recursive per-element free loop (elem_ty flowed from \
             owned_temp_drops, closing the nested-String leak); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_vec_of_vec_of_scalar_drop_keeps_one_level_fast_path() {
    // Slice 3n negative: a `Vec[Vec[i64]]` (inner SCALAR) is correctly handled
    // by the one-level vec-struct fast path (each inner buffer holds only
    // scalars — nothing deeper to free), so the fix must NOT route it through
    // the recursive per-element drop. `te_owns_heap_below_buffer` returns false
    // for a scalar inner, so `vec_elem_agg_drop_for_type_expr` keeps `None` and
    // the drop stays on the inline fast path — no `karac_drop_Vec_i64` call,
    // no agg-drop loop. Guards the minimal-blast-radius gate.
    let src = r#"
fn build() -> Vec[Vec[i64]] {
    let mut outer: Vec[Vec[i64]] = Vec.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(1_i64);
    outer.push(a);
    return outer;
}

fn main() {
    let vv = build();
    println(vv.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        !ir.contains("karac_drop_Vec_i64"),
        "Vec[Vec[i64]] must stay on the one-level fast path (scalar inner has no \
             deeper heap) — no recursive karac_drop_Vec_i64 should be emitted; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_vec_of_vec_of_struct_drop_emits_struct_elem_drop() {
    // Slice 3o: extends 3n's recursive drop to a `Vec[Vec[<user struct>]]`
    // element where the struct owns heap (`Rec { name: String }`). 3n's gate
    // excluded struct inners because the recursive drop family no-op'd a named
    // user type; 3o's `emit_drop_fn_for_type_expr` now delegates a struct
    // element to `vec_elem_agg_drop_for_type_expr` → `__karac_drop_struct_Rec`.
    // So the outer drop recurses: `karac_drop_Vec_Rec` (per inner Vec, in the
    // `cleanup.adrop` loop) calls `__karac_drop_struct_Rec` per element, which
    // frees each `Rec.name` String. Without it the innermost field Strings
    // leak (Linux LSan).
    let src = r#"
struct Rec { name: String, n: i64 }
fn build() -> Vec[Vec[Rec]] {
    let mut outer: Vec[Vec[Rec]] = Vec.new();
    let mut a: Vec[Rec] = Vec.new();
    a.push(Rec { name: "a field string padded out beyond thirty-six bytes ok", n: 1_i64 });
    outer.push(a);
    return outer;
}

fn main() {
    let vv = build();
    println(vv.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__karac_drop_struct_Rec"),
        "expected the struct-field drop __karac_drop_struct_Rec threaded through the \
             recursive Vec[Vec[Rec]] drop (frees each Rec.name String); got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.adrop"),
        "expected the agg-drop loop dropping each inner Vec[Rec] via the recursive \
             per-element drop; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_vec_of_vec_of_enum_drop_emits_enum_elem_drop() {
    // Slice 3o: the enum sibling. `Vec[Vec[Tok]]` where `Tok` has a heap
    // variant (`Word(String)`). The struct/enum delegation routes a `Tok`
    // element to `emit_enum_drop_switch` → `__karac_drop_Tok`, threaded through
    // the recursive `karac_drop_Vec_Tok` so each live variant's payload String
    // frees. Without it the `Word` payloads leak.
    let src = r#"
enum Tok { Word(String), Num(i64) }
fn build() -> Vec[Vec[Tok]] {
    let mut outer: Vec[Vec[Tok]] = Vec.new();
    let mut a: Vec[Tok] = Vec.new();
    a.push(Tok.Word("a token payload string padded beyond thirty-six bytes"));
    outer.push(a);
    return outer;
}

fn main() {
    let vv = build();
    println(vv.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__karac_drop_Tok"),
        "expected the enum drop-switch __karac_drop_Tok threaded through the recursive \
             Vec[Vec[Tok]] drop (frees each Word payload String); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_vec_of_option_scalar_drop_keeps_fast_path() {
    // Slice 3p negative: a `Vec[Option[i64]]` element owns NO heap — its
    // payload is a scalar with no cap word to guard on, so emitting the
    // Option drop would read w2 as garbage and free a junk pointer. The
    // `option_payload_inline_recursive_drop_ok` gate must keep it on the
    // (correct, heapless) fast path: no `karac_drop_Option_i64` anywhere.
    let src = r#"
fn build() -> Vec[Option[i64]] {
    let mut v: Vec[Option[i64]] = Vec.new();
    v.push(Some(7_i64));
    v.push(None);
    return v;
}

fn main() {
    let v = build();
    println(v.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        !ir.contains("karac_drop_Option_i64"),
        "Vec[Option[i64]] must NOT get an Option element drop (scalar payload has \
             no cap word — the drop would free garbage); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_vec_of_result_all_scalar_keeps_fast_path() {
    // Slice 3q negative: `Vec[Result[i64, i64]]` owns no heap on either side
    // — emitting a Result drop would read w2 as a cap and free garbage. The
    // `result_payload_inline_recursive_drop_ok` gate (at-least-one-heap-side)
    // keeps it on the correct heapless fast path.
    let src = r#"
fn build() -> Vec[Result[i64, i64]] {
    let mut v: Vec[Result[i64, i64]] = Vec.new();
    v.push(Ok(1_i64));
    v.push(Err(2_i64));
    return v;
}
fn main() {
    let v = build();
    println(v.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        !ir.contains("karac_drop_Result_"),
        "Vec[Result[i64,i64]] must NOT get a Result element drop (no heap side); \
             got:\n{}",
        ir
    );
}

#[test]
fn test_ir_vec_option_boxed_struct_elem_drop() {
    // Slice 3u: `Vec[Option[Holder]]` — Holder (4 words) exceeds the
    // 3-word inline area, so the element drop's Some path deboxes
    // (`box.free` branch) and runs the struct drop + frees the box.
    let src = r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut v: Vec[Option[Holder]] = Vec.new();
    v.push(Some(Holder { name: "a heap string padded out beyond thirty-six bytes!", id: 1 }));
    println(v.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("karac_drop_Option_Holder"),
        "expected the boxed-payload element drop karac_drop_Option_Holder; got:\n{}",
        ir
    );
    assert!(
        ir.contains("box.free"),
        "expected the boxed Some path (null-guarded box drop + free); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_vec_option_scalar_keeps_fast_path_3u() {
    // Slice 3u gate: an all-scalar payload stays on the heapless fast
    // path — no per-element Option drop is synthesized.
    let src = r#"
fn main() {
    let mut v: Vec[Option[i64]] = Vec.new();
    v.push(Some(1));
    println(v.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        !ir.contains("karac_drop_Option_"),
        "Vec[Option[i64]] must stay on the heapless fast path; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_ref_arg_nested_vec_elem_freed() {
    // Slice 2 part B: a fresh `Vec[String]` passed to a `ref Vec[String]`
    // param is materialized into a `ref_rvalue_arg` temp. The prior path
    // tracked it with `elem_ty: None` (outer buffer only). Now
    // `queue_ref_rvalue_arg_cleanup` recovers the element type from
    // `owned_temp_drops`, so the `FreeVecBuffer` cleanup emits its
    // recursive per-element free loop (`cleanup.drop.cond`) — proving the
    // nested-String leak is closed. Archive-independent (macOS ASAN has no
    // LeakSanitizer).
    let src = r#"
fn make_vv() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("a");
    return v;
}

fn show(v: ref Vec[String]) {
    println(v.len());
}

fn main() {
    show(make_vv());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("ref_rvalue_arg"),
        "expected the fresh Vec[String] rvalue materialized into a ref_rvalue_arg temp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.drop.cond"),
        "expected the recursive per-element free loop for the ref-arg temp \
             (elem_ty flowed from owned_temp_drops); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_vec_get_emits_owned_temp_free() {
    // Slice 3b (element-type-aware read methods on fresh-temp receivers):
    // `make_vec().get(0)` — the receiver is a fresh-owned Vec temp the
    // `get` borrows read-only. Codegen materializes it into a
    // `__vrecv_tmp` slot (with the scalar element type recovered from the
    // typechecker's `temp_recv_elem_types`) and queues a `FreeVecBuffer`
    // (`cleanup.free`) at the enclosing frame's exit. Without this the
    // receiver buffer leaked. Archive-independent leak-closure gate (macOS
    // ASAN has no LeakSanitizer).
    let src = r#"
fn make_vec() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    return v;
}

fn main() {
    match make_vec().get(0) {
        Some(x) => println(x),
        None => println(0_i64),
    };
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__vrecv_tmp"),
        "expected the fresh Vec receiver materialized into __vrecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.free"),
        "expected a FreeVecBuffer drain for the fresh-temp get receiver; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_vec_get_field_receiver_no_owned_temp() {
    // Negative / double-free guard: a *place*-expression receiver
    // (`h.items.get(0)`, a field access) reloads a buffer the `h` binding
    // owns. The slice-3b typechecker gate records only `Call`/`MethodCall`
    // receivers, so a field-access receiver is never serviced by the
    // fresh-temp path and must NOT materialize a `__vrecv_tmp` — freeing
    // it would double-free against `h`'s own cleanup. (`get` on the field
    // receiver routes through the existing named-binding dispatch.)
    let src = r#"
struct Holder { items: Vec[i64] }

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    let h = Holder { items: v };
    match h.items.get(0) {
        Some(x) => println(x),
        None => println(0_i64),
    };
}
"#;
    let ir = ir_for(src);
    assert!(
        !ir.contains("__vrecv_tmp"),
        "a field-access receiver must not materialize a fresh-temp Vec slot \
             (would double-free against the binding's cleanup); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_vec_nested_get_emits_per_element_drop() {
    // Slice 3e: `make_grid().get(i)` on a fresh-temp `Vec[Vec[i64]]`. The
    // element is itself a `{ptr,len,cap}` Vec, so the same vec-struct
    // recursion the `Vec[String]` case uses (`cleanup.drop.inner.free`)
    // per-element frees each inner row's data buffer before the outer buffer.
    // Without it the inner row buffers leak (LeakSanitizer on Linux CI). The
    // `Some(row)` borrow is `Option[ref Vec[i64]]` — NOT independently
    // dropped (`scrutinee_is_borrow_call`). Inner element is scalar (POD), so
    // the one-level recursion is complete (no innermost leak).
    let src = r#"
fn make_grid() -> Vec[Vec[i64]] {
    let mut g: Vec[Vec[i64]] = Vec.new();
    let mut row: Vec[i64] = Vec.new();
    row.push(10_i64);
    g.push(row);
    return g;
}

fn main() {
    match make_grid().get(0) {
        Some(r) => println(r[0]),
        None => println(0_i64),
    };
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__vrecv_tmp"),
        "expected the fresh Vec[Vec[i64]] receiver materialized into __vrecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.drop.inner.free"),
        "expected the vec-struct per-element drop loop (cleanup.drop.inner.free) \
             freeing each inner row buffer of the fresh-temp nested-Vec receiver; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_vec_struct_get_emits_agg_drop() {
    // Slice 3f: `make_recs().get(i)` on a fresh-temp `Vec[Rec]` where `Rec`
    // has a `String` field. Unlike the scalar/String/nested-Vec cases (which
    // use the inline vec-struct recursion or a plain free), a user-struct
    // element needs its synthesized per-element `__karac_drop_struct_Rec`
    // threaded into the `FreeVecBuffer` — emitted as the `cleanup.adrop`
    // loop, which calls the struct drop on every live element to free its
    // `name` String before the outer buffer. Without it each element's String
    // field leaks (LeakSanitizer on Linux CI). The `Some(r)` borrow
    // (`Option[ref Rec]`) is NOT independently dropped
    // (`scrutinee_is_borrow_call`).
    let src = r#"
struct Rec { name: String, n: i64 }

fn make_recs() -> Vec[Rec] {
    let mut v: Vec[Rec] = Vec.new();
    v.push(Rec { name: "a record name field padded beyond thirty-six bytes", n: 1_i64 });
    return v;
}

fn main() {
    match make_recs().get(0) {
        Some(r) => println(r.n),
        None => println(0_i64),
    };
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__vrecv_tmp"),
        "expected the fresh Vec[Rec] receiver materialized into __vrecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.adrop.body"),
        "expected the per-element aggregate-drop loop (cleanup.adrop) running \
             __karac_drop_struct_Rec on each element of the fresh-temp Vec[Rec]; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_vec_enum_get_emits_agg_drop() {
    // Slice 3g: `make_toks().get(i)` on a fresh-temp `Vec[Tok]` where `Tok`
    // is a user enum with a heap-bearing variant (`Word { s: String }`).
    // The enum element rides the SAME agg-drop machinery as the struct case
    // (slice 3f): `vec_elem_agg_drop_for_type_expr` routes a non-shared enum
    // to `emit_enum_drop_switch`, which synthesizes `__karac_drop_Tok`,
    // threaded into the `FreeVecBuffer` as the `cleanup.adrop` loop so every
    // live element's variant payload String is freed before the outer
    // buffer. Without it each `Word` element's String leaks (Linux LSan).
    // The `Some(t)` borrow (`Option[ref Tok]`) is NOT independently dropped
    // (`scrutinee_is_borrow_call`). No new codegen mechanism over 3f — this
    // is a typechecker gate lift.
    let src = r#"
enum Tok { Word { s: String }, Num { n: i64 } }

fn make_toks() -> Vec[Tok] {
    let mut v: Vec[Tok] = Vec.new();
    v.push(Tok.Word { s: "a token payload string padded beyond thirty-six bytes" });
    return v;
}

fn main() {
    match make_toks().get(0) {
        Some(t) => match t {
            Word { s } => println(s.len()),
            Num { n } => println(n),
        },
        None => println(0_i64),
    };
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__vrecv_tmp"),
        "expected the fresh Vec[Tok] receiver materialized into __vrecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.adrop.body") && ir.contains("__karac_drop_Tok"),
        "expected the per-element aggregate-drop loop (cleanup.adrop) running \
             __karac_drop_Tok on each element of the fresh-temp Vec[Tok]; got:\n{}",
        ir
    );
}

// ── ref parameter semantics ───────────────────────────────────

#[test]
fn test_e2e_ref_vec_param() {
    let out = run_program(
        r#"
fn sum(v: ref Vec[i64]) -> i64 {
    let mut total = 0;
    let mut i = 0;
    while i < v.len() {
        total = total + v[i];
        i = i + 1;
    }
    total
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    v.push(30);
    println(sum(v));
    println(v.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["60", "3"], "ref Vec should borrow, not move");
    }
}

#[test]
fn test_e2e_ref_vec_for_loop() {
    let out = run_program(
        r#"
fn print_all(v: ref Vec[i64]) {
    for x in v {
        println(x);
    }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    print_all(v);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "2", "3"]);
    }
}

#[test]
fn test_e2e_soa_push_len() {
    let out = run_program(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    entities.push(Entity { x: 1.0, y: 2.0, hp: 100 });
    entities.push(Entity { x: 3.0, y: 4.0, hp: 200 });
    println(entities.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2", "SoA push + len should work");
    }
}

#[test]
fn test_e2e_soa_mut_ref_push_across_function() {
    // Per-layout monomorphization slice 4 (multi-buffer WRITE, by mut ref):
    // a differently-named SoA buffer is FILLED through a shared helper that
    // takes `mut ref Vec[E]` and pushes. The push decomposes into per-group
    // scatter + realloc, writing the new group pointers / len / cap back
    // through the deref'd caller-struct pointer (`ref_params`), so after
    // `fill(mut grid)` returns `main`'s `grid` owns the populated buffers.
    // Borrow ownership: the callee only borrows (no `FreeSoaGroups` in the
    // mono), `main`'s `grid` frees once. Reads back (1+2)+(3+4)+(5+6)=21.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
fn fill(buf: mut ref Vec[E]) {
    buf.push(E { x: 1.0, y: 2.0 });
    buf.push(E { x: 3.0, y: 4.0 });
    buf.push(E { x: 5.0, y: 6.0 });
}
fn main() {
    let mut grid: Vec[E] = Vec.new();
    fill(mut grid);
    let mut s = 0.0;
    let mut i = 0;
    while i < grid.len() {
        s = s + grid[i].x + grid[i].y;
        i = i + 1;
    }
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "21",
            "SoA Vec filled via a mut-ref helper across a function boundary (differently named)"
        );
    }
}

#[test]
fn test_e2e_soa_vec_pod_field_compiles_and_runs() {
    // A `Vec[i64]` (Vec over a POD element) field in a SoA layout now
    // compiles (was rejected at layout validation) and runs cleanly — the
    // pushed Vec headers are stored into the `bulk` group and freed by the
    // synthesized per-element drop at scope exit. Asserts on a PRIMITIVE
    // read (`rows[i].tag`); the Vec field's *content* read-back is a
    // separate read-path concern (per-field method/index on a SoA heap
    // field needs the field's address — see the leak coverage in
    // `tests/memory_sanitizer.rs::asan_soa_vec_pod_field_no_leak`).
    let out = run_program(
        r#"
struct Row { tag: i64, data: Vec[i64] }
layout rows: Vec[Row] { group tags { tag } group bulk { data } }
fn make(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < n { v.push(i * 2); i = i + 1; }
    v
}
fn main() with panics {
    let mut rows: Vec[Row] = Vec.new();
    rows.push(Row { tag: 1, data: make(3) });
    rows.push(Row { tag: 2, data: make(5) });
    let mut s = 0;
    let mut i = 0;
    while i < rows.len() { s = s + rows[i].tag; i = i + 1; }
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "3",
            "SoA Vec[POD] field must compile, store, and drop cleanly"
        );
    }
}

#[test]
fn test_e2e_unsigned_vec_element_via_struct_field_zero_extends() {
    // A widening cast on an unsigned Vec element reached through a STRUCT
    // FIELD sign-extended instead of zero-extending. `expr_is_unsigned_int`
    // resolved the `Index` arm only when the container was named by a local
    // (`v[i]`, via `var_elem_type_exprs`); `holder.px[i]` fell through to
    // the signed default.
    //
    // Silent wrong numbers, not a crash, and AOT-only — the interpreter
    // carries the value's own type and was correct throughout, so `--interp`
    // disagreed with the binary.
    //
    // Found by Cumulus's calibration slice: a master dark is a `Vec[u16]`
    // held in a struct, and every pixel above 32767 came back negative, so
    // subtracting the dark ADDED ~65536 at exactly the hot pixels
    // calibration exists to remove. All three widths are pinned because
    // each has a distinct wrap point and one arm covers them all — a fix
    // that only handled u16 would leave the others silently broken.
    let out = run_program(
        r#"
struct H16 { px: Vec[u16] }
struct H8  { px: Vec[u8] }
struct H32 { px: Vec[u32] }
fn main() {
    let mut a: Vec[u16] = Vec.new(); a.push(45517u16);
    let mut b: Vec[u8]  = Vec.new(); b.push(200u8);
    let mut c: Vec[u32] = Vec.new(); c.push(3000000000u32);
    let h16 = H16 { px: a };
    let h8  = H8  { px: b };
    let h32 = H32 { px: c };
    println(f"{h16.px[0] as f64} {h16.px[0] as i64} {h8.px[0] as i64} {h32.px[0] as i64}");
}
"#,
    );
    if let Some(out) = out {
        // Pre-fix this read "-20019 -20019 -56 -1294967296".
        assert_eq!(out.trim(), "45517 45517 200 3000000000");
    }
}

#[test]
fn test_e2e_generic_owned_vec_param_return_no_double_free() {
    // Leg B: a `Vec[i64]` bound to a bare generic param, direct AND through
    // a nested forward — the element must be threaded so the body deep-copies
    // with the correct stride.
    let out = run_program(
        "fn id[T](x: T) -> T { x }\n\
             fn twice[T](x: T) -> T { id(x) }\n\
             fn main() {\n\
             \x20   let mut v: Vec[i64] = Vec.new(); v.push(5); v.push(9);\n\
             \x20   let w = id(v);\n\
             \x20   println(w[1].to_string());\n\
             \x20   let mut u: Vec[i64] = Vec.new(); u.push(7);\n\
             \x20   let r = twice(u);\n\
             \x20   println(r[0].to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "9\n7");
    }
}

#[test]
fn test_e2e_generic_enum_vec_payload_match_return() {
    // B-2026-07-13-3, Vec[i64] payload sibling: the deboxed 3-word Vec
    // reconstructs at `{ptr,i64,i64}` and dispatches `.len()`/index.
    let out = run_program(
        "enum Opt[T] { Yes(T), No }\n\
             fn get[T](o: Opt[T], d: T) -> T { match o { Opt.Yes(v) => v, Opt.No => d } }\n\
             fn main() {\n\
             \x20   let mut v: Vec[i64] = Vec.new(); v.push(7); v.push(8);\n\
             \x20   let e: Vec[i64] = Vec.new();\n\
             \x20   let w: Vec[i64] = get(Opt.Yes(v), e);\n\
             \x20   println(w.len().to_string());\n\
             \x20   println(w[1].to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2\n8");
    }
}

// ── Vec[T] extended methods ───────────────────────────────────

#[test]
fn test_e2e_vec_is_empty_true() {
    let out = run_program(
        r#"
fn main() {
    let v: Vec[i64] = Vec.new();
    println(v.is_empty());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "true");
    }
}

#[test]
fn test_e2e_vec_is_empty_false() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(42);
    println(v.is_empty());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "false");
    }
}

#[test]
fn test_e2e_vec_first_nonempty() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    v.push(30);
    match v.first() {
        Some(x) => println(x),
        None => println(0),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10");
    }
}

#[test]
fn test_e2e_vec_first_empty() {
    let out = run_program(
        r#"
fn main() {
    let v: Vec[i64] = Vec.new();
    match v.first() {
        Some(x) => println(x),
        None => println(99),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

#[test]
fn test_e2e_vec_last_nonempty() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    v.push(30);
    match v.last() {
        Some(x) => println(x),
        None => println(0),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "30");
    }
}

#[test]
fn test_e2e_vec_last_empty() {
    let out = run_program(
        r#"
fn main() {
    let v: Vec[i64] = Vec.new();
    match v.last() {
        Some(x) => println(x),
        None => println(99),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

#[test]
fn test_e2e_vec_get_in_bounds() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(100);
    v.push(200);
    v.push(300);
    match v.get(1) {
        Some(x) => println(x),
        None => println(0),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "200");
    }
}

#[test]
fn test_e2e_vec_get_out_of_bounds() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(100);
    match v.get(5) {
        Some(x) => println(x),
        None => println(99),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

// ── Vec.get_unchecked — unsafe direct-index, no bounds check ────────
//
// Counterpart to `test_e2e_vec_get_in_bounds`: same indexing semantics
// but skips the bounds-check CFG (no `oob_bb` / `valid_bb`, no Option
// wrap). Lever for the bounds-check tax measured on kata #5
// (`wip-kata5-perf.md`). Out-of-range index is UB at runtime — the
// codegen path emits no diagnostic.

#[test]
fn test_e2e_vec_get_unchecked_in_bounds_returns_element() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    v.push(30);
    unsafe {
        println(v.get_unchecked(0));
        println(v.get_unchecked(1));
        println(v.get_unchecked(2));
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10\n20\n30");
    }
}

#[test]
fn test_e2e_bounds_elision_length_pin_other_vec_still_checks() {
    // The pin binds `cols` to `dp`, but the loop indexes a *different*,
    // shorter Vec `short`. The `UpperBound { idx, vec }` fact names `dp`, so
    // `short[c]`'s check does not match and stays live — `short[c]` panics
    // at `c == 2`. Guards the vec-name matching in
    // `index_bounds_already_proven`.
    if let Some(c) = run_program_capturing(
        r#"
fn go(n: i64) -> i64 {
    let mut dp: Vec[i64] = Vec.new();
    let mut j = 0i64;
    while j < n { dp.push(1i64); j = j + 1i64; }
    let mut short: Vec[i64] = Vec.new();
    short.push(1i64);
    short.push(2i64);
    let mut acc = 0i64;
    let mut c = 0i64;
    while c < n { acc = acc + short[c]; c = c + 1i64; }
    acc
}
fn main() { println(go(6i64)); }
"#,
    ) {
        assert!(
            !c.status.success(),
            "pin on dp must not elide short[c]'s check (panic), got stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_bounds_elision_length_pin_nested_shadow_vec_still_checks() {
    // Soundness: a top-level fill pins (n, dp); a nested block re-binds `dp`
    // to an EMPTY Vec and index-reads it. The name-keyed pin must NOT apply
    // to the shadow — the exactly-once / rebind gate refuses it and the
    // empty-Vec read panics rather than reading out of bounds. (Regression
    // for a shadowing hole in the shipped elision.)
    if let Some(c) = run_program_capturing(
        r#"
fn go(n: i64) -> i64 {
    let mut dp: Vec[i64] = Vec.new();
    let mut j = 0i64;
    while j < n { dp.push(1i64); j = j + 1i64; }
    let mut acc = 0i64;
    let mut once = 1i64;
    while once > 0i64 {
        let mut dp: Vec[i64] = Vec.new();
        let mut d = 0i64;
        while d < n { acc = acc + dp[d]; d = d + 1i64; }
        once = 0i64;
    }
    acc
}
fn main() { println(go(5i64)); }
"#,
    ) {
        assert!(
            !c.status.success(),
            "nested shadow of the pinned Vec must keep the check (panic), got stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_for_in_vec_vec_inner_push() {
    // Iterating Vec[Vec[i64]] should bind `inner` as a Vec[i64] so
    // inner-Vec method dispatch resolves correctly.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[Vec[i64]] = Vec.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(1_i64);
    a.push(2_i64);
    v.push(a);
    let mut b: Vec[i64] = Vec.new();
    b.push(10_i64);
    b.push(20_i64);
    b.push(30_i64);
    v.push(b);
    for inner in v {
        println(inner.len());
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "3"]);
    }
}

// ── Clone trait surface (canonical: phase-8-stdlib-floor.md
//    "Clone trait surface for collections") ───────────────────────────

#[test]
fn test_e2e_vec_clone_preserves_contents() {
    // Cloned Vec contains the same elements as the source.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    v.push(20_i64);
    v.push(30_i64);
    let w: Vec[i64] = v.clone();
    println(w[0]);
    println(w[1]);
    println(w[2]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "20", "30"]);
    }
}

#[test]
fn test_e2e_vec_clone_independent_buffers() {
    // Mutating the source Vec after cloning leaves the clone unchanged
    // — independent buffers from a fresh malloc.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    let w: Vec[i64] = v.clone();
    v.push(99_i64);
    println(v.len());
    println(w.len());
    println(w[0]);
    println(w[1]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "2", "1", "2"]);
    }
}

#[test]
fn test_e2e_vec_clone_empty_fast_path() {
    // `v.clone()` on an empty Vec hits the empty-fast path: dst gets
    // {null, 0, 0} without any allocation. Verifies the cloned Vec is
    // still observably empty and supports push afterwards.
    let out = run_program(
        r#"
fn main() {
    let v: Vec[i64] = Vec.new();
    let mut w: Vec[i64] = v.clone();
    println(w.len());
    w.push(7_i64);
    println(w.len());
    println(w[0]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "1", "7"]);
    }
}

#[test]
fn test_e2e_vec_clone_borrowed_receiver_is_deep() {
    // Regression for B-2026-06-18-9: `path.clone()` where `path` is a
    // BORROWED receiver (`ref` / `mut ref` parameter) mis-built — the clone
    // fn was handed the alloca's raw pointer (a `**Vec` for a ref param), so
    // it copied the {data,len,cap} from the pointer's own bits, producing an
    // alias that shared the source buffer. A later mutation of the source
    // then drained the "snapshot". The interpreter always deep-copied, so it
    // was a run/build divergence. `get_data_ptr` now unwraps the ref level.
    //
    // Two receiver modes, both through a function-parameter borrow, both
    // followed by draining the source AFTER the snapshot:
    //   - `mut ref`: snapshot via a recursive backtracking shape (the kata
    //     #39 form — push, recurse, snapshot at the leaf, pop on unwind).
    //   - `ref`: snapshot a borrowed Vec, return it, then drain the source.
    let out = run_program(
        r#"
fn snap_mut(path: mut ref Vec[i64], out: mut ref Vec[Vec[i64]]) {
    out.push(path.clone());
}
fn snap_ref(path: ref Vec[i64]) -> Vec[i64] {
    path.clone()
}
fn rec(depth: i64, path: mut ref Vec[i64], out: mut ref Vec[Vec[i64]]) {
    if depth == 0i64 {
        snap_mut(path, out);
        return;
    }
    path.push(depth);
    rec(depth - 1i64, path, out);
    path.pop();
}
fn main() {
    // mut ref through recursion: snapshot [3,2,1] taken at the leaf must
    // survive the pops on the way back up.
    let mut out: Vec[Vec[i64]] = Vec.new();
    let mut path: Vec[i64] = Vec.new();
    rec(3i64, mut path, mut out);
    let row = ref out[0];
    println(row.len());
    println(row[0]);
    println(row[1]);
    println(row[2]);

    // ref borrow: snapshot must be independent of a later drain.
    let mut src: Vec[i64] = Vec.new();
    src.push(7_i64);
    src.push(8_i64);
    let kept = snap_ref(src);
    src.pop();
    src.pop();
    println(kept.len());
    println(kept[0]);
    println(kept[1]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "3", "2", "1", "2", "7", "8"]);
    }
}

#[test]
fn test_repeat_literal_const_nonzero_skips_insertvalue() {
    // Same fast path applies to non-zero constants: one constant
    // aggregate, no per-element ops.
    let ir = ir_for(
        r#"
fn main() {
    let buf: Array[i64, 8] = [42; 8];
    let _ = buf[0];
}
"#,
    );
    assert!(
        !ir.contains("insertvalue"),
        "const-nonzero repeat literal must not emit per-element insertvalue; got IR:\n{}",
        ir
    );
}

#[test]
fn test_repeat_literal_runtime_value_falls_back_to_insertvalue() {
    // When `val` is a runtime expression (e.g. function return),
    // the const fast path doesn't apply and we exercise the
    // per-element fallback. Locks in that the fallback path is
    // still reachable — if a future loop-CFG lowering replaces it,
    // this test should be updated rather than silently regressing.
    let ir = ir_for(
        r#"
fn compute() -> i64 { 7 }
fn main() {
    let n = compute();
    let buf: Array[i64, 4] = [n; 4];
    let _ = buf[0];
}
"#,
    );
    assert!(
        ir.contains("insertvalue"),
        "runtime-value repeat literal should fall back to insertvalue; got IR:\n{}",
        ir
    );
}

// ── Theme 6: with_provider[R] lowering (sub-step 3) ──────────────────
//
// Structural tests pinning the alloca + push + body + pop sequence
// emitted at each `with_provider[R](provider, ||body)` call site. The
// body's value is whatever the closure expression evaluates to;
// dispatch through `R.method(...)` is sub-step 4.

#[test]
fn test_with_provider_emits_push_and_pop() {
    let ir = ir_for(
        "pub trait Recorder { fn record(value: i64); }\n\
             pub struct Counter { n: i64 }\n\
             impl Recorder for Counter { fn record(value: i64) { } }\n\
             pub effect resource Metric: Recorder;\n\
             fn main() {\n\
               let p = Counter { n: 0 };\n\
               with_provider[Metric](p, || { 42 });\n\
             }",
    );
    assert!(
        ir.contains("call void @karac_provider_push"),
        "expected karac_provider_push call; IR: {}",
        ir
    );
    assert!(
        ir.contains("call void @karac_provider_pop"),
        "expected karac_provider_pop call; IR: {}",
        ir
    );
    assert!(
        ir.contains("@VT_Counter_Recorder"),
        "expected vtable reference @VT_Counter_Recorder in push args; IR: {}",
        ir
    );
}

#[test]
fn test_pattern_bound_nested_tuple_vec_payload() {
    // PB5 cross-check / Theme 5 headline regression gate: nested
    // destructure where the variant payload is itself a tuple
    // `(Vec[i64], i64)`. Lights up after Theme 5 (compound-payload
    // tuple-payload destructure) added the Tuple branch in
    // `bind_pattern_values` + `reconstruct_payload_value`.
    let out = run_program(
        r#"
enum E { V((Vec[i64], i64)) }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(4);
    let e = V((v, 100));
    match e {
        V((xs, n)) => {
            println(xs.len());
            println(n);
        }
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["4", "100"]);
    }
}

/// Companion to the field-access test above — exercises the
/// conditional-push-in-for-in-loop shape that surfaced the bug
/// in the clone-graph kata. Mirrors `cond_simple.kara`: a Map
/// declared + mutated adjacent to a VecDeque conditional push
/// inside a for-in-loop. Pre-fix prints `1` (seed-count only);
/// post-fix prints `6` (the outer loop exits via `count > 5`
/// after pushing both xs elements each iter).
#[test]
fn test_e2e_conditional_push_in_for_in_loop_with_map() {
    let src = r#"
shared struct Node { val: i64 }
fn main() {
    let mut m: Map[i64, Node] = Map.new();
    let mut q: VecDeque[Node] = VecDeque.new();
    let n1 = Node { val: 1 };
    let mut xs: Vec[Node] = Vec.new();
    xs.push(Node { val: 10 });
    xs.push(Node { val: 20 });
    let _ = m.insert(99, n1);
    q.push_back(n1);
    let mut count: i64 = 0;
    loop {
        if let Some(_curr) = q.pop_front() {
            count = count + 1;
            if count > 5 { break; }
            for x in xs.iter() {
                if x.val > 0 {
                    q.push_back(x);
                }
            }
        } else {
            break;
        }
    }
    println(count);
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(
            out, "6\n",
            "conditional push in for-in-loop must actually execute the for body — \
                 pre-fix the `if x.val > 0` lowered to `br i1 false` because \
                 `var_type_names[x]` (used by `field_index_for`) was unset"
        );
    }
}

/// Phase-7 line 14 — verify the `.kara_jit_template` section is
/// reserved with a 4-byte `version=0 / empty` manifest. v1
/// emission only — actual JIT-template payloads are post-v1 per
/// `deferred.md § Runtime Monomorphization JIT`.
///
/// Three layers of assertion catch different rot modes:
/// 1. IR — the global + initializer + section name appear with
///    the right shape. Catches accidental rename/drop.
/// 2. Object file — `nm` reports the symbol, confirming the
///    backend honors the codegen request.
/// 3. Linked executable — the symbol survives `--gc-sections` /
///    `-dead_strip`. Catches the case where v2+ readers can't
///    locate the manifest after the linker has run.
#[test]
fn test_jit_template_section_reserved() {
    use karac::codegen::compile_to_ir;

    let src = r#"
fn main() {}
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

    let ir = compile_to_ir(&parsed.program, None, None).expect("codegen failed");

    // Layer 1 — IR shape. The manifest global must exist with
    // the right name, type, and `version=0 / empty` initializer.
    assert!(
        ir.contains("@karac_jit_template_manifest"),
        "expected manifest global in IR; got:\n{ir}",
    );
    // The 4-byte zero initializer should appear in some IR form
    // (`zeroinitializer` if LLVM folds it, otherwise explicit
    // `[4 x i8] c\"\\00\\00\\00\\00\"`).
    let initializer_ok = ir.contains("zeroinitializer")
        || ir.contains(r#"c"\00\00\00\00""#)
        || ir.contains("[i8 0, i8 0, i8 0, i8 0]");
    assert!(
        initializer_ok,
        "expected version=0 / empty initializer for manifest; got:\n{ir}",
    );
    // Section name — picks `__TEXT,__jittmpl` on Apple targets
    // (Mach-O 16-char limit; parked inside `__TEXT` instead of a
    // fresh segment so the 4-byte payload doesn't cost a full
    // 16 KiB page per binary) and `.kara_jit_template` elsewhere.
    let section_name = if cfg!(target_vendor = "apple") {
        "__TEXT,__jittmpl"
    } else {
        ".kara_jit_template"
    };
    assert!(
        ir.contains(section_name),
        "expected section `{section_name}` in IR; got:\n{ir}",
    );
    // The manifest must register in `@llvm.used` so the linker
    // can't strip it under `--gc-sections` / `-dead_strip`.
    assert!(
        ir.contains("@llvm.used"),
        "expected @llvm.used to pin the manifest; got:\n{ir}",
    );
}

#[test]
fn test_state_struct_type_expands_vec_param_to_three_word_layout() {
    // A `Vec[i64]` parameter expands to the codegen's existing 3-word
    // Vec struct layout (`{ ptr, i64, i64 }`). With the tag field
    // prepended, the state struct = `{ i32, { ptr, i64, i64 } }`.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(items: Vec[i64]) { fetch(); }",
    );
    // Find the state-struct type definition line.
    let line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no state struct type def in IR:\n{ir}"));
    assert!(
        line.contains("i32"),
        "tag field missing in state struct line: {line}"
    );
    // Vec is the codegen's 3-word inline struct — search for the
    // `{ ptr, i64, i64 }` shape (or its anonymous-struct equivalent).
    // LLVM emits the inline struct as `{ ptr, i64, i64 }` on the type
    // definition line for the Vec field.
    assert!(
        line.contains("{ ptr, i64, i64 }") || line.contains("{ptr, i64, i64}"),
        "expected inline Vec layout in state struct: {line}"
    );
}

#[test]
fn test_caller_side_intercept_preserves_pure_function_direct_calls() {
    // Pure-function calls in the same caller still lower to direct
    // calls — the intercept only fires for network-boundary callees.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn pure_helper(x: i64) -> i64 { x + 1 }
             fn driver() { fetch(); }
             fn main() {
                 let _ = pure_helper(1);
                 driver();
             }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    // pure_helper still uses direct call.
    assert!(
        main_body.contains("call i64 @pure_helper(i64 1)"),
        "pure helper must still use direct call:\n{main_body}"
    );
    // driver uses the intercept.
    assert!(
        main_body.contains("@__kara_state_new_driver()"),
        "network-boundary driver must use the intercept:\n{main_body}"
    );
}

#[test]
fn test_method_call_intercept_preserves_non_network_methods() {
    // Non-network method calls still use direct dispatch — the
    // intercept fires only when the resolved Type.method key is
    // in state_machine_state_constructors.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub {
                 fn run(self) { fetch(); }
                 fn pure_count(self) -> i64 { self.count }
             }
             fn main() {
                 let h = Hub { count: 5 };
                 let _ = h.pure_count();
                 h.run();
             }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    // pure_count uses direct dispatch — no state constructor.
    assert!(
        !main_body.contains("@__kara_state_new_Hub.pure_count"),
        "pure method must not be intercepted:\n{main_body}"
    );
    // run still gets intercepted.
    assert!(
        main_body.contains("@__kara_state_new_Hub.run()"),
        "network-boundary method must use the intercept:\n{main_body}"
    );
}

#[test]
fn test_8ai_vec_return_state_struct_terminal_field_is_vec_struct() {
    // Vec[T] lowers to the inline `{ptr, i64, i64}` slice
    // descriptor — independent of T. Returning `Vec[i64]`
    // produces a terminal field with that 3-word layout.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() -> Vec[i64] with sends(Network) receives(Network) { fetch(); Vec.new() }",
    );
    let line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no driver state struct in IR:\n{ir}"));
    assert!(
        line.contains("i32, { ptr, i64, i64 }"),
        "Vec[i64] return: state struct must include the 3-word vec descriptor:\n{line}"
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("store { ptr, i64, i64 } zeroinitializer, ptr %kara.return.field_ptr"),
        "Vec[i64] return: terminal arm must store zeroinitializer placeholder:\n{body}"
    );
}

#[test]
fn test_body_splitting_8s_chained_let_with_vec_propagates_type() {
    // `let v = items; let w = v;` — the second let consumes the
    // first's typed slot. Slice 8s must put the Vec type into
    // slot_map for `v` so that the `let w = v` Slot-load reads the
    // Vec width, and the `w.slot` alloca matches. Without slice 8s,
    // v.slot is i64, the load reads 8 bytes of Vec data as i64, and
    // w.slot alloca's i64 — silent corruption chain.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(items: Vec[i64]) with sends(Network) receives(Network) {
                 fetch();
                 let v = items;
                 let w = v;
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // Both slots Vec-typed.
    assert!(
        body.contains("%v.slot = alloca { ptr, i64, i64 }")
            || body.contains("%v.slot = alloca {ptr, i64, i64}"),
        "v.slot must be Vec-shaped:\n{body}"
    );
    assert!(
        body.contains("%w.slot = alloca { ptr, i64, i64 }")
            || body.contains("%w.slot = alloca {ptr, i64, i64}"),
        "w.slot must propagate the Vec type from v:\n{body}"
    );
    // Chained Slot load reads Vec width from v.slot.
    assert!(
        body.contains("%v.let_rhs = load { ptr, i64, i64 }, ptr %v.slot")
            || body.contains("%v.let_rhs = load {ptr, i64, i64}, ptr %v.slot"),
        "let w = v must load Vec width from v.slot:\n{body}"
    );
}

#[test]
fn test_state_destructor_emits_cap_gt_zero_free_for_vec_field() {
    // The Vec captured-local field's destructor body emits the
    // `cap > 0 ? free(data)` pattern: GEP into the state-struct
    // field (offset 1, after tag), GEP cap (Vec struct field 2),
    // load i64, icmp SGT 0, conditional branch to a free BB that
    // loads the data ptr and calls free.
    //
    // The predicate is SIGNED, and that is load-bearing rather than
    // incidental: SSO encodes "these bytes are inline" as the SIGN BIT of
    // `cap` (`runtime/src/sso.rs`), so an inline descriptor has `cap < 0`
    // and owns no buffer. Under the old `ugt` that negative cap read as an
    // enormous unsigned and the gate freed the descriptor's own address.
    // Do not "restore" `ugt` here — for a Vec the two are identical (its
    // cap is a non-negative count), so this assertion is about keeping the
    // String path inline-safe.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(items: Vec[i64]) { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_state_drop_driver");
    // GEP captured-local field at struct-field idx 1 (tag at 0).
    assert!(
        body.contains("%items.drop.field_ptr"),
        "destructor must GEP the captured-local field as %items.drop.field_ptr:\n{body}"
    );
    // Load Vec cap and compare against 0.
    assert!(
        body.contains("%items.drop.cap = load i64"),
        "destructor must load Vec cap as i64:\n{body}"
    );
    assert!(
        body.contains("%items.drop.is_heap = icmp sgt i64 %items.drop.cap, 0"),
        "destructor must compare cap > 0:\n{body}"
    );
    // The free branch loads data and calls free.
    assert!(
        body.contains("%items.drop.data = load ptr"),
        "destructor's free branch must load the data ptr:\n{body}"
    );
    assert!(
        body.contains("call void @free(ptr %items.drop.data)"),
        "destructor's free branch must call free on the data ptr:\n{body}"
    );
}

// ── Phase 4 (2026-05-21): Vec.len() / Vec.is_empty() on a call-result receiver ──
//
// Surfaced building LeetCode #204 (count-primes) kata: the natural
// form `return list_primes_under(n).len();` panicked with
// "no handler for method 'len' on non-identifier receiver". Method
// dispatch only covered `ExprKind::Identifier` receivers; function
// calls returning `Vec[T]` fell through to the dispatch-fail Err.
// Fix: direct struct-field extraction for the element-type-agnostic
// read-only methods (`len`, `is_empty`). Mutating methods (`push`,
// `sort`) stay rejected — they'd lose the mutation when the temp
// goes out of scope at end-of-statement.

#[test]
fn test_e2e_vec_len_on_function_call_receiver() {
    let output = run_program(
        "fn list_primes_under(n: i64) -> Vec[i64] {\n\
                 let mut primes: Vec[i64] = Vec.new();\n\
                 let mut k: i64 = 2i64;\n\
                 while k < n {\n\
                     let mut is_prime: bool = true;\n\
                     let mut d: i64 = 2i64;\n\
                     while (d * d) <= k {\n\
                         if (k % d) == 0i64 { is_prime = false; }\n\
                         d = d + 1i64;\n\
                     }\n\
                     if is_prime { primes.push(k); }\n\
                     k = k + 1i64;\n\
                 }\n\
                 return primes;\n\
             }\n\
             fn main() {\n\
                 println(list_primes_under(10i64).len());\n\
                 println(list_primes_under(100i64).len());\n\
             }",
    )
    .expect("compile + run failed");
    // LC anchor values: π(10) = 4, π(100) = 25.
    assert_eq!(output, "4\n25\n");
}

#[test]
fn test_e2e_vec_is_empty_on_function_call_receiver() {
    let output = run_program(
        "fn make_vec(n: i64) -> Vec[i64] {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 let mut i: i64 = 0i64;\n\
                 while i < n {\n\
                     v.push(i);\n\
                     i = i + 1i64;\n\
                 }\n\
                 return v;\n\
             }\n\
             fn main() {\n\
                 if make_vec(0i64).is_empty() { println(1); } else { println(0); }\n\
                 if make_vec(5i64).is_empty() { println(1); } else { println(0); }\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "1\n0\n");
}

// --- Phase 7 § *defer / errdefer codegen* slice 3: LIFO interleave ---
//
// Slice 3 audits and pins that user `defer { ... }` and the compiler-
// internal cleanup actions (`FreeVecBuffer`, `RcDec`, `FreeMapHandle`,
// `EnumDrop`, `StructDrop`, `RcDecOption`) share a single per-scope
// stack (`scope_cleanup_actions.last_mut()`) and drain in reverse
// push order (LIFO) per `emit_scope_cleanup` (`src/codegen/runtime.rs`).
// The structural invariant was established by slice 1 (`UserDefer`
// pushes onto the same frame `track_vec_var` / `track_rc_var` /
// `track_map_handle` push onto) — slice 3's contribution is the
// verification + the regression-pin tests below.
//
// Design.md § *Drop ordering within a branch* mandates program-order
// LIFO interleave: `let v = Vec.new(); defer foo();` drains `foo()`
// first, then `drop(v)`. Both observable behaviours are pinned:
//   - E2E (`test_e2e_defer_lifo_with_let_vec_interleave`,
//     `test_e2e_defer_lifo_with_two_let_vecs_interleave`) — defer
//     bodies read the let-bound Vec's `.len()` at scope exit and the
//     output proves the Vec was still alive when the defer fired
//     (had the order been "all drops then defers", the read would
//     touch freed memory).
//   - IR (`test_ir_defer_drop_interleave_emission_order`) — pins the
//     emission order of `mark_b()` / `@free` / `mark_a()` / `@free`
//     within `compile_function`'s tail-cleanup block to distinguish
//     the unified-stack-LIFO behaviour from the "all defers then all
//     drops" two-phase alternative (which would emit both `mark_*`
//     calls before any `@free`).

#[test]
fn test_e2e_defer_lifo_with_let_vec_interleave() {
    // Single let + single defer: push order is `[FreeVec(v),
    // UserDefer(println)]`; LIFO drain fires the defer first (sees
    // live `v.len() == 3`), then the `FreeVec(v)` cleanup releases
    // the buffer. Under the alternative two-phase ordering
    // "all drops then all defers", the defer would read a freed
    // data pointer and either print garbage or crash; observing
    // `"len=3"` pins the unified-stack interleave.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    v.push(20_i64);
    v.push(30_i64);
    defer { println(f"len={v.len()}"); }
    println("body");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["body", "len=3"]);
    }
}

#[test]
fn test_ir_modbind_vec_new_emits_empty_aggregate() {
    // `Vec.new()` at module scope lowers to the canonical empty-Vec
    // `{ptr null, i64 0, i64 0}` aggregate matching the runtime
    // invariant in `assoc_call.rs`'s shared `Vec/VecDeque && method
    // == "new"` arm. LLVM prints an all-zero aggregate as
    // `zeroinitializer` rather than the expanded field list, which
    // is what we assert against. Mutable form → `internal global`;
    // immutable form → `internal constant`.
    let ir = ir_for(
        "let mut TODOS: Vec[i64] = Vec.new();\n\
             let EMPTY: Vec[i64] = Vec.new();\n\
             fn main() {}",
    );
    assert!(
        ir.contains("@TODOS = internal global { ptr, i64, i64 } zeroinitializer"),
        "expected @TODOS Vec global as zeroinitializer, got:\n{}",
        ir
    );
    assert!(
        ir.contains("@EMPTY = internal constant { ptr, i64, i64 } zeroinitializer"),
        "expected @EMPTY Vec constant as zeroinitializer, got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_modbind_vec_push_then_len() {
    // End-to-end pin for the uppercase-receiver method-dispatch fix —
    // the backend kata's canonical shape. The parser produces
    // `Call(Path([TODOS, push]))` because `TODOS` starts uppercase;
    // the typechecker rewrite at `infer_call` re-routes through
    // `infer_method_call`, lowering rewrites the AST to
    // `MethodCall(Identifier(TODOS), push, ...)`, and codegen's
    // method-call dispatch for module-binding receivers uses
    // `get_data_ptr`'s module-binding fall-back to thread the
    // global's pointer through `compile_vec_method`. Without any
    // one of those three steps, the call falls through to a
    // codegen error or compiles to a stub that no-ops.
    let output = run_program(
        "let mut TODOS: Vec[i64] = Vec.new();\n\
             fn main() {\n\
                 TODOS.push(10);\n\
                 TODOS.push(20);\n\
                 TODOS.push(30);\n\
                 println(TODOS.len());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "3\n");
}

#[test]
fn test_e2e_vec_remove_local() {
    // `Vec.remove(idx)` on a local: returns the removed element,
    // shifts the tail down by one, decrements len. v1 contract is
    // unchecked (UB on OOB) — the test pins an in-bounds case.
    let output = run_program(
        "fn main() {\n\
                 let mut xs: Vec[i64] = Vec.new();\n\
                 xs.push(10);\n\
                 xs.push(20);\n\
                 xs.push(30);\n\
                 let removed: i64 = xs.remove(1);\n\
                 println(removed);\n\
                 println(xs.len());\n\
                 println(xs[0]);\n\
                 println(xs[1]);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "20\n2\n10\n30\n");
}

#[test]
fn test_e2e_vec_remove_modbind() {
    // Same as the local-Vec test but on a module-bound Vec —
    // confirms the slice-b module-binding sibling dispatch in
    // `compile_method_call` routes `remove` correctly through
    // the same `compile_vec_method` arm.
    let output = run_program(
        "let mut XS: Vec[i64] = Vec.new();\n\
             fn main() {\n\
                 XS.push(10);\n\
                 XS.push(20);\n\
                 XS.push(30);\n\
                 let removed: i64 = XS.remove(1);\n\
                 println(removed);\n\
                 println(XS.len());\n\
                 println(XS[0]);\n\
                 println(XS[1]);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "20\n2\n10\n30\n");
}

#[test]
fn test_e2e_vec_remove_first() {
    // Remove the head — should memmove the entire tail down.
    let output = run_program(
        "fn main() {\n\
                 let mut xs: Vec[i64] = Vec.new();\n\
                 xs.push(1);\n\
                 xs.push(2);\n\
                 xs.push(3);\n\
                 let _ = xs.remove(0);\n\
                 println(xs[0]);\n\
                 println(xs[1]);\n\
                 println(xs.len());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "2\n3\n2\n");
}

#[test]
fn test_e2e_vec_remove_last() {
    // Remove the last element — memmove count should be 0,
    // len decrements, the previous element becomes the last.
    let output = run_program(
        "fn main() {\n\
                 let mut xs: Vec[i64] = Vec.new();\n\
                 xs.push(7);\n\
                 xs.push(8);\n\
                 xs.push(9);\n\
                 let _ = xs.remove(2);\n\
                 println(xs.len());\n\
                 println(xs[0]);\n\
                 println(xs[1]);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "2\n7\n8\n");
}

#[test]
fn test_e2e_vec_dedup_scalar_and_heap() {
    // `Vec[T].dedup()` — remove CONSECUTIVE duplicates, keeping the first of
    // each run (Rust semantics). In-place compaction mirroring `retain`, with
    // the keep decision = "differs from the previous kept element" via the
    // element `Eq` and drop-glue on the removed duplicates. Covers a scalar
    // Vec (runs collapse, non-adjacent dups preserved), a heap Vec[String]
    // (removed dups freed — leak-clean in the sibling LSan test), all-equal,
    // and no-dup pass-through.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut v: Vec[i64] = [1, 1, 2, 3, 3, 3, 1];\n\
                 v.dedup();\n\
                 println(v.len());\n\
                 println(v[0]); println(v[2]); println(v[3]);\n\
                 let mut s: Vec[String] = [\"a\", \"a\", \"bb\", \"bb\", \"a\"];\n\
                 s.dedup();\n\
                 println(s.len());\n\
                 println(s[0]); println(s[1]); println(s[2]);\n\
                 let mut e: Vec[i64] = [7, 7, 7];\n\
                 e.dedup();\n\
                 println(e.len());\n\
                 let mut n: Vec[i64] = [1, 2, 3];\n\
                 n.dedup();\n\
                 println(n.len());\n\
             }",
    ) {
        // scalar: [1,2,3,1] (len 4, non-adjacent trailing 1 kept);
        // heap: [a, bb, a] (len 3); all-equal: len 1; no-dup: len 3.
        assert_eq!(out, "4\n1\n3\n1\n3\na\nbb\na\n1\n3\n");
    }
}

#[test]
fn test_e2e_vec_split_off_scalar_and_heap() {
    // `Vec[T].split_off(i) -> Vec[T]` — self keeps [0, i), the returned Vec
    // owns [i, len). Tail elements MOVE into a fresh buffer (byte-copy of
    // each `{ptr,len,cap}` for heap); `self.len = i` excludes them from
    // self's drop so each frees once (leak-clean in the sibling LSan test).
    // Covers a scalar split, index clamping (0 / len / out-of-bounds), a heap
    // Vec[String] (both halves usable), and consuming both halves.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut v: Vec[i64] = [1, 2, 3, 4, 5];\n\
                 let t: Vec[i64] = v.split_off(2);\n\
                 println(v.len()); println(t.len());\n\
                 println(v[1]); println(t[0]); println(t[2]);\n\
                 let mut z: Vec[i64] = [1, 2, 3];\n\
                 let za: Vec[i64] = z.split_off(0);\n\
                 println(z.len()); println(za.len());\n\
                 let mut e: Vec[i64] = [1, 2, 3];\n\
                 let eo: Vec[i64] = e.split_off(10);\n\
                 println(e.len()); println(eo.len());\n\
                 let mut s: Vec[String] = [\"a\", \"b\", \"c\", \"d\"];\n\
                 let st: Vec[String] = s.split_off(2);\n\
                 println(s[0]); println(st[0]); println(st[1]);\n\
                 let mut n: Vec[i64] = [1, 2, 3, 4];\n\
                 let nt: Vec[i64] = n.split_off(2);\n\
                 let mut sum: i64 = 0;\n\
                 for x in n { sum = sum + x; }\n\
                 for y in nt { sum = sum + y; }\n\
                 println(sum);\n\
             }",
    ) {
        // split@2: v=[1,2] t=[3,4,5]; v[1]=2 t[0]=3 t[2]=5;
        // @0: z=[] za=[1,2,3]; @10(clamp): e=[1,2,3] eo=[]; strings s=[a,b] st=[c,d];
        // sum both halves = 10
        assert_eq!(out, "2\n3\n2\n3\n5\n0\n3\n3\n0\na\nc\nd\n10\n");
    }
}

#[test]
fn test_e2e_vec_get_unwrap_struct_heap_field_read() {
    // B-2026-07-20-9: `let a = v.get(i)/.first()/.last().unwrap()` on a
    // Vec whose element is a user STRUCT with a heap (`String`) field, then
    // `a.name`. The binding's type was never recorded (`bind_pattern_types`
    // doesn't peel the accessor's `Option[ref elem]`), so codegen fell to
    // the LLVM-shape reverse-lookup — which picked the FIRST same-shape
    // struct in HashMap iteration order. `Acct { name: String }` collides
    // with the prelude's `Regex`/`RegexError` (all `{ptr,i64,i64}`), so the
    // binding was mislabeled on a random subset of compiles and the field
    // read compiled to a SILENT `i64 0` constant — output flipped between
    // `alice` and `0` across byte-identical builds. Fixed three ways: the
    // binding registers from the typechecker's span-keyed peeled element
    // type (deterministic); the shape reverse-lookup only fires on a UNIQUE
    // match; an unresolvable field read now fails LOUD instead of emitting
    // `i64 0`. Covers get/first/last, a two-field struct, and two reads.
    // (A single run of this test was flaky-green before the fix; with
    // layers 2+3 a wrong label now fails loudly instead of passing wrong.)
    let output = run_program(
        "struct Acct { name: String, txns: i64 }\n\
             fn main() {\n\
                 let mut v: Vec[Acct] = Vec.new();\n\
                 v.push(Acct { name: \"alice\".to_string(), txns: 2 });\n\
                 v.push(Acct { name: \"bob\".to_string(), txns: 5 });\n\
                 let a = v.get(0).unwrap();\n\
                 println(f\"{a.name}:{a.txns}\");\n\
                 let b = v.get(1).unwrap();\n\
                 println(f\"{b.name}:{b.txns}\");\n\
                 let f = v.first().unwrap();\n\
                 println(f.name);\n\
                 let l = v.last().unwrap();\n\
                 println(l.name);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "alice:2\nbob:5\nalice\nbob\n");
}

#[test]
fn test_e2e_vec_prefix_literal_basic() {
    // `Vec[a, b, c]` at expression position now lowers via
    // `compile_vec_prefix_literal` — malloc + per-slot store +
    // `{buf, n, n}` return. Previously fell through to `i64 0`
    // because `ExprKind::PrefixCollectionLiteral` had no arm in
    // `compile_expr`. Surfaced building the backend TODO API
    // kata Slice 4.
    let output = run_program(
        "fn main() {\n\
                 let xs: Vec[i64] = Vec[10, 20, 30];\n\
                 println(xs.len());\n\
                 println(xs[0]);\n\
                 println(xs[1]);\n\
                 println(xs[2]);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "3\n10\n20\n30\n");
}

#[test]
fn test_e2e_vec_prefix_literal_as_enum_payload() {
    // The original kata-surfaced shape: `Json.Array(Vec[a, b])`.
    // Pre-fix: rendered as `[]` because the literal evaluated
    // to a null Vec inside the variant payload. Post-fix: the
    // payload's Vec carries its elements, `stringify()` walks
    // them, output matches the expected JSON.
    let output = run_program(
        "fn main() {\n\
                 let arr: Json = Json.Array(Vec[Json.Number(1.0), Json.Number(2.0)]);\n\
                 println(arr.stringify());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "[1.0,2.0]\n");
}

#[test]
fn test_e2e_vec_prefix_literal_push_after() {
    // The cap-equals-len shape means the first subsequent push
    // triggers grow. Verify the Vec is fully functional after
    // construction via the literal — push, len, indexed read all
    // continue to work uniformly with how Vec.new + push behaves.
    let output = run_program(
        "fn main() {\n\
                 let mut xs: Vec[i64] = Vec[1, 2];\n\
                 xs.push(3);\n\
                 xs.push(4);\n\
                 println(xs.len());\n\
                 println(xs[2]);\n\
                 println(xs[3]);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "4\n3\n4\n");
}

#[test]
fn test_e2e_modbind_vec_indexed_read() {
    // Indexed read on a module-bound Vec — uses the slice-10
    // collections.rs module-binding fall-back at `compile_index`,
    // unaffected by the method-dispatch rewrite but co-tested
    // because the kata Slice 4 needs both.
    let output = run_program(
        "let mut TODOS: Vec[i64] = Vec.new();\n\
             fn main() {\n\
                 TODOS.push(100);\n\
                 TODOS.push(200);\n\
                 println(TODOS[0]);\n\
                 println(TODOS[1]);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "100\n200\n");
}

#[test]
fn test_e2e_file_moved_into_a_vec_outlives_its_origin_binding() {
    // B-2026-08-09-17: a `File` bound by a match arm and MOVED into a
    // `Vec[File]` was still closed by that binding's `FreeFileHandle` at the
    // arm's scope exit. `karac_runtime_file_close` reconstructs the Box and
    // drops it, so the Vec was left holding freed memory, and the next
    // method call locked a `Mutex<std::fs::File>` inside that freed
    // allocation. A garbage lock word does not fault, it BLOCKS — the
    // program hung with no diagnostic, while `--interp` ran it correctly.
    //
    // Reads TWICE through the Vec: once inside the arm (where the origin
    // binding is still live, which always worked) and once after the arm
    // closes (where the close used to have fired). The second read must
    // CONTINUE where the first stopped. That is the part worth having — a
    // handle that had been reopened, or a copy of the struct rather than the
    // same allocation, would restart at byte 0 and still look healthy, so
    // the advancing file position is what pins this to the same live
    // handle rather than merely a non-crashing one.
    let tmp = std::env::temp_dir().join("karac_e2e_file_moved_into_vec.txt");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"ABCDEFGH").expect("temp write");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
fn main() with reads(FileSystem) writes(FileSystem) {{
    let mut hs: Vec[File] = Vec.new();
    let mut buf: Array[u8, 4] = [0u8; 4];
    match File.open("{path}") {{
        Ok(f) => {{
            hs.push(f);
            match hs[0].read(mut buf) {{
                Ok(_) => println(f"first {{buf[0]}}"),
                Err(_) => println("first-failed"),
            }}
        }}
        Err(_) => println("open-failed"),
    }}
    match hs[0].read(mut buf) {{
        Ok(_) => println(f"second {{buf[0]}}"),
        Err(_) => println("second-failed"),
    }}
}}
"#
    );
    let out = run_program(&src);
    if let Some(out) = out {
        // 'A' = 65 at offset 0, 'E' = 69 at offset 4.
        assert_eq!(out.trim(), "first 65\nsecond 69");
    }
    let _ = std::fs::remove_file(&tmp);
}

// Cross-function variable-name collision must not corrupt scope
// cleanup. The name-keyed collection side-tables (`vec_elem_types`,
// `string_vars`, …) are reset per function in `compile_function`;
// without that reset, `prefix_string(s: ref String, …)` registers
// `vec_elem_types["s"]`, which leaks into `lcp`'s `let mut s = 1i64`
// counter. The let-site then queues a `FreeVecBuffer` cleanup against
// the i64 alloca; at the inner loop's exit the cleanup reads a bogus
// `cap` past the 8-byte slot and frees a garbage pointer — SIGABRT at
// -O0, a miscompiled infinite loop at -O3. Asserting on the output
// confirms the program both terminates and computes correctly.
#[test]
fn test_e2e_cross_function_name_collision_no_stale_vec_cleanup() {
    let out = run_program(
        r#"
fn prefix_string(s: ref String, k: i64) -> String {
    let mut out: String = "";
    let mut i = 0i64;
    for c in s.chars() {
        if i >= k { break; }
        out.push(c);
        i = i + 1i64;
    }
    out
}
fn lcp(strs: ref Vec[String]) -> String {
    let n = strs.len();
    let first = strs[0i64].bytes();
    let first_len = first.len();
    let mut col = 0i64;
    while col < first_len {
        let c = first[col];
        let mut s = 1i64;
        while s < n {
            let other = strs[s].bytes();
            if col >= other.len() or other[col] != c {
                return prefix_string(strs[0i64], col);
            }
            s = s + 1i64;
        }
        col = col + 1i64;
    }
    prefix_string(strs[0i64], first_len)
}
fn main() {
    let mut a: Vec[String] = Vec.new();
    a.push("flower");
    a.push("flow");
    a.push("flight");
    let r = lcp(a);
    println(r.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2");
    }
}

#[test]
fn test_e2e_vec_sort_default_ascending() {
    // Bare `Vec.sort()` (no comparator) must actually sort in codegen.
    // Regression: it previously fell through the vec_method catch-all to
    // a stand-in `0` and silently left the Vec unsorted in compiled
    // binaries (correct only in the interpreter).
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(5); v.push(2); v.push(8); v.push(1); v.push(2);
    v.sort();
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "2", "2", "5", "8"]);
    }
}

#[test]
fn test_e2e_vec_reverse() {
    // `Vec.reverse()` must reverse in place in codegen (same silent-no-op
    // regression class as `sort`).
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1); v.push(2); v.push(3); v.push(4);
    v.reverse();
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["4", "3", "2", "1"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_identity_ascending() {
    // `sort_by_key(|x| x)` sorts in ascending order — the canonical key
    // closure. Replaces the prior loud-rejection test now that the
    // codegen arm in vec_method.rs is wired up.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(5); v.push(2); v.push(8); v.push(1); v.push(2);
    v.sort_by_key(|x| x);
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "2", "2", "5", "8"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_negation_descending() {
    // `sort_by_key(|x| -x)` is the standard descending-sort idiom — it
    // exercises a non-identity key closure body and pins that the key
    // is recomputed correctly for each element across the two body
    // compiles inside the bridge thunk.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(5); v.push(2); v.push(8); v.push(1); v.push(2);
    v.sort_by_key(|x| -x);
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["8", "5", "2", "2", "1"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_named_fn_callee() {
    // Named-function callee (`v.sort_by_key(key)`) routes through
    // `emit_sort_by_key_named_thunk`: direct-ABI call, no env_ptr.
    // Replaces the prior loud-rejection test now that the dispatch
    // is wired through the bridge thunk.
    let out = run_program(
        r#"
fn key(x: i64) -> i64 { x }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(30); v.push(10); v.push(20);
    v.sort_by_key(key);
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "20", "30"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_named_fn_struct_field_body() {
    // Named-fn key with a struct element, body returning a field.
    // Companion to the closure-typed-local variant above — exercises
    // the direct-ABI named-fn thunk path with a non-trivial body.
    let out = run_program(
        r#"
struct Item { v: i64, tag: i64 }
fn key(s: Item) -> i64 { s.v }
fn main() {
    let mut v: Vec[Item] = Vec.new();
    v.push(Item { v: 30, tag: 1 });
    v.push(Item { v: 10, tag: 2 });
    v.push(Item { v: 20, tag: 3 });
    v.sort_by_key(key);
    for s in v.iter() { println(s.v); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "20", "30"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_named_fn_callee() {
    // sort_by with a named-fn comparator returning Ordering — routes
    // through `emit_sort_by_named_thunk` (new helper). Extracts the
    // Ordering tag and returns `tag - 1` for the runtime's signed
    // comparator contract.
    let out = run_program(
        r#"
fn mycmp(a: i64, b: i64) -> Ordering { a.cmp(b) }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(30); v.push(10); v.push(20);
    v.sort_by(mycmp);
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "20", "30"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_mono_tuple_primary_key() {
    // Slice 6.4 — Vec[(i64, i64)].sort_by(|a, b| a.0.cmp(b.0)) — sort
    // intervals (records) by their FIRST tuple field. The canonical
    // sort-records-by-primary-key shape (kata 56 Merge Intervals idiom).
    // Pins that emit_sort_by_mono with elem_ty=struct{i64,i64} loads/
    // stores the 16-byte payload correctly and the closure body's
    // `.0` field access lowers through the existing tuple-extract path.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[(i64, i64)] = Vec.new();
    v.push((3, 100));
    v.push((1, 200));
    v.push((2, 300));
    v.sort_by(|a, b| a.0.cmp(b.0));
    for t in v.iter() {
        println(t.0);
        println(t.1);
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // Sorted by first field: (1,200), (2,300), (3,100).
        assert_eq!(lines, vec!["1", "200", "2", "300", "3", "100"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_mono_tuple_secondary_field_preserved() {
    // Slice 6.4 — pins that the SECOND tuple field (which the
    // comparator ignores) survives the sort intact. If the mono fn's
    // load/store of the struct value were truncating to the first
    // field, the secondary values would scramble.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[(i64, i64)] = Vec.new();
    v.push((5, 999));
    v.push((1, 111));
    v.push((3, 333));
    v.push((4, 444));
    v.push((2, 222));
    v.sort_by(|a, b| a.0.cmp(b.0));
    for t in v.iter() { println(t.1); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // Sorted by first field 1..5 — secondaries appear in 111..999 order.
        assert_eq!(lines, vec!["111", "222", "333", "444", "999"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_mono_tuple_computed_comparator() {
    // Slice 6.4 — comparator does arithmetic on tuple fields, then
    // `.cmp()`. Kata 1665 (Minimum Initial Energy) uses this exact
    // shape: `(b.1 - b.0).cmp(a.1 - a.0)` for descending sort by
    // `min - actual`. Pins that the closure body computes through
    // both `.0` and `.1` accesses on each param and the final cmp
    // result steers the sort.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[(i64, i64)] = Vec.new();
    v.push((2, 10));   // diff = 8
    v.push((5, 6));    // diff = 1
    v.push((1, 20));   // diff = 19
    v.push((3, 8));    // diff = 5
    // Sort by (b.1 - b.0) descending: largest gap first.
    v.sort_by(|a, b| (b.1 - b.0).cmp(a.1 - a.0));
    for t in v.iter() {
        println(t.0);
        println(t.1);
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // Gaps 19, 8, 5, 1 — descending: (1,20), (2,10), (3,8), (5,6).
        assert_eq!(lines, vec!["1", "20", "2", "10", "3", "8", "5", "6"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_mono_tuple_three_fields() {
    // Slice 6.4 — wider tuple `(i64, i64, i64)` (24-byte payload).
    // Pins that the struct-of-int fields gate predicate accepts
    // arbitrary-arity integer tuples, not just pairs.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[(i64, i64, i64)] = Vec.new();
    v.push((3, 99, 999));
    v.push((1, 11, 111));
    v.push((2, 22, 222));
    v.sort_by(|a, b| a.0.cmp(b.0));
    for t in v.iter() {
        println(t.0);
        println(t.1);
        println(t.2);
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["1", "11", "111", "2", "22", "222", "3", "99", "999"]
        );
    }
}

#[test]
fn test_e2e_vec_sort_by_mono_large_n_uses_runtime_path() {
    // Slice 6.4 — call-site dispatch threshold: insertion sort is
    // O(N²) and beats the runtime callback only up to ~N=32–64;
    // above that, the call site emits a runtime length check that
    // routes large N to `karac_vec_sort_by`. Kata 1665's N=50000
    // workload surfaced this 2026-05-29: a strawman mono-only
    // dispatch regressed kata 1665 from 3.2 ms (pre-Slice-6.4) to
    // 1.1 s. Pins correctness across the threshold by sorting
    // 100 elements (> 64, so the runtime path is taken).
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    let mut i: i64 = 0i64;
    while i < 100i64 {
        let x: i64 = (i * 1103515245i64 + 12345i64) % 1000i64;
        v.push(x);
        i = i + 1i64;
    }
    v.sort_by(|a, b| a.cmp(b));
    println(v[0i64]);
    println(v[50i64]);
    println(v[99i64]);
    let mut sorted: i64 = 1i64;
    let mut j: i64 = 1i64;
    while j < 100i64 {
        if v[j - 1i64] > v[j] {
            sorted = 0i64;
        }
        j = j + 1i64;
    }
    println(sorted);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[3], "1", "vec is not sorted: {:?}", lines);
        let first: i64 = lines[0].parse().unwrap();
        let mid: i64 = lines[1].parse().unwrap();
        let last: i64 = lines[2].parse().unwrap();
        assert!(first <= mid, "first ({}) > mid ({})", first, mid);
        assert!(mid <= last, "mid ({}) > last ({})", mid, last);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_tuple_lexicographic() {
    // Integer-tuple keys (`(i64, i64)`) sort lexicographically — first
    // field is primary, second tie-breaks. Pins the StructValue arm of
    // emit_sort_by_key_inline_thunk's key dispatch.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[(i64, i64)] = Vec.new();
    v.push((3, 1));
    v.push((1, 2));
    v.push((2, 3));
    v.sort_by_key(|t| (t.1, t.0));
    for t in v.iter() {
        let (a, b) = t;
        println(a);
        println(b);
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // Keys: (1,3), (2,1), (3,2) — already ascending by key, so order
        // is (3,1), (1,2), (2,3).
        assert_eq!(lines, vec!["3", "1", "1", "2", "2", "3"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_tuple_tie_break() {
    // First field ties on 1 across three elements; second field must
    // break the tie ascending. Verifies the cascade's `(neq ? cmp_i :
    // rest)` doesn't short-circuit on the tied primary field.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[(i64, i64)] = Vec.new();
    v.push((1, 5));
    v.push((2, 0));
    v.push((1, 2));
    v.push((1, 8));
    v.sort_by_key(|t| (t.0, t.1));
    for t in v.iter() {
        let (a, b) = t;
        println(a);
        println(b);
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // Expected: (1,2), (1,5), (1,8), (2,0).
        assert_eq!(lines, vec!["1", "2", "1", "5", "1", "8", "2", "0"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_triple_tuple_cascade() {
    // Three-component integer-tuple key exercises a three-level cascade.
    // All inputs are odd, so `x % 2 == 1` ties the primary component;
    // the second component (the raw value) must drive the order.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(7); v.push(3); v.push(7); v.push(1);
    v.sort_by_key(|x| (x % 2i64, x, x % 3i64));
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "3", "7", "7"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_struct_mixed_fields() {
    // `derive(Ord)` on a struct with mixed int + String fields produces
    // a lex compare in declaration order. The struct-aware cascade in
    // `emit_sort_by_key_inline_thunk` uses `expr_struct_type_names` to
    // recover the struct name from the body Expr's span, then
    // dispatches each field's compare via `struct_field_type_names`:
    // int fields use a signed select, the String field calls
    // `karac_string_cmp`. Three items with two scoring-10 entries that
    // tie on the primary field — the secondary String field must break
    // the tie in alphabetical order.
    let out = run_program(
        r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Item { score: i64, name: String }

fn main() {
    let mut v: Vec[Item] = Vec.new();
    v.push(Item { score: 10i64, name: "z" });
    v.push(Item { score: 10i64, name: "a" });
    v.push(Item { score: 5i64, name: "m" });
    v.sort_by_key(|i| i);
    for it in v.iter() {
        println(it.score);
        println(it.name);
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["5", "m", "10", "a", "10", "z"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_nested_struct_field() {
    // Two-level nesting: Outer { p: Inner, n: i64 } where Inner has its
    // own integer fields. The struct-aware cascade in
    // `emit_struct_cmp_cascade` recurses on the `p` field because its
    // type name resolves to another entry in `struct_field_type_names`.
    // Verifies that the first field that differs at any depth decides
    // the order, exactly like derived `Ord` on nested structs would.
    let out = run_program(
        r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Inner { a: i64, b: i64 }
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Outer { p: Inner, n: i64 }

fn main() {
    let mut v: Vec[Outer] = Vec.new();
    v.push(Outer { p: Inner { a: 2, b: 0 }, n: 7 });
    v.push(Outer { p: Inner { a: 1, b: 9 }, n: 3 });
    v.push(Outer { p: Inner { a: 1, b: 5 }, n: 1 });
    v.sort_by_key(|o| o);
    for o in v.iter() {
        println(o.p.a);
        println(o.p.b);
        println(o.n);
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // Sorted ascending by ((a, b), n):
        //   ((1, 5), 1), ((1, 9), 3), ((2, 0), 7).
        assert_eq!(lines, vec!["1", "5", "1", "1", "9", "3", "2", "0", "7"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_user_impl_ord_reverse() {
    // User `impl Ord for T` takes precedence over the derive cascade.
    // The cmp body intentionally REVERSES order (other.v.cmp(self.v))
    // — if the dispatch fell through to the all-int cascade, the sort
    // would come out ascending; with user-cmp dispatch it must come
    // out descending. Pins that:
    //   (1) `type_supports_ord` accepts user `impl Ord` (else the
    //       program wouldn't reach codegen at all);
    //   (2) `user_ord_typed_exprs` is populated by the lowering pass;
    //   (3) the codegen arm calls the user's `Score.cmp` directly
    //       rather than the field cascade.
    let out = run_program(
        r#"
struct Score { v: i64 }
impl PartialEq for Score { fn eq(self, other: Score) -> bool { self.v == other.v } }
impl Eq for Score {}
impl PartialOrd for Score { fn partial_cmp(self, other: Score) -> Option[Ordering] { Some(other.v.cmp(self.v)) } }
impl Ord for Score { fn cmp(self, other: Score) -> Ordering { other.v.cmp(self.v) } }

fn main() {
    let mut v: Vec[Score] = Vec.new();
    v.push(Score { v: 10i64 });
    v.push(Score { v: 30i64 });
    v.push(Score { v: 20i64 });
    v.sort_by_key(|s| s);
    for s in v.iter() { println(s.v); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["30", "20", "10"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_struct_field_access_body() {
    // Regression: `Vec[Struct].sort_by_key(|s| s.field)` (closure body
    // is a field access on a struct element) used to silently return
    // the input order. Root cause: the inline thunk emitter bound the
    // closure param only via `self.variables` but never registered
    // `self.var_type_names[param]`, so `compile_field_access` couldn't
    // recover the struct shape and the field-extract step elided —
    // the body compiled to a bare struct load with no extractvalue,
    // and the cascade returned an unsorted permutation. Fixed by
    // plumbing the Vec element's Kāra type name through the thunk
    // emitter (`emit_sort_by_key_inline_thunk`) so the param gets
    // registered alongside its variable slot. Pinned with the
    // canonical primitive-field key the original bug reproducer used.
    let out = run_program(
        r#"
struct Score { v: i64 }
fn main() {
    let mut v: Vec[Score] = Vec.new();
    v.push(Score { v: 30 });
    v.push(Score { v: 10 });
    v.push(Score { v: 20 });
    v.sort_by_key(|s| s.v);
    for s in v.iter() { println(s.v); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "20", "30"]);
    }
}

#[test]
fn test_e2e_vec_sort_general_elements_supported() {
    // B-2026-06-30-15: `sort()` now has a default comparator for the
    // whole ordered-leaf family — tuples, floats, and nested Vecs sort
    // lexicographically via the recursive `karac_cmp_<T>` thunks
    // (matching the interpreter's `value_compare`). This test pinned
    // the OLD rejection; it now pins the support.
    let src = r#"
fn main() {
    let mut v: Vec[(i64, i64)] = Vec.new();
    v.push((3, 1));
    v.push((1, 2));
    v.push((3, 0));
    v.sort();
    for t in v {
        println(t.0 * 10 + t.1);
    }
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "12\n30\n31\n");
}

#[test]
fn test_e2e_vec_elem_generic_mono_distinct_per_element_type() {
    // B-2026-07-02-41 (S6b-1): two element-type instantiations of a
    // `ref Vec[T]` generic fn must be DISTINCT monomorphs. The
    // LLVM-value-based `infer_type_args` sees only the element-erased
    // `{ptr,len,cap}` shape, so `T` stayed unbound and both calls
    // shared one mono — the f64 call read the i64 element width and
    // printed `2.5`'s bit pattern as an integer. Fixed by (a) a
    // typechecker `unify_types` owned-to-`ref` coercion arm so `T`
    // solves from the `Vec[i64]` arg, plumbed to codegen via
    // `call_type_subs`; and (b) a codegen container-element fallback
    // for the nested call `first(v)` inside `wrap[T]`, which the
    // typechecker records as a dropped self-referential `T -> T`.
    let src = r#"
fn first[T](v: ref Vec[T]) -> T {
    v[0]
}
fn wrap[T](v: ref Vec[T]) -> T {
    first(v)
}
fn main() {
    let a: Vec[i64] = vec![7, 8, 9];
    let b: Vec[f64] = vec![2.5, 3.5];
    println(f"{first(a)}");
    println(f"{first(b)}");
    println(f"{wrap(a)}");
    println(f"{wrap(b)}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "7\n2.5\n7\n2.5\n");
}

#[test]
fn test_e2e_vec_sort_unordered_element_still_rejected() {
    // A user-struct element has no default order — still rejected
    // loudly, pointing at sort_by.
    let src = r#"
struct P { x: i64 }
fn main() {
    let mut v: Vec[P] = Vec.new();
    v.push(P { x: 1 });
    v.sort();
    println(v.len());
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
    let err = compile_to_ir(&parsed.program, None, None)
        .expect_err("expected codegen to reject sort() on unordered element types")
        .message;
    assert!(
        err.contains("use sort_by"),
        "expected the sort_by-directing diagnostic; got: {}",
        err
    );
}

#[test]
fn test_ir_plain_alias_over_vec_matches_base_layout() {
    // B-2026-07-30-7, both alias arities: `type Ints = Vec[i64];` and
    // `type Boxed[T] = Vec[T];` must each lower a parameter to the
    // `Vec` fat pointer, not the `i64` fall-through. The generic arm is
    // the one that proved this was not a generics bug — the non-generic
    // alias fails identically.
    let plain = ir_for("fn takes(xs: Vec[i64]) -> i64 { 0 }");
    for src in [
        "type Ints = Vec[i64];\nfn takes(xs: Ints) -> i64 { 0 }",
        "type Boxed[T] = Vec[T];\nfn takes(xs: Boxed[i64]) -> i64 { 0 }",
    ] {
        let aliased = ir_for(src);
        assert_eq!(
            takes_param_type(&aliased),
            takes_param_type(&plain),
            "plain alias over Vec must lower to the Vec base layout\nsrc:\n{src}\nIR:\n{aliased}"
        );
    }
}

#[test]
fn test_ir_strip_contracts_removes_all_asserts() {
    // Release: every contract assert across all four kinds is gone.
    let ir = ir_for_contracts_stripped(ALL_CONTRACTS_SRC);
    assert!(
            !ir.contains("contract violated"),
            "release build must strip all contract asserts; IR still had a `contract violated` marker:\n{ir}"
        );
}

#[test]
fn test_e2e_generic_parent_vec_field_elem_bodies() {
    // B-2026-08-02-16 — the container-field sibling of B-2026-08-02-14:
    // a GENERIC parent's `Vec[T]`-of-Drop field (`Pack[T] { items:
    // Vec[T] }` at `Pack[Res]`). The dbv element walk was gated off for
    // mono parents, so AOT stayed silent while the interpreter's
    // value-driven Vec-field arm fired — a run-vs-build divergence.
    // The field TE now resolves through the mono subst before the walk,
    // so both backends print the element body at owner death.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Pack[T] { items: Vec[T], tag: i64 }
fn main() {
    println("a");
    {
        let mut p: Pack[Res] = Pack { items: Vec.new(), tag: 1 };
        p.items.push(Res { id: 7, name: f"qq{7}" });
        println(f"n {p.items.len()}");
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "a\nn 1\ndrop 7 qq7\nend");
    }
}

#[test]
fn test_e2e_tuple_field_parent_displaced_and_vec_elem_bodies() {
    // B-2026-08-02-18 (gate legs) — a parent whose ONLY Drop content is
    // a tuple field now classifies Drop-relevant on both backends'
    // widened gates: the displaced old value fires its tuple-held body
    // at reassignment, and a Vec element of such a parent fires it at
    // the container's death (the interp side pins the
    // field_value_carries_user_drop widening; pre-fix the interp was
    // silent on both while AOT fired — a run-vs-build divergence).
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct DuoR { pair: (Res, i64), tag: i64 }
fn main() {
    println("a");
    {
        let mut x = DuoR { pair: (Res { id: 1, name: f"a{1}" }, 5), tag: 7 };
        println(x.tag);
        x = DuoR { pair: (Res { id: 2, name: f"b{2}" }, 6), tag: 8 };
        println(x.tag);
    }
    println("mid");
    {
        let mut v: Vec[DuoR] = Vec.new();
        v.push(DuoR { pair: (Res { id: 3, name: f"x{3}" }, 5), tag: 9 });
        println(v.len());
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "a\n7\ndrop 1 a1\n8\ndrop 2 b2\nmid\n1\ndrop 3 x3\nend"
        );
    }
}

#[test]
fn test_e2e_discarded_vec_removal_fires_and_frees() {
    // B-2026-08-03-2 (class 2) — a DISCARDED `v.remove(i);` /
    // `v.swap_remove(i);` hands the element back BY VALUE with nothing to
    // receive it: the body never ran and the element's heap LEAKED, on both
    // backends and on the shipping path. Builtins have no
    // `fn_return_type_names` entry, so the discard battery could not resolve
    // what it was holding; it now falls back to the receiver's recorded
    // element TypeExpr. `bound` is the control that was always correct, and
    // `survivor` checks only the REMOVED element fires early while the one
    // left behind still fires at scope exit.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn main() {
    println("remove:");
    {
        let mut v: Vec[Res] = Vec.new();
        v.push(Res { id: 1, name: f"a{1}" });
        v.remove(0);
        println(v.len());
    }
    println("swapremove:");
    {
        let mut w: Vec[Res] = Vec.new();
        w.push(Res { id: 2, name: f"b{2}" });
        w.swap_remove(0);
        println(w.len());
    }
    println("bound:");
    {
        let mut u: Vec[Res] = Vec.new();
        u.push(Res { id: 3, name: f"c{3}" });
        let r = u.remove(0);
        println(u.len());
    }
    println("survivor:");
    {
        let mut t: Vec[Res] = Vec.new();
        t.push(Res { id: 4, name: f"d{4}" });
        t.push(Res { id: 5, name: f"e{5}" });
        t.remove(0);
        println(t.len());
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "remove:\ndrop 1 a1\n0\nswapremove:\ndrop 2 b2\n0\nbound:\ndrop 3 c3\n0\n\
                 survivor:\ndrop 4 d4\n1\ndrop 5 e5\nend"
        );
    }
}

#[test]
fn test_e2e_vec_of_tuple_element_drop_bodies() {
    // B-2026-08-02-22 — `Vec[(Res, i64)]`: the container's per-element
    // machinery had no tuple arm on either axis, so the element's Drop
    // body never fired (both backends) and its heap leaked. Two shapes:
    // an INLINE element (the minimal repro — no move involved) and one
    // built from a NAMED source, which additionally left the source's own
    // body armed so AOT fired it early over a moved-from slot, printing
    // an empty name. Both must now produce exactly ONE fire, at the
    // container's death, with the correct field content. The leak half is
    // pinned by asan_vec_of_tuple_element_heap_freed.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn main() {
    println("inline:");
    {
        let mut t: Vec[(Res, i64)] = Vec.new();
        t.push((Res { id: 9, name: f"in{9}" }, 8));
        println(t.len());
    }
    println("named:");
    {
        let mut u: Vec[(Res, i64)] = Vec.new();
        let r = Res { id: 4, name: f"tt{4}" };
        u.push((r, 8));
        println(u.len());
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "inline:\n1\ndrop 9 in9\nnamed:\n1\ndrop 4 tt4\nend"
        );
    }
}

#[test]
fn test_e2e_owned_vec_param_if_branch_return_no_double_free() {
    // B-2026-07-13-1, nested-heap sibling: a `Vec[String]` param returned
    // from an `if` branch tail must deep-copy the OUTER buffer AND recurse
    // into each String element (`emit_vecstr_defensive_copy`'s element
    // walk), or the shared inner buffers double-free.
    let out = run_program(
        r#"
fn choose(a: Vec[String], b: Vec[String], first: bool) -> Vec[String] {
    if first { a } else { b }
}

fn main() {
    let mut x: Vec[String] = Vec.new();
    x.push(f"one");
    x.push(f"two");
    let mut y: Vec[String] = Vec.new();
    y.push(f"three");
    let r = choose(x, y, false);
    println(r[0]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "three");
    }
}

/// A MUTUAL type cycle through a `Vec` must not crash the compiler.
///
/// `Outer.kids: Vec[Inner]` and `Inner.owner: Outer` is legal Kāra that
/// `karac check` accepts. Passing `Outer` by value engages the entry-copy,
/// whose emitter UNROLLS a per-element struct copy — so a cyclic type has
/// no finite emission and the copy-support analysis has to decline it.
///
/// B-2026-07-28-3 installed that guard but asked only whether the element
/// type was ALREADY on the walk stack, which catches the DIRECT cycle
/// (`Node.kids: Vec[Node]`, pinned below) and answers "terminates" for
/// every cycle of length two or more — then stops the walk, so
/// `Subcommand`'s edge back to `Outer` was never examined. Measured on
/// fda65f4, this program overflowed the compiler's stack (13 000 frames).
#[test]
fn test_e2e_mutual_type_cycle_through_vec_compiles() {
    let out = run_program(
        r#"
struct Inner { owner: Outer, tag: String }
struct Outer { kids: Vec[Inner], name: String }

fn count(o: Outer) -> i64 { o.kids.len() }

fn main() {
    let o = Outer { kids: [], name: "root" };
    println(count(o));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0");
    }
}

/// The DIRECT cycle B-2026-07-28-3 already handled, kept alongside its
/// mutual sibling so a future edit to the guard cannot fix one by
/// regressing the other.
#[test]
fn test_e2e_direct_type_cycle_through_vec_compiles() {
    let out = run_program(
        r#"
struct Node { kids: Vec[Node], name: String }

fn count(n: Node) -> i64 { n.kids.len() }

fn main() {
    let n = Node { kids: [], name: "r" };
    println(count(n));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0");
    }
}

// ── Owned Vec/String param moved into a local (kata-23, 2026-06-07) ──
//
// `let mut work = lists;` / `work = lists;` where `lists` is a bare
// by-value Vec/String param is a RETAINING consume site like the
// kata-22 family above: the caller keeps the buffer's scope-exit
// free, so the let/assign-move must deep-copy instead of arming the
// new binding as a second owner of the same buffer. Before the fix
// the caller's free and the moved binding's free double-freed —
// whether that trapped (exit 133/134), lost buffered stdout, or
// passed silently was pure allocator luck, which is why kata-23's
// ten cases split unpredictably. The param's header must stay
// intact after the copy (no cap-zero suppression) so later consume
// sites of the same param still see an owned buffer.

#[test]
fn test_e2e_owned_vec_param_let_move_then_index() {
    // kata-23 merge_k_lists shape, minimized: param vec of
    // Option[shared] moved to a mut local, elements read/merged/
    // re-assigned in place, slot 0 returned, caller walks the chain.
    let out = run_program(
        r#"
shared struct ListNode {
    val: i64,
    mut next: Option[ListNode],
}

fn pick(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {
    if let Some(n1) = l1 {
        let _ = n1.val;
        l1
    } else {
        l2
    }
}

fn merge_k(lists: Vec[Option[ListNode]]) -> Option[ListNode] {
    let mut work = lists;
    let k = work.len();
    if k == 0 {
        return None;
    }
    let mut interval = 1;
    while interval < k {
        let mut i = 0;
        while i + interval < k {
            work[i] = pick(work[i].clone(), work[i + interval].clone());
            i = i + 2 * interval;
        }
        interval = 2 * interval;
    }
    work[0]
}

fn main() {
    let n3 = ListNode { val: 3, next: None };
    let n2 = ListNode { val: 2, next: Some(n3) };
    let n1 = ListNode { val: 1, next: Some(n2) };
    let mut v: Vec[Option[ListNode]] = Vec.new();
    v.push(Some(n1));
    v.push(None);
    v.push(None);
    let mut cur = merge_k(v);
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
        assert_eq!(out.trim(), "1\n2\n3");
    }
}

#[test]
fn test_e2e_owned_vec_param_let_move_param_reusable() {
    // The param header survives the move-copy: a SECOND consume of
    // the same param (here another let-move) still sees cap > 0 and
    // copies again. Pre-fix, the first move's cap-zero suppression
    // turned the second binding into a cap=0 alias whose owner was
    // ambiguous.
    let out = run_program(
        r#"
fn twice(v: Vec[i64]) -> i64 {
    let a = v;
    let b = v;
    a[0] + b[1]
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(32);
    println(twice(v));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_owned_vec_param_assign_move() {
    // Assign-arm sibling: `work = lists;` onto an existing tracked
    // binding deep-copies the same way (and the LHS's prior buffer
    // is eagerly freed, not leaked).
    let out = run_program(
        r#"
fn second(v: Vec[i64]) -> i64 {
    let mut work: Vec[i64] = Vec.new();
    work.push(0);
    work = v;
    work[1]
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(7);
    v.push(9);
    println(second(v));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "9");
    }
}

#[test]
fn test_e2e_vec_of_struct_with_shared_and_option_field() {
    // B-2026-06-19 (phase-12 parser slice 2a): a `Vec[Arg]` whose element
    // struct `Arg { label: Option[String], value: Expr }` holds BOTH a
    // shared-enum field (`value`) AND an `Option[String]` field (`label`),
    // the Vec living inside a shared-enum struct payload
    // (`Call(CallNode { args: Vec[Arg] })`) — the self-hosted parser's
    // `Call(CallExpr { args: Vec[CallArg] })` shape exactly. Pins
    // value-correctness of build + recursive read + by-value consume on
    // that shape (the per-element shared field's box and the Option[String]
    // label were both being leaked by the Vec-element value drop — neither
    // is freed by `__karac_drop_struct_<S>`; the drop walker now routes a
    // shared-owning struct element through `__karac_vec_elem_full_drop_<S>`
    // and frees an `Option[String]` field directly). Leak-freedom under
    // LSan is pinned by the asan_* peer in tests/memory_sanitizer.rs.
    if let Some(out) = run_program(
            "shared enum Expr { Lit(LitNode), Call(CallNode) }\n\
             struct LitNode { name: String, val: i64 }\n\
             struct Arg { label: Option[String], value: Expr }\n\
             struct CallNode { callee: Expr, args: Vec[Arg] }\n\
             fn lit(s: String, v: i64) -> Expr { Expr.Lit(LitNode { name: s, val: v }) }\n\
             fn sum(e: Expr) -> i64 {\n\
                 match e {\n\
                     Lit(n) => n.val,\n\
                     Call(c) => {\n\
                         let mut acc = sum(c.callee);\n\
                         for a in c.args { acc = acc + sum(a.value); }\n\
                         acc\n\
                     }\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut args: Vec[Arg] = Vec.new();\n\
                 args.push(Arg { label: Some(\"label_one\".to_string()), value: lit(\"v1\".to_string(), 10) });\n\
                 args.push(Arg { label: None, value: lit(\"v2\".to_string(), 20) });\n\
                 let call = Expr.Call(CallNode { callee: lit(\"callee\".to_string(), 100), args: args });\n\
                 println(sum(call).to_string());\n\
             }",
        ) {
            assert_eq!(out.trim(), "130");
        }
}

#[test]
fn b34_value_enum_drop_with_nested_struct_vec_shared_field_compiles() {
    // B-2026-06-14-34 — the B-31 `Vec[shared]` drain arm in
    // `emit_nested_struct_shared_rc_decs` (plus its sibling `shared`/Option
    // arms) appends basic blocks to `self.current_fn`. When that walker is
    // reached through the VALUE-drop synthesizer `emit_enum_drop_switch`
    // (here: a NON-shared enum `Stmt` whose `Call` variant wraps a
    // `CallExpr` struct that owns a `Vec[Expr]` of `shared` enum elements),
    // `current_fn` historically still pointed at the OUTER fn that triggered
    // drop synthesis (`use_stmt`), not the `__karac_drop_Stmt` fn the
    // builder was emitting into. The walker's `nstr.*` blocks then landed in
    // `use_stmt` while the surrounding switch's `br exit` referenced
    // `__karac_drop_Stmt` — a cross-function basic-block reference that
    // failed module verification ("Referring to a basic block in another
    // function!"). The self-host lexer hit this via memoization order; the
    // pre-existing `asan_struct_wrapped_deep_build_and_vec_children` case
    // dodged it (its element drop was already memoized when the struct drop
    // synthesized). Fix: the value-drop synthesizers now set
    // `current_fn = drop_fn` for their whole body (mirroring the RC-drop
    // synthesizers), so the walker's `self.current_fn` append target is
    // always the fn being emitted. This test pins the cross-function shape
    // WITHOUT the full lexer so it can't silently regress.
    //
    // `ir_for` `.expect`s on codegen — a verification failure panics the
    // test. We additionally assert the drop fn was emitted and is internally
    // consistent (its `exit` block belongs to `__karac_drop_Stmt`).
    let ir = ir_for(
        "shared enum Expr { Num(i64), Add(BinOp) }\n\
             struct BinOp { left: Expr, right: Expr }\n\
             enum Stmt { Call(CallExpr) }\n\
             struct CallExpr { callee: Expr, args: Vec[Expr] }\n\
             fn eval(e: Expr) -> i64 {\n\
                 match e { Num(n) => n, Add(b) => eval(b.left) + eval(b.right) }\n\
             }\n\
             fn use_stmt(s: Stmt) -> i64 {\n\
                 match s {\n\
                     Call(c) => {\n\
                         let mut acc = eval(c.callee);\n\
                         for a in c.args { acc = acc + eval(a); }\n\
                         acc\n\
                     }\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut args: Vec[Expr] = Vec.new();\n\
                 let mut i: i64 = 0;\n\
                 while i < 3 { args.push(Num(i)); i = i + 1; }\n\
                 let s = Call(CallExpr { callee: Num(100), args: args });\n\
                 println(use_stmt(s));\n\
             }",
    );
    assert!(
        ir.contains("define internal void @__karac_drop_Stmt("),
        "expected the value-drop fn for the non-shared `Stmt` enum to be emitted; IR:\n{ir}"
    );
    // The walker's `nstr.*` blocks must live inside the drop fn, never in
    // `use_stmt`. A cross-function block reference would have aborted in
    // `ir_for` already; this guards against a future silent relocation.
    let use_stmt_body = ir
        .split("define ")
        .find(|chunk| chunk.contains("@use_stmt("))
        .expect("use_stmt definition present");
    assert!(
            !use_stmt_body.contains("nstr."),
            "drop-walker `nstr.*` blocks leaked into @use_stmt — current_fn append target regressed; IR:\n{ir}"
        );
}

#[test]
fn e2e_vec_filled_2d_table_rows_independent() {
    // Vec.filled(rows, Vec.filled(cols, x)) must give each row its OWN
    // buffer. Before the per-slot deep-clone fix this AOT-SIGTRAPped (rows
    // aliased one buffer → corruption + N-fold free). Write distinct values
    // per cell and read several back.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let n = 3i64;\n\
                 let mut dp: Vec[Vec[i64]] = Vec.filled(n, Vec.filled(n, 0i64));\n\
                 let mut i = 0i64;\n\
                 while i < n {\n\
                     let mut j = 0i64;\n\
                     while j < n { dp[i][j] = i * 10i64 + j; j = j + 1i64; }\n\
                     i = i + 1i64;\n\
                 }\n\
                 println(f\"{dp[0][0]} {dp[1][2]} {dp[2][1]} {dp[0][2]}\");\n\
             }",
    ) {
        assert_eq!(out, "0 12 21 2\n");
    }
}

#[test]
fn e2e_multi_assign_swaps_vec_slots() {
    // Index targets: `v[i], v[j] = v[j], v[i];` swaps two heap-vec slots —
    // the in-place idiom swap-sorts and swap-permutations rely on.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(10i64); v.push(20i64); v.push(30i64);\n\
                 let i = 0i64;\n\
                 let j = 2i64;\n\
                 v[i], v[j] = v[j], v[i];\n\
                 println(f\"{v[0]} {v[1]} {v[2]}\");\n\
             }",
    ) {
        assert_eq!(out, "30 20 10\n");
    }
}

#[test]
fn test_e2e_generic_mono_vec_param_for_loop_and_methods() {
    // B-2026-07-02-11: a `for x in xs` over a `Vec` param inside ANY
    // generic mono silently compiled to nothing (unknown-iterable
    // fallback skips the body), and `xs.len()` / `xs[i]` failed loudly.
    // The mono prologue now registers collection side-tables for params
    // like `compile_function` does. Covers owned AND `ref` forms (the
    // ref form also needed the direct-call pointer ABI for ref args).
    let out = run_program(
        "fn showall[A](xs: Vec[i64], tag: A) -> A {\n\
                 println(xs.len());\n\
                 println(xs[1]);\n\
                 for x in xs {\n\
                     println(x);\n\
                 }\n\
                 return tag;\n\
             }\n\
             fn showref[A](xs: ref Vec[i64], tag: A) -> A {\n\
                 println(xs.len());\n\
                 for x in xs {\n\
                     println(x);\n\
                 }\n\
                 return tag;\n\
             }\n\
             fn main() {\n\
                 println(showall(vec![5, 6], 9));\n\
                 let v = vec![7, 8];\n\
                 println(showref(v, 3));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out, "2\n6\n5\n6\n9\n2\n7\n8\n3\n");
    }
}

/// B-2026-07-26-1: the bounded-accumulator elision must be SEMANTICS-
/// PRESERVING, not merely fast. Two halves:
///
/// * the qualifying `let cnt = 0; while i < n { if … { cnt = cnt + 1 } }`
///   shape still computes the right count with the trap removed;
/// * the SAME loop shape with a non-zero initializer is rejected by the
///   analysis and therefore still traps. That is the behavioural pin on
///   the `init == 0` precondition — the bound is `acc <= init + trip`, so
///   a non-zero start is exactly the case that can still overflow.
#[test]
fn bounded_accumulator_elision_preserves_semantics() {
    let out = run_program(
        "fn count_set(nums: ref Vec[i64], b: i64) -> i64 {\n\
                 let mut cnt = 0i64;\n\
                 let mut i = 0i64;\n\
                 while i < nums.len() {\n\
                     if ((nums[i] >> b) & 1i64) == 1i64 {\n\
                         cnt = cnt + 1i64;\n\
                     }\n\
                     i = i + 1i64;\n\
                 }\n\
                 cnt\n\
             }\n\
             fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(1i64); v.push(2i64); v.push(3i64); v.push(7i64); v.push(8i64);\n\
                 println(f\"{count_set(v, 0i64)} {count_set(v, 1i64)} {count_set(v, 3i64)}\");\n\
             }\n",
    );
    // bit0 set in {1,3,7} = 3; bit1 in {2,3,7} = 3; bit3 in {8} = 1.
    assert_eq!(out.as_deref(), Some("3 3 1\n"));
}

/// B-2026-08-15-7 — a fresh-owned `Vec` temporary into a generic fn's owned
/// param, across the spellings that decide who frees it.
///
/// Also a regression guard rather than the detector: the values were right
/// before the fix and the leak is what
/// `asan_generic_owned_vec_param_temp_arg_no_leak` catches. The value this
/// adds is over the DOUBLE-FREE direction — `passthru` and `pick` (a
/// forwarding tail) are the shapes where the caller-side drop this fix adds
/// could collide with the result consumer's, and a collision that ASAN sees
/// as a double free would show here as a corrupted read. `nums` and `ns` are
/// re-read at the end: the callee must have deep-copied, so the caller's own
/// bindings are still intact after being cloned into six calls.
#[test]
fn test_e2e_generic_owned_vec_param_temp_arg() {
    assert_eq!(
        run_program(
            "fn take[T](v: Vec[T]) -> i64 { return v.len(); }\n\
                 fn head[T](v: Vec[T]) -> T { return v[0]; }\n\
                 fn take_i(v: Vec[i64]) -> i64 { return v.len(); }\n\
                 fn passthru[T](v: Vec[T]) -> Vec[T] { return v; }\n\
                 fn id[T](v: Vec[T]) -> Vec[T] { return v; }\n\
                 fn pick[T](a: Vec[T], b: Vec[T]) -> Vec[T] { return id(a); }\n\
                 fn sink[T](acc: mut ref Vec[Vec[T]], v: Vec[T]) { acc.push(v); }\n\
                 fn two[T](v: Vec[i64], t: T) -> i64 { return v.len(); }\n\
                 fn mk() -> Vec[i64] { let v: Vec[i64] = [10, 20, 30]; return v; }\n\
                 fn main() {\n\
                     let nums: Vec[i64] = [1, 2, 3, 4, 5];\n\
                     let ns: Vec[String] = [\"alpha\", \"beta\", \"gamma\"];\n\
                     println(take(nums.clone()));\n\
                     println(take(ns.clone()));\n\
                     println(head(nums.clone()));\n\
                     println(head(ns.clone()));\n\
                     println(take_i(nums.clone()));\n\
                     println(take(mk()));\n\
                     println(take([7, 8, 9]));\n\
                     let p = passthru(nums.clone());\n\
                     println(p[2]);\n\
                     let q = passthru(ns.clone());\n\
                     println(q[1]);\n\
                     let w = pick(nums.clone(), mk());\n\
                     println(w[0]);\n\
                     let mut acc: Vec[Vec[i64]] = Vec.new();\n\
                     sink(mut acc, nums.clone());\n\
                     sink(mut acc, mk());\n\
                     println(acc.len());\n\
                     println(acc[0][1]);\n\
                     println(acc[1][2]);\n\
                     println(two(nums.clone(), 5));\n\
                     println(nums.len());\n\
                     println(ns[0]);\n\
                 }\n"
        ),
        Some("5\n3\n1\nalpha\n5\n3\n3\n3\nbeta\n1\n2\n2\n30\n5\n5\nalpha\n".to_string())
    );
}

#[test]
fn test_e2e_vec_equality_compares_contents_not_bytes() {
    assert_eq!(
        run_program(
            r#"
fn cmp_ref(a: ref Vec[i64], b: ref Vec[i64]) -> bool { return a == b; }
fn main() {
    let mut a: Vec[i64] = Vec.new();
    a.push(1);
    a.push(2);
    let mut b: Vec[i64] = Vec.new();
    b.push(1);
    b.push(2);
    let mut c: Vec[i64] = Vec.new();
    c.push(1);
    c.push(9);
    let mut d: Vec[i64] = Vec.new();
    d.push(1);
    let e: Vec[i64] = Vec.new();
    println(f"{a == b}");
    println(f"{a == c}");
    println(f"{a == d}");
    println(f"{e == a}");
    println(f"{a != c}");
    println(f"{cmp_ref(a, c)}");
    let mut p: Vec[String] = Vec.new();
    p.push("hello");
    p.push("world");
    let mut q: Vec[String] = Vec.new();
    q.push("hello");
    q.push("world");
    let mut r: Vec[String] = Vec.new();
    r.push("hello");
    r.push("there");
    println(f"{p == q}");
    println(f"{p == r}");
}
"#,
        ),
        // a==b, a==c, a==d, e==a, a!=c, cmp_ref(a,c), p==q, p==r
        Some("true\nfalse\nfalse\nfalse\ntrue\nfalse\ntrue\nfalse\n".to_string())
    );
}

/// B-2026-08-27-13 — `Vec.contains` compared an ENUM element by its raw
/// payload words, reaching neither of the compiler's two structural enum
/// comparators, so it answered `false` for an element that IS in the vector.
///
/// WHY NEITHER WAS REACHABLE, which is the whole shape of the bug: the site
/// loaded `data[i]` and the needle as VALUES and called `compile_binop`.
/// `compile_enum_eq` is selected at the `==` OPERATOR site by running
/// `enum_name_of_expr` over the operand EXPRESSIONS, which a loaded value does
/// not carry; `emit_eq_fn_for_type_expr` — the one `Map`/`Set` use — takes
/// pointers, not values. So the aggregate fell to `compile_struct_eq`, whose
/// per-field recursion over an enum's `{tag, w0, w1, …}` compares payload words
/// as i64s.
///
/// THE STRING AND STRUCT LINES ARE THE CONTROLS, and they are why this looked
/// fine almost everywhere: `compile_struct_eq` recurses per FIELD, so a
/// `String` element and a struct with typed fields both compared by content
/// already. An enum aggregate's fields are raw payload words, and recursing
/// per field over those is a pointer compare wearing a field walk.
/// `enum-unit` and `int` are the second kind of control — wholly inline, so
/// the word compare was already right for them.
///
/// EVERY PAYLOAD IS BUILT BY A FUNCTION CALL, never a literal. Two identical
/// string literals can be folded to one global, which would let a pointer
/// compare answer `true` and hide the defect; `mk(n)` forces distinct
/// allocations, so a pointer compare must answer `false`.
///
/// `generic` and `shared` are what the one-comparator routing buys beyond the
/// filed shape: both now go through the same fixed path rather than needing
/// their own arm here.
#[test]
fn test_e2e_vec_contains_matches_an_enum_element_by_content() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
struct S { a: i64, s: String }
#[derive(Hash, Eq, PartialEq)]
enum E { A(String), Pair(i64, String), B }
#[derive(Hash, Eq, PartialEq)]
shared struct Sh { id: i64, name: String }

fn mk(n: i64) -> String { return f"v{n}"; }

fn main() {
    let ve: Vec[E] = vec![E.A(mk(1)), E.Pair(2, mk(3)), E.B];
    println(f"enum={ve.contains(E.A(mk(1)))}");
    println(f"enum-ne={ve.contains(E.A(mk(2)))}");
    println(f"enum-pair={ve.contains(E.Pair(2, mk(3)))}");
    println(f"enum-pair-ne={ve.contains(E.Pair(2, mk(4)))}");
    println(f"enum-unit={ve.contains(E.B)}");

    let vo: Vec[Option[String]] = vec![Some(mk(5)), None];
    println(f"generic={vo.contains(Some(mk(5)))}");
    println(f"generic-ne={vo.contains(Some(mk(6)))}");
    println(f"generic-none={vo.contains(None)}");

    let vh: Vec[Sh] = vec![Sh { id: 1, name: mk(7) }];
    println(f"shared={vh.contains(Sh { id: 1, name: mk(7) })}");
    println(f"shared-ne={vh.contains(Sh { id: 1, name: mk(8) })}");

    let vs: Vec[String] = vec![mk(9), mk(10)];
    println(f"str={vs.contains(mk(9))}");
    println(f"str-ne={vs.contains(mk(11))}");
    let vt: Vec[S] = vec![S { a: 1, s: mk(12) }];
    println(f"struct={vt.contains(S { a: 1, s: mk(12) })}");
    println(f"struct-ne={vt.contains(S { a: 1, s: mk(13) })}");
    let vi: Vec[i64] = vec![1, 2];
    println(f"int={vi.contains(2)}");
    println(f"int-ne={vi.contains(3)}");
}
"#
        ),
        Some(
            "enum=true\nenum-ne=false\nenum-pair=true\n\
                 enum-pair-ne=false\nenum-unit=true\ngeneric=true\n\
                 generic-ne=false\ngeneric-none=true\nshared=true\n\
                 shared-ne=false\nstr=true\nstr-ne=false\nstruct=true\n\
                 struct-ne=false\nint=true\nint-ne=false\n"
                .to_string()
        )
    );
}

/// B-2026-08-27-3 — the compiled twin of
/// `removing_a_shared_key_releases_it_through_the_refcount`, same bytes.
///
/// The `shared` key is the one key shape whose release is a REFCOUNT DEC
/// rather than a drop fn, and `remove` was giving back nothing at all — the
/// control block and everything under it leaked, one per removal
/// (`asan_map_remove_with_a_shared_key_is_balanced` holds that half).
///
/// Two shapes because the dec is conditional and only the count knows which
/// outcome is due. With no other reference the body runs AT the remove;
/// with a live alias it must wait for that alias's own end. Both were
/// wrong in the same way at one point in this fix: calling the body BESIDE
/// the dec instead of leaving it inside `emit_rc_dec`'s zero edge printed
/// `dropK` twice here and would have run it early there.
#[test]
fn test_e2e_remove_releases_a_shared_key_through_the_refcount() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
shared struct K { id: i64 }
impl Drop for K { fn drop(mut ref self) { println(f"dropK {self.id}") } }
fn main() {
    let mut m: Map[K, i64] = Map.new();
    let k = K { id: 1 };
    m.insert(k, 1);
    println("--before--");
    m.remove(k);
    println("--after--");
}
"#
        ),
        Some("--before--\ndropK 1\n--after--\n".to_string())
    );
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
shared struct K { id: i64 }
impl Drop for K { fn drop(mut ref self) { println(f"dropK {self.id}") } }
fn main() {
    let mut m: Map[K, i64] = Map.new();
    let k = K { id: 1 };
    let keep = k;
    m.insert(k, 1);
    println("--before--");
    m.remove(keep);
    println("--after-remove--");
    println(f"keep still {keep.id}");
    println("--end--");
}
"#
        ),
        Some("--before--\n--after-remove--\nkeep still 1\ndropK 1\n--end--\n".to_string())
    );
}

/// B-2026-08-27-2 — the compiled twin of
/// `removing_an_entry_runs_the_key_or_element_drop_body`, same bytes.
///
/// `remove` is the site B-2026-08-26-41 left out, because its fix is not
/// the whole-table walk the other four sites take. It destroys ONE entry's
/// key IN PLACE, and the body has to run on that key while its fields are
/// still readable — after the bucket is located, before the runtime frees
/// it. Codegen cannot reach that window, so the body travels INTO the
/// runtime as a function pointer: `karac_map_remove_old_with_key_drop_fn`
/// invokes it between `lookup` and `free_stored_key`.
///
/// A SEPARATE runtime symbol rather than a fifth parameter on
/// `karac_map_remove_old`: widening a signature keeps the name, so an
/// archive built before the change still links and is handed one argument
/// too few. A new name makes that staleness the undefined-reference the
/// E2E harness already reports.
///
/// All four containers, because all four were affected — a Set's ELEMENT
/// is the key half, so `Set.remove` ran no body at all. The VALUE's body
/// was already correct everywhere: it moves out into the returned
/// `Some(old)` and drops at the caller, which is why key-before-value here
/// is a consequence rather than an imposed order.
///
/// B-2026-09-15-5 UPDATED THE EXPECTATION: each `remove` now prints TWO
/// key bodies. The argument is a struct LITERAL -- a fresh temporary built,
/// hashed and discarded at the call -- so it owes a body of its own,
/// distinct from the STORED key B-2026-08-27-2 was about. The old
/// single-`dropK` expectation was this defect written down.
#[test]
fn test_e2e_remove_runs_the_key_drop_body() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq, Ord)]
struct K { n: i64 }
struct V { n: i64 }
impl Drop for K { fn drop(mut ref self) { println(f"dropK {self.n}") } }
impl Drop for V { fn drop(mut ref self) { println(f"dropV {self.n}") } }
fn main() {
    let mut m: Map[K, V] = Map.new();
    m.insert(K { n: 1 }, V { n: 1 });
    m.remove(K { n: 1 });
    println(f"map len={m.len()}")
    let mut sm: SortedMap[K, V] = SortedMap.new();
    sm.insert(K { n: 2 }, V { n: 2 });
    sm.remove(K { n: 2 });
    println(f"smap len={sm.len()}")
    let mut st: Set[K] = Set.new();
    st.insert(K { n: 3 });
    st.remove(K { n: 3 });
    println(f"set len={st.len()}")
    let mut ss: SortedSet[K] = SortedSet.new();
    ss.insert(K { n: 4 });
    ss.remove(K { n: 4 });
    println(f"sset len={ss.len()}")
}
"#
        ),
        Some(
            "dropK 1\ndropK 1\ndropV 1\nmap len=0\n\
                 dropK 2\ndropK 2\ndropV 2\nsmap len=0\n\
                 dropK 3\ndropK 3\nset len=0\n\
                 dropK 4\ndropK 4\nsset len=0\n"
                .to_string()
        )
    );
}

/// B-2026-08-18-6 — `#[derive(PartialEq)]` over a struct with a `Vec`
/// field. `type_supports_eq` and `type_supports_hash` both carried an
/// explicit `Vec` arm and `type_supports_partial_eq` did not, so the
/// WEAKER trait was the stricter gate: the `Eq` spelling of this program
/// compiled while the `PartialEq` spelling was refused at check time —
/// which also meant this lowering had never been exercised.
///
/// The assertions are B-2026-08-12-5's own shapes, where comparing bytes
/// instead of elements is SILENTLY wrong: `[7] == [263]` (same low byte)
/// and `[1,2] == [1,3]` must both be false, and equal `Vec[String]`
/// contents in distinct allocations must be true. Paired with
/// `test_derive_partial_eq_vec_field_oracle` in `tests/interpreter.rs`.
#[test]
fn test_e2e_derive_partial_eq_vec_field_compares_contents() {
    assert_eq!(
        run_program(
            r#"
#[derive(PartialEq)]
struct I { v: Vec[i64] }
#[derive(PartialEq)]
struct S { v: Vec[String] }
fn one(a: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(a); v }
fn two(a: i64, b: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(a); v.push(b); v }
fn strs(a: String) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(a); v }
fn main() {
    println(I { v: one(7) } == I { v: one(263) });
    println(I { v: two(1, 2) } == I { v: two(1, 3) });
    println(I { v: one(7) } == I { v: one(7) });
    println(I { v: one(1) } == I { v: two(1, 2) });
    println(S { v: strs("abcd") } == S { v: strs("abcd") });
    println(S { v: strs("abcd") } == S { v: strs("zzzz") });
    println(I { v: two(1, 2) } != I { v: two(1, 3) });
}
"#
        ),
        Some("false\nfalse\ntrue\nfalse\ntrue\nfalse\ntrue\n".to_string())
    );
}

/// B-2026-08-15-14 — a `Vec[shared struct]` clone used as a TEMPORARY
/// leaked one RC box per element per temp (the leak gate itself lives in
/// `tests/memory_sanitizer.rs`, which is where LSan runs). This is the
/// OBSERVABLE twin: the program must compute the right answers and,
/// critically, the ORIGINAL Vec must stay intact and readable after five
/// clone-temps have been created and dropped. That second half is what an
/// over-eager fix breaks — one dec too many per temp drives the shared
/// count to zero while `ns` still holds it, and `ns[0].label` then reads
/// freed memory. Paired with an interpreter oracle of the same program in
/// `tests/interpreter.rs`, so run-vs-build parity is pinned from both ends.
///
/// Both spellings of the temp appear on purpose: the by-value call arg
/// (fixed in fd7254d) and the `.clone().len()` chain (fixed here) reach
/// two different registration sites.
#[test]
fn test_e2e_vec_shared_struct_clone_temp_original_survives() {
    assert_eq!(
        run_program(
            r#"
shared struct Node { label: String }
fn agg(ns: Vec[Node]) -> i64 { return ns.len(); }
fn main() {
    let mut ns: Vec[Node] = Vec.new();
    ns.push(Node { label: "alpha" });
    ns.push(Node { label: "beta" });
    ns.push(Node { label: "gamma" });
    let mut k = 0;
    let mut t = 0;
    while k < 5 { t = t + agg(ns.clone()); k = k + 1; }
    println(t);
    println(ns.clone().len());
    println(ns[0].label);
    println(ns[2].label);
    println(ns.len());
}
"#
        ),
        Some("15\n3\nalpha\ngamma\n3\n".to_string())
    );
}

/// B-2026-08-21-10 — `Vec.from_fn(n, f)`, end to end.
///
/// The element type must be settled BEFORE the buffer is allocated,
/// because the buffer is sized and strided at `sizeof(elem)` — a wrong
/// width mis-aligns every slot, which is exactly how `Vec.filled` failed
/// in B-2026-07-08-10 (a silent exit-0 wrong output, not a crash). So the
/// sweep mixes a body whose type is inferred from the body itself with one
/// pushed down from an annotation, and reads back interior elements rather
/// than just the length.
#[test]
fn test_e2e_vec_from_fn_computes_each_slot_by_index() {
    let src = r#"
fn main() {
    // design.md's own example.
    let evens = Vec.from_fn(5, |i| i * 2);
    println(evens.len());
    println(evens[0]);
    println(evens[4]);

    // Zero elements: no slot is ever written.
    let e: Vec[i64] = Vec.from_fn(0, |i| i);
    println(e.len());

    // A RUNTIME count, so the loop bound is not a constant to fold.
    let n = 4;
    let sq = Vec.from_fn(n, |i| i * i);
    println(sq[3]);

    // The function is a closure: an outer binding it captures must be live at
    // every index, not only the first.
    let base = 100;
    let off = Vec.from_fn(3, |i| base + i);
    println(off[2]);

    // Element type pushed down from the annotation — a NARROW element whose
    // stride differs from the body's natural i64 width.
    let narrow: Vec[i32] = Vec.from_fn(3, |i| 7);
    println(narrow[0]);
    println(narrow[2]);

    // Nested collection element.
    let rows: Vec[Vec[i64]] = Vec.from_fn(2, |i| Vec.filled(2, i));
    println(rows[1][0]);
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("5\n0\n8\n0\n9\n102\n7\n7\n1\n")
    );
}

/// A heap element through the same path. The produced value is MOVED into
/// the buffer, so the body's own temporary must not also be freed at scope
/// exit — measured before the disarm as "free(): double free detected in
/// tcache 2" under both AOT and JIT while `--interp` printed correctly.
/// The leak/UAF side is pinned under ASAN in tests/memory_sanitizer.rs;
/// this pins that the VALUES survive.
#[test]
fn test_e2e_vec_from_fn_carries_a_heap_element() {
    let src = r#"
fn main() {
    let names: Vec[String] = Vec.from_fn(3, |i| f"n{i}");
    println(names[0]);
    println(names[2]);
    println(names.len());
    let built: Vec[String] = Vec.from_fn(2, |i| i.to_string());
    println(built[1]);
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("n0\nn2\n3\n1\n"));
}

/// B-2026-09-21-15 — a `Vec` enum payload whose ELEMENT is a generic
/// struct keeps its vec shape when bound in a match arm.
///
/// `pattern_binding_inner_types` holds the binding's own type for some
/// binding kinds and the ELEMENT type for a container (that is what the
/// `Array` / `Vector` arms read it for), and three separate sites asked
/// "is this a concretely-instantiated generic struct?" of it without
/// checking which. A `Vec[G[i64]]` binding records `bt = "Vec"` beside
/// `inner = G[i64]`, so all three answered with `G[i64]` and all three
/// outranked the explicit `"Vec" => 3 words / vec_struct_type()` arms
/// below them: the payload was sized at 1 word, which made the BOXED
/// payload look inline, and the deboxed `{ ptr, i64, i64 }` was rebuilt as
/// G's 1-field `{ i64 }` keeping only word 0. The binding held the buffer
/// POINTER where its length belonged, so `v.len()` returned an address —
/// an unbounded drop walk, then a SIGSEGV or `free(): double free detected
/// in tcache 2`, on a program `--interp` ran correctly.
///
/// The `b:` cell is the one-character control: `P` is the same struct with
/// its type parameter removed, and it always compiled correctly. It is
/// here so a regression that reaches only the generic side is still
/// distinguishable from one that breaks both.
///
/// No `Drop` impl is needed to trigger this — `a:` through `e:` carry
/// none. The `f:` cell adds one only to show the element bodies still fire
/// once each once the binding has the right shape.
///
/// The three sites are now one predicate
/// (`generic_struct_binding_type_expr`), which tests that the recorded
/// surface name agrees with the `TypeExpr`'s head — the thing the tier
/// always meant, and true of B-2026-07-12-2's `bt = "Wrap"` beside
/// `inner = Wrap[String]` that it exists for.
#[test]
fn e2e_vec_payload_of_generic_struct_elements_keeps_its_vec_shape() {
    let src = r#"
struct G[T] { v: T }
struct P { v: i64 }
enum Slot[T] { S(T), N }

struct D { id: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}") } }
struct Gd[T] { d: T }

fn main() {
    let a: Vec[G[i64]] = [G { v: 1 }, G { v: 2 }];
    match Slot.S(a) { Slot.S(v) => { println(f"a:{v.len()}:{v[1].v}") } Slot.N => { println("no") } }

    let b: Vec[P] = [P { v: 3 }, P { v: 4 }];
    match Slot.S(b) { Slot.S(v) => { println(f"b:{v.len()}:{v[1].v}") } Slot.N => { println("no") } }

    let c: Vec[G[i64]] = [G { v: 5 }, G { v: 6 }];
    let s: Slot[Vec[G[i64]]] = Slot.S(c);
    match s { Slot.S(v) => { println(f"c:{v.len()}:{v[0].v}") } Slot.N => { println("no") } }

    let d: Vec[G[String]] = [G { v: "ab" }, G { v: "cd" }];
    match Option.Some(d) { Option.Some(v) => { println(f"d:{v.len()}:{v[1].v}") } Option.None => { println("no") } }

    let e: Vec[G[i64]] = [G { v: 7 }, G { v: 8 }];
    if let Slot.S(v) = Slot.S(e) { println(f"e:{v.len()}:{v[0].v}") }

    let f: Vec[Gd[D]] = [Gd { d: D { id: 1 } }, Gd { d: D { id: 2 } }];
    match Slot.S(f) { Slot.S(v) => { println(f"f:{v.len()}") } Slot.N => { println("no") } }

    println("end");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("a:2:2\nb:2:4\nc:2:5\nd:2:cd\ne:2:7\nf:2\ndD1\ndD2\nend\n")
    );
}

#[test]
/// B-2026-09-23-38 — a tuple member read TWO or more hops below a container
/// element (`v[0].1.0` over `Vec[(i64, (String, i64))]`) cloned the whole
/// intermediate tuple at the inner hop, registered no cleanup for a tuple
/// clone, and handed the leaf out of it: one leaked String per
/// `println(v[0].1.0)`. The chain is now walked to its container read and only
/// the LEAF is cloned, with the same cleanup and consuming-destination takeover
/// the one-hop `v[0].0` read has. Legs: non-consuming reads in a loop, eight
/// consuming destinations, three hops, a `Vec` leaf, and the whole inner tuple.
fn test_e2e_nested_container_tuple_index_read_matches_interp() {
    let out = run_program(
        r#"
struct H { s: String }
fn take(s: String) -> i64 { s.len() }
fn pick(v: ref Vec[(i64, (String, i64))]) -> String { v[0].1.0 }
fn mkv() -> Vec[(i64, (String, i64))] { [(1, (f"alpha-{1}-long-enough-to-heap", 7)), (2, (f"beta-{2}-long-enough-to-heap", 8))] }
fn leg_reads() {
    let v = mkv();
    let mut i = 0;
    while i < 3 { println(v[1].1.0); i = i + 1; }
    println(f"rd {v[0].1.0} {v[0].1.0.len()} {v[0].1.0 == "x"} {v[0].1.1}");
}
fn leg_consumers() {
    let v = mkv();
    let s = v[0].1.0;
    let mut o: Vec[String] = [];
    o.push(v[1].1.0);
    o.push(v[0].1.0);
    let h = H { s: v[1].1.0 };
    let t = (v[0].1.0, 3);
    let mut a = String.new();
    a = v[1].1.0;
    println(f"co {s} {o.len()} {o[1]} {h.s} {t.0} {a} {take(v[0].1.0)} {pick(v)}");
    println(f"co {v[0].1.0} {v[1].1.0}");
}
fn leg_three_levels() {
    let v: Vec[(i64, (i64, (String, i64)))] = [(1, (2, (f"gamma-{3}-long-enough-to-heap", 7)))];
    println(v[0].1.1.0);
    let s = v[0].1.1.0;
    println(f"tl {s} {v[0].1.1.1}");
}
fn leg_vec_leaf() {
    let v: Vec[(i64, (Vec[String], i64))] = [(1, ([f"delta-{4}-long-enough-to-heap"], 7))];
    println(f"vl {v[0].1.0.len()}");
    let w = v[0].1.0;
    println(f"vl {w[0]} {v[0].1.0[0]}");
}
fn leg_whole_inner() {
    let v = mkv();
    let t = v[0].1;
    println(f"wi {t.0} {t.1} {v[0].1.0}");
}
fn main() {
    leg_reads();
    leg_consumers();
    leg_three_levels();
    leg_vec_leaf();
    leg_whole_inner();
    println("done");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "beta-2-long-enough-to-heap\nbeta-2-long-enough-to-heap\nbeta-2-long-enough-to-heap\nrd alpha-1-long-enough-to-heap 27 false 7\nco alpha-1-long-enough-to-heap 2 alpha-1-long-enough-to-heap beta-2-long-enough-to-heap alpha-1-long-enough-to-heap beta-2-long-enough-to-heap 27 alpha-1-long-enough-to-heap\nco alpha-1-long-enough-to-heap beta-2-long-enough-to-heap\ngamma-3-long-enough-to-heap\ntl gamma-3-long-enough-to-heap 7\nvl 1\nvl delta-4-long-enough-to-heap delta-4-long-enough-to-heap\nwi alpha-1-long-enough-to-heap 7 alpha-1-long-enough-to-heap\ndone\n",
            "every leg must match --interp; got {out:?}"
        );
    }
}

/// B-2026-09-24-2 — a field read on a struct reached through TUPLE hops below a
/// container element (`v[0].1.0.s`, `w[0].1.1.0.n`, `self.xs[0].1.0.s`, through
/// a `ref Vec` parameter) failed to build with "cannot resolve field": the
/// receiver's type was resolved for one tuple hop only. And a `match` / `if let`
/// that moves an enum field's payload out of the same place (`g2[0].1.0.k`, and
/// one hop up, `g1[0].1.k`) double-freed. Every spelling must build and print
/// what `--interp` prints.
#[test]
fn test_e2e_tuple_hop_struct_field_read_matches_interp() {
    let out = run_program(
        r#"
enum K { A(String), B }
struct H { s: String, n: i64 }
struct G { k: K, n: i64 }
struct C { xs: Vec[(i64, (H, i64))] }
impl C { fn show(ref self) -> String { f"c {self.xs[0].1.0.s} {self.xs[0].1.0.n}" } }
fn mk(i: i64) -> String { f"alpha-{i}-long-enough-to-heap" }
fn by_ref(v: ref Vec[(i64, (H, i64))]) -> i64 { v[0].1.0.s.len() + v[0].1.0.n }
fn field_reads() {
    let v: Vec[(i64, (H, i64))] = [(1, (H { s: mk(1), n: 9 }, 2))];
    let mut i = 0;
    while i < 2 { println(f"{v[0].1.0.n} {v[0].1.0.s} {v[0].1.0.s.len()}"); i = i + 1; }
    let a = v[0].1.0.s;
    println(f"a {a} {by_ref(v)}");
    let w: Vec[(i64, (i64, (H, i64)))] = [(1, (2, (H { s: mk(2), n: 8 }, 3)))];
    println(f"w {w[0].1.1.0.s} {w[0].1.1.0.n}");
    let c = C { xs: [(1, (H { s: mk(3), n: 7 }, 2))] };
    println(c.show());
}
fn enum_fields() {
    let g1: Vec[(i64, G)] = [(1, G { k: K.A(mk(4)), n: 6 })];
    let g2: Vec[(i64, (G, i64))] = [(1, (G { k: K.A(mk(5)), n: 5 }, 2))];
    match g1[0].1.k { K.A(s) => println(f"k1 {s.len()} {s}"), K.B => println("k1 b") }
    match g2[0].1.0.k { K.A(_) => println("k2 a"), K.B => println("k2 b") }
    let mut out: Vec[String] = [];
    match g1[0].1.k { K.A(s) => out.push(s), K.B => {} }
    match g2[0].1.0.k { K.A(s) => out.push(s), K.B => {} }
    if let K.A(s) = g2[0].1.0.k { out.push(s); }
    println(f"m {out.len()} {out[0]} {out[1]} {out[2]}");
    println(f"n {g1[0].1.n} {g2[0].1.0.n}");
}
fn main() {
    field_reads();
    enum_fields();
    println("done");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "9 alpha-1-long-enough-to-heap 27\n9 alpha-1-long-enough-to-heap 27\na alpha-1-long-enough-to-heap 36\nw alpha-2-long-enough-to-heap 8\nc alpha-3-long-enough-to-heap 7\nk1 27 alpha-4-long-enough-to-heap\nk2 a\nm 3 alpha-4-long-enough-to-heap alpha-5-long-enough-to-heap alpha-5-long-enough-to-heap\nn 6 5\ndone\n",
            "every tuple-hop field read must match --interp; got {out:?}"
        );
    }
}

// B-2026-09-24-6 — reading an `Option`/`Result` field out of a container
// element (`match g[0].o`, `let x = g[0].o`, `z = g[0].o`, and through a
// tuple hop) copies the payload and leaves the element intact, as the
// interpreter does; and a tuple holding a struct whose heap hangs off an
// `Option` field frees it.
#[test]
fn test_e2e_elem_optres_field_reads_copy_and_match_interp() {
    let out = run_program(
        r#"
struct G { o: Option[String], r: Result[String, i64], n: i64 }
fn mk(i: i64) -> String { f"alpha-{i}-long-enough-to-heap" }
fn mg(i: i64) -> G { G { o: Some(mk(i)), r: Ok(mk(i + 10)), n: i } }
fn cons(o: Option[String]) -> i64 { match o { Some(s) => s.len(), None => 0 } }
fn main() {
    let g: Vec[G] = [mg(1), mg(2)];
    let mut out: Vec[String] = [];
    match g[0].o { Some(s) => println(s), None => {} }
    match g[0].o { Some(s) => out.push(s), None => {} }
    if let Some(s) = g[0].o { out.push(s); }
    match g[1].r { Ok(s) => out.push(s), Err(_) => {} }
    let x = g[1].o;
    let y = g[1].o;
    let mut z: Option[String] = None;
    z = g[0].o;
    println(f"{out.len()} {cons(x)} {cons(y)} {cons(z)} {g[0].o.is_some()}");
    let h: Vec[(i64, G)] = [(1, mg(3))];
    match h[0].1.o { Some(s) => out.push(s), None => {} }
    match h[0].1.o { Some(s) => out.push(s), None => {} }
    let mut q: Option[String] = None;
    q = h[0].1.o;
    q = h[0].1.o;
    println(f"{out.len()} {cons(q)} {out[4]}");
    let t = (7, mg(4));
    println(f"{t.0} {t.1.n}");
}
"#,
    );
    assert_eq!(
        out.as_deref(),
        Some(
            "alpha-1-long-enough-to-heap\n3 27 27 27 true\n5 27 alpha-3-long-enough-to-heap\n7 4\n"
        ),
        "must match --interp"
    );
}
