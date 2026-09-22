//! drop bodies, destructors, drop ordering -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen drop_order::
//!
//! New fixtures about drop bodies, destructors, drop ordering belong in this file.

use super::*;

/// B-2026-09-03-35 — adding `impl[T] Drop for S[T]` to a clean generic
/// struct made it LEAK every heap field, the exact inverse of what the
/// declaration is for.
///
/// `has_user_drop` reads `drop_method_keys`, which is keyed by the impl
/// target's HEAD name, so a generic impl registers `Box3` and answers true.
/// The `karac_drop_<T>` WRAPPER, though, is only emitted when
/// `module.get_function("Box3.drop")` finds a symbol — and a generic impl's
/// methods are deferred to the mono pipeline, which instantiates from CALL
/// SITES. `drop` has none: it is reached only from the wrapper being built.
/// So no symbol, no wrapper, and `track_user_drop_var` returned having
/// registered nothing — while the true `has_user_drop` had already steered
/// the binding past the `StructDrop` arm it used to take. Both halves lost
/// from one cause; `Box3[String] { v, tag }` leaked both `String`s.
///
/// THIS IS AN IR ASSERTION, NOT AN ASAN FIXTURE, and that is forced rather
/// than preferred: the leak is `-O0`-ONLY. Measured on the pre-fix compiler,
/// the row's own repro loses 16 B in 2 blocks at `KARAC_OPT_LEVEL=0` and is
/// completely clean at the default `-O2`, where LLVM elides the allocations
/// outright — and a five-cell fixture built to defeat that (payloads read
/// through `contains`, lengths seeded from `env.args().len()`) still
/// measured 720 B + 168 B at `-O0` and zero at `-O2`. `memory_sanitizer`'s
/// harness compiles in-process with no opt-level knob, so a fixture there
/// would assert nothing on the very build CI runs. The emitted call is
/// opt-level-independent, so it is what the regression is pinned to.
///
/// The `$String` suffix is load-bearing: it is the PER-MONOMORPH drop, the
/// one that drains the concrete field layout. A name-shared
/// `__karac_drop_struct_Box3` would GEP the erased layout, where a bare-`T`
/// field is one word and every field after it sits at the wrong offset.
#[test]
fn generic_struct_with_user_drop_still_registers_its_memory_drop() {
    let src = r#"
struct Box3[T] { v: T, tag: String }
impl[T] Drop for Box3[T] { fn drop(mut ref self) { println("dB") } }
fn main() { let b: Box3[String] = Box3 { v: f"vvvvvvvv", tag: f"tttttttt" };
            println(f"v{b.v.len()}") }
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
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    let ir = compile_to_ir(&parsed.program, Some(&ownership), None).expect("codegen failed");
    assert!(
        ir.contains(r#"call void @"__karac_drop_struct_Box3$String""#),
        "the generic struct's binding registered no memory drop, so both \
             `String` fields leak — B-2026-09-03-35. `main` IR:\n{}",
        ir.lines()
            .skip_while(|l| !l.starts_with("define i32 @main"))
            .take_while(|l| *l != "}")
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// B-2026-09-04-4 — the BODY half of B-2026-09-03-35: an `impl[T] Drop for
/// S[T]` ran no body at all on the compiled backends.
///
/// `drop` is the ONE impl method a program never calls. Every other method
/// of a generic impl reaches `compile_generic_call` through a call in the
/// source; `Drop::drop` is reached only from the synthesized
/// `karac_drop_<T>` wrapper, which is built by looking up
/// `module.get_function("S.drop")` — a symbol the mono pipeline only emits
/// when a call site asks for it. So the declaration pass parked `S.drop` in
/// `generic_fns` and waited for a call that never came. The fix drives the
/// instantiation from the BINDING's recorded type instead, emitting
/// `S.drop$<concrete>` and a `karac_drop_S$<concrete>` wrapper around it.
///
/// MEASURED PRE-FIX, this exact fixture: the compiled backend printed
/// SEVEN fewer lines than `--interp` — `dB7`, `dB9`, `dB8`, `dG`, `dR3`,
/// `dH2`, `dB6`, i.e. every generic `Drop` body in the program, plus the
/// non-generic `dR3` that is a FIELD of one (the wrapper's field-body step
/// went with the wrapper). `dP`, the non-generic control, was the only body
/// that survived, and still is.
///
/// `two` IS THE CELL THAT DECIDES THE DESIGN, not an extra case.
/// `Box3[String]` and `Box3[i64]` are live in one function and must print
/// `dB9` and `dB8` — two different `tag` reads, at two different field
/// offsets, because a bare-`T` field WIDENS under a monomorph (`String` is
/// a 3-word triple, `i64` one word) and shifts every field after it. One
/// name-shared symbol cannot serve both, which is why the erased shortcut
/// this row's predecessor considered was ruled out.
///
/// `str` PINS PLACEMENT, not just presence: `dB7` must land between `v8`
/// and `after`, the binding's NLL last use — which is where `--interp`
/// fires it. That half needed its own fix, because the NLL last-use map was
/// gated on `user_drop_wrapper_fns` being non-empty and a program whose
/// only `Drop` impl is generic reaches the gate before any wrapper exists.
#[test]
fn e2e_generic_impl_drop_runs_its_body_per_monomorph() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }

struct Box3[T] { v: T, tag: String }
impl[T] Drop for Box3[T] { fn drop(mut ref self) { println(f"dB{self.tag.len()}") } }

struct G[T] { v: T, r: R }
impl[T] Drop for G[T] { fn drop(mut ref self) { println("dG") } }

struct H[T] { items: Vec[T] }
impl[T] Drop for H[T] { fn drop(mut ref self) { println(f"dH{self.items.len()}") } }

struct P { s: String }
impl Drop for P { fn drop(mut ref self) { println("dP") } }

fn cell_str() {
    let b: Box3[String] = Box3 { v: f"vvvvvvvv", tag: f"ttttttt" };
    println(f"  v{b.v.len()}");
    println("  after");
}
fn cell_two() {
    let a: Box3[String] = Box3 { v: f"aaaaaaaa", tag: f"ttttttttt" };
    println(f"  a{a.v.len()}");
    let b: Box3[i64] = Box3 { v: 7, tag: f"uuuuuuuu" };
    println(f"  b{b.v}");
}
fn cell_field() { let g: G[String] = G { v: f"gggggggg", r: R { id: 3 } }; println(f"  g{g.v.len()}"); }
fn cell_vec()   { let h: H[String] = H { items: [f"aaaaaaaa", f"bbbbbbbb"] }; println(f"  h{h.items.len()}"); }
fn cell_nest()  { let d: Box3[Vec[String]] = Box3 { v: [f"zzzzzzzz"], tag: f"wwwwww" }; println(f"  d{d.v.len()}"); }
fn cell_plain() { let p = P { s: f"pppppppp" }; println(f"  p{p.s.len()}"); }

fn main() {
    println("str");   cell_str();
    println("two");   cell_two();
    println("field"); cell_field();
    println("vec");   cell_vec();
    println("nest");  cell_nest();
    println("plain"); cell_plain();
    println("done");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"str
  v8
dB7
  after
two
  a8
dB9
  b7
dB8
field
  g8
dG
dR3
vec
  h2
dH2
nest
  d1
dB6
plain
  p8
dP
done
"#,
        "a generic `impl[T] Drop for S[T]` lost its body on a compiled \
             backend — B-2026-09-04-4. `dP` alone surviving is the pre-fix \
             signature; a missing `dB7`/`after` ORDER is the NLL-placement half."
    );
}

/// B-2026-09-04-4, ARGUMENT-POSITION half — and the regression the
/// binding-site half caused on its way in.
///
/// Once a generic binding registers a per-monomorph `karac_drop_S$<c>`
/// wrapper, MOVING it into a by-value param double-freed: the caller's
/// retraction (`move_declined_copy_struct_arg`) scans for a `StructDrop`
/// and silently skips a `UserDrop`, so the caller kept a cleanup the callee
/// had just taken ownership of. Measured as `AddressSanitizer: attempting
/// double-free` on the 8-byte `v` buffer, with the non-generic control
/// (`plain`) and a generic struct with NO `Drop` impl both clean — which is
/// what isolated it to the registration this fix introduced rather than to
/// the ownership rule itself. It is the same omission
/// `move_transferred_struct_arg` documents having to repair for its own
/// arm, one arm over.
///
/// The repair is the transfer bargain, not a retraction alone: the caller
/// gives up its half AND the callee registers the per-monomorph wrapper, so
/// the body runs exactly once, in the frame that owns the value. That is
/// why `dB6` lands BEFORE `  after` — inside `take`, where `--interp` puts
/// it — rather than at the caller's scope exit.
///
/// `temp` came along with it: a fresh struct-literal argument reaches the
/// same callee-side registration, so it went from losing its body to
/// running it. `parami` keeps the two-monomorph question alive on this
/// path (`Box3[i64]` through a different callee than `Box3[String]`), and
/// `plain` is the non-generic control that must be untouched — every gate
/// in this fix keys on the bare-name wrapper being ABSENT, so a
/// non-generic `Drop` type cannot reach any of it.
///
/// STILL OPEN, deliberately out of this fixture: a generic `Drop` struct
/// held as a `Vec` ELEMENT runs no body on the compiled backends. That is
/// the container element-walk, a different mechanism from either binding
/// site, and it lost the body before this change too.
#[test]
fn e2e_generic_impl_drop_survives_a_by_value_argument() {
    let Some(out) = run_program(
        r#"struct Box3[T] { v: T, tag: String }
impl[T] Drop for Box3[T] { fn drop(mut ref self) { println(f"dB{self.tag.len()}") } }
struct Pl { v: String, tag: String }
impl Drop for Pl { fn drop(mut ref self) { println(f"dP{self.tag.len()}") } }

fn take(b: Box3[String]) { println(f"  t{b.v.len()}") }
fn takei(b: Box3[i64]) { println(f"  ti{b.v}") }
fn takep(b: Pl) { println(f"  tp{b.v.len()}") }
fn make() -> Box3[String] { return Box3 { v: f"mmmmmmmm", tag: f"rrrrrrr" }; }

fn c_param() { let b: Box3[String] = Box3 { v: f"pppppppp", tag: f"qqqqqq" }; take(b); println("  after") }
fn c_parami() { let b: Box3[i64] = Box3 { v: 5, tag: f"iiiii" }; takei(b) }
fn c_ret()   { let r = make(); println(f"  r{r.v.len()}") }
fn c_temp()  { take(Box3 { v: f"tttttttt", tag: f"sssss" }) }
fn c_plain() { let b = Pl { v: f"pppppppp", tag: f"qqqqqq" }; takep(b) }

fn main() {
    println("param");  c_param();
    println("parami"); c_parami();
    println("ret");    c_ret();
    println("temp");   c_temp();
    println("plain");  c_plain();
    println("done");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"param
  t8
dB6
  after
parami
  ti5
dB5
ret
  r8
dB7
temp
  t8
dB5
plain
  tp8
dP6
done
"#,
        "a generic `Drop` value moved into a by-value param lost its body \
             or ran it in the wrong frame — B-2026-09-04-4 argument half."
    );
}

#[test]
fn e2e_nested_generic_struct_field_runs_its_drop_body_on_every_spelling() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Gd[T]  { r: T, z: i64 }
struct Gn3    { inner: Gd[R], z: i64 }
struct Gn2[T] { inner: Gd[T], z: i64 }
fn take3(h: Gn3) -> i64 { println("  in"); return h.z; }
fn gNest[T](h: Gn2[T]) -> i64 { println("  in"); return h.z; }
fn gDir[T](h: Gd[T]) -> i64 { println("  in"); return h.z; }
fn c_conc_temp()  { let _ = take3(Gn3 { inner: Gd[R] { r: mk(6), z: 1 }, z: 9 }); }
fn c_conc_local() { let h = Gn3 { inner: Gd[R] { r: mk(7), z: 1 }, z: 9 }; let _ = take3(h); }
fn c_gen_local()  { let h = Gn2[R] { inner: Gd[R] { r: mk(8), z: 1 }, z: 9 }; let _ = gNest(h); }
fn c_gen_temp()   { let _ = gNest(Gn2[R] { inner: Gd[R] { r: mk(4), z: 1 }, z: 9 }); }
fn c_one_level()  { let _ = gDir(Gd[R] { r: mk(2), z: 9 }); }
struct Hd { r: R, z: i64 }
struct Hn { inner: Hd, z: i64 }
fn takeH(h: Hn) -> i64 { println("  in"); return h.z; }
fn c_plain_nested()    { let _ = takeH(Hn { inner: Hd { r: mk(11), z: 1 }, z: 9 }); }
fn main() {
    println("conc_temp");  c_conc_temp();
    println("conc_local"); c_conc_local();
    println("gen_local");  c_gen_local();
    println("gen_temp");   c_gen_temp();
    println("one_level");  c_one_level();
    println("plain_nested"); c_plain_nested();
    println("done");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"conc_temp
  in
dR6
conc_local
  in
dR7
gen_local
  in
dR8
gen_temp
  in
dR4
one_level
  in
dR2
plain_nested
  in
dR11
done
"#,
        "a generic-struct-instantiation FIELD was invisible to the Drop-field \
             gate — B-2026-09-05-5. All four nested cells silent with `dR2` (the \
             one-level control) intact is the pre-fix signature; any cell \
             printing its body TWICE means the gate now admits a field whose body \
             a callee also runs."
    );
}

/// B-2026-09-05-15 — a nested generic-instantiation field WITH ITS OWN
/// `impl[T] Drop`, under a parent that also declares a `Drop`, runs that
/// field's own body on the compiled backends.
///
/// The sibling above (B-2026-09-05-5) had NO-`Drop` intermediate structs, so
/// the only body at stake was the grandchild `R`'s, reached through the
/// field-bodies walk. Here the intermediate `Go[T]` declares its own
/// `impl[T] Drop`, and its body was resolved by the name-keyed
/// `get_function("Go.drop")` in `emit_user_drop_field_bodies_fn` — a symbol a
/// generic impl never has (parked until instantiated, and `drop` is never
/// called directly), so `dGo` was lost while the walk still reached THROUGH
/// the field to the grandchild `dR`. The fix resolves the field's own body
/// through `user_drop_body_fn_mono` with the field's nested subst.
///
/// Four cells: a generic parent (`Gouter[T]`), a NON-generic parent holding
/// the same generic-instantiation field (`Nouter`, the sibling the row names
/// as sharing the defect), the fully non-generic control (`Pouter`/`Pn`,
/// correct before and after — it must not double-fire), and the bound-local
/// spelling. Order per cell is parent body, then field body, then grandchild.
#[test]
fn e2e_nested_generic_field_with_own_drop_runs_its_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Go[T] { r: T, z: i64 }
impl[T] Drop for Go[T] { fn drop(mut ref self) { println(f"dGo{self.z}") } }
struct Gouter[T] { inner: Go[T], z: i64 }
impl[T] Drop for Gouter[T] { fn drop(mut ref self) { println(f"dOut{self.z}") } }
struct Nouter { inner: Go[R], z: i64 }
impl Drop for Nouter { fn drop(mut ref self) { println(f"dN{self.z}") } }
struct Pn { r: R, z: i64 }
impl Drop for Pn { fn drop(mut ref self) { println(f"dPn{self.z}") } }
struct Pouter { inner: Pn, z: i64 }
impl Drop for Pouter { fn drop(mut ref self) { println(f"dPo{self.z}") } }
fn oOwn[T](h: Gouter[T]) -> i64 { println("  in"); return h.z; }
fn main() {
    println("gen_parent");  let _ = oOwn(Gouter[R] { inner: Go[R] { r: mk(1), z: 51 }, z: 52 });
    println("plain_parent"); let nb = Nouter { inner: Go[R] { r: mk(2), z: 55 }, z: 56 }; println(f"  b{nb.z}");
    println("control");     let pc = Pouter { inner: Pn { r: mk(3), z: 61 }, z: 62 }; println(f"  c{pc.z}");
    println("bound");       let gd = Gouter[R] { inner: Go[R] { r: mk(4), z: 71 }, z: 72 }; println(f"  d{gd.z}");
    println("done");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"gen_parent
  in
dOut52
dGo51
dR1
plain_parent
  b56
dN56
dGo55
dR2
control
  c62
dPo62
dPn61
dR3
bound
  d72
dOut72
dGo71
dR4
done
"#,
        "a nested generic-instantiation field's OWN Drop body was lost under an \
             own-Drop parent — B-2026-09-05-15. A cell missing its `dGo`/`dN`-level \
             body is the pre-fix signature; a cell printing one TWICE means the \
             mono own-body resolution now double-fires against the memory walk."
    );
}

/// B-2026-09-05-4 — a GENERIC struct with its OWN `impl[T] Drop` registered
/// NOTHING as a temp-literal argument: not its body, not its Drop-bearing
/// field's, and not the memory either.
///
/// The caller's own-`Drop` arm asked
/// `emit_user_drop_wrapper_skipping` for a wrapper keyed by NAME, and all
/// three of its steps were name-keyed. A generic `impl[T] Drop for S[T]`
/// has no bare `S.drop` symbol and never will — the declaration pass parks
/// a generic impl's methods in `generic_fns` awaiting a call site, and
/// `drop` is the one method a program never calls (B-2026-09-04-4's
/// finding) — so the lookup's `?` returned `None` for the whole wrapper.
/// The arm's `None` fallback is `track_user_drop_var`, which no-ops for
/// precisely that class, so the slot was materialized and then abandoned.
///
/// MEASURED PRE-FIX, in the IR: `gTemp` stored into `%__owned_agg_tmp` and
/// emitted NO cleanup call whatsoever, where the non-generic twin `nTemp`
/// emitted `karac_dropsk_Gn___karac_dropbodies_Gn$keep0$s1` and the
/// named-local `gLocal` emitted the PER-MONOMORPH `karac_drop_Go$R`. That
/// last one is the argument that this belongs caller-side: `gLocal` and
/// `gTemp` reach the SAME callee monomorph `gOwn$R`, so the callee cannot
/// be the one that differs, and the caller demonstrably runs the wrapper
/// for the generic parent already — just not for the temp spelling.
///
/// Two controls sit on the other side of the defect and are load-bearing,
/// because each one falsifies a simpler story:
///  * `nTemp` / `nLocal` — the NON-generic twin is correct in both
///    spellings, so it is not the temp form alone;
///  * `gLocal` — the same generic type as a NAMED LOCAL is correct, so it
///    is not the generic parent alone. It is the combination.
///
/// A third discriminator is not in this fixture but was measured: the same
/// generic parent whose only field is PRIMITIVE (`Gp[i64] { v: i64, z: i64 }`)
/// printed its body correctly pre-fix, as did this fixture with a heap-free
/// `R { id: i64 }`. So the shape needs a HEAP-BEARING field to be reachable
/// at all, and a heap-free `R` here would silently stop testing anything.
/// Why the two part company was NOT run down: it is not the base
/// copy-support check, which reads the declared field types and bails on
/// `Go`'s bare `T` regardless of `R`.
///
/// This was a LEAK as well as a run-vs-build divergence, which the row did
/// not record: the abandoned slot's field was never freed either. Measured
/// on this fixture, pre-fix 22 allocs / 20 frees with 10 bytes definitely
/// lost in 2 blocks; post-fix 24 allocs / 24 frees, 0 errors, nothing lost.
#[test]
fn e2e_generic_own_drop_struct_temp_arg_runs_its_body_and_its_fields() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Go[T] { r: T, z: i64 }
impl[T] Drop for Go[T] { fn drop(mut ref self) { println(f"dGo{self.z}") } }
struct Gn { r: R, z: i64 }
impl Drop for Gn { fn drop(mut ref self) { println(f"dGn{self.z}") } }
fn gOwn[T](h: Go[T]) -> i64 { println("  in"); return h.z; }
fn nOwn(h: Gn) -> i64 { println("  in"); return h.z; }
fn gTemp()  { let _ = gOwn(Go[R] { r: mk(3), z: 9 }); }
fn gLocal() { let h = Go[R] { r: mk(4), z: 10 }; let _ = gOwn(h); }
fn nTemp()  { let _ = nOwn(Gn { r: mk(5), z: 11 }); }
fn nLocal() { let h = Gn { r: mk(6), z: 12 }; let _ = nOwn(h); }
fn main() {
    println("gTemp");  gTemp();
    println("gLocal"); gLocal();
    println("nTemp");  nTemp();
    println("nLocal"); nLocal();
    println("done");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"gTemp
  in
dGo9
dR3
gLocal
  in
dGo10
dR4
nTemp
  in
dGn11
dR5
nLocal
  in
dGn12
dR6
done
"#,
        "a generic struct with its own `impl[T] Drop` registered nothing as \
             a temp-literal argument — B-2026-09-05-4. `gTemp` missing BOTH \
             `dGo9` and `dR3`, with the other three cells intact, is the exact \
             pre-fix signature; either line appearing TWICE would mean the \
             caller and the callee are now both registering it."
    );
}

/// B-2026-09-05-16 — a struct that is NOT copy-supported AND declares its
/// own `impl Drop`, passed BY VALUE, was owned by BOTH frames.
///
/// The callee takes such a param OWN BY TRANSFER (B-2026-08-05-33: no entry
/// copy is possible, so it keeps the caller's buffers and registers the
/// drop), and that arm's safety argument is an explicit lockstep with the
/// caller giving its cleanup up. B-2026-08-07-15 closed the lockstep for the
/// plain class, on BOTH caller spellings. Neither half reached a type with
/// an `impl Drop`: the named binding because
/// `move_declined_copy_struct_arg`'s retraction was gated on the bare
/// wrapper being ABSENT (B-2026-09-04-4 narrowed it to the generic-`Drop`
/// class on purpose), and the fresh temp because its cleanup is registered
/// on the `UserDrop` channel by a different arm than the memory gate
/// `struct_param_owned_by_transfer` guards.
///
/// So the callee freed the caller's buffers through its own param alloca
/// and the caller then freed them again through the aggregate it had copied
/// from — two allocas holding the same pointers, so neither one's
/// cap-zeroing makes the other no-op.
///
/// THE OPT LEVEL IS WHAT HID IT, not the backend. The row was filed as
/// LLJIT-only against a `karac build` that was clean, which reads as the two
/// compiled backends disagreeing. They do not: `KARAC_OPT_LEVEL=0 karac
/// build` aborts identically, so it is one latent defect in the IR both
/// share, masked at `-O2` exactly as B-2026-09-02-20 and B-2026-08-04-19
/// were. A JIT-vs-AOT split is worth one `KARAC_OPT_LEVEL=0` build before it
/// is believed.
///
/// `mo*` are the failing rows — a `Map` field makes `field_copy_supported`
/// decline, and `R`'s own `impl Drop` puts the parent's cleanup on the
/// `UserDrop` channel. BOTH caller spellings fail, and they fail
/// differently: pre-fix the whole fixture reported 8 invalid frees and 28
/// invalid reads, 18 invalid operations from each `mo` cell alone, with
/// stdout empty because the abort precedes the flush.
///
/// TWO CONTROLS, each a way this fix could have been too broad:
///   * `cs*` — a plain `String` field, i.e. COPY-SUPPORTED. It takes the
///     other branch entirely (the callee entry-copies, so the two frames own
///     distinct heap) and must be untouched. Correct pre- and post-fix.
///   * `ao*` — an `Array[i64, 2]` field. Filed against the non-`Drop` class
///     (B-2026-08-07-15's `ig_arr`) as a way to close copy-support WITHOUT a
///     collection, so it was expected to fail here too. MEASURED CORRECT
///     pre-fix, 0 invalid operations, and correct after — kept as the
///     control it turned out to be rather than the second failing row it was
///     drafted as.
///
/// Both spellings of the argument are exercised for each type, because the
/// two caller sites are different code and only one of them had a
/// retraction to widen.
///
/// The `Map` cells leave 48 bytes "possibly lost" under valgrind. That is a
/// one-time `Map` runtime artifact, not this fixture's: a four-line program
/// that inserts one key and prints `len()` reports the same 48 bytes, and
/// two maps still report 48. Post-fix this fixture has 0 invalid operations
/// and 0 bytes DEFINITELY lost.
#[test]
fn e2e_copy_unsupported_own_drop_by_value_arg_has_one_owner() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}" }; }

struct Mo { m: Map[i64, String], r: R, z: i64 }
impl Drop for Mo { fn drop(mut ref self) { println(f"dMo{self.z}") } }
fn moOwn(h: Mo) -> i64 { println("  in"); return h.z; }

struct Ao { a: Array[i64, 2], r: R, z: i64 }
impl Drop for Ao { fn drop(mut ref self) { println(f"dAo{self.z}") } }
fn aoOwn(h: Ao) -> i64 { println("  in"); return h.z; }

struct Cs { s: String, r: R, z: i64 }
impl Drop for Cs { fn drop(mut ref self) { println(f"dCs{self.z}") } }
fn csOwn(h: Cs) -> i64 { println("  in"); return h.z; }

fn moTemp()  { let mut m: Map[i64, String] = Map.new(); m.insert(1, f"v1"); let _ = moOwn(Mo { m: m, r: mk(3), z: 21 }); }
fn moLocal() { let mut m: Map[i64, String] = Map.new(); m.insert(2, f"v2"); let h = Mo { m: m, r: mk(4), z: 22 }; let _ = moOwn(h); }
fn aoTemp()  { let _ = aoOwn(Ao { a: [7, 8], r: mk(5), z: 23 }); }
fn aoLocal() { let h = Ao { a: [7, 8], r: mk(6), z: 24 }; let _ = aoOwn(h); }
fn csTemp()  { let _ = csOwn(Cs { s: f"s7", r: mk(7), z: 25 }); }
fn csLocal() { let h = Cs { s: f"s8", r: mk(8), z: 26 }; let _ = csOwn(h); }

fn main() {
    println("moTemp");  moTemp();
    println("moLocal"); moLocal();
    println("aoTemp");  aoTemp();
    println("aoLocal"); aoLocal();
    println("csTemp");  csTemp();
    println("csLocal"); csLocal();
    println("done");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"moTemp
  in
dMo21
dR3
moLocal
  in
dMo22
dR4
aoTemp
  in
dAo23
dR5
aoLocal
  in
dAo24
dR6
csTemp
  in
dCs25
dR7
csLocal
  in
dCs26
dR8
done
"#
    );
}

/// B-2026-09-05-4, widened — the same defect across the shapes the base
/// fixture leaves out, all of which were measured silent on the compiled
/// backends before the fix and correct after it.
///
/// `a1`/`a2` are two MONOMORPHS of one generic own-`Drop` struct in a
/// single program: the mask symbol folds in the bodies fn's own monomorph
/// suffix (`karac_dropsk_Go___karac_dropbodies_Go$R$keep0$s1`), so the two
/// instantiations must not share a wrapper — the check is that `dR1` and
/// `dQ2` each fire, against their own field type. `c` gives the parent TWO
/// Drop-bearing fields, which is what distinguishes a per-field mask from
/// the all-or-nothing `karac_dropnf_<T>`; the fields run in reverse
/// declaration order after the parent's own body, per design.md § Drop
/// ordering. `d` takes two generic params, so the mangled suffix carries
/// both bindings.
///
/// MEASURED PRE-FIX: `a1 a2 c d` all printed their marker and NOTHING
/// else, with 21 allocs / 15 frees and 24 bytes definitely lost in 6
/// blocks. Post-fix 31 allocs / 31 frees, 0 errors, nothing lost. (The
/// alloc counts differ because the recovered `println` bodies allocate.)
#[test]
fn e2e_generic_own_drop_struct_temp_arg_across_monomorphs_and_field_counts() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Q { qid: i64, s: String }
impl Drop for Q { fn drop(mut ref self) { println(f"dQ{self.qid}") } }
fn mq(i: i64) -> Q { return Q { qid: i, s: f"q{i}" }; }
struct Go[T] { r: T, z: i64 }
impl[T] Drop for Go[T] { fn drop(mut ref self) { println(f"dGo{self.z}") } }
fn gOwn[T](h: Go[T]) -> i64 { return h.z; }
struct Gp[T] { v: T, z: i64 }
impl[T] Drop for Gp[T] { fn drop(mut ref self) { println(f"dGp{self.z}") } }
fn pOwn[T](h: Gp[T]) -> i64 { return h.z; }
struct Gt[T] { a: T, b: Q, z: i64 }
impl[T] Drop for Gt[T] { fn drop(mut ref self) { println(f"dGt{self.z}") } }
fn tOwn[T](h: Gt[T]) -> i64 { return h.z; }
struct Gd[A, B] { a: A, b: B, z: i64 }
impl[A, B] Drop for Gd[A, B] { fn drop(mut ref self) { println(f"dGd{self.z}") } }
fn dOwn[A, B](h: Gd[A, B]) -> i64 { return h.z; }
fn main() {
    println("a1"); let _ = gOwn(Go[R] { r: mk(1), z: 41 });
    println("a2"); let _ = gOwn(Go[Q] { r: mq(2), z: 42 });
    println("b");  let _ = pOwn(Gp[i64] { v: 7, z: 43 });
    println("c");  let _ = tOwn(Gt[R] { a: mk(3), b: mq(4), z: 44 });
    println("d");  let _ = dOwn(Gd[R, Q] { a: mk(5), b: mq(6), z: 45 });
    println("done");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"a1
dGo41
dR1
a2
dGo42
dQ2
b
dGp43
c
dGt44
dQ4
dR3
d
dGd45
dQ6
dR5
done
"#,
        "a generic own-`Drop` parent lost its bodies as a temp argument \
             across monomorphs and field counts — B-2026-09-05-4. `b` is the \
             control: an all-primitive generic parent printed `dGp43` correctly \
             even PRE-fix, so a run where only `b` survives is the unfixed tree."
    );
}

/// B-2026-09-04-24 — a by-value STRUCT temp literal at a GENERIC call site
/// lost its tuple element's `Drop` body.
///
/// The argument FORM is the whole discriminator, which is why all three
/// controls here sit on the other side of it: the same generic callee handed
/// a NAMED LOCAL is correct, and the non-generic twin is correct with either
/// form. So it is neither the generic callee nor the temp literal alone.
///
/// In the IR the caller emitted, `gLocal` ends with the pair
/// `__karac_dropbodies_G$R` + `__karac_drop_struct_G$R`, and `nTemp` with
/// `__karac_dropbodies_Gn` + `__karac_drop_struct_Gn` — but `gTemp` had ONLY
/// `__karac_drop_struct_G$R`. The memory half has resolved the temp's
/// instantiation since B-2026-08-06-2 (defect B), so the buffer was always
/// freed — valgrind reported no leaks pre-fix, which is exactly why the
/// memory-sanitizer suite could not see this. The BODIES half was still
/// name-keyed, so it read `G[T] { pe: (T, i64) }`, found the element erased
/// to a bare `T`, declined, and emitted no call at all.
///
/// Both cells reach the SAME monomorph `gfn$R`, so a callee-body explanation
/// cannot distinguish them — and call ORDER does not change the result
/// (`gTemp` alone still loses it, `gLocal` alone is still correct). That is
/// what places the defect caller-side rather than in the mono.
#[test]
fn e2e_generic_call_struct_temp_arg_runs_its_tuple_element_drop_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct G[T] { pe: (T, i64), z: i64 }
struct Gn { pe: (R, i64), z: i64 }
fn gfn[T](h: G[T]) -> i64 { let (r, k) = h.pe; println("  in"); return k; }
fn nfn(h: Gn) -> i64 { let (r, k) = h.pe; println("  in"); return k; }
fn gLocal() { let h = G[R] { pe: (mk(80), 5), z: 9 }; let _ = gfn(h); }
fn gTemp()  { let _ = gfn(G[R] { pe: (mk(81), 5), z: 9 }); }
fn nLocal() { let h = Gn   { pe: (mk(82), 5), z: 9 }; let _ = nfn(h); }
fn nTemp()  { let _ = nfn(Gn   { pe: (mk(83), 5), z: 9 }); }
fn main() {
    println("gLocal"); gLocal();
    println("gTemp");  gTemp();
    println("nLocal"); nLocal();
    println("nTemp");  nTemp();
    println("done");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"gLocal
  in
dR80/t80/1
gTemp
  in
dR81/t81/1
nLocal
  in
dR82/t82/1
nTemp
  in
dR83/t83/1
done
"#,
        "a struct temp literal at a generic call site lost its tuple \
             element's `Drop` body — B-2026-09-04-24. A missing `dR81` with the \
             other three cells intact is the exact pre-fix signature: the three \
             controls are what prove it is the generic-plus-temp COMBINATION."
    );
}

/// B-2026-09-04-27 — the CONTAINER-ELEMENT half of B-2026-09-04-4: a
/// generic `impl[T] Drop for S[T]` ran no body when the value sat in a
/// container rather than a binding.
///
/// B-2026-09-04-4 fixed the two BINDING sites by instantiating `S.drop$<T>`
/// from the binding's recorded type and wrapping it. A container element
/// is reached by a different family — the BODIES-ONLY walkers
/// (`__karac_dropelems_*`) armed on the `ContainerElemBodies` channel —
/// and every one of those found the element's body with
/// `module.get_function("S.drop")`, the bare symbol that a generic impl
/// never has. So the body was `None`; with no Drop-bearing field beside it
/// the walker declined, and nothing was armed at all.
///
/// MEASURED PRE-FIX on this fixture's shapes: `--interp` printed every
/// `dB*`; all three compiled backends printed NONE of them — the `Vec`
/// element the row reports, and the `Map` value, `Array[T, N]` element and
/// `Vec[Vec[..]]` element the row listed as unmeasured, all five silent for
/// the same reason. `dP8`, the non-generic control, survived on all four,
/// which is what isolated the lookup rather than the channel. The
/// only line missing on every compiled backend was the body's; the
/// walkers this fix touches free nothing by construction, and the row's
/// own program runs 13 allocs / 13 frees, 0 errors under valgrind with
/// the body now firing — so this was the body alone: `run-vs-build`,
/// not a leak.
///
/// The fix resolves the body per monomorph from the container's element
/// `TypeExpr` — the same `S.drop$<concrete>` the binding-site wrapper
/// instantiates — at the four walker sites: the `Vec` walker, the
/// nested-`Vec` struct arm, the slot primitive behind the `Array` and tuple
/// walks, and the `Map`/`Set` half walks. Each site also threads that
/// subst into the field-bodies walk it used to call with an empty map.
///
/// `two` decides the design here as it did for the binding half:
/// `Vec[Box3[String]]` and `Vec[Box3[i64]]` are live in one function and
/// print `dB2` / `dB3` from `tag` at two different offsets, so one
/// name-shared walker cannot serve both. `vec` pins PLACEMENT: `dB1` lands
/// between `n1` and `after` — the binding's NLL last use, where `--interp`
/// fires it — not at scope exit. `arr` pins the forward `0..n` element
/// order (`dB5` then `dB6`) that both backends agree on for containers.
#[test]
fn e2e_generic_impl_drop_runs_its_body_for_container_elements() {
    let Some(out) = run_program(
        r#"struct Box3[T] { v: T, tag: String }
impl[T] Drop for Box3[T] { fn drop(mut ref self) { println(f"dB{self.tag.len()}") } }
struct Pl { tag: String }
impl Drop for Pl { fn drop(mut ref self) { println(f"dP{self.tag.len()}") } }

fn cell_vec() {
    let v: Vec[Box3[String]] = [Box3 { v: f"e1111111", tag: f"a" }];
    println(f"  n{v.len()}");
    println("  after");
}
fn cell_two() {
    let a: Vec[Box3[String]] = [Box3 { v: f"aaaaaaaa", tag: f"tt" }];
    println(f"  a{a.len()}");
    let b: Vec[Box3[i64]] = [Box3 { v: 7, tag: f"uuu" }];
    println(f"  b{b.len()}");
}
fn cell_nest() {
    let vv: Vec[Vec[Box3[String]]] = [[Box3 { v: f"e3333333", tag: f"cccc" }]];
    println(f"  vv{vv.len()}");
}
fn cell_arr() {
    let a: Array[Box3[String], 2] = [Box3 { v: f"e4444444", tag: f"ddddd" }, Box3 { v: f"e5555555", tag: f"eeeeee" }];
    println(f"  arr{a[1].tag.len()}");
}
fn cell_map() {
    let mut m: Map[String, Box3[String]] = Map.new();
    m.insert(f"k", Box3 { v: f"e6666666", tag: f"fffffff" });
    println(f"  m{m.len()}");
}
fn cell_plain() {
    let p: Vec[Pl] = [Pl { tag: f"pppppppp" }];
    println(f"  p{p.len()}");
}
fn main() {
    println("vec");   cell_vec();
    println("two");   cell_two();
    println("nest");  cell_nest();
    println("arr");   cell_arr();
    println("map");   cell_map();
    println("plain"); cell_plain();
    println("done");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"vec
  n1
dB1
  after
two
  a1
dB2
  b1
dB3
nest
  vv1
dB4
arr
  arr6
dB5
dB6
map
  m1
dB7
plain
  p1
dP8
done
"#,
        "a generic `impl[T] Drop for S[T]` lost its body when the value was \
             a container element — B-2026-09-04-27. `dP8` alone surviving is \
             the pre-fix signature; `dB1` after `after` is the NLL-placement \
             half; `dB2`/`dB3` swapped is the two-monomorph offset half."
    );
}

/// B-2026-07-29-39 — an aggregate runs its fields' user `impl Drop`.
///
/// Pre-fix, drop glue dispatched a user body for a DIRECT binding of a Drop
/// type but never walked an aggregate's fields, so a struct holding a
/// Drop-implementing value dropped nothing at scope exit. Every resource
/// type with a synthesized drop (`TcpListener`, `TcpStream`, `TlsStream`, …)
/// therefore leaked its fd whenever it was held in a field rather than a
/// bare local — which is how a server is normally written.
///
/// Also pins the two properties that make the fix safe rather than merely
/// present: fields die in REVERSE declaration order (`Two` prints 5 then 4),
/// and a field MOVED OUT is dropped by its destination only, never twice.
#[test]
fn e2e_aggregate_runs_field_user_drop() {
    let Some(out) = run_program(
            "struct A { t: i64 }\n\
             impl Drop for A { fn drop(mut ref self) { println(f\"dA{self.t}\"); } }\n\
             struct Mid { a: A }\n\
             struct Outer { m: Mid, a2: A }\n\
             struct Two { x: A, y: A }\n\
             struct Moved { a: A }\n\
             fn main() {\n\
             \x20   { let o = Outer { m: Mid { a: A { t: 1 } }, a2: A { t: 2 } }; println(f\"{o.a2.t}\"); }\n\
             \x20   { let t = Two { x: A { t: 4 }, y: A { t: 5 } }; println(f\"{t.x.t}\"); }\n\
             \x20   { let h = Moved { a: A { t: 7 } }; let x = h.a; println(f\"{x.t}\"); }\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(
        out,
        // Outer: `a2` before `m` (reverse declaration), and `m` recurses
        // into `Mid.a`. Two: y before x. Moved: exactly one dA7, from `x`.
        "2\ndA2\ndA1\n4\ndA5\ndA4\n7\ndA7\nend\n"
    );
}

/// B-2026-07-30-11 SHAPE 2 — an owned aggregate TEMP runs its fields' user
/// `impl Drop`, in every position a temp can occupy.
///
/// B-2026-07-29-39 (the test above) taught the `let`-path to walk an
/// aggregate's Drop-bearing fields and stopped there. Every owned-temp
/// registrar still gated on the type's OWN `drop_method_keys` entry, so a
/// holder that merely CONTAINS a Drop type ran nothing when the temp died:
/// `consume(H { .. })`, `consume(mk())` and `mk();` each leaked the field's
/// resource once per call, while the `let`-bound sibling worked. Three
/// separate arms, all three fixed here.
///
/// The last case is the one that makes this safe rather than merely
/// present: on the RETURN-PASSTHROUGH path the body must NOT fire here.
/// `pass` entry-copies the struct and returns an independent copy, so the
/// caller temp's MEMORY does need freeing (B-2026-07-08-6) — but the VALUE
/// flows out to `p`, whose own drop runs the body. Firing both prints `dA4`
/// twice; an intermediate version of this fix did exactly that.
///
/// Paired with `tests/interpreter.rs`'s
/// `test_owned_aggregate_temp_runs_field_user_drop` on the same source and
/// the same expected string — the pair IS the run/build parity contract.
#[test]
fn e2e_owned_aggregate_temp_runs_field_user_drop() {
    let Some(out) = run_program(
        "struct A { t: i64 }\n\
             impl Drop for A { fn drop(mut ref self) { println(f\"dA{self.t}\"); } }\n\
             struct H { a: A }\n\
             fn consume(h: H) { println(\"c\"); }\n\
             fn mk(t: i64) -> H { H { a: A { t: t } } }\n\
             fn pass(h: H) -> H { h }\n\
             fn main() {\n\
             \x20   consume(H { a: A { t: 1 } });\n\
             \x20   println(\"-\");\n\
             \x20   consume(mk(2));\n\
             \x20   println(\"-\");\n\
             \x20   mk(3);\n\
             \x20   println(\"-\");\n\
             \x20   let p = pass(H { a: A { t: 4 } });\n\
             \x20   println(f\"{p.a.t}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        // Each temp drops at the `;` that ends its statement — after the
        // callee returns, before the next statement. `dA4` appears ONCE,
        // at `p`'s live-range end, not also at the call that produced it.
        "c\ndA1\n-\nc\ndA2\n-\ndA3\n-\n4\ndA4\nend\n"
    );
}

/// B-2026-07-30-11 (displaced-value leg) — overwriting a struct binding
/// runs the OLD value's user `impl Drop` body (and frees its field heap)
/// at the assignment, and a moved source fires exactly once.
///
/// Three shapes, one program: `a = G{..}` fires the displaced value's
/// body BEFORE the store (drop 1, reading the old id) and the survivor's
/// at its own NLL end (drop 2); `x = y` (identifier move) fires the
/// displaced old x once and the moved value once — y's own UserDrop
/// action is retracted, the double body this leg found; and
/// `c = consume(c)` fires nothing at the assignment (the old value moved
/// into the callee — the RHS-mentions-target guard) and once for the
/// final value. The old value's field heap used to be a hard leak too
/// (13 bytes definitely lost on the un-elidable-payload probe): nothing
/// on the reassign path freed it, only LLVM DCE hid it in trivial
/// programs.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_struct_reassign_displaced_drop_semantics`.
#[test]
fn e2e_struct_reassign_displaced_drop_semantics() {
    let Some(out) = run_program(
        "struct G { id: i64, s: String }\n\
             impl Drop for G {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             #[allow(partial_move_of_drop_struct)]\n\
             fn consume(g: G) -> G {\n\
             \x20   G { id: g.id + 10, s: g.s }\n\
             }\n\
             fn main() {\n\
             \x20   let mut a = G { id: 1, s: \"first\".to_string() };\n\
             \x20   a = G { id: 2, s: \"second\".to_string() };\n\
             \x20   println(\"after overwrite\");\n\
             \x20   let mut x = G { id: 3, s: \"xxx\".to_string() };\n\
             \x20   let y = G { id: 4, s: \"yyy\".to_string() };\n\
             \x20   x = y;\n\
             \x20   println(\"after move\");\n\
             \x20   let mut c = G { id: 5, s: \"ccc\".to_string() };\n\
             \x20   c = consume(c);\n\
             \x20   println(\"after consume\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "drop 1\ndrop 2\nafter overwrite\ndrop 3\ndrop 4\nafter move\ndrop 15\nafter consume\n"
    );
}

/// B-2026-07-31-38 (sibling filing) — a binding moved into a variant
/// constructor and then REASSIGNED gets a drop for its fresh value.
/// The ctor move correctly retracted r's UserDrop action, but nothing
/// re-armed it, so `let s = Slot.Held(r); r = Res{2};` never ran D2
/// under codegen while the interpreter did. The reassign path now
/// re-registers when the type declares `impl Drop` and no action is
/// armed — and the displaced-value fire is gated on an ARMED action, so
/// the moved-from slot's stale payload is not replayed (this exact
/// shape printed `D1 D1 x` under an ungated displaced fire).
///
/// Twin of `tests/interpreter.rs`'s
/// `test_ctor_moved_binding_reassign_rearms_drop`.
#[test]
fn e2e_ctor_moved_binding_reassign_rearms_drop() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"D{self.id}\")\n\
             \x20   }\n\
             }\n\
             enum Slot { Empty, Held(Res) }\n\
             fn main() {\n\
             \x20   let mut r = Res { id: 1 };\n\
             \x20   let s = Slot.Held(r);\n\
             \x20   r = Res { id: 2 };\n\
             \x20   println(\"x\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "D1\nD2\nx\n");
}

/// B-2026-07-30-11 (param-tuple leg, the A shape) — a tuple LITERAL arg
/// runs its Drop-carrying elements' bodies after the call returns, for
/// fresh literal and fresh-call elements; a moved-binding element
/// (`take_tuple((h, 20))`) keeps its own binding's single NLL fire. The
/// struct-arg baselines (`take_res(Res { .. })`, `take_res(g)`) pin the
/// pre-existing behavior around the new shapes. Twin of
/// `tests/interpreter.rs`'s `test_param_tuple_elements_run_drop_bodies`.
#[test]
fn e2e_param_tuple_elements_run_drop_bodies() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mk(n: i64) -> Res {\n\
             \x20   Res { id: n }\n\
             }\n\
             fn take_tuple(t: (Res, i64)) {\n\
             \x20   println(f\"callee sees {t.1}\")\n\
             }\n\
             fn take_res(r: Res) {\n\
             \x20   println(f\"callee sees res {r.id}\")\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   take_tuple((Res { id: 41 }, 10));\n\
             \x20   println(\"b\");\n\
             \x20   take_res(Res { id: 42 });\n\
             \x20   println(\"c\");\n\
             \x20   let g = Res { id: 43 };\n\
             \x20   take_res(g);\n\
             \x20   println(\"d\");\n\
             \x20   let h = Res { id: 44 };\n\
             \x20   take_tuple((h, 20));\n\
             \x20   println(\"e\");\n\
             \x20   take_tuple((mk(45), 30));\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a\ncallee sees 10\ndrop 41\nb\ncallee sees res 42\ndrop 42\nc\n\
             callee sees res 43\ndrop 43\nd\ncallee sees 20\ndrop 44\ne\n\
             callee sees 30\ndrop 45\nend\n"
    );
}

/// B-2026-07-30-11 (user-method discard) — a USER impl method's owned
/// Drop return discarded bare (`f.make();`) or via wildcard-let runs its
/// body at the discard point. Twin of `tests/interpreter.rs`'s
/// `test_discarded_user_method_return_runs_drop`, same source and
/// expected string.
#[test]
fn e2e_discarded_user_method_return_runs_drop() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             struct Fac { n: i64 }\n\
             impl Fac {\n\
             \x20   fn make(ref self) -> Res {\n\
             \x20       return Res { id: self.n };\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let f = Fac { n: 13 };\n\
             \x20   println(\"a\");\n\
             \x20   f.make();\n\
             \x20   println(\"b\");\n\
             \x20   let _ = f.make();\n\
             \x20   println(\"c\");\n\
             \x20   let r = f.make();\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 13\nb\ndrop 13\nc\ndrop 13\nend\n");
}

/// The ANTI-VACUITY GUARD for the E2E above, and the reason the fix is
/// "withhold the body" rather than "stop cloning".
///
/// The E2E asserts an absence, so it also passes if the defensive clone
/// simply stops being emitted — which would be a silent regression of the
/// double-frees those clone legs exist to fix (B-2026-07-21-5/-6,
/// B-2026-07-14-1), invisible to any output comparison. So assert the two
/// halves separately against `via_ref`'s IR: the clone IS still emitted and
/// the memory glue IS still registered on it, while the user body is NOT.
///
/// `leg_fresh_temp` is the paired positive: the same enum, the same
/// `match`, a genuine fresh temp — `karac_drop_E` present. Without it "no
/// `karac_drop_E` anywhere" would pass on a build that lost the call
/// entirely.
#[test]
fn ir_defensive_scrutinee_copy_keeps_memory_cleanup_and_drops_the_user_body() {
    let ir = ir_for(SCRUTINEE_CLONE_DROP_BODY_SRC);
    // Slice one `define` block out of the module.
    let body_of = |sym: &str| -> String {
        let mut out = String::new();
        let mut inside = false;
        for l in ir.lines() {
            if !inside && l.starts_with("define") && l.contains(sym) {
                inside = true;
            }
            if inside {
                out.push_str(l);
                out.push('\n');
                if l == "}" {
                    break;
                }
            }
        }
        assert!(!out.is_empty(), "no define block for {sym}\n{ir}");
        out
    };

    let via_ref = body_of("@via_ref(");
    // The defensive clone still happens — this is what keeps the consuming
    // arm's binding off the caller's buffer.
    assert!(
        via_ref.contains("refchain.enum.clone"),
        "the borrowed ref-chain clone stopped being emitted, so the E2E \
             twin is now vacuous — it would pass with no clone at all:\n{via_ref}"
    );
    // …and the clone's own heap is still freed.
    assert!(
        via_ref.contains("call void @__karac_drop_E("),
        "the clone's memory cleanup was dropped along with its body — the \
             duplicated payload now leaks:\n{via_ref}"
    );
    // …but the enum's user `Drop` body is NOT run on it.
    assert!(
        !via_ref.contains("call void @karac_drop_E("),
        "B-2026-08-29-37: the enum's own Drop body runs on a defensive copy \
             of a place the caller still owns:\n{via_ref}"
    );

    // The control: a genuine fresh temp keeps its body (B-2026-07-11-26).
    let fresh = body_of("@leg_fresh_temp(");
    assert!(
        fresh.contains("call void @karac_drop_E("),
        "a fresh owned temp lost its user Drop body — B-2026-07-11-26 \
             regressed:\n{fresh}"
    );
}

/// B-2026-09-04-30 — A BY-VALUE `self` RECEIVER ON A TEMP RUNS ITS `Drop`
/// BODIES, and the fresh-temp receiver's field walk runs BEFORE its memory
/// is freed.
///
/// Two defects, one site. The row's own is the missing OWNER: codegen
/// treats a by-value `self` exactly like a by-value param — caller-retained
/// (`owner_runs_bodies` is true for a `SelfValue` source), so the callee
/// registers no walker — while B-2026-08-01-5 had excluded owned `self`
/// from the CALLER's receiver-temp registration to stop a passthrough chain
/// double-firing. A local receiver still had an owner (its own binding), and
/// a by-value param temp always had one (`param-twin` here), but a TEMP
/// receiver had none on any surface: `temp-recv-fields` printed `rd1` and
/// neither field body, and `temp-recv-own` lost `R`'s own. Owned `self` is
/// admitted again behind `owned_self_return_is_opaque_to_receiver`, which
/// declines every return shape that could hand the receiver back — so the
/// three guard cells below (`returns-self`, `hands-field-out`,
/// `generic-return`) still stand the caller down and their bodies come from
/// the result binding, exactly once.
///
/// The second was underneath it and is why `temp-recv-refself` is here: the
/// no-own-`Drop` arm registered the field-bodies walk and then the memory
/// `StructDrop`, and every drain over that frame is LIFO, so the FREE ran
/// first and each body read its own fields out of freed storage
/// (`dR105/dR10` — the body's own f-string showing through the tag it had
/// just released; valgrind: invalid read of the two-byte tag). Pre-existing
/// on the `ref self` path, which is the spelling that cell pins; the fix
/// registers the memory action first so LIFO puts the bodies ahead of it.
///
/// `param-twin` and `local-recv` are the controls that say what the temp
/// receiver OWES — both were always right, and `temp-recv-fields` now
/// matches `param-twin` cell for cell.
///
/// Interpreter twin `test_owned_self_temp_receiver_runs_drop_bodies`; ASAN
/// twin `asan_owned_self_temp_receiver_is_balanced`.
#[test]
fn e2e_owned_self_temp_receiver_runs_drop_bodies() {
    let src = r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct Pair { a: R, b: R }
struct OwnD { a: R, n: i64 }
impl Drop for OwnD { fn drop(mut ref self) { println(f"dOwnD{self.n}") } }

impl HoRes { fn plain(self) { println(f"  rd{self.a.id}") } }
impl Pair { fn eat(self) { println(f"  pr{self.a.id}") } fn peek(ref self) { println(f"  bo{self.a.id}") } }
impl OwnD { fn eat(self) { println(f"  od{self.n}") } }
impl R {
  fn ident(self) { println(f"  id{self.id}") }
  fn area(self) -> i64 { return self.id * 2; }
  fn me(self) -> R { return self; }
}
struct W { a: R }
impl W { fn unwrap_a(self) -> R { return self.a; } fn opt(self) -> Option[R] { return Option.Some(self.a); } }

fn p_plain(h: HoRes) { println(f"  pd{h.a.id}") }

fn main() {
  println("temp-recv-fields");  HoRes { a: mk(1), b: Result.Ok(mk(101)) }.plain()
  println("temp-recv-own");     mk(2).ident()
  println("temp-recv-ownd");    OwnD { a: mk(3), n: 3 }.eat()
  println("temp-recv-pair");    Pair { a: mk(4), b: mk(104) }.eat()
  println("temp-recv-refself"); Pair { a: mk(5), b: mk(105) }.peek()
  println("param-twin");        p_plain(HoRes { a: mk(6), b: Result.Ok(mk(106)) })
  println("local-recv");        let h = HoRes { a: mk(7), b: Result.Ok(mk(107)) }; h.plain()
  println("scalar-return");     let v = mk(8).area(); println(f"  v{v}")
  println("returns-self");      let m = mk(9).me(); println(f"  m{m.id}")
  println("hands-field-out");   let g = W { a: mk(10) }.unwrap_a(); println(f"  g{g.id}")
  println("generic-return");    let o = W { a: mk(11) }.opt(); println("  built")
  println("done")
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            r#"temp-recv-fields
  rd1
dR101/t101
dR1/t1
temp-recv-own
  id2
dR2/t2
temp-recv-ownd
  od3
dOwnD3
dR3/t3
temp-recv-pair
  pr4
dR104/t104
dR4/t4
temp-recv-refself
  bo5
dR105/t105
dR5/t5
param-twin
  pd6
dR106/t106
dR6/t6
local-recv
  rd7
dR107/t107
dR7/t7
scalar-return
dR8/t8
  v16
returns-self
  m9
dR9/t9
hands-field-out
  g10
dR10/t10
generic-return
dR11/t11
  built
done
"#
        )
    );
}

/// B-2026-09-04-7 — a SCALAR read through a `Drop`-bearing struct field is
/// a COPY, so the source keeps every field and still owes every body.
///
/// `let z = h.a.id;` over `struct H2 { a: R, b: R }` printed `z1` and ran
/// NEITHER body on all three compiled surfaces (three fields lost all
/// three), while `--interp` ran `b`'s body and then hit an `unreachable!`
/// reading `self.id` off an `a` it had emptied. One cause, two shapes: both
/// backends recorded the read as a MOVE of the field it was reached
/// through. Codegen's `disarm_user_drop_fields_for_moved_field` is handed
/// the FIRST chain segment at depth 2 (B-2026-08-01-31, so the disarm lands
/// on the root's subtree walk), and its `type_runs_user_drop` gate then
/// answered about that INTERMEDIATE rather than the leaf — an intermediate
/// that is Drop-bearing whenever the leaf is interesting at all. Depth 1
/// asks the same gate about the field actually read, which is why
/// `let z = h.n;` was always correct.
///
/// The `shared` cell is the SECOND route to the same defect, not a garnish:
/// `copy_is_only_an_rc_retain` is the typechecker's other exemption from
/// `partial_move_of_drop_struct` ("a `shared` read RETAINS"), so
/// `let sv = h.a.sh;` reached it by the identical path — the compiled
/// backends losing both bodies and the interpreter hitting the same
/// `unreachable!`. It is pinned here because codegen was measured broken on
/// it: `type_runs_user_drop` answers `false` for a shared type, but only
/// when it is asked about the shared field, and at depth 2 it was being
/// asked about the Drop-bearing intermediate instead.
///
/// THE FIXTURE CARRIES ITS OWN ORACLES, which is what makes the expectation
/// below something other than "whatever the compiler prints today":
/// `scalar`, `method` (`h.a.idm()`, a call returning the same scalar) and
/// `nodest` (no projection at all) are three spellings of one program and
/// must print the same body sequence modulo ids — they did not before.
/// `whole` and `plain` are the two controls the filing row measured as
/// already correct, and `move` is the genuine deep move the disarm exists
/// for (B-2026-07-29-39): exactly one `dR8`, never two.
#[test]
fn e2e_scalar_read_through_drop_field_keeps_every_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
impl R { fn idm(ref self) -> i64 { return self.id; } }

struct H2 { a: R, b: R }
struct H3 { a: R, b: R, c: R }
struct Hn { a: R, n: i64 }
struct HOwn { a: R, b: R }
impl Drop for HOwn { fn drop(mut ref self) { println("dHOwn") } }
struct Inner { r: R }
struct Outer { h: Inner }
shared struct Box1 { v: i64 }
struct R2 { id: i64, sh: Box1 }
impl Drop for R2 { fn drop(mut ref self) { println(f"dR2{self.id}/{self.sh.v}") } }
struct Hb { a: R2, b: R2 }
struct Mid { r: R, q: R }
struct Top { m: Mid, s: R }

fn cell_scalar() { let h = H2 { a: mk(1), b: mk(101) }; let z = h.a.id; println(f"  z{z}") }
fn cell_method() { let h = H2 { a: mk(2), b: mk(102) }; let z = h.a.idm(); println(f"  z{z}") }
fn cell_nodest() { let h = H2 { a: mk(3), b: mk(103) }; println("  no") }
fn cell_three()  { let h = H3 { a: mk(4), b: mk(104), c: mk(204) }; let z = h.a.id; println(f"  z{z}") }
fn cell_own()    { let h = HOwn { a: mk(5), b: mk(105) }; let z = h.a.id; println(f"  z{z}") }
fn cell_whole()  { let h = H2 { a: mk(6), b: mk(106) }; let r = h.a; println(f"  z{r.id}") }
fn cell_plain()  { let h = Hn { a: mk(7), n: 4 }; let z = h.n; println(f"  z{z}") }
fn cell_move()   { let o = Outer { h: Inner { r: mk(8) } }; let x = o.h.r; println(f"  z{x.id}") }
fn cell_three_hop() { let t = Top { m: Mid { r: mk(9), q: mk(109) }, s: mk(209) }; let z = t.m.r.id; println(f"  z{z}") }
fn cell_shared() { let h = Hb { a: R2 { id: 11, sh: Box1 { v: 111 } }, b: R2 { id: 12, sh: Box1 { v: 112 } } }; let sv = h.a.sh; println(f"  s{sv.v}") }
fn cell_live()   { let h = H2 { a: mk(10), b: mk(110) }; let z = h.a.id; println(f"  z{z}"); println(f"  b{h.b.id}") }

fn main() {
    println("scalar");   cell_scalar()
    println("method");   cell_method()
    println("nodest");   cell_nodest()
    println("three");    cell_three()
    println("own");      cell_own()
    println("whole");    cell_whole()
    println("plain");    cell_plain()
    println("move");     cell_move()
    println("threehop"); cell_three_hop()
    println("shared");   cell_shared()
    println("live");     cell_live()
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"scalar
dR101/t101
dR1/t1
  z1
method
dR102/t102
dR2/t2
  z2
nodest
dR103/t103
dR3/t3
  no
three
dR204/t204
dR104/t104
dR4/t4
  z4
own
dHOwn
dR105/t105
dR5/t5
  z5
whole
dR106/t106
  z6
dR6/t6
plain
dR7/t7
  z4
move
  z8
dR8/t8
threehop
dR209/t209
dR109/t109
dR9/t9
  z9
shared
dR212/112
dR211/111
  s111
live
  z10
  b110
dR110/t110
dR10/t10
done
"#
    );
}

/// B-2026-09-03-23 — A MONOMORPH BODY MUST ANSWER THE `Drop`-OWNERSHIP QUESTION
/// FROM ITS OWN PARAMETERS, NOT ITS CALLER'S.
///
/// `compile_generic_call` emits the monomorph body INLINE, mid-caller, and
/// populated neither `current_fn_param_names` nor `owned_struct_params` for it.
/// Those sets are per-FUNCTION and were still the ENCLOSING function's, so a
/// LOCAL inside `fn inner[T]` whose name matched ANY caller's parameter answered
/// `owner_runs_bodies = true`, took the tuple element's MEMORY, cap-zeroed the
/// source without recording the take, and left the source's own walk to run the
/// element's `Drop` body against the slot the cap-zeroing had just emptied.
///
/// Every body renders `tag` and `xs.len()`, so a body running on a cap-zeroed
/// husk prints `dRnn//0` and is distinguishable from a correct one — a test that
/// asserted body COUNTS would see one body on both sides and pass. Silent
/// otherwise: no crash, no leak, no diagnostic (valgrind reports the program
/// fully balanced either way).
///
/// ACTION AT A DISTANCE, AND IT SPREADS. The trigger is a name in a DIFFERENT
/// function — `takesH`'s parameter, never called from `main`. Because the body is
/// emitted ONCE and shared by every call site, the corruption reached
/// `plainLocal`, which has no parameter at all. `fresh` is the same shape under a
/// different name (`q`, colliding with `takesQ`) so the fixture cannot pass by
/// special-casing one identifier; `strfield` is the sibling whose struct carries a
/// `String` field, which is what makes the mono param loop's `owned_struct_params`
/// registration fire.
///
/// `marker` pins PLACEMENT, not only content: it extends the SOURCE's live range
/// past the read (`z{h.z}`), so a body belonging to the leaf prints before `z9`
/// and one belonging to the source prints after it. Pre-fix it printed
/// `dR64//0` after `z9` — wrong on both axes at once.
///
/// OVER-REACH CONTROLS, in the opposite direction, so a widened fix fails here
/// rather than passing quietly. `nongen` is the non-generic twin that was correct
/// throughout; `nodrop` instantiates the same shape at a `Drop`-less type and must
/// stay silent; `genparam` is the by-value param of a generic function that
/// B-2026-09-03-16 guarded and this fix un-guards — it must still read ONE body,
/// and is the same cell as that row's `genericfn`.
///
/// Measured pre-fix: `collide`, `fresh`, `strfield` and `marker` all print the
/// husk on all three compiled surfaces against a clean `--interp`; the three
/// controls are byte-identical either way.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_monomorph_body_answers_drop_ownership_from_its_own_params`, pinned to the
/// same string.
#[test]
fn e2e_monomorph_body_answers_drop_ownership_from_its_own_params() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Hn { pe: (R, i64), z: i64 }
struct Hs { pe: (R, i64), s: String }
struct G[T] { pe: (T, i64), z: i64 }
struct Plain { a: i64, b: i64 }

fn genLocal[T](x: T) -> i64 { let h = Hn { pe: (mk(60), 0), z: 1 }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}/{r.xs.len()}"); return k; }
fn genFresh[T](x: T) -> i64 { let q = Hn { pe: (mk(61), 0), z: 1 }; let (r, k) = q.pe; println(f"  b{r.id}/{r.tag}/{r.xs.len()}"); return k; }
fn genStr[T](x: T)   -> i64 { let h = Hs { pe: (mk(62), 0), s: f"s" }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}/{r.xs.len()}"); return k; }
fn ngLocal(x: i64)   -> i64 { let h = Hn { pe: (mk(63), 0), z: 1 }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}/{r.xs.len()}"); return k; }
fn genMark[T](x: T)  -> i64 { let h = Hn { pe: (mk(64), 0), z: 9 }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}"); println(f"  z{h.z}"); return k; }
fn genNoDrop[T](x: T) -> i64 { let h = G[Plain] { pe: (Plain { a: 1, b: 2 }, 0), z: 1 }; let (p, k) = h.pe; println(f"  b{p.a}/{p.b}"); return k; }

fn takesH(h: Hn) -> i64 { return genLocal(5); }
fn takesQ(q: Hn) -> i64 { return genFresh(5); }
fn takesHs(h: Hs) -> i64 { return genStr(5); }
fn takesHm(h: Hn) -> i64 { return genMark(5); }
fn takesHn(h: G[Plain]) -> i64 { return genNoDrop(5); }

fn plainLocal() -> i64 { return genLocal(5); }
fn plainFresh() -> i64 { return genFresh(5); }
fn plainStr()   -> i64 { return genStr(5); }
fn plainMark()  -> i64 { return genMark(5); }
fn plainNoDrop() -> i64 { return genNoDrop(5); }

fn gfn[T](h: G[T]) -> i64 { let (r, k) = h.pe; println("  in"); return k; }
fn gfnLocal() { let h = G[R] { pe: (mk(65), 5), z: 9 }; let _ = gfn(h); }

fn main() {
    println("collide");  let _ = plainLocal();  println("collide end")
    println("fresh");    let _ = plainFresh();  println("fresh end")
    println("strfield"); let _ = plainStr();    println("strfield end")
    println("nongen");   let _ = ngLocal(5);    println("nongen end")
    println("marker");   let _ = plainMark();   println("marker end")
    println("nodrop");   let _ = plainNoDrop(); println("nodrop end")
    println("genparam"); gfnLocal();            println("genparam end")
    println("done")
    }
    "#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"collide
  b60/t60/1
dR60/t60/1
collide end
fresh
  b61/t61/1
dR61/t61/1
fresh end
strfield
  b62/t62/1
dR62/t62/1
strfield end
nongen
  b63/t63/1
dR63/t63/1
nongen end
marker
  b64/t64
dR64/t64/1
  z9
marker end
nodrop
  b1/2
nodrop end
genparam
  in
dR65/t65/1
genparam end
done
"#
    );
}

/// B-2026-09-09-21 / B-2026-09-16-37 — the CODEGEN twin of
/// `tests/interpreter.rs`'s `test_discarded_tuple_return_runs_its_element_drop_body`,
/// same cells and same expectations.
///
/// IT EXISTS BECAUSE ITS ABSENCE COST A DOUBLED BODY. `8377932` fixed the
/// discarded-tuple body on both backends but pinned only the INTERPRETER
/// side, so a codegen-only regression in the same commit went unseen: the
/// bodies registration was gated on the MEMORY walk's verdict, which also
/// claims a method-call tail, and for `impl S {{ fn m(self) -> (R, i64) }}`
/// the receiver's own walk already owns the element the method moved into
/// the tuple — `s.m();` printed `dR dR` compiled against one interpreted.
/// An A/B pair pinning the SAME bytes is what makes that impossible; an
/// interpreter-only fixture cannot see a compiled double.
#[test]
fn e2e_discarded_tuple_return_runs_its_element_drop_body() {
    // 1 — the row's shape: a bare discard statement.
    let Some(out) = run_program(
        "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\" }; }\n\
             fn f(r: R) -> (R, i64) { return (r, 9); }\n\
             fn main() { f(mk(20)); println(\"ok\"); }\n",
    ) else {
        return;
    };
    assert_eq!(out, "dR20\nok\n");

    // 2 — the `let _ =` spelling, broken the same way and fixed by the
    //     same pair.
    let Some(out) = run_program(
        "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\" }; }\n\
             fn f(r: R) -> (R, i64) { return (r, 9); }\n\
             fn main() { let _ = f(mk(22)); println(\"ok\"); }\n",
    ) else {
        return;
    };
    assert_eq!(out, "dR22\nok\n");

    // 3 — CONTROL: the discarded BARE struct, correct before the fix.
    let Some(out) = run_program(
        "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\" }; }\n\
             fn f(r: R) -> (R, i64) { return (r, 9); }\n\
             fn main() { mk(21); println(\"ok\"); }\n",
    ) else {
        return;
    };
    assert_eq!(out, "dR21\nok\n");

    // 3b — B-2026-09-16-37: a GENERIC callee is declined, because codegen
    //      resolves this shape from the DECLARED element types and an
    //      erased `T` yields no walker. Agreed gap, both backends silent.
    let Some(out) = run_program(
        "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\" }; }\n\
             fn f(r: R) -> (R, i64) { return (r, 9); }\n\
             fn fgen[T](t: T) -> (T, i64) { return (t, 5); }\n\
             fn main() { fgen(mk(31)); println(\"ok\"); }\n",
    ) else {
        return;
    };
    assert_eq!(out, "ok\n");

    // 3c — B-2026-09-16-37: THE CELL THAT CATCHES THE DOUBLE. A method
    //      callee is declined; the receiver's own walk owns the element.
    //      Exactly one `dR32`.
    let Some(out) = run_program(
        "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\" }; }\n\
             fn f(r: R) -> (R, i64) { return (r, 9); }\n\
             struct S { r: R }\n\
             impl S { fn m(self) -> (R, i64) { return (self.r, 3); } }\n\
             fn main() { let s: S = S { r: mk(32) }; s.m(); println(\"ok\"); }\n",
    ) else {
        return;
    };
    assert_eq!(out, "dR32\nok\n");

    // 4 — CONTROL: no Drop-bearing element, stays silent.
    let Some(out) = run_program(
        "fn g(i: i64) -> (i64, i64) { return (i, 9); }\n\
             fn main() { g(5); println(\"ok\"); }\n",
    ) else {
        return;
    };
    assert_eq!(out, "ok\n");
}

/// B-2026-09-02-38 — the STRUCT-PATTERN spelling of B-2026-09-02-25: a
/// `let S { r, k } = s;` over an owned struct param binds VIEWS of the callee's
/// entry copy, so a later `let m = r;` must MOVE the body rather than mint a
/// second owner. Was `b9 dR9 dR9` on all four surfaces where one body is due.
///
/// THE `end` MARKER IS LOAD-BEARING, and it is what refutes the reason this shape
/// was held out of -25. That row expected `param_view_locals` to be the wrong
/// instrument here, because `finish_owned_struct_destructure` TRANSFERS the
/// field's body to the leaf instead of leaving it with the source — and after a
/// transfer "someone else runs it" is false. Measured: the transfer happens for a
/// LOCAL source and NOT for a param, being gated on `var_owns_struct_field_bodies`
/// — on the source carrying a `StructFieldBodies` action, which a by-value param
/// has none of. So the `local` cell fires its body BEFORE `end` (the leaf's
/// live-range end) while every param cell fires AFTER it (the source's owner),
/// which is also where the tuple spelling puts it. Both orderings in one pinned
/// string is what would catch a regression that MOVES a fire rather than losing
/// one.
///
/// `heapstr` is the `owned_struct_params` cell. `Hs { r: R, name: String }` has a
/// direct `String` field, so the param IS in that set — the set whose presence
/// makes `finish_place_source_tuple_destructure` bail outright. The struct path
/// has no such bail and converges at one body here, the same refutation
/// `e2e_projection_source_tuple_destructure_is_a_view`'s `ownstr` cell records for
/// the tuple side.
///
/// `nested` binds a whole nested STRUCT FIELD (`let Ou { inner, k } = o;`), not a
/// nested PATTERN: codegen registers only dispatch for a nested pattern's leaves
/// and leaves their cleanup a tracked narrow leak, so the interpreter's
/// `collect_destructure_binding_names` deliberately does not recurse into one
/// either.
///
/// `norebind` and `refparam` are the over-reach controls, failing in opposite
/// directions: withholding a body too eagerly shows up as `norebind` running NONE,
/// and `refparam` (a borrowed receiver the caller still owns) must never gain one.
///
/// NO METHOD CELL, deliberately. A by-value param destructure inside a METHOD
/// places this body at the callee's scope exit on the compiled backends and at the
/// leaf's NLL death interpreted — one body either way, different point — and that
/// split is PRE-EXISTING and family-wide: the TUPLE spelling has it on `main`
/// today, from -25/-40. This fix takes the struct spelling's method case from two
/// bodies to one, i.e. onto exactly the tuple spelling's behaviour, rather than
/// inventing a new answer for it. Filed separately.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_struct_pattern_destructure_of_owned_param_is_a_view`, pinned to the
/// same string.
/// B-2026-09-03-7 — WHERE a method's fresh-temp argument runs its `Drop` body.
///
/// The row's fixture note says its own cells "deliberately carry NO method
/// cell, since they are pinned to one string across all four surfaces and this
/// shape cannot be". It can now, which is the whole content of the fix: the
/// interpreter fired a method's fresh-temp argument at the leaf's NLL DEATH
/// while every compiled backend fired it after the call returned. Six cells,
/// one string, all four surfaces.
///
/// `m1`-`m3`, `m5`, `m6` place the body AFTER the callee's `end` — design.md's
/// temporary-lifetime table, "Function/method call argument | After the call
/// returns". `m4` is the one shape that keeps it inside: the parameter is
/// handed to a LOCAL aggregate, which owns it from there
/// (`fn_moves_param_into_local_aggregate`). The count is one everywhere, and it
/// is PLACEMENT that this pin exists to hold — a body count cannot see this
/// bug at all, which is how it survived B-2026-09-02-25/-40/-38.
///
/// `m3`/`m5` (a move into another call) and `m6` (a destructure leaf that just
/// dies) are the cells that separate this from the neighbouring rows: all
/// three fire caller-side, so a fix that made the callee own its parameter
/// outright would move them and is not what this row asked for.
#[test]
fn e2e_method_fresh_temp_arg_drop_body_placement() {
    assert_eq!(
            run_program(
                r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct S1 { r: R }
struct H  { n: i64 }
fn mk(n: i64) -> R { return R { id: n }; }
fn read(x: R) -> i64 { return x.id; }

impl H {
    fn m1(ref self, r: R)        { println("  end") }
    fn m2(ref self, r: R)        { let m = r; println("  end") }
    fn m3(ref self, r: R)        { let m = r; read(m); println("  end") }
    fn m4(ref self, r: R) -> i64 { let s = S1 { r: r }; let x = s.r; return 7; }
    fn m5(ref self, t: (R, i64)) { let (a, b) = t; let m = a; read(m); println("  end") }
    fn m6(ref self, t: (R, i64)) { let (a, b) = t; println("  end") }
}

fn main() {
    let h = H { n: 0 };
    println("m1 bare-die");       h.m1(mk(1))
    println("m2 rebind-die");     h.m2(mk(2))
    println("m3 into-call");      h.m3(mk(3))
    println("m4 wrap-moveout");   let v = h.m4(mk(4)); println(f"  v={v}")
    println("m5 destr-intocall"); h.m5((mk(5), 9))
    println("m6 destr-die");      h.m6((mk(6), 9))
    println("done")
}
"#
            ),
            Some(
                "m1 bare-die\n  end\n  dR1\nm2 rebind-die\n  end\n  dR2\nm3 into-call\n  end\n  dR3\nm4 wrap-moveout\n  dR4\n  v=7\nm5 destr-intocall\n  end\n  dR5\nm6 destr-die\n  end\n  dR6\ndone\n"
                    .to_string()
            ),
            "a method's fresh-temp arg body belongs after the call returns"
        );
}

/// B-2026-08-31-39 (the rendering half) — an `Option[T]` / `Result[T, E]`
/// inside a GENERIC fn renders at the type the call actually instantiated
/// `T` at, for the AGGREGATE instantiations as well as the scalar ones.
///
/// The scalars always worked and the aggregates always refused, and the
/// split was the whole diagnosis: a monomorph body resolves `T`
/// symbolically through `subst_monomorph_type_params`, and both channels
/// feeding it are lossy in DIFFERENT ways. The name map is HEAD-ONLY, so
/// `T = Vec[i64]` resolved to a bare `Vec` — an elementless container no
/// program can write. A NAMELESS type argument (`Array`, `Slice`, a tuple)
/// has no name at all, so `T` stayed `T`. `i64` and `String` are the two
/// cases a head name happens to spell exactly, which is why exactly those
/// two rendered. The fix adds the typechecker's own solution as a third,
/// exact channel (`call_type_subs_te`) rather than teaching the Display
/// gate about generics — the gate was already right, it was being handed
/// half a type.
///
/// EVERY LINE HERE IS A DISTINCT LOSS CHANNEL, which is why the fixture is
/// one program rather than a matrix of small ones: `Vec[i64]` and
/// `Vec[String]` pin the head-only loss at two elements (a shared body
/// would print one of them wrong), `Array[i64, 2]` beside `Array[i64, 3]`
/// pins the nameless loss AND the mono-symbol collision that made it
/// dangerous to fix (B-2026-08-31-48 — two lengths at one element type must
/// not share a body), `Slice` and the tuple are the other two nameless
/// shapes, and `Vec[Vec[i64]]` is the nested case a head name flattens.
/// `Result[T, E]` and the generic METHOD were this row's two NOT MEASURED
/// items: both were miscompiling, and both are fixed by the same channel.
///
/// RED pre-fix as a compile ERROR, not a wrong answer: `error: codegen
/// failed: Display of `Option[T]` is not yet supported under codegen`.
/// B-2026-08-30-23 — the conditional-return parameter matrix: three call
/// spellings x two argument shapes x both branch directions, with an ABSOLUTE
/// expectation.
///
/// `fn pick(a: R, k: bool) -> R {{ if k {{ return R {{ .. }}; }} return a; }}` hands the
/// parameter back on one path and lets it die on the other. Correct is one
/// `Drop` body per object: at `k = true` the parameter dies inside the callee
/// (`d<arg>` before the caller's print), at `k = false` it becomes the result
/// and dies at the caller's scope exit (print, then `d<arg>`).
///
/// FOUR OF THE TWELVE CELLS WERE WRONG, in two opposite directions, and the row
/// as filed reported only one of them:
///
///     free  / fresh / k=true    body LOST      (the row's headline)
///     free  / named / k=false   body DOUBLED
///     assoc / named / k=false   body DOUBLED
///     method/ fresh / k=false   body DOUBLED   (compiled lanes only)
///
/// The row's own matrix additionally claimed `free/named/k=true` and
/// `assoc/named/k=false` were correct; re-measured here they were not, and its
/// claim that the method spelling was a working model to copy was measured
/// against one argument shape only.
///
/// ALL THREE SPELLINGS ARE IN ONE PROGRAM DELIBERATELY. The three arms have
/// drifted apart repeatedly (B-2026-08-28-70 moved the method arm,
/// B-2026-08-29-54 the associated one, this row the free one), each time
/// because a fix was verified on the spelling it was written for. A single
/// expectation over all three is what makes the next drift fail here.
///
/// Every cell is wrapped in its own function so the drops are scoped and the
/// ordering is unambiguous; reading them at `main` scope would interleave
/// twelve objects' bodies at one exit.
#[test]
fn codegen_conditional_return_param_drop_matrix() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct H { n: i64 }
fn mk(i: i64) -> String { return f"pay-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; }
fn fpick(a: R, k: bool) -> R { if k { return R { id: 98, s: mk(98) }; } return a; }
impl H {
    fn apick(a: R, k: bool) -> R { if k { return R { id: 98, s: mk(98) }; } return a; }
    fn mpick(ref self, a: R, k: bool) -> R { if k { return R { id: 98, s: mk(98) }; } return a; }
}
fn ff_t() { let x = fpick(R { id: 11, s: mk(11) }, true); println(f"A{x.id}"); }
fn ff_f() { let x = fpick(R { id: 12, s: mk(12) }, false); println(f"B{x.id}"); }
fn fn_t() { let r = R { id: 13, s: mk(13) }; let x = fpick(r, true); println(f"C{x.id}"); }
fn fn_f() { let r = R { id: 14, s: mk(14) }; let x = fpick(r, false); println(f"D{x.id}"); }
fn af_t() { let x = H.apick(R { id: 21, s: mk(21) }, true); println(f"E{x.id}"); }
fn af_f() { let x = H.apick(R { id: 22, s: mk(22) }, false); println(f"F{x.id}"); }
fn an_t() { let r = R { id: 23, s: mk(23) }; let x = H.apick(r, true); println(f"G{x.id}"); }
fn an_f() { let r = R { id: 24, s: mk(24) }; let x = H.apick(r, false); println(f"I{x.id}"); }
fn mf_t() { let h = H { n: 0 }; let x = h.mpick(R { id: 31, s: mk(31) }, true); println(f"J{x.id}"); }
fn mf_f() { let h = H { n: 0 }; let x = h.mpick(R { id: 32, s: mk(32) }, false); println(f"K{x.id}"); }
fn mn_t() { let h = H { n: 0 }; let r = R { id: 33, s: mk(33) }; let x = h.mpick(r, true); println(f"L{x.id}"); }
fn mn_f() { let h = H { n: 0 }; let r = R { id: 34, s: mk(34) }; let x = h.mpick(r, false); println(f"M{x.id}"); }
fn main() {
    ff_t(); ff_f(); fn_t(); fn_f();
    af_t(); af_f(); an_t(); an_f();
    mf_t(); mf_f(); mn_t(); mn_f();
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "d11\nA98\nd98\n\
             B12\nd12\n\
             d13\nC98\nd98\n\
             D14\nd14\n\
             d21\nE98\nd98\n\
             F22\nd22\n\
             d23\nG98\nd98\n\
             I24\nd24\n\
             d31\nJ98\nd98\n\
             K32\nd32\n\
             d33\nL98\nd98\n\
             M34\nd34\n\
             end\n"
    );
}

/// B-2026-09-01-44 — the compiled half of
/// `test_assoc_fn_two_by_value_params_drop_matrix` (tests/interpreter.rs).
///
/// These lanes were already correct when the interpreter lost the dying
/// param's body, which is what made the gap an A/B divergence rather than a
/// shared one. Pinned here so the fix cannot later be "restored" by
/// weakening this side to match a regressed interpreter.
#[test]
fn codegen_assoc_fn_two_by_value_params_drop_matrix() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct H { n: i64 }
fn mk(i: i64) -> String { return f"pay-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; }

impl H {
    fn two(a: R, b: R, k: bool) -> R { if k { return b; } return a; }
}

fn t_t() { let x = H.two(R { id: 41, s: mk(41) }, R { id: 42, s: mk(42) }, true); println(f"T{x.id}"); }
fn t_f() { let x = H.two(R { id: 43, s: mk(43) }, R { id: 44, s: mk(44) }, false); println(f"T{x.id}"); }

fn main() { t_t(); t_f(); println("end"); }
"#,
    ) else {
        return;
    };
    assert_eq!(out, "d41\nT42\nd42\nd44\nT43\nd43\nend\n");
}

/// B-2026-08-01-4 — a fresh Drop-bearing call-arg temp in LET position
/// fires its body at the END OF THE STATEMENT, where the interpreter
/// fires it (`run_fresh_temp_arg_drops` runs as the call returns).
/// Pre-fix `karac build` fired the owned-param shape (`let x =
/// consume(mk(1))`) at SCOPE EXIT — after every later statement's
/// output — and the ref-param shape (`let y = peek(mk(2))`) NEVER (no
/// body was registered for a fresh rvalue borrowed by the callee). The
/// bare-statement shape (`consume(mk(3));`) was already correct via the
/// one-shot discard frame and pins that nothing regressed. Twin of
/// `tests/interpreter.rs`'s
/// `test_fresh_arg_temp_drop_fires_at_statement_end`.
#[test]
fn e2e_fresh_arg_temp_drop_fires_at_statement_end() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             fn mk(n: i64) -> Res {\n\
             \x20   return Res { id: n, name: f\"r{n}\" };\n\
             }\n\
             fn consume(r: Res) -> i64 {\n\
             \x20   return r.id + 100;\n\
             }\n\
             fn peek(r: ref Res) -> i64 {\n\
             \x20   return r.id;\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let x = consume(mk(1));\n\
             \x20   println(f\"x={x}\");\n\
             \x20   println(\"b\");\n\
             \x20   let y = peek(mk(2));\n\
             \x20   println(f\"y={y}\");\n\
             \x20   println(\"c\");\n\
             \x20   consume(mk(3));\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a\ndrop 1 r1\nx=101\nb\ndrop 2 r2\ny=2\nc\ndrop 3 r3\nend\n"
    );
}

/// B-2026-08-01-4 (struct-literal residual, closed) — a struct LITERAL
/// rvalue borrowed by a `ref` param (`peek(Res { .. })`) fires its Drop
/// body at statement end like the Call-shaped fresh args: literals are
/// fresh by construction, but `expr_yields_fresh_owned_temp` only
/// admits Call/MethodCall shapes, so the refarg registration silently
/// skipped them while the interpreter's arg hook fired. Twin of
/// `tests/interpreter.rs`'s `test_struct_literal_ref_param_arg_drop`.
#[test]
fn e2e_struct_literal_ref_param_arg_drop() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             fn peek(r: ref Res) -> i64 {\n\
             \x20   return r.id;\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let z = peek(Res { id: 4, name: f\"r4\" });\n\
             \x20   println(f\"z={z}\");\n\
             \x20   println(\"b\");\n\
             \x20   peek(Res { id: 5, name: f\"r5\" });\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 4 r4\nz=4\nb\ndrop 5 r5\nend\n");
}

/// A by-value param that escapes through a CALL in return position runs its
/// `Drop` body once (B-2026-08-28-62).
///
/// Kara passes a by-value argument under a caller-drops convention, and the
/// caller declines to drop only where it can see the value leaving. Two
/// routes were modelled — the param returned bare or moved into a returned
/// aggregate literal (`fn_returns_param`), and the param stored into `self`
/// or a `ref` param (B-2026-08-26-9). `fn outer(y: R) -> … { return src(y); }`
/// is neither, so the caller fired `y`'s body while the value was still
/// travelling out through `src`'s return: two bodies for one object, on all
/// three backends.
///
/// `callee-consumes` is the row that makes the predicate interprocedural
/// rather than syntactic. Passing the param to a call proves nothing on its
/// own — `fn consumer(y: R) -> i64 { return uses(y); }` genuinely consumes
/// it — so the CALLEE's own answer decides, and a fix that keyed on "the
/// param appears as an argument at a return site" would take this row's only
/// body away. That is the expensive direction here: a missed escape keeps a
/// double body, a false one loses the value's only `Drop`.
///
/// TWO SHAPES STAY DOUBLED, by the same conservative rule and measured
/// rather than assumed: a two-hop chain (`mid` forwards to `outer` forwards
/// to a return) and a param forwarded into a PROJECTION of the call's result
/// (`return (src(y).0, 9)`). Both are one level past what this recognizes,
/// and admitting them means proving the value survives two frames, not one.
/// They are noted here rather than pinned so this fixture asserts only counts
/// that are correct.
#[test]
fn e2e_param_escaping_through_a_forwarded_call_drops_once() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
             struct BoxR { v: R }\n\
             fn src2(x: R) -> (BoxR, i64) { return (BoxR { v: x }, 1); }\n";
    for (label, body, want) in [
        // The row's own repro.
        (
            "forwarding",
            "fn outer2(y: R) -> (BoxR, i64) { return src2(y); }\n\
                 fn main() { let (a, n) = outer2(R { id: 49 }); println(f\"{n}\"); }\n",
            "drop 49\n1\n",
        ),
        // The GENERIC spelling, which was silent before B-2026-08-28-61 —
        // right by accident, two bugs cancelling — and doubled after it.
        (
            "forwarding-generic",
            "struct Box2[T] { v: T }\n\
                 fn src[T](x: T) -> (Box2[T], i64) { return (Box2[T] { v: x }, 1); }\n\
                 fn outerg[U](y: U) -> (Box2[U], i64) { return src(y); }\n\
                 fn main() { let (b, n) = outerg(R { id: 51 }); println(f\"{n}\"); }\n",
            "drop 51\n1\n",
        ),
        // CONTROL — the callee CONSUMES the argument, so the body belongs to
        // this frame and a false escape would take it away entirely.
        (
            // ORDER CORRECTED by B-2026-08-29-55 -- the argument temp dies AS THE
            // CALL RETURNS (design.md's "Function/method call argument | After the
            // call returns"), which is BEFORE the enclosing `println` runs. The old
            // expectation held it to the statement's `;`; `--interp` printed the
            // body first all along, so that line recorded a run-vs-build divergence
            // rather than a decision.
            "callee-consumes",
            "fn uses(x: R) -> i64 { return x.id; }\n\
                 fn consumer(y: R) -> i64 { return uses(y); }\n\
                 fn main() { println(f\"{consumer(R { id: 50 })}\"); }\n",
            "drop 50\n50\n",
        ),
        // CONTROL — no forwarding at all, the shape that always worked.
        (
            "direct-call",
            "fn main() { let (e, n) = src2(R { id: 54 }); println(f\"{n}\"); }\n",
            "drop 54\n1\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-28-1 — a user `Drop` body must run for a struct DESTRUCTURED
/// out of a `let`, on the compiled backends as well as the interpreter.
///
/// A destructure leaf registered its MEMORY drop only, never the
/// `karac_drop_<T>` wrapper that runs the body, so
/// `let p = (R { id: 41 }, 1); let (r, n) = p;` printed the body under
/// `karac run --interp` and NOTHING under `karac run` or `karac build`.
/// Peak RSS stayed flat (the buffers were freed), so this was silent RAII
/// rather than a leak: a `Drop` that closes a file or releases a lock did
/// not happen once compiled. The interpreter was right and both SHIPPED
/// backends were wrong, which is what made it high severity.
///
/// The tuple-PARAM row is the load-bearing control, and it asserts ONE
/// body, not merely a non-empty one. A param already owns its elements
/// (`make_tuple_param_callee_owned` deep-copies them at entry and registers
/// a tuple drop that runs their bodies), so an unconditional leaf
/// registration makes it print TWICE — which is exactly what the first cut
/// of this fix did. `run_program` compares whole stdout, so a duplicated
/// body fails here.
#[test]
fn e2e_user_drop_body_runs_for_a_let_destructure_leaf() {
    const DROPPER: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n";
    for (label, body, want) in [
            // The row's own repro: the source is a tuple LOCAL.
            (
                "tuple-local",
                "fn main() { let p = (R { id: 41 }, 1); let (r, n) = p; println(f\"{r.id + n}\"); }\n",
                "42\ndrop 41\n",
            ),
            // The row's sibling: the source is a CALL RESULT.
            (
                "call-result",
                "fn make() -> (R, i64) { return (R { id: 41 }, 1); }\n\
                 fn main() { let (r, n) = make(); println(f\"{r.id + n}\"); }\n",
                "42\ndrop 41\n",
            ),
            // A tuple LITERAL RHS reached NEITHER branch of the destructure
            // finisher — not "fresh" by the general predicate, not a place —
            // so its leaves registered no cleanup at all.
            (
                "tuple-literal",
                "fn main() { let (r, n) = (R { id: 41 }, 1); println(f\"{r.id + n}\"); }\n",
                "42\ndrop 41\n",
            ),
            // The leaf declares no Drop of its own but OWNS a Drop-bearing
            // field: the second arm of the cascade, which has no wrapper to
            // hang off and takes the field-bodies walk plus ordinary memory.
            (
                "field-bearing-leaf",
                "struct W { r: R }\n\
                 fn main() { let p = (W { r: R { id: 41 } }, 1); let (w, n) = p; println(f\"{w.r.id + n}\"); }\n",
                "42\ndrop 41\n",
            ),
            // Both elements drop: the bodies run at each leaf's own NLL end.
            (
                "two-droppers",
                "fn main() { let p = (R { id: 41 }, R { id: 7 }); let (a, b) = p; println(f\"{a.id + b.id}\"); }\n",
                "48\ndrop 7\ndrop 41\n",
            ),
            // A nested tuple pattern, on the fresh path.
            (
                "nested-from-call",
                "fn make() -> ((R, i64), i64) { return ((R { id: 41 }, 2), 1); }\n\
                 fn main() { let ((r, m), n) = make(); println(f\"{r.id + m + n}\"); }\n",
                "44\ndrop 41\n",
            ),
            // An ENUM leaf whose live variant carries a Drop-bearing payload —
            // the enum arm of the same helper.
            (
                "enum-leaf-from-call",
                "enum E { A(R), B }\n\
                 fn make() -> (E, i64) { return (E.A(R { id: 41 }), 1); }\n\
                 fn main() { let (e, n) = make(); println(f\"{n}\"); }\n",
                "drop 41\n1\n",
            ),
            // B-2026-08-28-8 — the same nested pattern with a LOCAL source.
            // The place-source walker `continue`d past a nested `Tuple`
            // pattern outright, so the inner leaf was never reached and the
            // body was silent on all three compiled surfaces. The `from-call`
            // row above passed throughout, which is what pins the defect to
            // the SOURCE rather than to the nesting.
            (
                "nested-from-local",
                "fn main() { let p = ((R { id: 41 }, 2), 1); let ((r, m), n) = p; println(f\"{r.id + m + n}\"); }\n",
                "44\ndrop 41\n",
            ),
            // The literal source of the same nested shape — correct before the
            // fix (it takes the fresh path), so it holds that half in place.
            (
                "nested-from-literal",
                "fn main() { let ((r, m), n) = ((R { id: 41 }, 2), 1); println(f\"{r.id + m + n}\"); }\n",
                "44\ndrop 41\n",
            ),
            // CONTROL for the nested shape — the leaf takes the body only
            // because it also takes the MEMORY (the recursion cap-zeroes the
            // source at the nested index). Undestructured, the source's own
            // walk reaches the nested leaf and runs the body; this row is what
            // fails if the recursion ever registers a body without the
            // matching cap-zero, since both would then fire.
            (
                "nested-no-destructure-control",
                "fn main() { let p = ((R { id: 41 }, 2), 1); println(f\"{p.1}\"); }\n",
                "1\ndrop 41\n",
            ),
            // B-2026-08-28-9 — the LOCAL twin of `enum-leaf-from-call`. The
            // per-leaf `TypeExpr` arrived as an EMPTY path, so the `is_enum`
            // test could never fire and the leaf was skipped before any
            // registration. Fixed upstream of the walker, by having the
            // element-type lookup prefer the let-site's exact record over the
            // name-derived synthesis that cannot spell an enum constructor.
            (
                "enum-leaf-from-local",
                "enum E { A(R), B }\n\
                 fn main() { let p = (E.A(R { id: 41 }), 1); let (e, n) = p; println(f\"{n}\"); }\n",
                "drop 41\n1\n",
            ),
            // Leaf inside a nested block: the body fires at the leaf's live-range
            // end, BEFORE the statement following the block.
            (
                "inner-scope",
                "fn main() { { let p = (R { id: 41 }, 1); let (r, n) = p; println(f\"{r.id + n}\"); } println(\"after\"); }\n",
                "42\ndrop 41\nafter\n",
            ),
            // CONTROL — a tuple PARAM source, which was already correct. It must
            // stay at exactly ONE body; the first cut of this fix printed two.
            (
                "tuple-param-control",
                "fn take(p: (R, i64)) { let (r, n) = p; println(f\"{r.id + n}\"); }\n\
                 fn main() { take((R { id: 41 }, 1)); }\n",
                "42\ndrop 41\n",
            ),
            // CONTROL — the same local read by FIELD instead of destructured.
            // Correct on all three backends before the fix, so it pins the gap
            // to the destructuring `let` rather than to the tuple or the struct.
            (
                "field-read-control",
                "fn main() { let p = (R { id: 41 }, 1); println(f\"{p.0.id + p.1}\"); }\n",
                "42\ndrop 41\n",
            ),
        ] {
            let prog = format!("{DROPPER}{body}");
            assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
        }
}

/// B-2026-08-28-53 — a DISCARDED own-`Drop` parent temp orders its own body
/// against the returned FIELD's the way the interpreter does.
///
/// `take(W { r: R { id: 47 }, n: 5 });` with the result thrown away printed
/// `drop 47` / `drop W5` under LLJIT and AOT against `--interp`'s
/// `drop W5` / `drop 47`. Counts were right on every backend — one body per
/// object — so this was ordering alone, and no count-based assertion in the
/// suite could see it.
///
/// THE APPLICABLE RULE IS LIVE-RANGE END, not "parent before fields" and not
/// LIFO. `fn take(w: W) -> R { let W { r, n } = w; r }` MOVES `r` out inside
/// the callee, so by the time either body runs in the CALLER there is no
/// parent/child relation left — they are two independent temporaries. Read
/// as LIFO-by-introduction the compiled order would be right. What settles
/// it is design.md § Drop ordering's other rule: destructors fire at each
/// binding's live-range end, and the ARGUMENT temporary's last use is the
/// call. It dies there, before the result exists, so the two live ranges do
/// not overlap and LIFO never gets to arbitrate.
///
/// `bound` IS THE ORACLE, and it is why this is a correction rather than a
/// preference: codegen already produced the right order one spelling over.
/// A `let`-bound result retires the argument temp through
/// `drain_statement_temp_user_drops` — instrumented, that drain fires
/// exactly `karac_dropnf_W` in the bound spelling and NOTHING in the
/// discarded one — so the bound form printed `drop W5` first on all three
/// backends all along. The discarded form put both temporaries on one
/// discard frame, which drained in reverse index order and ran the
/// later-pushed RESULT first. This makes the discarded spelling agree with
/// its own bound twin.
///
/// `discarded-wildcard` covers `let _ = take(..);`, the other discard
/// spelling: it routes through a different arm of `compile_stmt` and had the
/// same inversion, so the two must not drift apart.
///
/// `no-own-drop-parent` and `scalar-parent` are the controls — a parent with
/// no `Drop` of its own, and one whose moved-out field is scalar. Both were
/// correct before and are byte-identical after, so a fix that reordered the
/// discard frame wholesale rather than at the argument/result boundary fails
/// them.
///
/// The memory twin is
/// `asan_discarded_own_drop_parent_temp_order_is_memory_balanced`, which
/// exists because this reordering moves the parent's heap free AHEAD of the
/// returned field's body — the one thing that could turn an ordering fix
/// into a use-after-free.
#[test]
fn e2e_discarded_own_drop_parent_temp_orders_its_body_first() {
    const D: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n\
             struct W { r: R, n: i64 }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"drop W{self.n}\"); } }\n\
             #[allow(partial_move_of_drop_struct)]\n\
             fn take(w: W) -> R { let W { r, n } = w; r }\n";
    for (label, body, want) in [
        (
            "discarded",
            format!(
                "{D}fn main() {{ take(W {{ r: R {{ id: 47 }}, n: 5 }}); println(\"end\"); }}\n"
            ),
            "drop W5\ndrop 47\nend\n",
        ),
        (
            "discarded-wildcard",
            format!(
                "{D}fn main() {{ let _ = take(W {{ r: R {{ id: 47 }}, n: 5 }});\n\
                     \x20            println(\"end\"); }}\n"
            ),
            "drop W5\ndrop 47\nend\n",
        ),
        // THE ORACLE — correct on every backend before this row, and the
        // behaviour the discarded spellings are matched to.
        (
            "bound",
            format!(
                "{D}fn main() {{ let got = take(W {{ r: R {{ id: 47 }}, n: 5 }});\n\
                     \x20            println(f\"got {{got.id}}\"); println(\"end\"); }}\n"
            ),
            "drop W5\ngot 47\ndrop 47\nend\n",
        ),
        // CONTROL — the parent declares no `Drop`, so only the field's body
        // exists and there is no order to get wrong.
        (
            "no-own-drop-parent",
            "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n\
                 struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> R { let W { r, n } = w; r }\n\
                 fn main() { take(W { r: R { id: 47 }, n: 5 }); println(\"end\"); }\n"
                .to_string(),
            "drop 47\nend\n",
        ),
        // CONTROL — a scalar moved-out field, so the parent's body is the
        // only one.
        (
            "scalar-parent",
            "struct W { a: i64, n: i64 }\n\
                 impl Drop for W { fn drop(mut ref self) { println(f\"drop W{self.n}\"); } }\n\
                 fn take(w: W) -> i64 { let W { a, n } = w; a }\n\
                 fn main() { take(W { a: 47, n: 5 }); println(\"end\"); }\n"
                .to_string(),
            "drop W5\nend\n",
        ),
    ] {
        assert_eq!(run_program(&body).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-28-65 — the UNDER-FIRE horn of B-2026-08-28-51's mechanism: a
/// `return <local>` NESTED IN A BRANCH lost the local's `Drop` body on the
/// path that never takes the `return`, on all three COMPILED backends while
/// the interpreter was right.
///
/// `fn take(k) -> R { let r = R { id: 41 }; if k { return r; } R { id: 99 } }`
/// with `k = false` printed `99` / `drop 99` under LLJIT, AOT and AOT with
/// `KARAC_AUTO_PAR=0`, against `--interp`'s correct `drop 41` / `99` /
/// `drop 99`. `r` dies in the callee on that path and must run its body
/// there.
///
/// The removal was `suppress_user_drop_for_var` in the `ExprKind::Return`
/// arm: a compile-time frame retraction, so it disarmed EVERY path, not the
/// one that returns. The interpreter's twin retracts from the CURRENT
/// block's cleanup vector, which for a nested `return` does not hold the
/// binding, so its retraction silently no-ops and it stayed correct by an
/// accident of scoping — which is why this was run-vs-build rather than a
/// shared wrong answer.
///
/// The fix keeps the action armed and clears B-2026-08-28-51's `i1` flag at
/// the `return`, so the guarded fire skips the body on the returning path
/// and runs it on the others. It is the same trade the MEMORY-side siblings
/// in that same `return` arm already make — `neutralize_moved_soa_groups_slot`
/// uses a runtime sentinel "not the tail path's compile-time frame removal"
/// for exactly this reason, since "the early-return cleanup frame is shared
/// with the fall-through path". Bodies simply had no sentinel until -51
/// built one.
///
/// `unconditional-return` is the row that CONSTRAINS the fix rather than
/// reproducing the bug. Its `return r;` is the body's own tail, where the
/// static removal is correct and today's behaviour must survive; the guard
/// is therefore gated on the action living in an ENCLOSING frame, which is
/// exactly the test for "this `return` is nested". Any version that guards
/// every `return` takes this row from one body to two.
///
/// `displaced-fallthrough` is the row the analysis said was the risk, and it
/// is the one that turned out to VALIDATE the fix. Retaining the action
/// makes `has_armed_user_drop` answer `true` where it answered `false`, and
/// that predicate gates the displaced-value leg (B-2026-07-30-11) which runs
/// a reassigned binding's old body. Firing it on a moved-from slot would
/// replay B-2026-07-31-38's shape — but control flow makes the proxy exact
/// here: if the `return` had executed, the function would have left, so any
/// path reaching the reassignment still owns the value. The row therefore
/// goes from a MISSING body to a correct one rather than to a stale-slot
/// read, and it is the fixture that would catch a regression either way.
///
/// `param-nested-return` is NOT fixed and is pinned at its current answer on
/// purpose: an owned by-value param is caller-drops, so its body is lost in
/// the CALLEE by a different channel — the interprocedural escape predicates
/// unioning over return sites, which is B-2026-08-28-22. All four surfaces
/// agree there, so it is aligned-wrong rather than run-vs-build, and this
/// row exists so that fixing -22 is noticed here rather than silently
/// changing an untested expectation.
///
/// Twin: `tests/interpreter.rs`'s
/// `test_nested_return_local_user_drop_body_runs_on_the_fallthrough`, whose
/// expectations are these verbatim — the point of the row is that the three
/// backends now agree, so a divergence surfaces as one of the two failing
/// against a shared constant.
#[test]
fn e2e_nested_return_local_user_drop_body_runs_on_the_fallthrough() {
    const DROPPER: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n\
             struct H { id: i64, name: String }\n\
             impl Drop for H { fn drop(mut ref self) { println(f\"drop {self.name}\"); } }\n";
    for (label, body, want) in [
        // The row's own repro: the path that never takes the `return`.
        // Pre-fix the three compiled backends printed only `99` / `drop 99`.
        (
            "nested-return-fallthrough",
            "fn take(k: bool) -> R { let r = R { id: 41 }; if k { return r; } R { id: 99 } }\n\
                 fn main() { let x = take(false); println(f\"{x.id}\"); }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // The same program on the path that DOES return: `r` escapes, so
        // exactly one body must run, at the caller. A guard that failed to
        // clear would double it here.
        (
            "nested-return-taken",
            "fn take(k: bool) -> R { let r = R { id: 41 }; if k { return r; } R { id: 99 } }\n\
                 fn main() { let x = take(true); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // BOUNDARY — an UNCONDITIONAL `return r;` at the body's tail, where
        // the static removal is correct. Guarding every `return` doubles it.
        (
            "unconditional-return",
            "fn take() -> R { let r = R { id: 41 }; return r; }\n\
                 fn main() { let x = take(); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // The `match`-arm spelling of the repro — a different statement
        // form reaching the same nested `ExprKind::Return`.
        (
            "match-arm-return-fallthrough",
            "fn take(k: bool) -> R { let r = R { id: 41 };\n\
                 \x20  match k { true => { return r; } false => {} }\n\
                 \x20  R { id: 99 } }\n\
                 fn main() { let x = take(false); println(f\"{x.id}\"); }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // TWO levels of nesting, so the enclosing-frame test cannot be
        // reading only the immediately-enclosing scope.
        (
            "twice-nested-return-fallthrough",
            "fn take(k: bool, j: bool) -> R { let r = R { id: 41 };\n\
                 \x20  if k { if j { return r; } }\n\
                 \x20  R { id: 99 } }\n\
                 fn main() { let x = take(true, false); println(f\"{x.id}\"); }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // A `return` inside a LOOP body, where the fall-through reaches the
        // reassignment-free tail after the loop finishes.
        (
            "loop-return-fallthrough",
            "fn take(k: bool) -> R { let r = R { id: 41 }; let mut i = 0;\n\
                 \x20  while i < 1 { if k { return r; } i = i + 1; }\n\
                 \x20  R { id: 99 } }\n\
                 fn main() { let x = take(false); println(f\"{x.id}\"); }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // The DISPLACED-VALUE leg (B-2026-07-30-11) — the predicate the fix
        // changes the answer of. `r` is reassigned on the fall-through, so
        // the OLD value's body must run at the assignment.
        (
            "displaced-fallthrough",
            "fn take(k: bool) -> R { let mut r = R { id: 41 }; if k { return r; }\n\
                 \x20  r = R { id: 99 }; r }\n\
                 fn main() { let x = take(false); println(f\"{x.id}\"); }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        (
            "displaced-return-taken",
            "fn take(k: bool) -> R { let mut r = R { id: 41 }; if k { return r; }\n\
                 \x20  r = R { id: 99 }; r }\n\
                 fn main() { let x = take(true); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // A SECOND local that is never returned must keep firing where it
        // always did, on both paths — the fix must disarm one binding on one
        // path, not a whole frame.
        (
            "two-locals-fallthrough",
            "fn take(k: bool) -> R { let a = R { id: 41 }; let b = R { id: 42 };\n\
                 \x20  if k { return a; } b }\n\
                 fn main() { let x = take(false); println(f\"{x.id}\"); }\n",
            "drop 41\n42\ndrop 42\n",
        ),
        (
            "two-locals-return-taken",
            "fn take(k: bool) -> R { let a = R { id: 41 }; let b = R { id: 42 };\n\
                 \x20  if k { return a; } b }\n\
                 fn main() { let x = take(true); println(f\"{x.id}\"); }\n",
            "drop 42\n41\ndrop 41\n",
        ),
        // A HEAP-carrying local, so the body reads a live buffer rather than
        // a moved-from husk. The memory twin is
        // `asan_nested_return_local_drop_body_is_memory_balanced`.
        (
            "heap-nested-return-fallthrough",
            "fn take(k: bool) -> H { let h = H { id: 41, name: f\"n{41}\" };\n\
                 \x20  if k { return h; } H { id: 99, name: f\"n{99}\" } }\n\
                 fn main() { let x = take(false); println(f\"{x.id}\"); }\n",
            "drop n41\n99\ndrop n99\n",
        ),
        // WAS the tripwire for B-2026-08-28-22's channel — an owned by-value
        // PARAM is caller-drops, and this shape lost its body, aligned-wrong
        // on all four surfaces. It fires correctly since B-2026-08-29-21,
        // which taught `fn_conditionally_returns_param_bare` to read a
        // `return` operand as an exit leaf instead of declining the function
        // outright. Kept as the regression guard for that. The interpreter
        // twin of this case is in `tests/interpreter.rs`, same label.
        (
            "param-nested-return",
            "fn take(r: R, k: bool) -> R { if k { return r; } R { id: 99 } }\n\
                 fn main() { let x = take(R { id: 41 }, false); println(f\"{x.id}\"); }\n",
            "drop 41\n99\ndrop 99\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{DROPPER}{body}")).as_deref(),
            Some(want),
            "{label}"
        );
    }
}

/// B-2026-08-28-51, codegen leg — a CONDITIONALLY-MOVED local runs its
/// user `Drop` body exactly once, on every path.
///
/// `fn take(k: bool) -> R { let r = R { id: 41 }; if k { r } else { R { id: 99 } } }`
/// with `k = true` printed `drop 41` / `41` / `drop 41`: the callee ran the
/// body on a value it had already handed to the caller, the caller then READ
/// that value, and its own drop ran the body a second time. The middle `41` is
/// a read of an already-dropped value, which is what made this the
/// high-severity half of the row.
///
/// WHY NEITHER EXISTING CHANNEL COULD FIX IT. A value moved on SOME paths and
/// dead on others needs runtime knowledge at the drop point, and the two
/// static channels guess in opposite directions. `merge_outer_states` re-marks
/// a conditionally-moved place `Owned` and leans on codegen's cap/null guard —
/// which protects MEMORY and has nothing to test for a user `Drop` BODY, so an
/// over-scheduled body simply runs twice. The move-suppression family
/// (`suppress_tail_expr_user_drop` and siblings) removes the action outright,
/// which disarms on ALL paths and can only under-fire. Teaching the suppressor
/// to descend into branch arms just moves a shape from the first failure to the
/// second.
///
/// The interpreter gets the missing bit for free: it evaluates only the TAKEN
/// arm, so reaching an arm's tail IS the proof that this path moved the value.
/// `record_conditional_move_tail` marks there. Codegen cannot do that — it
/// emits both arms — so its twin clears an `i1` flag in the arm's own basic
/// block and the drain tests it. Same classification, two idioms; the shared
/// `note_escaping_site` rule is what keeps them agreeing.
///
/// `discarded-if-statement` and `discarded-match-statement` are the rows that
/// CONSTRAIN the fix rather than reproduce the bug, and they are why the
/// marking is keyed to an escaping position instead of to "a block tail that is
/// an identifier". Their arm tails are the same bare `r`, but the value is
/// discarded rather than moved; marking it would take a program that runs one
/// body today to ZERO. Any version that keys on shape alone fails these two.
///
/// `two-locals-merge` pins the other direction: `s` must still fire where it
/// always did, so the fix cannot disarm a whole branch — only the arm that ran.
/// `heap-field-no-husk` pins the read: pre-fix the compiled backends printed
/// `drop ` (empty) because the spurious body read the moved-from slot, so this
/// row fails if the body ever runs on a husk again.
///
/// THE OTHER HORN, since closed. `fn take(k) -> R { let r = ...; if k { return r; } R { id: 99 } }`
/// with `k = false` lost `r`'s body on the COMPILED backends when this slice
/// landed — the UNDER-fire horn, a static removal in the `ExprKind::Return`
/// arm. It was filed as B-2026-08-28-65 and fixed by RETAINING the action and
/// clearing this slice's flag at the `return`, gated on the action living in
/// an enclosing frame so an unconditional `return r;` keeps the static
/// removal. `return-in-branch` below still covers only the `k = true`
/// direction; the fall-through direction lives in
/// `e2e_nested_return_local_user_drop_body_runs_on_the_fallthrough`.
///
/// Twin: `tests/interpreter.rs`'s
/// `test_conditionally_moved_local_user_drop_body_runs_once`, whose
/// expectations are these verbatim — the whole point of the row is that the
/// three backends now agree, so a divergence shows up as one of these two
/// tests failing against a shared constant.
#[test]
fn e2e_conditionally_moved_local_user_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n";
    for (label, body, want) in [
            // The row's own repro: the arm that MOVES. Pre-fix `drop 41`/`41`/`drop 41`.
            (
                "branch-tail-moved",
                "fn take(k: bool) -> R { let r = R { id: 41 }; if k { r } else { R { id: 99 } } }\n\
                 fn main() { let x = take(true); println(f\"{x.id}\"); }\n",
                "41\ndrop 41\n",
            ),
            // The same program on the arm that does NOT move: `r` dies in the
            // callee and must still run its body there. A static disarm would
            // silence this row, which is why the marking has to be per-path.
            (
                "branch-tail-not-moved",
                "fn take(k: bool) -> R { let r = R { id: 41 }; if k { r } else { R { id: 99 } } }\n\
                 fn main() { let x = take(false); println(f\"{x.id}\"); }\n",
                "drop 41\n99\ndrop 99\n",
            ),
            // BOUNDARY — a DISCARDED `if` statement. Same bare `r` at the arm
            // tail, but the value goes nowhere, so it must keep running exactly
            // one body. Marking on shape alone takes this to zero.
            (
                "discarded-if-statement",
                "fn main() { let r = R { id: 41 }; let k = true; \
                 if k { r } else { R { id: 99 } }; println(\"end\"); }\n",
                "drop 41\nend\n",
            ),
            // BOUNDARY — the `match` twin of the row above.
            (
                "discarded-match-statement",
                "fn main() { let r = R { id: 41 }; let n = 0; \
                 match n { 0 => r, _ => R { id: 9 } }; println(\"end\"); }\n",
                "drop 41\nend\n",
            ),
            // Two locals, one `if`: the taken arm moves `r`, and `s` must still
            // die where it always did. Pre-fix this printed THREE bodies for two
            // objects.
            (
                "two-locals-merge",
                "fn main() { let r = R { id: 41 }; let s = R { id: 99 }; let k = true; \
                 let y = if k { r } else { s }; println(f\"{y.id}\"); }\n",
                "drop 99\n41\ndrop 41\n",
            ),
            // `else if` — the else branch is another `if`, so the escaping
            // property has to recurse through it to reach `b`.
            (
                "else-if-chain",
                "fn take(n: i64) -> R { let a = R { id: 1 }; let b = R { id: 2 }; \
                 if n == 0 { a } else if n == 1 { b } else { R { id: 9 } } }\n\
                 fn main() { let x = take(1); println(f\"{x.id}\"); }\n",
                "drop 1\n2\ndrop 2\n",
            ),
            // An `if` nested INSIDE an arm: the outer arm's tail is itself a
            // branch, so its own arms are escaping too.
            (
                "nested-if-arm",
                "fn take(p: bool, q: bool) -> R { let a = R { id: 1 }; let b = R { id: 2 }; \
                 if p { if q { a } else { b } } else { R { id: 9 } } }\n\
                 fn main() { let x = take(true, false); println(f\"{x.id}\"); }\n",
                "drop 1\n2\ndrop 2\n",
            ),
            // A bare-expression `match` arm never reaches the block-tail hook —
            // it is not a block — so it needs its own marking site.
            (
                "match-arm",
                "fn take(n: i64) -> R { let a = R { id: 1 }; let b = R { id: 2 }; \
                 match n { 0 => a, 1 => b, _ => R { id: 9 } } }\n\
                 fn main() { let x = take(0); println(f\"{x.id}\"); }\n",
                "drop 2\n1\ndrop 1\n",
            ),
            // `return r;` nested in a branch is the same conditional move: the
            // static retraction targets the IF-block's cleanup, which does not
            // hold a binding declared in the enclosing function block.
            (
                "return-in-branch",
                "fn take(k: bool) -> R { let r = R { id: 41 }; if k { return r; } R { id: 99 } }\n\
                 fn main() { let x = take(true); println(f\"{x.id}\"); }\n",
                "41\ndrop 41\n",
            ),
            // The heap-field shape. Pre-fix the spurious body read the moved-from
            // slot, so the compiled backends printed an EMPTY name; this row fails
            // if a body ever runs on a husk again.
            (
                "heap-field-no-husk",
                "fn take2(k: bool) -> H { let h = H { id: 41, name: \"forty-one\" }; \
                 if k { h } else { H { id: 99, name: \"ninety-nine\" } } }\n\
                 fn main() { let x = take2(true); println(f\"{x.id} {x.name}\"); }\n",
                "41 forty-one\ndrop forty-one\n",
            ),
                // A CLOSURE body's tail is returned too, so it is the same escaping
            // site as a function's. The interpreter reaches it through
            // `next_block_is_fn_body`, which covers closures; codegen needed the
            // seed planted separately, and without it this shape was the one place
            // the fix TURNED an aligned-wrong program into a run-vs-build
            // divergence.
            (
                "closure-branch-tail-moved",
                "fn main() { let f = || { let r = R { id: 41 }; let k = true; \
                 if k { r } else { R { id: 99 } } }; let x = f(); println(f\"{x.id}\"); }\n",
                "41\ndrop 41\n",
            ),
            (
                "closure-branch-tail-not-moved",
                "fn main() { let f = || { let r = R { id: 41 }; let k = false; \
                 if k { r } else { R { id: 99 } } }; let x = f(); println(f\"{x.id}\"); }\n",
                "drop 41\n99\ndrop 99\n",
            ),
            // A METHOD body, which codegen compiles through `compile_function`
            // like any other — recorded so the closure row above is not mistaken
            // for covering every non-free-function body.
            (
                "method-branch-tail",
                "struct Box2 { n: i64 }\n\
                 impl Box2 { fn pick(ref self, k: bool) -> R { let r = R { id: 41 }; \
                 if k { r } else { R { id: 99 } } } }\n\
                 fn main() { let b = Box2 { n: 1 }; let x = b.pick(true); println(f\"{x.id}\"); }\n",
                "41\ndrop 41\n",
            ),
        // CONTROL — a straight-line local tail return, correct before and
            // after. It owns its binding in THIS block, so it keeps taking the
            // static retraction and must not acquire a runtime mark.
            (
                "straight-line-control",
                "fn take() -> R { let r = R { id: 41 }; r }\n\
                 fn main() { let x = take(); println(f\"{x.id}\"); }\n",
                "41\ndrop 41\n",
            ),
            // CONTROL — a PARAM at a branch tail on the direction that MOVES.
            // Correct before and after: the caller declines its side (the callee
            // returns the param) and the callee's own guard is cleared on this
            // path, so exactly one body runs.
            (
                "param-branch-tail-moved",
                "fn take(r: R, k: bool) -> R { if k { r } else { R { id: 99 } } }\n\
                 fn main() { let x = take(R { id: 41 }, true); println(f\"{x.id}\"); }\n",
                "41\ndrop 41\n",
            ),
            // B-2026-08-28-22 — the SAME program on the direction that does NOT
            // move. `R{41}` dies inside the callee and must run its body there.
            //
            // This case was added alongside the one above as a second control, on
            // the belief stated in the old comment here — that a param at a branch
            // tail "was already correct on both directions". It was not: this
            // direction is B-2026-08-28-22's headline program, and the expectation
            // recorded was that row's defect (`99` / `drop 99`, R{41}'s body
            // lost), pinned as if it were intended. Both backends now run it.
            (
                "param-branch-tail-not-moved",
                "fn take(r: R, k: bool) -> R { if k { r } else { R { id: 99 } } }\n\
                 fn main() { let x = take(R { id: 41 }, false); println(f\"{x.id}\"); }\n",
                "drop 41\n99\ndrop 99\n",
            ),
        ] {
            let heap = "struct H { id: i64, name: String }\n\
                 impl Drop for H { fn drop(mut ref self) { println(f\"drop {self.name}\"); } }\n";
            assert_eq!(
                run_program(&format!("{DROPPER}{heap}{body}")).as_deref(),
                Some(want),
                "{label}"
            );
        }
}

/// B-2026-08-28-22 — a callee that returns an owned param on SOME tail paths
/// and not others now runs that param's user `Drop` body on the paths where it
/// dies, instead of nowhere.
///
/// An owned by-value param is caller-drops, and the caller declines wherever
/// `fn_returns_param` sees the value leaving. That predicate answers over the
/// UNION of a callee's return sites, so on a branchy callee the caller stood
/// down on EVERY path while only one path actually returned the value — and
/// whichever object died inside the call lost its body on all four surfaces.
/// The controls are what make it a static-vs-dynamic mismatch rather than a
/// missing case: the SAME program with `k` flipped was already correct, so the
/// callee, the argument and the predicate's answer are identical and only the
/// branch taken differs.
///
/// The fix is the callee-local ownership flip the row's addendum names, built
/// on B-2026-08-28-51's per-path conditional-move flag, with two constraints
/// that are the whole safety argument:
///
///   * BODIES ONLY (`emit_struct_user_drop_bodies_only_fn`). The caller still
///     owns the memory; installing the binding's own wrapper double-freed a
///     heap-carrying param. The row's own finding is that these channels lost a
///     BODY while the memory registrations stayed correct.
///   * ADMITTED ONLY WHERE THE FLAG CAN CLEAR
///     (`fn_conditionally_returns_param_bare`). `aggregate-return-declined`
///     below is why: `fn_returns_param` counts a struct/tuple-literal return as
///     an escape, the flag does not clear on one, and admitting it produced a
///     double body plus a read of the dropped value.
///
/// NOT the intersect-across-return-sites change the row warned about — the
/// union answer is untouched, so nothing depending on it moves. The
/// registration is added ARMED and then guarded, so `has_armed_user_drop`,
/// `has_armed_own_user_drop` and `has_armed_container_elem_bodies` answer for
/// these params where they previously had nothing to answer for.
///
/// Twin: `tests/interpreter.rs`'s `test_conditionally_returned_param_user_drop_body_runs_once`, whose expectations are
/// these verbatim — the row is about the four surfaces agreeing, so a
/// divergence shows up as one of the two tests failing.
#[test]
fn e2e_conditionally_returned_param_user_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n";
    for (label, body, want) in [
        // The row's headline program, on the direction where the param DIES
        // INSIDE THE CALLEE. Pre-fix `99` / `drop 99` on all four surfaces: the
        // caller declined its side because the callee returns `r` on SOME path,
        // and the callee had no registration at all, so R{41}'s body ran nowhere.
        (
            "if-else-not-moved",
            "fn take(r: R, k: bool) -> R { if k { r } else { R { id: 99 } } }\n\
                 fn main() { let x = take(R { id: 41 }, false); println(f\"{x.id}\"); }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // CONTROL — the same program on the direction that DOES move. Correct
        // before and after; the guard is cleared on this path, so adding the
        // registration must not make it fire twice.
        (
            "if-else-moved",
            "fn take(r: R, k: bool) -> R { if k { r } else { R { id: 99 } } }\n\
                 fn main() { let x = take(R { id: 41 }, true); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // `match` arms clear the flag through `control_flow_match.rs` rather
        // than the block-tail site, so both channels need covering.
        (
            "match-not-moved",
            "fn take(r: R, k: i64) -> R { match k { 1 => { r } _ => { R { id: 99 } } } }\n\
                 fn main() { let x = take(R { id: 41 }, 2); println(f\"{x.id}\"); }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // An `else if` chain — the escaping property has to recurse through the
        // nested `if` to reach the leaf tails.
        (
            "else-if-chain-not-moved",
            "fn take(r: R, k: i64) -> R { if k == 1 { r } else if k == 2 { R { id: 98 } } \
                 else { R { id: 99 } } }\n\
                 fn main() { let x = take(R { id: 41 }, 3); println(f\"{x.id}\"); }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // TWO owned params, one returned. The one that dies runs its body where
        // it dies; the one handed back runs its body at the caller.
        (
            "two-params-one-returned",
            "fn take(a: R, b: R, k: bool) -> R { if k { a } else { b } }\n\
                 fn main() { let x = take(R { id: 41 }, R { id: 42 }, true); \
                 println(f\"{x.id}\"); }\n",
            "drop 42\n41\ndrop 41\n",
        ),
        // The call DISCARDED rather than bound. Both objects die here and each
        // runs exactly one body.
        (
            "discarded-call",
            "fn take(r: R, k: bool) -> R { if k { r } else { R { id: 99 } } }\n\
                 fn main() { take(R { id: 41 }, false); println(\"end\"); }\n",
            "drop 41\ndrop 99\nend\n",
        ),
        // BOUNDARY — an AGGREGATE-LITERAL return route. `fn_returns_param`
        // counts `Holder { r: r }` as an escape (it recurses into struct and
        // tuple literals) but the conditional-move flag only clears on a BARE
        // identifier, so admitting this shape leaves the callee running a body
        // for a value that left the frame. Measured with it admitted:
        // `drop 41` / `41` / `drop 41` on all three compiled backends — a double
        // body plus a read of the dropped value.
        (
            "aggregate-return-declined",
            "struct Holder { r: R }\n\
                 fn take(r: R, k: bool) -> Holder { if k { Holder { r: r } } \
                 else { Holder { r: R { id: 99 } } } }\n\
                 fn main() { let x = take(R { id: 41 }, true); println(f\"{x.r.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // A `return` statement inside a branch — the MIXED spelling, one exit
        // a `return` and the other a block tail. Declined outright on the
        // stated ground that "codegen clears the flag at match arms and block
        // tails but NOT at a `return` operand". That had stopped being true:
        // B-2026-08-28-65 added `guard_user_drop_for_nested_return`, which
        // stores `false` into the same per-path flag at a nested `return`, so
        // the mechanism already reached here and only the predicate was still
        // refusing. B-2026-08-29-21 lifted it. (-65 and -52 were the LOCAL
        // spelling; the PARAM analogue is what stayed open.)
        (
            "return-in-branch-admitted",
            "fn take(r: R, k: bool) -> R { if k { return r; } R { id: 99 } }\n\
                 fn main() { let x = take(R { id: 41 }, false); println(f\"{x.id}\"); }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // Its escaping twin — the path the flag must clear on. This is what
        // proves the admission did not create a double.
        (
            "return-in-branch-admitted-escaping",
            "fn take(r: R, k: bool) -> R { if k { return r; } R { id: 99 } }\n\
                 fn main() { let x = take(R { id: 41 }, true); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // The TAIL spelling of the same mixed shape (`return r` with no
        // semicolon, so it is the branch block's final expression rather than
        // a statement). It is a separate cell because the statement hooks
        // never see it: on the interpreter it needed its own seeding, and
        // without that it ran the body TWICE on the escaping path.
        (
            "return-as-branch-tail-escaping",
            "fn take(r: R, k: bool) -> R { if k { return r } R { id: 99 } }\n\
                 fn main() { let x = take(R { id: 41 }, true); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // B-2026-08-29-21's own program: BOTH exits spelled as `return`, on a
        // METHOD. Pre-fix the escaping path ran `drop 7` / `got 7` / `drop 7`
        // on all three compiled backends against one interpreter body.
        (
            "method-return-both-exits-escaping",
            "struct T { n: i64 }\n\
                 impl T { fn take(ref self, r: R, k: bool) -> R \
                 { if k { return r; } return R { id: 98 }; } }\n\
                 fn main() { let t = T { n: 1 }; \
                 let a = t.take(R { id: 7 }, true); println(f\"got {a.id}\"); }\n",
            "got 7\ndrop 7\n",
        ),
        (
            "method-return-both-exits-dying",
            "struct T { n: i64 }\n\
                 impl T { fn take(ref self, r: R, k: bool) -> R \
                 { if k { return r; } return R { id: 98 }; } }\n\
                 fn main() { let t = T { n: 1 }; \
                 let a = t.take(R { id: 7 }, false); println(f\"got {a.id}\"); }\n",
            "drop 7\ngot 98\ndrop 98\n",
        ),
        // A GENERIC callee, admitted since B-2026-08-28-71. A monomorph is
        // compiled through `compile_mono_function`, which has its own param
        // loop, so this shape kept the pre-B-2026-08-28-22 defect (a MISSED
        // body) until that loop gained the same registration. The row filed
        // for it blamed the mono param slot for holding a pointer rather than
        // the struct; instrumentation refuted that — the slot is the struct by
        // value, exactly as in `compile_function`. What the mono prologue
        // actually lacked was B-2026-08-28-51's flag isolation and the
        // body-TAIL escaping site (`compile_function` seeds it per function;
        // `let` initializers and `return` operands are seeded per statement in
        // shared code). Without the seeding the guard never cleared, so the
        // body ALSO fired on the path that returned the value — which is what
        // the row measured as a corrupted name.
        (
            "generic-callee-dying-path",
            "fn take[T](r: R, k: bool, t: T) -> R { if k { r } else { R { id: 99 } } }\n\
                 fn main() { let x = take(R { id: 41 }, false, 7); println(f\"{x.id}\"); }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // The ESCAPING half of the same generic callee. A CONTROL against the
        // pre-fix compiler — it already printed this — and the case that
        // fails against the INTERMEDIATE version of the fix: with the mono
        // registration in place but the escaping site unseeded, the guard
        // never cleared and this ran the body a second time inside the
        // callee (measured `drop ` / `1` / `drop i1` on all three compiled
        // backends). Both halves of a branch, as the family requires.
        (
            "generic-callee-escaping-path",
            "fn take[T](r: R, k: bool, t: T) -> R { if k { r } else { R { id: 99 } } }\n\
                 fn main() { let x = take(R { id: 41 }, true, 7); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // The generic twin of `heap-payload-not-moved`, and THE case
        // B-2026-08-28-71 was filed on: a `String` field is what made the
        // unguarded second body visible (it printed `drop dr` — the format
        // literal's own bytes — where `drop n41` was due). An `i64`-only
        // payload prints a plausible number instead, which is why the first
        // pass of probes missed it. `asan_generic_conditionally_returned_-
        // param_bodies_are_memory_balanced` pins the memory side.
        (
            "generic-heap-payload-dying-path",
            "fn take[T](h: H, k: bool, t: T) -> H { if k { h } \
                 else { H { id: 99, name: f\"n99\" } } }\n\
                 fn main() { let x = take(H { id: 41, name: f\"n41\" }, false, 7); \
                 println(f\"{x.id}\"); }\n",
            "drop n41\n99\ndrop n99\n",
        ),
        // Control, like `generic-callee-escaping-path`: correct pre-fix, and
        // the case the unseeded-escaping-site version got wrong.
        (
            "generic-heap-payload-escaping-path",
            "fn take[T](h: H, k: bool, t: T) -> H { if k { h } \
                 else { H { id: 99, name: f\"n99\" } } }\n\
                 fn main() { let x = take(H { id: 41, name: f\"n41\" }, true, 7); \
                 println(f\"{x.id}\"); }\n",
            "41\ndrop n41\n",
        ),
        // BOUNDARY — the callee never returns the param, so the caller already
        // owned the drop and nothing changes.
        (
            "never-returns-param",
            "fn take(r: R, k: bool) -> i64 { if k { 1 } else { 2 } }\n\
                 fn main() { let x = take(R { id: 41 }, false); println(f\"{x}\"); }\n",
            "drop 41\n2\n",
        ),
        // BOUNDARY — an UNCONDITIONAL return. One leaf tail, no branch, nothing
        // to guard: the predicate requires a path that does NOT yield the param.
        (
            "unconditional-return",
            "fn take(r: R) -> R { r }\n\
                 fn main() { let x = take(R { id: 41 }); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // A HEAP-CARRYING param. The registration is BODIES-ONLY: the caller
        // still owns the memory, so installing the binding's own `Drop` wrapper
        // here (which frees the fields too) double-freed — measured `free():
        // double free detected in tcache 2` under the JIT. The ASAN twin
        // `asan_conditionally_returned_param_bodies_are_memory_balanced` pins
        // the memory side.
        (
            "heap-payload-not-moved",
            "fn take(h: H, k: bool) -> H { if k { h } \
                 else { H { id: 99, name: f\"n99\" } } }\n\
                 fn main() { let x = take(H { id: 41, name: f\"n41\" }, false); \
                 println(f\"{x.id}\"); }\n",
            "drop n41\n99\ndrop n99\n",
        ),
        // B-2026-09-02-3 — the conditionally-returned param's declared type
        // IS THE TYPE PARAMETER, which is the shape every generic case
        // above misses. Each of those declares it concretely (`r: R`,
        // `h: H`) and uses `T` only for an extra scalar argument, so the
        // mono registration's `drop_method_keys` lookup was handed a real
        // struct name and succeeded. With `a: T` it is handed the literal
        // string "T", finds no such type, and silently registers nothing —
        // so the value that died inside ran no body on any compiled lane
        // while `--interp` ran it.
        //
        // Both branch directions, because which param dies swaps with `k`
        // and the two must each run exactly once.
        (
            "generic-param-is-T-dying-first-arg",
            "fn pick[T](a: T, k: bool, alt: T) -> T { if k { return alt; } return a; }\n\
                 fn main() { let x = pick(H { id: 41, name: f\"n41\" }, true, \
                 H { id: 42, name: f\"n42\" }); println(f\"{x.id}\"); }\n",
            "drop n41\n42\ndrop n42\n",
        ),
        (
            "generic-param-is-T-dying-second-arg",
            "fn pick[T](a: T, k: bool, alt: T) -> T { if k { return alt; } return a; }\n\
                 fn main() { let x = pick(H { id: 43, name: f\"n43\" }, false, \
                 H { id: 44, name: f\"n44\" }); println(f\"{x.id}\"); }\n",
            "drop n44\n43\ndrop n43\n",
        ),
        // A BOUNDED type parameter resolves through the same channel, so it
        // must answer the same. Pinned because a bound is the one thing
        // that could plausibly route the type differently.
        (
            "generic-param-is-bounded-T",
            "#[derive(Display)]\n\
                 struct D { id: i64, name: String }\n\
                 impl Drop for D { fn drop(mut ref self) { println(f\"drop {self.name}\"); } }\n\
                 fn pickd[T: Display](a: T, k: bool, alt: T) -> T \
                 { if k { return alt; } return a; }\n\
                 fn main() { let x = pickd(D { id: 45, name: f\"n45\" }, true, \
                 D { id: 46, name: f\"n46\" }); println(f\"{x.id}\"); }\n",
            "drop n45\n46\ndrop n46\n",
        ),
        // B-2026-09-12-26 — the `return` in TAIL position, i.e. the last
        // exit written WITHOUT a trailing semicolon. Every cell above whose
        // last exit is a `return` at all spells it `return X;`
        // (`method-return-both-exits-*`, the `generic-param-is-*` group), and
        // the rest end on a tail EXPRESSION (`return-in-branch-*`,
        // `if-else-*`). Both of those are admitted. Drop the semicolon from
        // the first group and the `return` becomes the body's `final_expr`,
        // and `fn_conditionally_returns_param_bare` collapsed to `false` for
        // the whole function — a spelling with no cell anywhere in this
        // suite.
        //
        // The mechanism is `leaf_tails`, which is handed the body's tail and
        // had no `Return` arm, so its catch-all pushed the `Return` NODE as a
        // leaf. `may_mention` does not recognise `ExprKind::Return` and
        // answers `true` for it by design, so condition 3 read that leaf as
        // "mentions the param by a route no flag clears" and declined. The
        // fix gives `leaf_tails` the operand, which is the same leaf
        // `collect_return_leaves` already contributes for the statement
        // spelling — so the two spellings are now analysed identically.
        //
        // WHAT IT COST DIFFERED BY CALL POSITION, which is why the row was
        // filed as an associated-vs-free defect rather than a syntactic one:
        //
        //  * ASSOCIATED and METHOD callers gate `escapes_frame` on this
        //    predicate, so `false` left it false, the argument registrar took
        //    its non-escaping arm and hung the FULL `karac_drop_<T>` wrapper
        //    on the temp — its body ran beside the result binding's:
        //    `drop 1` / `1` / `drop 1` at -O0, -O0 autopar and -O2 autopar
        //    against `--interp`'s `1` / `drop 1`.
        //  * FREE callers gate on `call_arg_flows_into_return`, the
        //    `fn_returns_param` UNION, which sees the `return` either way —
        //    so the free spelling's hand-back path was right, and only its
        //    DYING path was wrong, by a body that ran nowhere. Both backends
        //    agreed on that miss, so no A/B gate reported it.
        (
            "assoc-return-tail-both-exits-escaping",
            "struct Sk { n: i64 }\n\
                 impl Sk { fn pick(r: R, flag: bool) -> R \
                 { if flag { return r } return R { id: 9 } } }\n\
                 fn main() { let x = Sk.pick(R { id: 1 }, true); println(f\"{x.id}\"); }\n",
            "1\ndrop 1\n",
        ),
        (
            "assoc-return-tail-both-exits-dying",
            "struct Sk { n: i64 }\n\
                 impl Sk { fn pick(r: R, flag: bool) -> R \
                 { if flag { return r } return R { id: 9 } } }\n\
                 fn main() { let x = Sk.pick(R { id: 1 }, false); println(f\"{x.id}\"); }\n",
            "drop 1\n9\ndrop 9\n",
        ),
        // THE DISCRIMINATOR, and the reason the semicolon is named above: the
        // identical associated function with `return R { id: 9 };` was correct
        // before this fix and after it. A cell that differs from the one above
        // by one character is what keeps a future reader from re-deriving the
        // trigger as "associated functions".
        (
            "assoc-return-STATEMENT-both-exits-escaping",
            "struct Sk { n: i64 }\n\
                 impl Sk { fn pick(r: R, flag: bool) -> R \
                 { if flag { return r; } return R { id: 9 }; } }\n\
                 fn main() { let x = Sk.pick(R { id: 1 }, true); println(f\"{x.id}\"); }\n",
            "1\ndrop 1\n",
        ),
        // The METHOD spelling of the tail form — the third column the row
        // asked for. Same defect as the associated one, same registrar.
        (
            "method-return-tail-both-exits-escaping",
            "struct Mk { n: i64 }\n\
                 impl Mk { fn pick(ref self, r: R, flag: bool) -> R \
                 { if flag { return r } return R { id: 9 } } }\n\
                 fn main() { let m = Mk { n: 0 }; \
                 let x = m.pick(R { id: 1 }, true); println(f\"{x.id}\"); }\n",
            "1\ndrop 1\n",
        ),
        // The FREE twin. Its hand-back path was already right; its DYING path
        // is the half this fix repairs, and it is repaired on BOTH backends
        // because the predicate is shared.
        (
            "free-return-tail-both-exits-escaping",
            "fn pickf(r: R, flag: bool) -> R { if flag { return r } return R { id: 9 } }\n\
                 fn main() { let x = pickf(R { id: 1 }, true); println(f\"{x.id}\"); }\n",
            "1\ndrop 1\n",
        ),
        (
            "free-return-tail-both-exits-dying",
            "fn pickf(r: R, flag: bool) -> R { if flag { return r } return R { id: 9 } }\n\
                 fn main() { let x = pickf(R { id: 1 }, false); println(f\"{x.id}\"); }\n",
            "drop 1\n9\ndrop 9\n",
        ),
        // BOUNDARY — an UNCONDITIONAL `return r` in tail position. Admitting
        // the operand as a leaf must not turn this into a conditional shape:
        // both leaves yield the param, no leaf yields nothing, so the
        // predicate still declines and the caller's result binding stays the
        // only owner.
        (
            "assoc-unconditional-return-tail",
            "struct Sk2 { n: i64 }\n\
                 impl Sk2 { fn pick(r: R) -> R { return r } }\n\
                 fn main() { let x = Sk2.pick(R { id: 1 }); println(f\"{x.id}\"); }\n",
            "1\ndrop 1\n",
        ),
    ] {
        let heap = "struct H { id: i64, name: String }\n\
                 impl Drop for H { fn drop(mut ref self) { println(f\"drop {self.name}\"); } }\n";
        assert_eq!(
            run_program(&format!("{DROPPER}{heap}{body}")).as_deref(),
            Some(want),
            "{label}"
        );
    }
}

/// B-2026-08-29-15, codegen leg — a NAMED binding passed by value to a
/// callee that hands it straight back runs its user `Drop` body ONCE.
///
/// The interpreter twin is
/// `test_named_arg_returned_bare_user_drop_body_runs_once` and these
/// expectations are shared with it verbatim — the two backends AGREED on
/// the wrong count pre-fix (`drop 32` / `32` / `drop 32` on every named
/// case here), which is precisely why no A/B gate reported it and an
/// absolute expectation is the only thing that could.
///
/// What licenses removing a body rather than it being a preference: the
/// named and fresh-temp spellings of this program allocate identically —
/// with a 256-element `Vec[i64]` field, `11 allocs, 11 frees, 10,269 bytes`
/// byte-for-byte under valgrind at `KARAC_OPT_LEVEL=0` — so whatever the
/// two spellings differ in, it is not the number of buffers. An entry copy
/// IS made (any by-value call adds one 2,048-byte allocation over the same
/// program with no call), but it is made for BOTH spellings, and the
/// `fresh-temp-control` row below has always run one body with it present.
/// That row is the oracle for the count.
///
/// B-2026-08-29-50 widened the gate to the union of
/// `fn_always_returns_param` and `fn_conditionally_returns_param_bare` —
/// every shape where SOME OTHER frame runs the body on EVERY path — and
/// the last four rows are the shapes that union added. The stand-down
/// survives the widening because it DOWNGRADES the binding's
/// `karac_drop_<T>` wrapper to field-cleanup-only rather than retracting
/// it, so the aggregate's caller still frees the buffer it owns. All four
/// were RED on all three backends before that change, and all four agreed
/// across the backends while wrong, so no A/B gate could have caught them.
///
/// The conditional rows are the ones that fail in BOTH directions if the
/// gate is got wrong: standing the caller down where the callee does NOT
/// take over loses a body (the regression B-2026-08-28-22 measured), while
/// leaving it armed doubles on both paths — on `k = true` against the
/// callee's own registration, on `k = false` against the result binding.
#[test]
fn e2e_named_arg_returned_bare_user_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n\
             struct T2 { n: i64 }\n";
    for (label, body, want) in [
        (
            "free-named-return",
            "fn takef(r: R) -> R { return r; }\n\
                 fn main() { let a = R { id: 32, tag: f\"h\" }; let b = takef(a); \
                 println(f\"{b.id}\"); }\n",
            "32\ndrop 32\n",
        ),
        (
            "free-named-tail",
            "fn keepf(r: R) -> R { r }\n\
                 fn main() { let a = R { id: 32, tag: f\"h\" }; let b = keepf(a); \
                 println(f\"{b.id}\"); }\n",
            "32\ndrop 32\n",
        ),
        (
            "method-named-return",
            "impl T2 { fn take(ref self, r: R) -> R { return r; } }\n\
                 fn main() { let t = T2 { n: 1 }; let a = R { id: 32, tag: f\"h\" }; \
                 let b = t.take(a); println(f\"{b.id}\"); }\n",
            "32\ndrop 32\n",
        ),
        (
            "method-named-tail",
            "impl T2 { fn keep(ref self, r: R) -> R { r } }\n\
                 fn main() { let t = T2 { n: 1 }; let a = R { id: 32, tag: f\"h\" }; \
                 let b = t.keep(a); println(f\"{b.id}\"); }\n",
            "32\ndrop 32\n",
        ),
        // The caller's binding spelled DIFFERENTLY from the param, so a fix
        // that only worked on a name coincidence cannot pass this row.
        (
            "caller-renamed",
            "impl T2 { fn take(ref self, r: R) -> R { return r; } }\n\
                 fn main() { let t = T2 { n: 1 }; let qq = R { id: 32, tag: f\"h\" }; \
                 let b = t.take(qq); println(f\"{b.id}\"); }\n",
            "32\ndrop 32\n",
        ),
        // CONTROL — the fresh-temp spelling, correct before this fix.
        (
            "fresh-temp-control",
            "fn takef(r: R) -> R { return r; }\n\
                 fn main() { let b = takef(R { id: 32, tag: f\"h\" }); \
                 println(f\"{b.id}\"); }\n",
            "32\ndrop 32\n",
        ),
        // BOUNDARY — the param dies inside, so the caller must keep firing.
        (
            "dies-inside-keeps-caller-fire",
            "fn eatf(r: R) -> i64 { println(f\"saw {r.id}\"); return 0; }\n\
                 fn main() { let a = R { id: 32, tag: f\"h\" }; let n = eatf(a); \
                 println(f\"{n}\"); }\n",
            "saw 32\ndrop 32\n0\n",
        ),
        // B-2026-08-29-50 — the param moved into a RETURNED AGGREGATE.
        // The returned `Hh` owns it, so the caller is a duplicate exactly
        // as in the bare rows; what differs is only that the caller's slot
        // holds its own copy's buffer, which the DOWNGRADED action still
        // frees.
        (
            "aggregate-return-free",
            "struct Hh { r: R }\n\
                 fn wrapf(r: R) -> Hh { return Hh { r: r }; }\n\
                 fn main() { let a = R { id: 41, tag: f\"h\" }; let h = wrapf(a); \
                 println(f\"{h.r.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        (
            "aggregate-tail-method",
            "struct Hh { r: R }\n\
                 impl T2 { fn wrap(ref self, r: R) -> Hh { Hh { r: r } } }\n\
                 fn main() { let t = T2 { n: 1 }; let a = R { id: 21, tag: f\"h\" }; \
                 let h = t.wrap(a); println(f\"{h.r.id}\"); }\n",
            "21\ndrop 21\n",
        ),
        // The TUPLE spelling of the same aggregate route — `yields`
        // recurses into tuple literals as well as struct ones, and a fix
        // that only handled `StructLiteral` would pass the two rows above
        // and fail this one.
        (
            "aggregate-tuple-free",
            "fn tupf(r: R) -> (R, i64) { (r, 9) }\n\
                 fn main() { let a = R { id: 17, tag: f\"h\" }; let t = tupf(a); \
                 println(f\"{t.1}\"); }\n",
            "9\ndrop 17\n",
        ),
        // B-2026-08-29-50 — the CONDITIONAL callee, exercised on BOTH
        // paths in one program. `k = true` lets the param die inside (the
        // callee frame owns the body); `k = false` hands it back (the
        // result binding owns it). Pre-fix both doubled, against two
        // DIFFERENT second owners, which is why neither half alone fixes
        // this shape.
        (
            "conditional-both-paths-free",
            "fn pick(r: R, k: bool) -> R { if k { return R { id: 98, tag: f\"z\" }; } r }\n\
                 fn main() { let a = R { id: 7, tag: f\"h\" }; let x = pick(a, true); \
                 println(f\"{x.id}\"); let b = R { id: 5, tag: f\"h\" }; \
                 let y = pick(b, false); println(f\"{y.id}\"); }\n",
            "drop 7\n98\ndrop 98\n5\ndrop 5\n",
        ),
        (
            "conditional-both-paths-method",
            "impl T2 { fn pick(ref self, r: R, k: bool) -> R \
                 { if k { return R { id: 98, tag: f\"z\" }; } r } }\n\
                 fn main() { let t = T2 { n: 1 }; let a = R { id: 7, tag: f\"h\" }; \
                 let x = t.pick(a, true); println(f\"{x.id}\"); \
                 let b = R { id: 5, tag: f\"h\" }; let y = t.pick(b, false); \
                 println(f\"{y.id}\"); }\n",
            "drop 7\n98\ndrop 98\n5\ndrop 5\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{DROPPER}{body}")).as_deref(),
            Some(want),
            "{label}"
        );
    }
}

/// B-2026-08-28-70 — a METHOD's owned param reached NO owner at all in the
/// interpreter and TWO in the compiled backends, in opposite directions,
/// because the method-argument path carries neither half of the ownership
/// protocol the free-function path has.
///
/// Every case below is measured against its FREE-FUNCTION twin, which is
/// unanimous at one body on all four surfaces and is what makes one the
/// right answer rather than a preference. Pre-fix counts are in each case's
/// comment; the two failure directions are:
///
///  * caller ALWAYS fired, so a param the method hands back ran its body at
///    the arg site AND at the result binding — a double body;
///  * interpreter fired NOWHERE, because a method frame reaches no
///    caller-side `run_fresh_temp_arg_drops` and did not own its params
///    either.
#[test]
fn e2e_method_owned_param_user_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n\
             struct B2 { n: i64 }\n";
    for (label, body, want) in [
            // The row's headline program, on the direction where the param DIES
            // INSIDE. Pre-fix: `99` / `drop 99` in the interpreter (R{41}'s body
            // lost) against the correct three lines on the compiled backends.
            (
                "cond-return-dies",
                "impl B2 { fn pick(ref self, r: R, k: bool) -> R { if k { r } else { R { id: 99 } } } }\n\
                 fn main() { let b = B2 { n: 1 }; let x = b.pick(R { id: 41 }, false); \
                 println(f\"{x.id}\"); }\n",
                "drop 41\n99\ndrop 99\n",
            ),
            // The SAME program with `k` flipped, so the param escapes. Pre-fix
            // the compiled backends printed `drop 41` / `41` / `drop 41` — two
            // bodies for one object, the opposite error on the same source.
            (
                "cond-return-escapes",
                "impl B2 { fn pick(ref self, r: R, k: bool) -> R { if k { r } else { R { id: 99 } } } }\n\
                 fn main() { let b = B2 { n: 1 }; let x = b.pick(R { id: 41 }, true); \
                 println(f\"{x.id}\"); }\n",
                "41\ndrop 41\n",
            ),
            // NEVER RETURNED — the plainest shape there is, and the one that
            // shows this is not only about conditional returns: pre-fix the
            // interpreter printed `7` alone, zero bodies for R{41}.
            (
                "never-returned",
                "impl B2 { fn eat(ref self, r: R) -> i64 { 7 } }\n\
                 fn main() { let b = B2 { n: 1 }; let v = b.eat(R { id: 41 }); \
                 println(f\"{v}\"); }\n",
                "drop 41\n7\n",
            ),
            // UNCONDITIONALLY returned. Pre-fix the compiled backends doubled
            // it; the free-fn twin `fn id2(r: R) -> R { r }` prints these two
            // lines on all four surfaces.
            (
                "always-returned",
                "impl B2 { fn id(ref self, r: R) -> R { r } }\n\
                 fn main() { let b = B2 { n: 1 }; let x = b.id(R { id: 41 }); \
                 println(f\"{x.id}\"); }\n",
                "41\ndrop 41\n",
            ),
            // The AGGREGATE-LITERAL return route — `fn_always_returns_param`
            // recurses into struct/tuple literals exactly as `fn_returns_param`
            // does, so the value crossing the boundary inside `H { r: r }`
            // counts as handed back. Pre-fix: compiled doubled.
            (
                "always-returned-aggregate",
                "struct H { r: R }\n\
                 impl B2 { fn wrap(ref self, r: R) -> H { H { r: r } } }\n\
                 fn main() { let b = B2 { n: 1 }; let h = b.wrap(R { id: 21 }); \
                 println(f\"{h.r.id}\"); }\n",
                "21\ndrop 21\n",
            ),
            // BOUNDARY — every exit hands the param back, one of them through a
            // `return` STATEMENT. Admitted (both legs yield the param), so the
            // caller stands down. Pre-fix: compiled doubled.
            (
                "return-stmt-all-exits-yield",
                "impl B2 { fn both(ref self, r: R, k: bool) -> R { if k { return r; } r } }\n\
                 fn main() { let b = B2 { n: 1 }; let z = b.both(R { id: 23 }, false); \
                 println(f\"{z.id}\"); }\n",
                "23\ndrop 23\n",
            ),
            // BOUNDARY, and the one that pins why the caller may NOT stand down
            // on `fn_returns_param`. Here a `return` yields something ELSE, so
            // the param dies on that path. An earlier draft of this fix used the
            // union predicate and LOST this body on all three compiled backends
            // — a regression against the pre-existing always-fire, caught only
            // because the baseline was measured rather than assumed.
            (
                "return-stmt-other-value-keeps-caller-fire",
                "impl B2 { fn early(ref self, r: R, k: bool) -> R \
                 { if k { return R { id: 98 } ; } r } }\n\
                 fn main() { let b = B2 { n: 1 }; let y = b.early(R { id: 22 }, true); \
                 println(f\"{y.id}\"); }\n",
                "drop 22\n98\ndrop 98\n",
            ),
            // TWO owned params, one returned and one not — each runs exactly one
            // body. Pre-fix the compiled backends printed `drop 5` twice.
            (
                "two-params-one-returned",
                "impl B2 { fn two(ref self, a: R, b: R, k: bool) -> R { if k { a } else { b } } }\n\
                 fn main() { let s = B2 { n: 1 }; \
                 let x = s.two(R { id: 4 }, R { id: 5 }, false); println(f\"{x.id}\"); }\n",
                "drop 4\n5\ndrop 5\n",
            ),
            // Receiver MODES: the ownership question is about the argument, not
            // the receiver, so `mut ref self` and owned `self` must answer the
            // same as `ref self`. Pre-fix all three printed nothing in the
            // interpreter.
            (
                "mut-ref-self-receiver",
                "impl B2 { fn eat(mut ref self, r: R) -> i64 { self.n = self.n + 1; 8 } }\n\
                 fn main() { let mut c = B2 { n: 1 }; let v = c.eat(R { id: 2 }); \
                 println(f\"{v}\"); }\n",
                "drop 2\n8\n",
            ),
            (
                "owned-self-receiver",
                "impl B2 { fn eat(self, r: R) -> i64 { 9 } }\n\
                 fn main() { let d = B2 { n: 1 }; let v = d.eat(R { id: 3 }); \
                 println(f\"{v}\"); }\n",
                "drop 3\n9\n",
            ),
            // A GENERIC method whose param DIES inside. The caller-side half
            // needs no callee cooperation, so generics are admitted here even
            // though B-2026-08-28-71 still declines the callee-side flip for
            // them. Pre-fix the interpreter printed `3` alone.
            (
                "generic-method-param-dies",
                "struct G1 { n: i64 }\n\
                 impl G1 { fn gm[T](ref self, r: R, t: T) -> i64 { 3 } }\n\
                 fn main() { let g = G1 { n: 1 }; let a = g.gm(R { id: 31 }, 5); \
                 println(f\"{a}\"); }\n",
                "drop 31\n3\n",
            ),
            // CROSS-FRAME NAME COLLISION — codegen's side, which is CORRECT and
            // must stay so. The interpreter misses `drop 12` here: its moved-out
            // sets are keyed by BINDING NAME with no frame qualifier, so `add`'s
            // `r` (legitimately marked, it goes into `self.xs`) suppresses this
            // later method's unrelated `r`. 277621a isolated the callee frame to
            // fix that; B-2026-08-29-9 reverted the isolation, because the same
            // leak is what suppresses a payload bound out of an owned enum param
            // and returned — isolating it produced a DOUBLE body, the worse
            // defect. So this case is a one-sided control while that divergence
            // is tracked on its own row.
            (
                "moved-out-marks-do-not-leak-across-frames",
                "struct Box3 { xs: Vec[R] }\n\
                 impl Box3 { fn add(mut ref self, r: R) { self.xs.push(r); } }\n\
                 struct G2 { n: i64 }\n\
                 impl G2 { fn eat(ref self, r: R) -> i64 { 3 } }\n\
                 fn main() { let mut bx = Box3 { xs: Vec.new() }; \
                 bx.add(R { id: 11 }); println(\"added\"); \
                 let g = G2 { n: 1 }; let v = g.eat(R { id: 12 }); println(f\"{v}\"); }\n",
                "drop 11\nadded\ndrop 12\n3\n",
            ),
            // CONTROL — the free-function twin of `always-returned`, correct on
            // all four surfaces before and after. It is the oracle the method
            // cases are measured against, so a change that "fixed" methods by
            // moving free functions would fail here.
            (
                "free-fn-oracle-always-returned",
                "fn id2(r: R) -> R { r }\n\
                 fn main() { let x = id2(R { id: 41 }); println(f\"{x.id}\"); }\n",
                "41\ndrop 41\n",
            ),
            // CONTROL — the free-function twin of `never-returned`.
            (
                "free-fn-oracle-never-returned",
                "fn eat2(r: R) -> i64 { 7 }\n\
                 fn main() { let v = eat2(R { id: 41 }); println(f\"{v}\"); }\n",
                "drop 41\n7\n",
            ),
        ] {
            assert_eq!(
                run_program(&format!("{DROPPER}{body}")).as_deref(),
                Some(want),
                "{label}"
            );
        }
}

/// B-2026-08-28-2 — a user `Drop` body ran TWICE FOR ONE OBJECT when the
/// callee pulled an element out of an owned TUPLE PARAM and RETURNED it.
///
/// `fn take(p: (R, i64)) -> R { let (r, n) = p; r }` called as
/// `take((R { id: 41 }, 1))` printed `drop 41` twice under `--interp`,
/// LLJIT and AOT alike. One body came from the caller's fresh-temp
/// argument walk, which fires every element of a tuple-LITERAL argument on
/// the theory that the whole temp dies inside the call; the other from the
/// result binding's own live-range end. Exactly one `R` is ever
/// constructed, so both prints are one object's body running twice.
///
/// BACKEND-CONSISTENT, which is why nothing caught it: every parity
/// harness in the tree compares the backends against each other and they
/// AGREE here, at the wrong number. The 31-shape destructure matrix swept
/// for B-2026-08-27-48 marks this shape "OK" on an interp-vs-AOT
/// comparison — it takes counting the CONSTRUCTIONS to see it.
///
/// `two-droppers` is the row that constrains the FIX rather than just
/// reproducing the bug, and it is why the guard is per-ELEMENT. The
/// obvious reading of "the callee returns it" — suppress the caller's walk
/// for the whole argument — fixes element 0 and takes element 1's body
/// from one to ZERO, trading a double body for a missing one. Any version
/// that is not element-precise fails this row.
///
/// `tuple-index-return` shows the trigger is NOT the destructure the row
/// was filed against: `p.0` returned directly is the same defect with no
/// `let` anywhere. What the two spellings share is that the escaping value
/// is a PART of the param, which the whole-param passthrough guard
/// (`fn_returns_param`, unchanged here) cannot express.
///
/// The last three rows are CONTROLS that were already correct and must
/// stay correct: returning the OTHER element (so the dropper really does
/// die inside the call, and its caller-side body is the only one there
/// is), returning the param BARE (the pre-existing whole-param guard), and
/// a non-tuple param. Twin: `tests/interpreter.rs`'s
/// `test_returned_tuple_param_element_user_drop_body_runs_once`.
#[test]
fn e2e_user_drop_body_of_a_returned_tuple_param_element_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n";
    for (label, body, want) in [
        // The row's own repro.
        (
            "destructure-return",
            "fn take(p: (R, i64)) -> R { let (r, n) = p; r }\n\
                 fn main() { let x = take((R { id: 41 }, 1)); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // Same, spelled with an explicit `return`.
        (
            "explicit-return",
            "fn take(p: (R, i64)) -> R { let (r, n) = p; return r; }\n\
                 fn main() { let x = take((R { id: 41 }, 1)); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // No destructure at all — a direct tuple-index projection.
        (
            "tuple-index-return",
            "fn take(p: (R, i64)) -> R { p.0 }\n\
                 fn main() { let x = take((R { id: 41 }, 1)); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // The RESULT is discarded, so the escaping element's owner is the
        // discarded temp. Still exactly one body.
        (
            "result-discarded",
            "fn take(p: (R, i64)) -> R { let (r, n) = p; r }\n\
                 fn main() { take((R { id: 41 }, 1)); println(\"end\"); }\n",
            "drop 41\nend\n",
        ),
        // BOTH elements drop and only element 0 escapes: 41 leaves through
        // the result, 42 dies in the call. One body each, never two and
        // never none.
        //
        // B-2026-08-28-19 — `drop 42` used to come LAST here, because the
        // caller-side walk sat on the scope frame while the interpreter fired
        // it at the call. It now fires at STATEMENT end on both, so this row
        // matches its interpreter twin exactly rather than carrying a
        // per-backend expectation.
        (
            "two-droppers",
            "fn take(p: (R, R)) -> R { let (a, b) = p; a }\n\
                 fn main() { let x = take((R { id: 41 }, R { id: 42 })); println(f\"{x.id}\"); }\n",
            "drop 42\n41\ndrop 41\n",
        ),
        // B-2026-08-28-16 — the same shapes with a LOCAL as the argument
        // instead of a literal. Everything above passes a fresh tuple TEMP,
        // which the caller-side parts filter reaches; a bare identifier is
        // a PLACE and never enters that walk, so the second body came from
        // the local's OWN element walk firing at its live-range end on a
        // value the callee had already handed back. Fixed by recording the
        // escape as a per-element move-out on the local, the same mask
        // `let x = t.N` uses.
        (
            "local-arg-destructure-return",
            "fn take(p: (R, i64)) -> R { let (r, n) = p; r }\n\
                 fn main() { let q = (R { id: 41 }, 1); let x = take(q);\n\
                 \x20            println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        (
            "local-arg-tuple-index-return",
            "fn take(p: (R, i64)) -> R { p.0 }\n\
                 fn main() { let q = (R { id: 41 }, 1); let x = take(q);\n\
                 \x20            println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // THE ROW THAT PINS THE MASK AS PER-ELEMENT. Only element 0
        // escapes; element 1 dies inside the call and its body must still
        // run caller-side. Suppressing the local's whole walk instead —
        // the obvious coarser fix — loses `drop 42` here.
        (
            "local-arg-two-droppers",
            "fn take(p: (R, R)) -> R { let (a, b) = p; a }\n\
                 fn main() { let q = (R { id: 41 }, R { id: 42 }); let x = take(q);\n\
                 \x20            println(f\"{x.id}\"); }\n",
            "drop 42\n41\ndrop 41\n",
        ),
        // CONTROL — the callee returns the OTHER element, so nothing
        // escapes and the local's walk must stay fully armed. Correct
        // before this fix; it is what a too-eager mask would break.
        (
            "local-arg-nothing-escapes",
            "fn take(p: (R, i64)) -> i64 { let (r, n) = p; n }\n\
                 fn main() { let q = (R { id: 41 }, 1); let x = take(q);\n\
                 \x20            println(f\"{x}\"); }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — no call at all. The local's own walk is the only
        // owner and runs once.
        (
            "local-no-call",
            "fn main() { let q = (R { id: 41 }, 1); println(f\"{q.1}\"); }\n",
            "1\ndrop 41\n",
        ),
        // Two calls — the per-callee answer is not cached across sites in a
        // way that leaks between them.
        (
            "two-calls",
            "fn take(p: (R, i64)) -> R { let (r, n) = p; r }\n\
                 fn main() { let x = take((R { id: 41 }, 1)); println(f\"{x.id}\");\n\
                 \x20            let y = take((R { id: 42 }, 2)); println(f\"{y.id}\"); }\n",
            "41\ndrop 41\n42\ndrop 42\n",
        ),
        // CONTROL — the callee returns the OTHER element, so the dropper
        // dies inside the call and the caller's walk is its ONLY body.
        // Suppressing per-argument instead of per-element takes this to
        // zero.
        //
        // B-2026-08-28-19 — the body used to come AFTER the `1`, because the
        // caller-side walk sat on the scope frame while the interpreter fired
        // it at the call. It now fires at STATEMENT end on both, matching the
        // `local-source` row two above, which already read this way.
        (
            "other-element-control",
            "fn take(p: (R, i64)) -> i64 { let (r, n) = p; n }\n\
                 fn main() { let x = take((R { id: 41 }, 1)); println(f\"{x}\"); }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — the param returned BARE, already handled by the
        // whole-param passthrough guard this fix deliberately leaves alone.
        (
            "bare-param-control",
            "fn take(r: R) -> R { r }\n\
                 fn main() { let x = take(R { id: 41 }); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // CONTROL — nothing escapes at all; the whole temp dies in the call.
        (
            "nothing-escapes-control",
            "fn take(p: (R, i64)) { let (r, n) = p; println(f\"{r.id + n}\"); }\n\
                 fn main() { take((R { id: 41 }, 1)); println(\"end\"); }\n",
            "42\ndrop 41\nend\n",
        ),
    ] {
        let prog = format!("{DROPPER}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-28-17 — the STRUCT twin of
/// `e2e_user_drop_body_of_a_returned_tuple_param_element_runs_once`: a
/// field extracted out of an owned STRUCT param and returned ran its user
/// `Drop` body TWICE. Same two owners as the tuple leg — the caller-side
/// fresh-temp argument walk, which fires every Drop-bearing field of a
/// struct-LITERAL argument on the theory that the whole temp dies inside
/// the call, plus the result binding's own live-range end — and
/// backend-consistent for the same reason, so no parity harness saw it.
///
/// The fix is the same shape as the tuple one and reuses its analysis:
/// `fn_returns_param_part_paths` already emitted `ParamPart::Field`, and
/// what was missing was a per-field caller-side site to apply it at. Codegen
/// masks the escaping indices out of `__karac_dropbodies_<T>` (the
/// pre-existing `_skipping` emitter, whose symbol name folds in the
/// surviving index list); the interpreter removes the fields from the
/// `Value::Struct` before the walk, the B-2026-08-03-8 masking pattern.
///
/// `two-droppers-a` / `two-droppers-b` are the rows that constrain the fix
/// rather than just reproducing the bug, and they are why the mask is
/// per-FIELD. Suppressing the walk for the whole argument — the obvious
/// reading of "the callee returns it" — fixes the escaping field and takes
/// the OTHER field's body from one to zero. Both orders are pinned so a
/// mask that is off by one field fails one of them.
///
/// `projection-return` shows the trigger is not the destructure the row
/// was filed against: `w.r` returned directly is the same defect with no
/// `let` anywhere, exactly as `p.0` was on the tuple leg. What the
/// spellings share is that a PART of the param escapes, which the
/// whole-param passthrough guard (`fn_returns_param`, untouched here)
/// cannot express.
///
/// `two-callees` guards the symbol memo: two callees over the SAME struct
/// type disagree about which field escapes, so a mask that leaked between
/// them through `__karac_dropbodies_W` would give one of the two the
/// other's answer.
///
/// The last three rows are CONTROLS that were already correct and must
/// stay correct: returning a non-field (`-> i64`, so the fields really do
/// die in the call and the caller-side walk is their only body), returning
/// the param BARE (the pre-existing whole-param guard), and a two-dropper
/// struct nothing escapes from. Twin: `tests/interpreter.rs`'s
/// `test_returned_struct_param_field_user_drop_body_runs_once`.
///
/// Three neighbouring shapes were deliberately NOT here, each filed rather
/// than pinned so this fixture asserts only counts that are actually
/// right: a parent struct that declares its OWN `Drop` (B-2026-08-28-21 —
/// a different registration channel, the full `karac_drop_<T>` wrapper;
/// now fixed and pinned by the sibling fixture below, which found that the
/// wrapper DOES decompose — it is body, then a bodies walk, then the field
/// frees, so masking the middle step alone was enough); a callee that
/// yields a different field on each BRANCH (B-2026-08-28-22 — the analysis
/// unions over return sites, so both get masked and whichever one died in
/// the call loses its body, the same conservative-true trade
/// `fn_returns_param` already makes for the whole-param case); and a
/// NESTED projection `w.inner.r` (B-2026-08-28-23 — the analysis declines
/// to classify it, so the pre-fix double body survives).
#[test]
fn e2e_user_drop_body_of_a_returned_struct_param_field_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n";
    for (label, body, want) in [
        // The row's own repro.
        (
            "destructure-return",
            "struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> R { let W { r, n } = w; r }\n\
                 fn main() { let x = take(W { r: R { id: 41 }, n: 1 }); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // No destructure at all — a direct field projection.
        (
            "projection-return",
            "struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> R { w.r }\n\
                 fn main() { let x = take(W { r: R { id: 41 }, n: 1 }); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // Same, spelled with an explicit `return`.
        (
            "explicit-return",
            "struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> R { let W { r, n } = w; return r; }\n\
                 fn main() { let x = take(W { r: R { id: 41 }, n: 1 }); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // RENAMED destructure leaf — the escaping part is keyed by the
        // FIELD name, not the binding the pattern introduces for it.
        (
            "renamed-leaf",
            "struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> R { let W { r: inner, n } = w; inner }\n\
                 fn main() { let x = take(W { r: R { id: 41 }, n: 1 }); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // BOTH fields drop and only `a` escapes: 41 leaves through the
        // result, 42 dies in the call. One body each, never two, never none.
        (
            "two-droppers-a",
            "struct W { a: R, b: R }\n\
                 fn take(w: W) -> R { let W { a, b } = w; a }\n\
                 fn main() { let x = take(W { a: R { id: 41 }, b: R { id: 42 } });\n\
                 \x20            println(f\"{x.id}\"); }\n",
            "drop 42\n41\ndrop 41\n",
        ),
        // The same struct with the OTHER field escaping — pins the mask to
        // the right index rather than merely to "one of them".
        (
            "two-droppers-b",
            "struct W { a: R, b: R }\n\
                 fn take(w: W) -> R { let W { a, b } = w; b }\n\
                 fn main() { let x = take(W { a: R { id: 41 }, b: R { id: 42 } });\n\
                 \x20            println(f\"{x.id}\"); }\n",
            "drop 41\n42\ndrop 42\n",
        ),
        // The field escapes INSIDE a returned struct literal rather than
        // bare — `yielded` recurses through the literal to find it.
        (
            "escape-via-struct-literal",
            "struct W { r: R, n: i64 }\n\
                 struct Q { r: R }\n\
                 fn take(w: W) -> Q { let W { r, n } = w; Q { r: r } }\n\
                 fn main() { let x = take(W { r: R { id: 41 }, n: 1 });\n\
                 \x20            println(f\"{x.r.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // The RESULT is discarded, so the escaping field's owner is the
        // discarded temp. Still exactly one body.
        (
            "result-discarded",
            "struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> R { let W { r, n } = w; r }\n\
                 fn main() { take(W { r: R { id: 41 }, n: 1 }); println(\"end\"); }\n",
            "drop 41\nend\n",
        ),
        // Two callees over the SAME struct type disagreeing about which
        // field escapes — the symbol memo must not hand one the other's mask.
        (
            "two-callees",
            "struct W { r: R, n: i64 }\n\
                 fn keep(w: W) -> R { let W { r, n } = w; r }\n\
                 fn eat(w: W) -> i64 { let W { r, n } = w; n }\n\
                 fn main() { let a = keep(W { r: R { id: 41 }, n: 1 }); println(f\"{a.id}\");\n\
                 \x20            let b = eat(W { r: R { id: 42 }, n: 2 }); println(f\"{b}\"); }\n",
            "41\ndrop 41\ndrop 42\n2\n",
        ),
        // CONTROL — the callee returns a NON-field, so the dropper dies
        // inside the call and the caller's walk is its ONLY body.
        // Suppressing per-argument instead of per-field takes this to zero.
        (
            "no-field-escapes-control",
            "struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> i64 { let W { r, n } = w; n }\n\
                 fn main() { let x = take(W { r: R { id: 41 }, n: 1 }); println(f\"{x}\"); }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — the param returned BARE, already handled by the
        // whole-param passthrough guard this fix deliberately leaves alone.
        (
            "bare-param-control",
            "fn take(r: R) -> R { r }\n\
                 fn main() { let x = take(R { id: 41 }); println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // CONTROL — two droppers and NOTHING escapes; both bodies fire in
        // the call, in reverse declaration order.
        (
            "nothing-escapes-control",
            "struct W { a: R, b: R }\n\
                 fn take(w: W) -> i64 { let W { a, b } = w; 7 }\n\
                 fn main() { let x = take(W { a: R { id: 41 }, b: R { id: 42 } });\n\
                 \x20            println(f\"{x}\"); }\n",
            "drop 42\ndrop 41\n7\n",
        ),
    ] {
        let prog = format!("{DROPPER}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-28-21 — the same escaping-field mask, for a parent that
/// declares its OWN `Drop`.
///
/// The sibling fixture above masks `__karac_dropbodies_<T>`, the walker
/// registered for a parent that carries a Drop-bearing field but declares
/// no `Drop` itself. A parent WITH one takes a different branch on both
/// backends and never reaches that mask: codegen registers the full
/// `karac_drop_<T>` wrapper, the interpreter calls
/// `run_user_drop_body_on_value`. So the escaping field's body ran here and
/// again at the result's owner, on all three backends.
///
/// THE ROW EXPECTED THIS TO BE EXPENSIVE and it was not, which is the part
/// worth recording: it reads the wrapper as an indivisible "body + fields +
/// frees" unit and concluded a mask would need a whole variant emitter with
/// a body/free split to get wrong. The wrapper is already three separate
/// calls in that order, and only the MIDDLE one — the same maskable walker
/// the fixture above uses — had to change. The interpreter's helper is
/// literally `run_user_drop_body_only` followed by the field walk, so it
/// decomposes the same way.
///
/// ONLY THE BODY STEP IS MASKED. The escaping field's MEMORY is still the
/// caller temp's to release — the callee got a copy of the aggregate, not
/// its allocation — so a mask reaching the frees would trade the double
/// body for a leak. `tests/memory_sanitizer.rs`'s
/// `asan_own_drop_parent_masking_a_returned_field_frees_it` is that half.
///
/// `two-droppers` is what makes the mask per-FIELD rather than a wholesale
/// "skip the field bodies": `karac_dropnf_<T>`, which already existed for
/// the moved-out-field shape, would take `b`'s body from one to zero.
///
/// `from-call` is not decoration either. The fn-call arm of the same helper
/// registers the identical wrapper for `take(mk())`, and fixing only the
/// struct-literal arm left it as the WORSE half of the pair — the
/// interpreter got it right and both compiled backends doubled, a
/// run-vs-build divergence in place of a uniform wrong answer.
///
/// The last row is a CONTROL that was already correct: a callee returning a
/// NON-field, where both bodies must still run here. The parent's own body
/// fires in every row and would be the first casualty of a mask applied one
/// step too early.
///
/// THE DISCARDED-RESULT SHAPE IS NOT HERE, and its absence is a finding
/// rather than an omission. `take(W { .. });` with the result thrown away
/// now runs each body exactly ONCE on all three backends — this fix — but
/// the two compiled backends emit the FIELD's body before the parent's,
/// while the interpreter emits the parent's first, which is the order
/// design.md § Drop ordering specifies. That divergence predates this fix
/// (verified by stashing `src/`: pre-fix compiled output was `drop 47`,
/// `drop W5`, `drop 47` against the interpreter's `drop W5`, `drop 47`,
/// `drop 47`), so it is a separate defect on a separate mechanism and is
/// filed as B-2026-08-28-53 rather than pinned here in whichever direction
/// happens to be wrong. The interpreter twin covers the shape's COUNTS,
/// which this fix does settle.
#[test]
fn e2e_own_drop_parent_runs_a_returned_fields_body_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n\
             struct W { r: R, n: i64 }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"drop W{self.n}\"); } }\n";
    for (label, body, want) in [
        // The row's own repro.
        (
            "destructure-return",
            "#[allow(partial_move_of_drop_struct)]\n\
                 fn take(w: W) -> R { let W { r, n } = w; r }\n\
                 fn main() { let x = take(W { r: R { id: 41 }, n: 1 }); println(f\"{x.id}\"); }\n",
            "drop W1\n41\ndrop 41\n",
        ),
        // The `w.r` spelling — same defect with no `let` anywhere, exactly
        // as on the no-own-Drop leg.
        (
            "projection-return",
            // B-2026-09-03-20 — the `#[allow]` is new, the OUTPUT is not.
            // `return w.r;` is the same partial move the destructure cell
            // above spells with a `let`, and it was silent only while the
            // rule was site-based. The expected output is unchanged, which
            // is the point: this cell pins B-2026-08-28-21's escape mask,
            // and the mask still makes the PARAM root emit one body. The
            // rule rejects the SHAPE, not the misbehaviour.
            "#[allow(partial_move_of_drop_struct)]\n\
                 fn take(w: W) -> R { return w.r; }\n\
                 fn main() { let x = take(W { r: R { id: 42 }, n: 2 }); println(f\"{x.id}\"); }\n",
            "drop W2\n42\ndrop 42\n",
        ),
        // The temp comes from a CALL rather than a literal — the other arm
        // of the same helper, and the one whose omission would show up as a
        // run-vs-build divergence rather than a uniform wrong count.
        (
            "from-call",
            "fn mk() -> W { return W { r: R { id: 43 }, n: 3 }; }\n\
                 #[allow(partial_move_of_drop_struct)]\n\
                 fn take(w: W) -> R { let W { r, n } = w; r }\n\
                 fn main() { let x = take(mk()); println(f\"{x.id}\"); }\n",
            "drop W3\n43\ndrop 43\n",
        ),
        // TWO droppers, one escaping: the survivor's body must still run
        // here, which is what forbids the wholesale `karac_dropnf_<T>`.
        (
            "two-droppers",
            "struct Two { a: R, b: R }\n\
                 impl Drop for Two { fn drop(mut ref self) { println(\"drop Two\"); } }\n\
                 #[allow(partial_move_of_drop_struct)]\n\
                 fn take(t: Two) -> R { let Two { a, b } = t; a }\n\
                 fn main() { let x = take(Two { a: R { id: 44 }, b: R { id: 45 } });\n\
                 \x20            println(f\"{x.id}\"); }\n",
            "drop Two\ndrop 45\n44\ndrop 44\n",
        ),
        // CONTROL — nothing escapes, so both bodies belong here.
        (
            "nothing-escapes-control",
            "fn use_n(w: W) -> i64 { return w.n; }\n\
                 fn main() { let k = use_n(W { r: R { id: 46 }, n: 4 }); println(f\"{k}\"); }\n",
            "drop W4\ndrop 46\n4\n",
        ),
    ] {
        let prog = format!("{DROPPER}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-30-15 — the STRUCT flavour of the fresh-temp scrutinee, which
/// had no owner at all: `materialize_freshtemp_enum_scrutinee` bails at
/// `variant_pattern_enum_name` (`None` for a struct pattern) and no sibling
/// picked the value up, so the type's own `drop()` never ran under
/// `karac build`. Not a placement divergence — the body was ABSENT, which is
/// why the B-2026-08-29-28 machinery next door could not have reached it.
///
/// THE ROW UNDERSTATED THIS, and the rows below are the corrected
/// measurement. It reported only the destructuring arm, where the moved-out
/// binding happens to carry the field's body and buffer. The arms that move
/// NOTHING out lose both bodies AND leak the payload — pre-fix, compiled:
///
///   wildcard-field   `v`                  vs interp `v dS dR7`
///   whole-value-bind `v7`                 vs interp `v7 dS dR7`
///   all-scalar       `v7`                 vs interp `v7 dS`
///   heap-field-bound `v7[…]`              vs interp `v7[…] dS`
///   if-let           `v`                  vs interp `v dS dR7`
///   let-else         `v … s2`             vs interp `v dS dR7 s1 dS dR7 s2`
///   loop             `w it w it`          vs interp `w dS dR7 it …`
///
/// The memory half is pinned separately in
/// `memory_sanitizer::asan_freshtemp_struct_scrutinee_frees_its_payload`.
///
/// `destructuring-arm-boundary` IS A KNOWN GAP PINNED AS A BOUNDARY, not a
/// passing case. An arm that moves a `Drop`-bearing field out is DECLINED by
/// the fix, so it still prints `v7 dR7` against the interpreter's
/// `v7 dS dR7`. Registering the wrapper anyway makes it `v7 dR7 dS dR7` —
/// the missing `dS` appears and `dR7` runs TWICE, a double close for a
/// resource type — because the wrapper's field-body step re-runs a body the
/// arm binding already owns, and the cap-zeroing suppressor stops the second
/// FREE but not the second BODY. That is B-2026-08-31-31, which the BOUND
/// spelling exhibits identically; when it lands, this row becomes
/// `v7 dS dR7` and the decline gate goes with it. It is pinned HERE so the
/// gate cannot be dropped silently.
///
/// `no-own-drop-boundary` is the other untouched boundary and points the
/// opposite way: a struct with no `Drop` of its own has no wrapper to
/// register, and on that shape it is the INTERPRETER that runs no body while
/// compiled runs the binding's (`v7 dR7` vs `v7`) — B-2026-08-31-32, an
/// interpreter-side row this must not perturb from the codegen side.
#[test]
fn e2e_freshtemp_struct_scrutinee_runs_its_own_drop_body() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct S { r: R }\n\
             impl Drop for S { fn drop(mut ref self) { println(\"dS\") } }\n\
             struct Sc { n: i64 }\n\
             impl Drop for Sc { fn drop(mut ref self) { println(\"dSc\") } }\n\
             struct Sh { s: String, n: i64 }\n\
             impl Drop for Sh { fn drop(mut ref self) { println(\"dSh\") } }\n\
             struct N { r: R }\n\
             fn mkS() -> S { return S { r: R { id: 7 } } }\n\
             fn mkSc() -> Sc { return Sc { n: 7 } }\n\
             fn mkSh() -> Sh { return Sh { s: \"pay\", n: 7 } }\n\
             fn mkN() -> N { return N { r: R { id: 7 } } }\n";
    for (label, body, want) in [
        (
            "wildcard-field",
            "match mkS() { S { r: _ } => { println(\"v\") } }\n",
            "v\ndS\ndR7\npost\n",
        ),
        (
            "whole-value-bind",
            "match mkS() { s => { println(f\"v{s.r.id}\") } }\n",
            "v7\ndS\ndR7\npost\n",
        ),
        (
            "all-scalar",
            "match mkSc() { Sc { n: n } => { println(f\"v{n}\") } }\n",
            "v7\ndSc\npost\n",
        ),
        (
            "heap-field-bound",
            "match mkSh() { Sh { s: s, n: n } => { println(f\"v{n}[{s}]\") } }\n",
            "v7[pay]\ndSh\npost\n",
        ),
        (
            "if-let",
            "if let S { r: _ } = mkS() { println(\"v\") }\n",
            "v\ndS\ndR7\npost\n",
        ),
        (
            "let-else",
            "let S { r: _ } = mkS() else { println(\"miss\"); return };\n\
                 println(\"s1\")\n",
            "dS\ndR7\ns1\npost\n",
        ),
        (
            "in-loop-body",
            "let mut i = 0;\n\
                 while i < 2 { match mkS() { S { r: _ } => { println(\"w\") } }\n\
                 \x20   println(\"it\"); i = i + 1; }\n",
            "w\ndS\ndR7\nit\nw\ndS\ndR7\nit\npost\n",
        ),
        (
            "destructuring-arm-boundary",
            "match mkS() { S { r: r } => { println(f\"v{r.id}\") } }\n",
            "v7\ndR7\npost\n",
        ),
        (
            "no-own-drop-boundary",
            "match mkN() { N { r: r } => { println(f\"v{r.id}\") } }\n",
            "v7\ndR7\npost\n",
        ),
    ] {
        // Exactly two cells bind a non-`Copy` field out of an own-`Drop`
        // scrutinee, which `partial_move_of_drop_struct` denies
        // (B-2026-09-01-43). The opt-out is the point rather than a
        // workaround: what they pin is codegen's drop placement for that
        // shape, and the shape stays reachable through this attribute.
        // Applied per cell, not to the shared wrapper, so the other eight
        // keep `assert_check_clean`'s gate.
        let attr = match label {
            "heap-field-bound" | "destructuring-arm-boundary" => {
                "#[allow(partial_move_of_drop_struct)]\n"
            }
            _ => "",
        };
        let src = format!("{H}{attr}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-28-26 — a TUPLE-TYPED leaf BOUND out of a tuple destructure
/// runs its inner element's user `Drop` body, at the interpreter's
/// placement.
///
/// The leaf is a `PatternKind::Binding` whose element `TypeExpr` is a
/// `TypeKind::Tuple`, so it clears the place-source walker's PATTERN test —
/// which B-2026-08-28-8 taught to recurse — and then exits at that walker's
/// `TypeKind::Path` test, one step later and silent for the same reason.
///
/// IT NEEDS A DIFFERENT CHANNEL from the enum / nested-struct leaves beside
/// it: `track_destructure_leaf_cleanup` dispatches off the
/// String/Vec/Map/Set/struct side-tables and a tuple leaf is none of them.
/// The tuple channel is the pair the let-site for a tuple BINDING uses —
/// `emit_tuple_elem_user_drop_bodies_fn` on the `ContainerElemBodies` (NLL)
/// channel, over `synthesize_tuple_drop_fn_te` for the memory.
///
/// PLACEMENT IS PART OF THE CLAIM, not just the count, which the row asked
/// be settled with the fix rather than after it. `leaf-unused` and
/// `leaf-used` differ only in whether the bound leaf is read, and the body
/// moves from before the `println` to after it on every backend — the NLL
/// end of the leaf's live range, which is where the interpreter puts it.
///
/// BOTH SOURCE KINDS, because they run through different functions
/// (`place_source_tuple_leaf_cleanups` and
/// `track_tuple_destructure_leaf_cleanups`) and only the place spelling is
/// in the row. Fixing one would have left the other silent beside it.
#[test]
fn e2e_tuple_typed_binding_leaf_runs_its_inner_drop_body() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    for (label, body, want) in [
        // The row's own repro: PLACE source, leaf bound and unread, so the
        // body belongs at the destructure.
        (
            "place-source-leaf-unused",
            "fn main() { let p = ((R { id: 41 }, 2), 1); let (inner, n) = p;\n\
                 \x20            println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // The same leaf READ — the body moves after the read.
        (
            "place-source-leaf-used",
            "fn main() { let p = ((R { id: 41 }, 2), 1); let (inner, n) = p;\n\
                 \x20            println(f\"{inner.1 + n}\") }\n",
            "3\ndrop 41\n",
        ),
        // FRESH tuple-literal source — the sibling path, not in the row.
        (
            "fresh-literal-source",
            "fn main() { let (inner, n) = ((R { id: 41 }, 2), 1); println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // FRESH call source, same path.
        (
            "call-source",
            "fn mk() -> ((R, i64), i64) { ((R { id: 41 }, 2), 1) }\n\
                 fn main() { let (inner, n) = mk(); println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // TWO Drop-bearing elements inside the bound tuple.
        (
            "two-inner-elements",
            "fn main() { let p = ((R { id: 41 }, R { id: 42 }), 1); let (inner, n) = p;\n\
                 \x20            println(f\"{n}\") }\n",
            "drop 41\ndrop 42\n1\n",
        ),
        // DEPTH — a doubly nested tuple bound at the middle level.
        (
            "triple-nesting",
            "fn main() { let p = (((R { id: 41 }, 3), 2), 1); let (mid, n) = p;\n\
                 \x20            println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — the nested-PATTERN spelling, which B-2026-08-28-8 fixed
        // and which this must not disturb.
        (
            "nested-pattern-control",
            "fn main() { let p = ((R { id: 41 }, 2), 1); let ((r, m), n) = p;\n\
                 \x20            println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — the same source NEVER destructured. Correct on every
        // surface before this, and the row that proves the source's own walk
        // does reach this element — which is why the leaf can only take the
        // body by taking the element with it.
        (
            "no-destructure-control",
            "fn main() { let p = ((R { id: 41 }, 2), 1); println(f\"{p.1}\") }\n",
            "1\ndrop 41\n",
        ),
        // The leaf read one level DEEPER — through the inner element and
        // into its field. Pinned separately from `place-source-leaf-used`,
        // which stops at `inner.1`, because it exercises a different thing:
        // the leaf's element TYPE has to be recorded well enough for
        // codegen to name `.id` on it. A second session's independent fix
        // for this row registered the bodies without that, and this program
        // did not lower at all there — `cannot resolve field 'id' on this
        // receiver`. The fix as committed lowers it.
        (
            "place-source-leaf-read-through",
            "fn main() { let p = ((R { id: 42 }, 2), 1); let (inner, n) = p;\n\
                 \x20            println(f\"{inner.0.id}\"); println(f\"{n}\") }\n",
            "42\ndrop 42\n1\n",
        ),
        // A SIBLING top-level dropper beside the tuple leaf — one body each,
        // and the row that constrains HOW the leaf takes its body rather
        // than just showing that it does.
        //
        // Every other row here has a single dropper, so any registration
        // that fires once passes them. Built one way round, this shape went
        // to TWO bodies for the sibling: transferring the leaf's element by
        // re-registering the SOURCE's element-bodies walker with that index
        // masked also re-arms the walker over the destructure's OTHER
        // elements, whose bodies the arm beside this one has already taken.
        // Measured while landing this row from a second session, against the
        // fix as committed — which passes it.
        (
            "sibling-top-level-dropper",
            "fn main() { let p = ((R { id: 46 }, 2), R { id: 56 }); let (inner, n) = p;\n\
                 \x20            println(\"mid\") }\n",
            "drop 56\ndrop 46\nmid\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-28-10 — a struct-pattern destructure of a PLACE source runs
/// its leaf's user `Drop` body at the LEAF's live-range end, on the leaf's
/// own value.
///
/// The row measured this as a placement error — `drop 41` before the
/// `println` that reads the leaf, against the interpreter's after. It is
/// worse than that, and the heap-carrying rows are what show it: the body
/// was running on the SOURCE, which `bind_pattern` had already moved out of,
/// so it printed `drop 41 ` where the interpreter printed `drop 41 n41`. A
/// body observing an emptied field is a wrong-value bug wearing an ordering
/// bug's clothes.
///
/// `field-access-model` is the row that made the fix a transcription rather
/// than an invention: `let x = w.r` is the SAME move, already correct on
/// every backend, because `disarm_struct_field_move_bodies` masks the field
/// out of `w`'s walk and the destination registers its own. The destructure
/// spelling simply never did that. Three steps carry over — disarm the
/// source's bodies, suppress its memory for that field, register the leaf.
///
/// THE ROW'S OTHER TWO HALVES ARE ELSEWHERE, and are pinned here as
/// controls rather than restated: the CALL source it reported as running no
/// body at all was closed by B-2026-08-28-29, and the by-value PARAM source
/// is B-2026-08-28-19's caller-side-vs-scope-exit ordering, deliberately
/// untouched.
#[test]
fn e2e_place_source_destructure_runs_its_leaf_drop_body_at_the_leaf() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
             struct W { r: R, n: i64 }\n\
             fn make() -> W { W { r: R { id: 41 }, n: 1 } }\n";
    for (label, body, want) in [
        // The row's own repro.
        (
            "place-source",
            "fn main() { let w = W { r: R { id: 41 }, n: 1 };\n\
                 \x20            let W { r, n } = w; println(f\"{r.id + n}\") }\n",
            "42\ndrop 41\n",
        ),
        // TWO Drop-bearing fields — both move, and their relative order
        // (reverse declaration) has to survive the transfer.
        (
            "two-fields",
            "struct V { a: R, b: R }\n\
                 fn main() { let v = V { a: R { id: 41 }, b: R { id: 42 } };\n\
                 \x20            let V { a, b } = v; println(f\"{a.id + b.id}\") }\n",
            "83\ndrop 42\ndrop 41\n",
        ),
        // The leaf CONSUMED by a call rather than read.
        (
            "consumed-leaf",
            "fn take(r: R) -> i64 { r.id }\n\
                 fn main() { let w = W { r: R { id: 41 }, n: 1 };\n\
                 \x20            let W { r, n } = w; println(f\"{take(r) + n}\") }\n",
            "42\ndrop 41\n",
        ),
        // THE MODEL — the same move written as a field access, correct on
        // every backend before this and the shape the fix transcribes.
        (
            "field-access-model",
            "fn main() { let w = W { r: R { id: 41 }, n: 1 };\n\
                 \x20            let x = w.r; println(f\"{x.id + w.n}\") }\n",
            "42\ndrop 41\n",
        ),
        // BOUNDARY — the leaf is bound but never READ, so both placements
        // coincide. Correct before and after, and the reason the defect
        // hides in the shapes people usually write.
        (
            "leaf-unread",
            "fn main() { let w = W { r: R { id: 41 }, n: 1 };\n\
                 \x20            let W { r, n } = w; println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — a WILDCARD leaf, which never had a leaf to move to.
        (
            "wildcard-leaf-control",
            "fn main() { let w = W { r: R { id: 41 }, n: 1 };\n\
                 \x20            let W { r: _, n } = w; println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — the CALL source, the row's other reported half, closed
        // by B-2026-08-28-29.
        (
            "call-source-control",
            "fn main() { let W { r, n } = make(); println(f\"{r.id + n}\") }\n",
            "42\ndrop 41\n",
        ),
        // CONTROL — the TUPLE spelling, which has transferred since
        // B-2026-08-28-1 and is what the struct path now matches.
        (
            "tuple-source-control",
            "fn main() { let p = (R { id: 41 }, 1); let (r, n) = p;\n\
                 \x20            println(f\"{r.id + n}\") }\n",
            "42\ndrop 41\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
    // THE VALUE, not just its position. With a heap field the pre-fix body
    // printed `drop 41 ` — the source copy `bind_pattern` had already moved
    // out of — so this row fails on CONTENT even if a future regression
    // happens to restore the ordering.
    assert_eq!(
            run_program(
                "struct R { id: i64, name: String }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n\
                 struct W { r: R, n: i64 }\n\
                 fn main() { let w = W { r: R { id: 41, name: f\"n{41}\" }, n: 1 };\n\
                 \x20            let W { r, n } = w; println(f\"{r.name} {n}\") }\n"
            )
            .as_deref(),
            Some("n41 1\ndrop 41 n41\n"),
            "heap-field-value"
        );
    // The leaf type declares NO `Drop` of its own but carries a Drop-bearing
    // field. It has no wrapper to take, so the body is registered AFTER the
    // memory owner rather than instead of it — the other half of the
    // placement rule, and the row that catches getting it backwards
    // (`drop res 41 ` off a freed buffer).
    assert_eq!(
            run_program(
                "struct Res { id: i64, tag: String }\n\
                 impl Drop for Res { fn drop(mut ref self) { println(f\"drop res {self.id} {self.tag}\") } }\n\
                 struct R { res: Res }\n\
                 struct W { r: R, n: i64 }\n\
                 fn main() { let w = W { r: R { res: Res { id: 41, tag: f\"t{1}\" } }, n: 1 };\n\
                 \x20            let W { r, n } = w; println(f\"{r.res.id + n}\") }\n"
            )
            .as_deref(),
            Some("42\ndrop res 41 t1\n"),
            "field-only-drop-leaf"
        );
    // CONTROL — a CLOSURE parameter source. Its bodies are owned
    // caller-side under caller-retains, so there is nothing to transfer;
    // a first cut that gated on an exclusion list instead of on "does the
    // source actually hold the walk" printed the body TWICE here.
    assert_eq!(
            run_program(
                "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
                 struct W { r: R, n: i64 }\n\
                 fn main() { let f = |w: W| { let W { r, n } = w; r.id + n };\n\
                 \x20            println(f\"{f(W { r: R { id: 41 }, n: 1 })}\"); println(\"end\"); }\n"
            )
            .as_deref(),
            // ORDER CORRECTED by B-2026-08-29-55 (see the tuple-list sites above):
            // the closure argument temp dies as the call returns, before `println`.
            Some("drop 41\n42\nend\n"),
            "closure-param-source-control"
        );
}

/// B-2026-08-28-29 — a destructure of a FRESH STRUCT source runs its leaf's
/// user `Drop` body, in both source spellings and for both leaf kinds.
///
/// TWO SEPARABLE ROOTS, which is why fixing either alone leaves half the
/// surface broken:
///
///   (a) `expr_yields_fresh_owned_temp` admits only `Call` / `MethodCall`,
///       so a struct LITERAL source reached NO arm of the leaf loop. The
///       TUPLE path took exactly this widening in B-2026-08-28-1; the
///       struct spelling never did.
///   (b) even on the admitted CALL spelling, a BOUND leaf registered memory
///       cleanup and no user body — `track_owned_destructure_field_cleanup`
///       is memory-only, while the tuple path's leaf has carried the body
///       cascade since B-2026-08-28-1. That cascade is now SHARED rather
///       than transplanted a second time.
///
/// The rows below cross the two axes deliberately: `bound-call` fails on
/// (b) alone, `wildcard-literal` on (a) alone, and `bound-literal` needs
/// both. `field-only-drop` is the third leaf kind — a type with no `Drop`
/// of its own but a Drop-bearing FIELD, which takes the field-bodies walk
/// rather than the wrapper.
///
/// THE CONTROLS ARE WHAT LOCALIZE IT to a fresh STRUCT source rather than
/// to struct destructures generally: place, param and tuple sources were
/// all correct on all three backends before this and must stay at one.
#[test]
fn e2e_fresh_struct_source_destructure_runs_its_leaf_drop_body() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
             struct W { r: R, n: i64 }\n\
             fn mk() -> W { W { r: R { id: 41 }, n: 1 } }\n";
    for (label, body, want) in [
        // (b) alone — the CALL source was admitted; the bound leaf ran no body.
        (
            "bound-call",
            "fn main() { let W { r, n } = mk(); println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // (a) and (b) together — a LITERAL source reached no arm at all.
        (
            "bound-literal",
            "fn main() { let W { r, n } = W { r: R { id: 41 }, n: 1 }; println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // (a) alone — B-2026-08-28-12's wildcard arm lives inside the gate,
        // so it never ran on the literal spelling.
        (
            "wildcard-literal",
            "fn main() { let W { r: _, n } = W { r: R { id: 41 }, n: 1 }; println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — the wildcard leaf over a CALL source, B-2026-08-28-12's
        // own fix. Correct before this and pinned so the widening does not
        // take it to two.
        (
            "wildcard-call-control",
            "fn main() { let W { r: _, n } = mk(); println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — a PLACE source.
        (
            "place-source-control",
            "fn main() { let w = W { r: R { id: 41 }, n: 1 };\n\
                 \x20            let W { r, n } = w; println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — a by-value PARAM source, pinned at the COMPILED
        // ordering. The interpreter disagrees with it (`drop 41` before the
        // `1`), and that split is B-2026-08-28-19: the caller-side
        // fresh-temp argument walk fires at the call there and at scope exit
        // here. Neither this row nor B-2026-08-28-10 touches it; the row is
        // here so a future ownership change cannot move the compiled side
        // without someone noticing.
        (
            // ORDER CORRECTED by B-2026-08-29-55 -- the argument temp dies AS THE
            // CALL RETURNS (design.md's "Function/method call argument | After the
            // call returns"), which is BEFORE the enclosing `println` runs. The old
            // expectation held it to the statement's `;`; `--interp` printed the
            // body first all along, so that line recorded a run-vs-build divergence
            // rather than a decision.
            "param-source-control",
            "fn take(w: W) -> i64 { let W { r, n } = w; n }\n\
                 fn main() { println(f\"{take(mk())}\") }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — the TUPLE spelling, which got this cascade in
        // B-2026-08-28-1 and is the model the struct path now shares.
        (
            "tuple-source-control",
            "fn main() { let p = (R { id: 41 }, 1); let (r, n) = p; println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
    // The leaf type declares no `Drop` of its own but CARRIES a Drop-bearing
    // field — the arm that takes the field-bodies walk PLUS the ordinary
    // memory drop, rather than the whole-value wrapper. Its own header.
    const F: &str = "struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(f\"drop res {self.id}\") } }\n\
             struct R { res: Res }\n\
             struct W { r: R, n: i64 }\n";
    assert_eq!(
        run_program(&format!(
            "{F}fn mk() -> W {{ W {{ r: R {{ res: Res {{ id: 41 }} }}, n: 1 }} }}\n\
                 \x20            fn main() {{ let W {{ r, n }} = mk(); println(f\"{{n}}\") }}\n"
        ))
        .as_deref(),
        Some("drop res 41\n1\n"),
        "field-only-drop-call"
    );
    assert_eq!(
        run_program(&format!(
            "{F}fn main() {{ let w = W {{ r: R {{ res: Res {{ id: 41 }} }}, n: 1 }};\n\
                 \x20            let W {{ r, n }} = w; println(f\"{{n}}\") }}\n"
        ))
        .as_deref(),
        Some("drop res 41\n1\n"),
        "field-only-drop-place-control"
    );
}

/// B-2026-09-03-10 — A CONDITIONAL `return <aggregate literal>` MUST NOT
/// DISARM THE SOURCE ON THE PATHS THAT DO NOT TAKE IT.
///
/// B-2026-08-30-18 gave a returned aggregate literal a static retraction of
/// every source's `UserDrop`, and said so in its own comment: "Static and
/// flow-insensitive like every sibling: a conditional return disarms on all
/// paths, which can only under-fire." Under-firing is a LOST `Drop` body.
/// The BARE-`Identifier` arm a few lines below had already been through
/// this (B-2026-08-28-65) and prefers a runtime bit when the action lives in
/// an ENCLOSING frame; the literal arm now asks the same question through
/// the same helper.
///
/// `if-not-taken` is the SIMPLEST repro and the one the filing missed — no
/// loop at all. `build(14, false)` never reaches the `return W { r: r }`,
/// and printed `mid v99 dR99 post` on all three compiled surfaces against
/// the interpreter's `mid dR14 v99 dR99 post`.
///
/// `loop-*` is where it compounds: the disarm is per-FUNCTION, so THREE
/// iterations returning at `i == 2` lose BOTH earlier bodies, not one.
/// `loop-tuple` is the same defect through the `Tuple` half of the arm's
/// `matches!`, and `loop-two-fields` shows it is per-source rather than
/// per-statement.
///
/// THE MEMORY IS NOT LEAKED, and that is why this was invisible to every
/// leak gate: valgrind reports 0 errors and 0 bytes lost on the PRE-FIX
/// binaries for every row here. The buffer's owner was transferred by
/// `suppress_source_vec_cleanup_for_arg`, which is a separate hook; what
/// went missing is only the observable side effect. A `Drop` that closes a
/// handle, releases a lock or decrements an external counter simply did not
/// run.
///
/// The three `ctl-*` rows are the shapes that were always correct and pin
/// the boundary: a BARE identifier return took the runtime-bit path already,
/// an UNCONDITIONAL top-level return finds its action in the INNERMOST frame
/// so the guard declines and the static removal still stands, and the
/// conditional return that IS taken must keep handing the value out exactly
/// once.
#[test]
fn e2e_conditional_aggregate_return_keeps_the_untaken_paths_drop() {
    const H: &str = "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct W { r: R }\n\
             struct Two { a: R, b: R }\n\
             fn if_lit(k: i64, flag: bool) -> W {\n\
             \x20   let r: R = R { id: k, tag: f\"t\" };\n\
             \x20   println(\"mid\");\n\
             \x20   if flag { return W { r: r } }\n\
             \x20   return W { r: R { id: 99, tag: f\"z\" } }\n\
             }\n\
             fn loop_lit(k: i64, n: i64) -> W {\n\
             \x20   let mut i: i64 = 0;\n\
             \x20   while i < n {\n\
             \x20       let r: R = R { id: k + i, tag: f\"t\" };\n\
             \x20       println(f\"it{i}\");\n\
             \x20       if i == n - 1 { return W { r: r } }\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   return W { r: R { id: 0, tag: f\"z\" } }\n\
             }\n\
             fn loop_tup(k: i64) -> (R, i64) {\n\
             \x20   let mut i: i64 = 0;\n\
             \x20   while i < 2 {\n\
             \x20       let r: R = R { id: k + i, tag: f\"t\" };\n\
             \x20       println(f\"it{i}\");\n\
             \x20       if i == 1 { return (r, 3) }\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   return (R { id: 0, tag: f\"z\" }, 0)\n\
             }\n\
             fn loop_two(k: i64) -> Two {\n\
             \x20   let mut i: i64 = 0;\n\
             \x20   while i < 2 {\n\
             \x20       let x: R = R { id: k + i, tag: f\"x\" };\n\
             \x20       let y: R = R { id: k + i + 50, tag: f\"y\" };\n\
             \x20       println(f\"it{i}\");\n\
             \x20       if i == 1 { return Two { a: x, b: y } }\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   return Two { a: R { id: 0, tag: f\"z\" }, b: R { id: 1, tag: f\"z\" } }\n\
             }\n\
             fn loop_bare(k: i64) -> R {\n\
             \x20   let mut i: i64 = 0;\n\
             \x20   while i < 2 {\n\
             \x20       let r: R = R { id: k + i, tag: f\"t\" };\n\
             \x20       println(f\"it{i}\");\n\
             \x20       if i == 1 { return r }\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   return R { id: 0, tag: f\"z\" }\n\
             }\n\
             fn top_lit(k: i64) -> W { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); return W { r: r } }\n";
    for (label, body, want) in [
        // THE SIMPLEST REPRO: no loop, the `return` is simply not reached.
        (
            "if-not-taken",
            "let v: W = if_lit(14, false); println(f\"v{v.r.id}\");\n",
            "mid\ndR14\nv99\ndR99\npost\n",
        ),
        // The same function on the path that DOES return: one body, at the
        // caller. This is the half the static retraction got right.
        (
            "ctl-if-taken",
            "let v: W = if_lit(14, true); println(f\"v{v.r.id}\");\n",
            "mid\nv14\ndR14\npost\n",
        ),
        // IN A LOOP the disarm is per-FUNCTION, so every non-returning
        // iteration loses its body, not just one.
        (
            "loop-two-iterations",
            "let v: W = loop_lit(14, 2); println(f\"v{v.r.id}\");\n",
            "it0\ndR14\nit1\nv15\ndR15\npost\n",
        ),
        (
            "loop-three-iterations",
            "let v: W = loop_lit(14, 3); println(f\"v{v.r.id}\");\n",
            "it0\ndR14\nit1\ndR15\nit2\nv16\ndR16\npost\n",
        ),
        // The `Tuple` half of the same `matches!` gate.
        (
            "loop-tuple",
            "let v: (R, i64) = loop_tup(14); println(f\"v{v.0.id}\");\n",
            "it0\ndR14\nit1\nv15\ndR15\npost\n",
        ),
        // Per-SOURCE, not per-statement: two consumed locals, both restored.
        // Fields die in reverse declaration order (design.md § Drop ordering).
        (
            "loop-two-fields",
            "let v: Two = loop_two(1); println(f\"v{v.a.id}-{v.b.id}\");\n",
            "it0\ndR51\ndR1\nit1\nv2-52\ndR52\ndR2\npost\n",
        ),
        // CONTROLS. The bare identifier already took the runtime-bit path;
        // the unconditional top-level return finds its action in the
        // INNERMOST frame, so the guard declines and the static removal
        // stands. Both were correct before this change and must stay so.
        (
            "ctl-loop-bare-identifier",
            "let v: R = loop_bare(14); println(f\"v{v.id}\");\n",
            "it0\ndR14\nit1\nv15\ndR15\npost\n",
        ),
        (
            "ctl-unconditional-top-level",
            "let v: W = top_lit(14); println(f\"v{v.r.id}\");\n",
            "mid\nv14\ndR14\npost\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
    }
}

// ── A by-value argument the callee STORES (B-2026-08-26-9) ──────────
//
// Kāra's calling convention is caller-drops: a fresh temporary passed by
// value is dropped by the CALLER after the call, and the callee registers
// nothing for it. That is right whenever the value dies inside the call,
// and codegen already carved out the one escape route it knew about — the
// callee returning the parameter (`fn_returns_param`). It did not model the
// other route: the callee STORING the parameter into a place the caller
// still holds. `fn push(mut ref self, x: T) { self.xs.push(x); }` leaves
// the value alive in the caller's own queue, so the caller's drop and the
// container's element drain were two owners for one value.
//
// Three call paths reach the same registrar and each had to be gated
// separately — free function, concrete method, and monomorphized generic
// impl method (which is the leg `PriorityQueue.push` takes, and the leg
// whose callee AST lives in `mono_state.generic_fns` rather than in the
// user program snapshot, since the stdlib is baked).

/// The METHOD leg, and the only one of the three that presented as a
/// run-vs-build divergence: AOT printed the element's `drop` body at the
/// push AND at the pop while `--interp` printed it once. The interpreter
/// runs its fresh-temp argument drops on the free-function path only, which
/// is why the same defect on a free function (below) went unnoticed — both
/// backends were wrong there, so an A/B check reported agreement.
#[test]
fn e2e_method_drops_a_stored_by_value_argument_once() {
    let src = r#"
struct Item { id: i64 }
impl Drop for Item {
    fn drop(mut ref self) { println(f"drop {self.id}") }
}
struct Bag { xs: Vec[Item] }
impl Bag {
    fn add(mut ref self, x: Item) { self.xs.push(x); }
}
fn main() {
    let mut b = Bag { xs: Vec.new() };
    b.add(Item { id: 7 });
    println("stored");
    while b.xs.len() > 0 {
        match b.xs.pop() { Some(e) => { println(f"pop {e.id}"); } None => {} }
    }
    println("end");
}
"#;
    let out = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run_program(src))
        .expect("failed to spawn sized worker")
        .join()
        .expect("compile worker panicked");
    assert_eq!(out.as_deref(), Some("stored\npop 7\ndrop 7\nend\n"));
}

/// The FREE-FUNCTION leg. Both backends double-dropped here before the fix,
/// so this one is a genuine regression oracle on each surface rather than
/// an A/B comparison — see the interpreter twin.
#[test]
fn e2e_free_fn_drops_a_stored_by_value_argument_once() {
    let src = r#"
struct Item { id: i64 }
impl Drop for Item {
    fn drop(mut ref self) { println(f"drop {self.id}") }
}
fn add_to(v: mut ref Vec[Item], x: Item) { v.push(x); }
fn main() {
    let mut v: Vec[Item] = Vec.new();
    add_to(mut v, Item { id: 7 });
    println("stored");
    while v.len() > 0 {
        match v.pop() { Some(e) => { println(f"pop {e.id}"); } None => {} }
    }
    println("end");
}
"#;
    let out = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run_program(src))
        .expect("failed to spawn sized worker")
        .join()
        .expect("compile worker panicked");
    assert_eq!(out.as_deref(), Some("stored\npop 7\ndrop 7\nend\n"));
}

/// The MONOMORPHIZED generic-impl leg — `PriorityQueue.push`, the shape the
/// row was filed on. Distinct from the concrete-method case above because
/// the callee is stdlib-baked: its AST is absent from the user program, so
/// resolving it needs the `generic_fns` fallback, and its receiver sits in
/// `params[0]` (the `make_generic_impl_method_function` desugaring) rather
/// than in `self_param`. Both details are load-bearing — with either one
/// wrong the gate silently answers "no escape" and this test double-drops.
///
/// Heap-free element on purpose: `PriorityQueue` of a struct carrying a
/// `String` still leaks that buffer (B-2026-08-26-18), which is a separate
/// defect and would make this fixture assert two things at once.
#[test]
fn e2e_priority_queue_drops_a_stored_by_value_argument_once() {
    let src = r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Item { id: i64 }
impl Drop for Item {
    fn drop(mut ref self) { println(f"drop {self.id}") }
}
fn main() {
    let mut q: PriorityQueue[Item] = PriorityQueue.new();
    q.push(Item { id: 3 });
    q.push(Item { id: 1 });
    println("stored");
    while q.len() > 0 {
        match q.pop() { Some(v) => { println(f"pop {v.id}"); } None => {} }
    }
    println("end");
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
        Some("stored\npop 1\ndrop 1\npop 3\ndrop 3\nend\n")
    );
}

/// The NEGATIVE side of the same gate, and the reason it is not simply
/// "suppress the caller's drop whenever the callee has a `mut ref` param".
/// A callee that merely READS its by-value parameter still hands the drop
/// back to the caller — nothing else will run it — so this must keep
/// printing exactly one `drop`. It is the fixture that fails if the escape
/// predicate is ever widened to match on the receiver's mode alone.
#[test]
fn e2e_by_value_argument_that_is_only_read_still_drops_in_the_caller() {
    let src = r#"
struct Item { id: i64 }
impl Drop for Item {
    fn drop(mut ref self) { println(f"drop {self.id}") }
}
struct Bag { n: i64 }
impl Bag {
    fn look(mut ref self, x: Item) { self.n = self.n + x.id; }
}
fn main() {
    let mut b = Bag { n: 0 };
    b.look(Item { id: 7 });
    println(b.n);
    println("end");
}
"#;
    let out = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run_program(src))
        .expect("failed to spawn sized worker")
        .join()
        .expect("compile worker panicked");
    assert_eq!(out.as_deref(), Some("drop 7\n7\nend\n"));
}

#[test]
fn test_e2e_local_moved_into_elem_slot_drops_once() {
    let Some(out) = run_program(
        r#"
struct Item { id: i64, tag: String }
impl Drop for Item { fn drop(mut ref self) { println(f"D{self.id}"); } }
struct Box { xs: Vec[Item] }
fn main() {
    let mut b = Box { xs: Vec.new() };
    b.xs.push(Item { id: 1i64, tag: "first_payload_long_enough_to_heap".to_string() });
    b.xs.push(Item { id: 2i64, tag: "second_payload_long_enough_to_heap".to_string() });
    println("built");
    b.xs.swap(0, 1);
    println(f"{b.xs[0].id}{b.xs[1].id}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "built\n21\nD2\nD1\n",
        "two live values must run exactly two Drop bodies; a third (`D1`) is \
             the moved-from source firing again at live-range end"
    );
}

/// B-2026-08-01-5 (chain leg) — owned-self passthrough chains no longer
/// double- or stale-fire: pre-fix `karac build` printed the receiver
/// body TWICE at scope exit for `mk(3).me().ident()`, re-fired r2 after
/// y's own death, and fired `mk(1).plus(10)`'s body over a STALE slot
/// (id 1 after the value had become 11). Each value now fires exactly
/// once, at its owner's death; chain-link receivers stay body-silent on
/// both backends (recorded residual). Twin of `tests/interpreter.rs`'s
/// `test_owned_self_chain_no_double_drop`.
#[test]
fn e2e_owned_self_chain_no_double_drop() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             impl Res {\n\
             \x20   fn plus(self, n: i64) -> Res {\n\
             \x20       return Res { id: self.id + n, name: self.name.clone() };\n\
             \x20   }\n\
             \x20   fn me(self) -> Res {\n\
             \x20       return self;\n\
             \x20   }\n\
             \x20   fn ident(ref self) -> i64 {\n\
             \x20       return self.id;\n\
             \x20   }\n\
             }\n\
             fn mk(n: i64) -> Res {\n\
             \x20   return Res { id: n, name: f\"r{n}\" };\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a: rebuild-chain\");\n\
             \x20   let x = mk(1).plus(10);\n\
             \x20   println(f\"x={x.id}\");\n\
             \x20   println(\"b: passthrough self\");\n\
             \x20   let y = mk(2).me();\n\
             \x20   println(f\"y={y.id}\");\n\
             \x20   println(\"c: chain then ref-method\");\n\
             \x20   let z = mk(3).me().ident();\n\
             \x20   println(f\"z={z}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a: rebuild-chain\nx=11\ndrop 11 r1\nb: passthrough self\ny=2\ndrop 2 r2\n\
             c: chain then ref-method\nz=3\nend\n"
    );
}

/// B-2026-09-14-15, element-shape half — a NESTED container element runs
/// the SAME `Drop` bodies its flat sibling does, whatever shape it is.
///
/// The nesting fix opened this: once codegen's array element walker
/// recursed, `Array[Array[X, N], M]` reached `emit_slot_drop_bodies_at` for
/// every `X` it has an arm for — a user enum, a tuple, an `Option` — while
/// the interpreter's nested walk still had arms for a struct and another
/// array ALONE, which turned four agreed silences into four divergences.
/// The walk gained the three missing arms, mirroring the FLAT element loop
/// in `run_array_element_user_drops` shape for shape.
///
/// That value-level walk cannot tell an `Array` from a `Vec` — both are one
/// `Value::Array` — so widening it necessarily widened the `Vec` nesting
/// too, and `emit_nested_vec_elem_bodies_fn` had to gain the one arm it was
/// missing (a user enum) in the same commit or `Vec[Vec[E]]` would have
/// become the new divergence. Two cells below were DIVERGENT before this
/// row for that reason and nothing to do with arrays — `Vec[Vec[(D, i64)]]`
/// and `Vec[Vec[Option[D]]]`, compiled-printing and interp-silent — and
/// they close here.
///
/// Each cell's FLAT sibling is asserted beside it: the flat spelling is
/// what defines the right answer, and a nested cell that disagrees with it
/// is the defect this fixture exists to catch.
#[test]
fn e2e_nested_container_element_shapes_run_their_drop_bodies() {
    const HDR: &str = "struct D { a: String, b: i64 }\n\
                           impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.b}\") } }\n\
                           fn pay() -> String { return \"heap\"; }\n\
                           fn mkd(n: i64) -> D { return D { a: pay(), b: n }; }\n\
                           enum E { A(i64), B }\n\
                           impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
                           enum E2 { P(D), Q }\n\
                           fn mke2(n: i64) -> E2 { return E2.P(mkd(n)); }\n";
    for (label, body, want) in [
            // An own-`Drop` enum element, flat then nested, in both containers.
            ("flat Array[E, 2]", "let a: Array[E, 2] = [E.A(1), E.B];", "dE\ndE\nmid\n"),
            ("flat Vec[E]", "let a: Vec[E] = [E.A(1), E.B];", "dE\ndE\nmid\n"),
            (
                "nested Array[Array[E, 2], 1]",
                "let a: Array[Array[E, 2], 1] = [[E.A(1), E.B]];",
                "dE\ndE\nmid\n",
            ),
            (
                "nested Vec[Vec[E]]",
                "let a: Vec[Vec[E]] = [[E.A(1)], [E.B]];",
                "dE\ndE\nmid\n",
            ),
            (
                "mixed Array[Vec[E], 2]",
                "let a: Array[Vec[E], 2] = [[E.A(1)], [E.B]];",
                "dE\ndE\nmid\n",
            ),
            // An enum with NO own `Drop` but a Drop-bearing payload.
            ("flat Array[E2, 2]", "let a: Array[E2, 2] = [mke2(1), E2.Q];", "dD1\nmid\n"),
            (
                "nested Array[Array[E2, 2], 1]",
                "let a: Array[Array[E2, 2], 1] = [[mke2(1), E2.Q]];",
                "dD1\nmid\n",
            ),
            (
                "nested Vec[Vec[E2]]",
                "let a: Vec[Vec[E2]] = [[mke2(1)], [E2.Q]];",
                "dD1\nmid\n",
            ),
            // A tuple element.
            (
                "nested Array[Array[(D, i64), 1], 2]",
                "let a: Array[Array[(D, i64), 1], 2] = [[(mkd(1), 7)], [(mkd(2), 8)]];",
                "dD1\ndD2\nmid\n",
            ),
            (
                // Divergent before this row, and not an array shape at all.
                "nested Vec[Vec[(D, i64)]]",
                "let a: Vec[Vec[(D, i64)]] = [[(mkd(1), 7)], [(mkd(2), 8)]];",
                "dD1\ndD2\nmid\n",
            ),
            // An Option element.
            (
                "nested Array[Array[Option[D], 1], 2]",
                "let a: Array[Array[Option[D], 1], 2] = [[Option.Some(mkd(1))], [Option.Some(mkd(2))]];",
                "dD1\ndD2\nmid\n",
            ),
            (
                // Divergent before this row, likewise.
                "nested Vec[Vec[Option[D]]]",
                "let a: Vec[Vec[Option[D]]] = [[Option.Some(mkd(1))], [Option.Some(mkd(2))]];",
                "dD1\ndD2\nmid\n",
            ),
            // A Vec element inside an array, which was already correct.
            (
                "control: Array[Array[Vec[D], 1], 2] was already correct",
                "let a: Array[Array[Vec[D], 1], 2] = [[[mkd(1)]], [[mkd(2)]]];",
                "dD1\ndD2\nmid\n",
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

#[test]
fn e2e_mixed_owndrop_literal_masks_only_the_view_body() {
    let hdr = "struct R { id: i64, name: String }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   fn mk(i: i64) -> R { return R { id: i, name: f\"heap-{i}\" }; }\n\
                   struct Sd3 { a: R, b: R }\n\
                   impl Drop for Sd3 { fn drop(mut ref self) { println(\"dSd3\") } }\n\
                   struct Sd { r: R }\n\
                   impl Drop for Sd { fn drop(mut ref self) { println(\"dSd\") } }\n\
                   struct Plain3 { a: R, b: R }\n\
                   struct Sd4 { a: R, b: R, c: R }\n\
                   impl Drop for Sd4 { fn drop(mut ref self) { println(\"dSd4\") } }\n\
                   struct Mix { a: R, n: i64 }\n\
                   impl Drop for Mix { fn drop(mut ref self) { println(\"dMix\") } }\n";
    for (label, fns, main, want) in [
            (
                "the row: view + fresh",
                "fn take(r: R) -> i64 { let s = Sd3 { a: r, b: mk(2) }; return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "dSd3\ndR2\ndR1\nv=7\n",
            ),
            (
                "order reversed: fresh + view",
                "fn take(r: R) -> i64 { let s = Sd3 { a: mk(2), b: r }; return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "dSd3\ndR2\ndR1\nv=7\n",
            ),
            (
                "boundary: ALL views (the shape already fixed)",
                "fn take(r: R) -> i64 { let s = Sd { r: r }; return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "dSd\ndR1\nv=7\n",
            ),
            (
                "boundary: ALL fresh — nothing masked",
                "fn take(r: R) -> i64 { let s = Sd3 { a: mk(1), b: mk(2) }; return 7; }",
                "let v = take(mk(9)); println(f\"v={v}\");",
                "dSd3\ndR2\ndR1\ndR9\nv=7\n",
            ),
            (
                "boundary: MIXED with NO own Drop — the per-field path, untouched",
                "fn take(r: R) -> i64 { let s = Plain3 { a: r, b: mk(2) }; return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "dR2\ndR1\nv=7\n",
            ),
            (
                "three fields, ONE view: both fresh bodies survive",
                "fn take(r: R) -> i64 { let s = Sd4 { a: r, b: mk(2), c: mk(3) }; return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "dSd4\ndR3\ndR2\ndR1\nv=7\n",
            ),
            (
                "three fields, TWO views: the fresh body survives",
                "fn take(p: R, q: R) -> i64 { let s = Sd4 { a: p, b: q, c: mk(3) }; return 7; }",
                "let v = take(mk(1), mk(2)); println(f\"v={v}\");",
                "dSd4\ndR3\ndR2\ndR1\nv=7\n",
            ),
            (
                "view beside a NON-Drop field",
                "fn take(r: R) -> i64 { let s = Mix { a: r, n: 5 }; return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "dMix\ndR1\nv=7\n",
            ),
            (
                "the masked binding is still readable",
                "fn take(r: R) -> i64 { let s = Sd3 { a: r, b: mk(2) }; println(f\"rd{s.b.id}\"); return 7; }",
                "let v = take(mk(1)); println(f\"v={v}\");",
                "rd2\ndSd3\ndR2\ndR1\nv=7\n",
            ),
            (
                "two mixed literals in one frame keep separate masks",
                "fn take(p: R, q: R) -> i64 { let s = Sd3 { a: p, b: mk(2) }; let t = Sd3 { a: q, b: mk(4) }; return 7; }",
                "let v = take(mk(1), mk(3)); println(f\"v={v}\");",
                "dSd3\ndR2\ndSd3\ndR4\ndR3\ndR1\nv=7\n",
            ),
        ] {
            let src = format!("{hdr}{fns}\nfn main() {{ {main} }}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
        }
}

/// B-2026-07-30-11 (tuple leg) — a tuple's ELEMENTS run their user
/// `impl Drop` bodies when the tuple binding dies.
///
/// The registration deliberately sits OUTSIDE the existing
/// `type_expr_has_drop_heap` gate on the tuple drop: that gate asks whether
/// an element owns HEAP, and `(Res, i64)` where `Res { id: i64 }` owns none
/// — yet its body still has to run. Pinning the heapless case is the point.
///
/// Also pins forward element order, the nested `(W, i64)` case where only
/// W's FIELD is Drop, and that an all-scalar tuple emits nothing.
///
/// LET-SITE ONLY: a tuple drop is registered from six-plus places, and the
/// others still run no body. That is a leak, not a divergence, and it stays
/// parity-safe because the interpreter reaches its tuple arm from
/// `push_drops_for_stmt`, which also fires only for `let` bindings.
///
/// Twinned with `tests/interpreter.rs`'s
/// `test_tuple_elements_run_user_drop_bodies`.
#[test]
fn e2e_tuple_elements_run_user_drop_bodies() {
    let Some(out) = run_program(
            "struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(self.id); } }\n\
             struct W { r: Res }\n\
             fn main() {\n\
             \x20   { let t: (Res, i64) = (Res { id: 21 }, 7); println(t.1); }\n\
             \x20   { let u: (i64, Res, Res) = (1, Res { id: 22 }, Res { id: 23 }); println(u.0); }\n\
             \x20   { let w: (W, i64) = (W { r: Res { id: 24 } }, 0); println(w.1); }\n\
             \x20   { let p: (i64, i64) = (1, 2); println(p.0); }\n\
             \x20   println(999);\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(out, "7\n21\n1\n22\n23\n0\n24\n1\n999\n");
}

/// B-2026-07-30-12 — a by-value owned-struct arg that the callee RETURNS
/// runs its `Drop` body exactly ONCE, and its buffer is freed exactly once.
///
/// `pass(g: Guard) -> Guard { g }` entry-copies the param and returns an
/// INDEPENDENT copy, so the caller's original buffer is orphaned.
/// B-2026-07-08-6 recovered that buffer by registering the caller temp's
/// drop even on the passthrough path — but it registered the FULL
/// `karac_drop_<T>` wrapper, so the user body ran on the caller's temp AND
/// on the consumer of the result: one drop under the interpreter, two under
/// AOT/JIT. A run/build parity break, shipped, and invisible to the suite.
///
/// The split this pins: on the passthrough path the caller registers the
/// MEMORY drop only. Memory follows the buffer, bodies follow the value.
///
/// Both arg shapes, because the predicate that routes them here matched only
/// the first: a struct LITERAL and a fn CALL. The literal double-fired; the
/// call fell through the override entirely and leaked instead.
///
/// `G` MUST carry a heap field. `arg_is_entry_copied_heap_struct` requires
/// one, so a scalar-only `G` never reaches the override at all and the whole
/// shape is already correct — a scalar version of this test passes on the
/// unfixed compiler and pins nothing.
///
/// Twinned with `tests/interpreter.rs`'s
/// `test_fnret_passthrough_arg_drop_fires_once` — the interpreter was always
/// right here, so the twin is what pins codegen to it.
#[test]
fn e2e_fnret_passthrough_arg_drop_fires_once() {
    let Some(out) = run_program(
        "struct G { name: String, id: i64 }\n\
             impl Drop for G { fn drop(mut ref self) { println(f\"dG{self.id}\"); } }\n\
             fn pass(g: G) -> G { g }\n\
             fn mk(i: i64) -> G { G { name: \"a padded payload string here\", id: i } }\n\
             fn main() {\n\
             \x20   let p = pass(G { name: \"a padded payload string here\", id: 1 });\n\
             \x20   println(f\"{p.id}\");\n\
             \x20   let q = pass(mk(2));\n\
             \x20   println(f\"{q.id}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    // Pre-fix AOT appended a second `dG1` after `end` — the caller temp's
    // body firing on top of `p`'s. The interpreter printed this string.
    assert_eq!(out, "1\ndG1\n2\ndG2\nend\n");
}

#[test]
fn e2e_with_provider_early_return_drops_body_local_before_pop() {
    // A heap-owning body local (f-string String) plus an early return:
    // the return edge drains the body's own frame (freeing the String)
    // BEFORE the ProviderPop frame — the interpreter's order. Also runs
    // the fall-through path of the same body for both-path coverage.
    if let Some(out) = run_program(
        "trait Counter { fn get(ref self) -> i64; }\n\
             effect resource Ctr: Counter;\n\
             struct InMem { n: i64 }\n\
             impl Counter for InMem { fn get(ref self) -> i64 { self.n } }\n\
             fn read() -> i64 with reads(Ctr) { Ctr.get() }\n\
             fn heapy(flag: bool) -> i64 with reads(Ctr) {\n\
                 with_provider[Ctr](InMem { n: 5 }, || {\n\
                     let s = f\"local-{read()}\";\n\
                     if flag { return s.len(); }\n\
                     read()\n\
                 })\n\
             }\n\
             fn main() with reads(Ctr) {\n\
                 println(f\"{heapy(true)}\");\n\
                 println(f\"{heapy(false)}\");\n\
             }",
    ) {
        assert_eq!(out, "7\n5\n");
    }
}

/// B-2026-09-04-13 — a `shared` / `par` holder runs its owned fields'
/// `Drop` bodies, in reverse declaration order, at the same point the
/// identical NON-shared struct does.
///
/// The gap was one keyword wide: `shared struct Sh { a: R, b: R }` with a
/// `Drop`-bearing `R` printed NEITHER body on any of the four surfaces,
/// while `struct Sh` printed both. Two independent causes, which is why
/// all four agreed and the A/B rule caught nothing —
/// `emit_shared_struct_rc_drop_fn` had no `SharedFieldKind` arm for a
/// plain struct field (it fell to `None`, a no-op), and the interpreter's
/// shared arm returned without the field walk every other path pairs with
/// the own-body call.
///
/// The `par` cell is not decoration: `par` is a separate flag over the
/// same Arc storage, and it lost its bodies identically.
///
/// PLACEMENT is asserted, not just presence. `nll_fireable_binding`
/// admitted only shared types with their OWN `impl Drop` to the NLL
/// channel, so a holder without one fired at scope exit — invisible while
/// its field bodies never ran, and a run/build divergence the moment they
/// did (`mid dR109 dR9` compiled against `dR109 dR9 mid` interpreted).
/// The bodies must land BEFORE `mid`, exactly as the plain control does.
#[test]
fn test_e2e_shared_holder_runs_its_field_drop_bodies() {
    const BODY: &str = "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}/{self.tag}\") } }\n\
             fn mk(n: i64) -> R { return R { id: n, tag: f\"t{n}\" }; }\n";
    // Reverse declaration order (design.md § Drop ordering), then `mid`.
    const EXPECT: &str = "dR109/t109\ndR9/t9\nmid\n";
    for kw in ["struct", "shared struct", "par struct"] {
        assert_eq!(
            run_program(&format!(
                "{BODY}{kw} Sh {{ a: R, b: R }}\n\
                     fn main() {{\n\
                         let h = Sh {{ a: mk(9), b: mk(109) }};\n\
                         println(\"mid\")\n\
                     }}"
            ))
            .as_deref(),
            Some(EXPECT),
            "holder spelled `{kw}` did not run its field Drop bodies",
        );
    }
}

/// B-2026-08-29-46, codegen leg — the caller's fresh ARGUMENT temporaries
/// run their `Drop` bodies in REVERSE argument order.
///
/// The twin of `test_owned_param_temps_drop_in_reverse_argument_order`, with
/// the SAME expected output for every case, which is the point of the pair:
/// the row was a run-vs-build divergence in which both backends ran each
/// body exactly once and only the ORDER differed, so no count-based
/// assertion and no A/B parity gate could see it — only an absolute
/// expectation on both sides can.
///
/// This side needed no change. Argument temps ride a caller cleanup frame
/// that drains LIFO, which is design.md § Drop ordering within a branch
/// rule 1 ("a single LIFO stack ordered by program-order of introduction")
/// falling out of the mechanism; the fix was to the interpreter's forward
/// walk. These cases exist so a future change to the frame — reordering the
/// drain, or moving argument cleanup to a different registration point —
/// cannot silently re-open the divergence from this end.
/// B-2026-08-30-51, codegen twin — the compiled backends were already
/// correct here, and this pins that so the interpreter's fix stays honest.
///
/// Every expectation is byte-identical to
/// `test_shadowed_binding_drops_its_own_value` in `tests/interpreter.rs`.
/// That is the point: the interpreter's slots are name-keyed and resolved
/// through the env at drain time, so shadowing collapsed two generations
/// onto one value — the shadowed body never ran and the survivor's ran
/// twice — while these backends key on the SLOT. Only an absolute
/// expectation on both sides holds the two together.
#[test]
fn e2e_shadowed_binding_drops_its_own_value() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   fn mk(n: i64) -> R { R { id: n } }\n";
    for (label, src, want) in [
        (
            "shadow-unread-first",
            format!(
                "{hdr}fn main() {{\n\
                     \x20   let t = mk(1);\n\
                     \x20   let t = mk(2);\n\
                     \x20   println(f\"t={{t.id}}\");\n\
                     }}"
            ),
            "dR1\nt=2\ndR2\n",
        ),
        (
            "shadow-first-read-before-shadow",
            format!(
                "{hdr}fn main() {{\n\
                     \x20   let u = mk(3);\n\
                     \x20   println(f\"u={{u.id}}\");\n\
                     \x20   let u = mk(4);\n\
                     \x20   println(f\"u={{u.id}}\");\n\
                     }}"
            ),
            "u=3\ndR3\nu=4\ndR4\n",
        ),
        (
            "shadow-three-deep",
            format!(
                "{hdr}fn main() {{\n\
                     \x20   let w = mk(5);\n\
                     \x20   let w = mk(6);\n\
                     \x20   let w = mk(7);\n\
                     \x20   println(f\"w={{w.id}}\");\n\
                     }}"
            ),
            "dR5\ndR6\nw=7\ndR7\n",
        ),
        (
            "shadow-neither-generation-read",
            format!(
                "{hdr}fn main() {{\n\
                     \x20   let b = mk(8);\n\
                     \x20   let b = mk(9);\n\
                     \x20   println(\"mid\");\n\
                     }}"
            ),
            // NO LONGER PINNED AT A DEFECT (B-2026-08-31-6, fixed), and
            // no longer different from the interpreter twin: both now
            // expect `dR9 dR8 mid`.
            //
            // With NEITHER generation read the endpoint comes from
            // `compute_block_last_use`'s never-read fallback. Codegen used
            // to read a variant pinning it to the FIRST `let`, so the first
            // generation fired before the shadowing `let` and the second,
            // whose slot did not exist at that index, drained at scope exit
            // — the `dR8 mid dR9` this used to assert. The shadow-aware
            // variant the interpreter uses puts both at the LAST `let`.
            //
            // Feeding that to codegen was tried, landed in B-2026-08-30-51
            // and was reverted in B-2026-08-31-5 because it REGRESSED
            // auto-par programs: an outlined region left the statements it
            // spanned with no firing point, so an endpoint moved INTO that
            // span never fired. That was the swallowing, and it is fixed —
            // a group covering a pending drop's endpoint is now declined
            // (`par_group_swallows_nll_drop`). With the hazard gone there
            // is one fallback and one entry point for both backends, and
            // the sequential column this harness compiles agrees with the
            // interpreter: measured `dR8 mid dR9` before, `dR9 dR8 mid`
            // after, against the interpreter's unchanged `dR9 dR8 mid`.
            //
            // WHAT IS STILL NOT PINNED HERE: this harness compiles
            // SEQUENTIALLY, so it cannot see the auto-par column at all.
            // For THIS three-statement program that column agrees anyway
            // (`dR9 dR8 mid` on all four surfaces, measured) — nothing here
            // forms a group over the two `let`s. Put a statement before
            // them and more after, so groups do form, and a residual
            // appears: `dR3 dR4 mid c=0` under auto-par against the
            // interpreter's and the sequential build's `dR4 dR3 mid c=0` —
            // right position, LIFO order reversed, because neither
            // generation is read, so neither is published as a return slot
            // and each fires inside its own branch at its own `let`
            // instead of both at the shared name-keyed endpoint. That
            // residual is its own row, and it needs `tests/par_codegen.rs`
            // to be seen.
            "dR8\ndR9\nmid\n",
        ),
        (
            "shadow-inside-nested-block",
            format!(
                "{hdr}fn main() {{\n\
                     \x20   let x = {{ let y = mk(10); let y = mk(11); y.id }};\n\
                     \x20   println(f\"x={{x}}\");\n\
                     }}"
            ),
            "dR11\ndR10\nx=11\n",
        ),
        (
            "shadow-move-rebind-is-one-object",
            format!(
                "{hdr}fn main() {{\n\
                     \x20   let z = mk(12);\n\
                     \x20   let z = z;\n\
                     \x20   println(f\"z={{z.id}}\");\n\
                     }}"
            ),
            "z=12\ndR12\n",
        ),
        (
            "shadow-struct-with-drop-field",
            format!(
                "{hdr}struct W {{ r: R, n: i64 }}\n\
                     fn main() {{\n\
                     \x20   let a = W {{ r: mk(13), n: 1 }};\n\
                     \x20   let a = W {{ r: mk(14), n: 2 }};\n\
                     \x20   println(f\"a={{a.n}}\");\n\
                     }}"
            ),
            "dR13\na=2\ndR14\n",
        ),
        (
            "shadow-in-a-loop-body",
            format!(
                "{hdr}fn main() {{\n\
                     \x20   for i in 0..2 {{\n\
                     \x20       let h = mk(20 + i);\n\
                     \x20       let h = mk(30 + i);\n\
                     \x20       println(f\"h={{h.id}}\");\n\
                     \x20   }}\n\
                     }}"
            ),
            "dR20\nh=30\ndR30\ndR21\nh=31\ndR31\n",
        ),
        (
            "shadow-block-tail-moves-the-shadowed-name",
            format!(
                "{hdr}fn main() {{\n\
                     \x20   let x = {{ let y = mk(16); let y = mk(17); y }};\n\
                     \x20   println(f\"x={{x.id}}\");\n\
                     }}"
            ),
            // B-2026-08-30-57. The tail hands `y` out, and the retraction
            // that models the move used to match by NAME, so BOTH
            // generations lost their action and `mk(16)`'s body never ran.
            // Only the generation being handed out moves.
            "dR16\nx=17\ndR17\n",
        ),
        (
            "shadow-fn-tail-returns-the-shadowed-name",
            format!(
                "{hdr}fn f() -> R {{ let s = mk(18); let s = mk(19); s }}\n\
                     fn main() {{\n\
                     \x20   let q = f();\n\
                     \x20   println(f\"q={{q.id}}\");\n\
                     }}"
            ),
            // The same retraction reached through `suppress_user_drop_for_var`
            // rather than the block-tail site, which is what shows the
            // subject is the by-name matching and not the block.
            "dR18\nq=19\ndR19\n",
        ),
        (
            "shadow-block-tail-field-read-is-not-a-move",
            format!(
                "{hdr}fn main() {{\n\
                     \x20   let z = {{ let w = mk(22); let w = mk(23); w.id }};\n\
                     \x20   println(f\"z={{z}}\");\n\
                     }}"
            ),
            // CONTROL: a field read hands out no binding, so no retraction
            // runs and both generations always died correctly. Its passing
            // is what localized the defect to the move path.
            "dR23\ndR22\nz=23\n",
        ),
        (
            "shadow-a-param",
            format!(
                "{hdr}fn f(r: R) -> i64 {{ let r = mk(99); r.id }}\n\
                     fn main() {{\n\
                     \x20   let d = f(mk(15));\n\
                     \x20   println(f\"d={{d}}\");\n\
                     }}"
            ),
            "dR99\ndR15\nd=99\n",
        ),
    ] {
        assert_eq!(run_program(&src).as_deref(), Some(want), "case {label}");
    }
}

#[test]
fn e2e_owned_param_temps_drop_in_reverse_argument_order() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    for (label, src, want) in [
            (
                "two-fresh-temps",
                format!(
                    "{hdr}fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                     fn main() {{ let v = take(R {{ id: 1 }}, R {{ id: 2 }}); println(f\"v={{v}}\"); }}"
                ),
                "dR2\ndR1\nv=7\n",
            ),
            (
                "three-fresh-temps",
                format!(
                    "{hdr}fn take3(a: R, b: R, c: R) -> i64 {{ 7 }}\n\
                     fn main() {{\n\
                     \x20   let v = take3(R {{ id: 1 }}, R {{ id: 2 }}, R {{ id: 3 }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "dR3\ndR2\ndR1\nv=7\n",
            ),
            (
                "callee-local-pops-before-params",
                format!(
                    "{hdr}fn take(r: R, q: R) -> i64 {{ let z = R {{ id: 3 }}; 7 }}\n\
                     fn main() {{ let v = take(R {{ id: 1 }}, R {{ id: 2 }}); println(f\"v={{v}}\"); }}"
                ),
                "dR3\ndR2\ndR1\nv=7\n",
            ),
            (
                "call-result-args",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                     fn main() {{ let v = take(mk(1), mk(2)); println(f\"v={{v}}\"); }}"
                ),
                "dR2\ndR1\nv=7\n",
            ),
            (
                "two-calls-in-sequence",
                format!(
                    "{hdr}fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                     fn main() {{\n\
                     \x20   let v = take(R {{ id: 1 }}, R {{ id: 2 }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     \x20   let w = take(R {{ id: 3 }}, R {{ id: 4 }});\n\
                     \x20   println(f\"w={{w}}\");\n\
                     }}"
                ),
                "dR2\ndR1\nv=7\ndR4\ndR3\nw=7\n",
            ),
            (
                "guard-method-two-temps",
                format!(
                    "{hdr}struct H {{ n: i64 }}\n\
                     impl H {{ fn take(ref self, r: R, q: R) -> i64 {{ 7 }} }}\n\
                     fn main() {{\n\
                     \x20   let h = H {{ n: 0 }};\n\
                     \x20   let v = h.take(R {{ id: 1 }}, R {{ id: 2 }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "dR2\ndR1\nv=7\n",
            ),
            (
                "guard-method-three-temps",
                format!(
                    "{hdr}struct H {{ n: i64 }}\n\
                     impl H {{ fn t3(ref self, a: R, b: R, c: R) -> i64 {{ 7 }} }}\n\
                     fn main() {{\n\
                     \x20   let h = H {{ n: 0 }};\n\
                     \x20   let v = h.t3(R {{ id: 1 }}, R {{ id: 2 }}, R {{ id: 3 }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "dR3\ndR2\ndR1\nv=7\n",
            ),
            (
                "guard-moved-locals",
                format!(
                    "{hdr}fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                     fn main() {{\n\
                     \x20   let a = R {{ id: 1 }};\n\
                     \x20   let b = R {{ id: 2 }};\n\
                     \x20   let v = take(a, b);\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "dR2\ndR1\nv=7\n",
            ),
            // The rule is program-order of introduction, NOT argument position,
            // and these two are where the difference shows: a named binding is
            // introduced at its `let`, a temp during argument evaluation. So
            // `guard-mixed-temp-first` runs FORWARD, and a backend that reversed
            // by argument position instead would break it.
            (
                "guard-mixed-local-first",
                format!(
                    "{hdr}fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                     fn main() {{\n\
                     \x20   let a = R {{ id: 1 }};\n\
                     \x20   let v = take(a, R {{ id: 2 }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "dR2\ndR1\nv=7\n",
            ),
            // B-2026-08-29-54, formerly pinned here at the defect: a STATIC
            // (associated) function's fresh-temp arguments ran NO `Drop` body on
            // this backend — not misordered, missing — because the assoc-call
            // arm registered no caller-side owner at all. Now ordered exactly
            // like the free-function twin above it, which is the point of
            // keeping it in THIS test.
            (
                "static-two-fresh-temps",
                format!(
                    "{hdr}struct H {{ n: i64 }}\n\
                     impl H {{ fn s2(a: R, b: R) -> i64 {{ 7 }} }}\n\
                     fn main() {{ let v = H.s2(R {{ id: 1 }}, R {{ id: 2 }}); println(f\"v={{v}}\"); }}"
                ),
                "dR2\ndR1\nv=7\n",
            ),
            (
                "guard-mixed-temp-first",
                format!(
                    "{hdr}fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                     fn main() {{\n\
                     \x20   let b = R {{ id: 2 }};\n\
                     \x20   let v = take(R {{ id: 1 }}, b);\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "dR1\ndR2\nv=7\n",
            ),
        ] {
            assert_eq!(run_program(&src).as_deref(), Some(want), "case {label}");
        }
    // TIMING rather than order (B-2026-08-29-55, formerly pinned here at the
    // defect as `pin-arg-temps-held-to-statement-end`). design.md's
    // temporary-lifetime table ends an argument temporary's live range
    // "After the call returns", so with two calls in one expression the
    // FIRST call's temps must die before the SECOND call's are built. This
    // backend held all four to the statement's `;` — the direction the
    // section's composition-with-NLL paragraph forbids ("NLL never EXTENDS
    // a temporary's live range past the position-specific end") — so the
    // first call's guard was still checked out while the second ran.
    //
    // The expectations below are the SAME strings the interpreter twin
    // asserts, which is the whole point: every body ran exactly once on
    // both backends before the fix and only the point differed, so no
    // count-based assertion and no A/B parity gate could see this. Only an
    // absolute expectation on both sides holds it.
    //
    // The three call spellings are here together because the position
    // table's argument row does not distinguish them and the fix hooks each
    // one separately (`compile_call`, `compile_method_call`, and the assoc
    // path `compile_call` reaches). `deep-nest` is the shape that pins the
    // WINDOW rather than the drain: an argument that is itself a call must
    // drain inside the outer call's window, interleaved, not batched with
    // it.
    for (label, src, want) in [
            (
                "two-calls-one-statement-free",
                format!(
                    "{hdr}fn take(r: R, q: R) -> i64 {{ println(\"in-take\"); 7 }}\n\
                     fn main() {{\n\
                     \x20   let v = take(R {{ id: 1 }}, R {{ id: 2 }}) + take(R {{ id: 3 }}, R {{ id: 4 }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "in-take\ndR2\ndR1\nin-take\ndR4\ndR3\nv=14\n",
            ),
            (
                "two-calls-one-statement-method",
                format!(
                    "{hdr}struct H {{ n: i64 }}\n\
                     impl H {{ fn take(ref self, r: R, q: R) -> i64 {{ println(\"in-m\"); 7 }} }}\n\
                     fn main() {{\n\
                     \x20   let h = H {{ n: 0 }};\n\
                     \x20   let v = h.take(R {{ id: 1 }}, R {{ id: 2 }}) + h.take(R {{ id: 3 }}, R {{ id: 4 }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "in-m\ndR2\ndR1\nin-m\ndR4\ndR3\nv=14\n",
            ),
            (
                "two-calls-one-statement-assoc",
                format!(
                    "{hdr}struct H {{ n: i64 }}\n\
                     impl H {{ fn s2(r: R, q: R) -> i64 {{ println(\"in-s\"); 7 }} }}\n\
                     fn main() {{\n\
                     \x20   let v = H.s2(R {{ id: 1 }}, R {{ id: 2 }}) + H.s2(R {{ id: 3 }}, R {{ id: 4 }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "in-s\ndR2\ndR1\nin-s\ndR4\ndR3\nv=14\n",
            ),
            (
                "mixed-wrapper-arg-fresh-branch-taken",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                     fn main() {{\n\
                     \x20   let k = mk(30);\n\
                     \x20   let v = one(if false {{ k }} else {{ mk(31) }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                // TWO objects die here and both must run: the argument temp the
                // taken (minting) arm produced, and `k`, which the untaken arm
                // never handed over so it dies in place. B-2026-08-30-38 ran
                // only `k`'s.
                "in-one\ndR31\ndR30\nv=31\n",
            ),
            (
                "mixed-wrapper-arg-binding-branch-taken",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                     fn main() {{\n\
                     \x20   let j = mk(40);\n\
                     \x20   let v = one(if true {{ j }} else {{ mk(41) }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                // ONE object, ONE body. The conditional-move flag disarms `j`
                // on this path, so the argument temp is the single owner —
                // this is the case that would double-fire if the binding kept
                // its own drop, and it is why the seed must reach here.
                "in-one\ndR40\nv=40\n",
            ),
            (
                "all-places-wrapper-arg-is-not-seeded",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                     fn main() {{\n\
                     \x20   let a1 = mk(1);\n\
                     \x20   let a2 = mk(2);\n\
                     \x20   let v = one(if true {{ a1 }} else {{ a2 }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                // No tail MINTS, so nothing can name a type for an argument
                // temp — and the seed is withheld for exactly that reason.
                // Seeding it anyway disarmed `a1` with no registration to
                // replace it and its body vanished on all three compiled
                // surfaces; both bindings stay armed and die in place instead.
                "in-one\ndR2\ndR1\nv=1\n",
            ),
            (
                "mixed-wrapper-arg-in-statement-position",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                     fn main() {{\n\
                     \x20   let b = mk(10);\n\
                     \x20   one(if false {{ b }} else {{ mk(11) }});\n\
                     \x20   println(\"after\");\n\
                     }}"
                ),
                // A DISCARDED call still consumes its arguments, so its
                // wrapper arguments are seeded like a `let`'s. A discarded
                // `if` is deliberately not — its arm tails have no consumer.
                "in-one\ndR11\ndR10\nafter\n",
            ),
            (
                "mixed-wrapper-arg-method-call",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     struct H {{ n: i64 }}\n\
                     impl H {{ fn take(mut ref self, r: R) -> i64 {{ println(\"in-take\"); r.id }} }}\n\
                     fn main() {{\n\
                     \x20   let c = mk(20);\n\
                     \x20   let mut h = H {{ n: 0 }};\n\
                     \x20   let v = h.take(if false {{ c }} else {{ mk(21) }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "in-take\ndR21\ndR20\nv=21\n",
            ),
            (
                "mixed-wrapper-arg-passthrough-callee",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     fn pass(r: R) -> R {{ r }}\n\
                     fn main() {{\n\
                     \x20   let d = mk(30);\n\
                     \x20   let v = pass(if false {{ d }} else {{ mk(31) }});\n\
                     \x20   println(f\"v={{v.id}}\");\n\
                     }}"
                ),
                // The passthrough guard still holds through the widening: the
                // minting arm's value travels out of the call, so the RESULT's
                // owner runs its body at its own last use, after the read.
                "dR30\nv=31\ndR31\n",
            ),
            (
                "mixed-wrapper-arg-match-arm-binding",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                     fn main() {{\n\
                     \x20   let g = mk(50);\n\
                     \x20   let v = one(match 2 {{ 1 => g, _ => mk(51) }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "in-one\ndR51\ndR50\nv=51\n",
            ),
            (
                "wrapper-arg-struct-literal-tail",
                format!(
                    "{hdr}fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                     fn main() {{\n\
                     \x20   let v = one(if true {{ R {{ id: 1 }} }} else {{ R {{ id: 2 }} }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "in-one\ndR1\nv=1\n",
            ),
            (
                "wrapper-arg-method-receiver",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     struct H {{ n: i64 }}\n\
                     impl H {{ fn take(mut ref self, r: R) -> i64 {{ println(\"in-take\"); r.id }} }}\n\
                     fn main() {{\n\
                     \x20   let mut h = H {{ n: 0 }};\n\
                     \x20   let v = h.take(if true {{ mk(10) }} else {{ mk(11) }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "in-take\ndR10\nv=10\n",
            ),
            (
                "wrapper-arg-two-args-reverse-order",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     fn two(a: R, b: R) -> i64 {{ println(\"in-two\"); a.id + b.id }}\n\
                     fn main() {{\n\
                     \x20   let v = two(if true {{ mk(1) }} else {{ mk(2) }}, if true {{ mk(3) }} else {{ mk(4) }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                // Both temps expire when the call returns, so program order of
                // introduction decides: they pop right to left (B-2026-08-29-46).
                "in-two\ndR3\ndR1\nv=4\n",
            ),
            (
                "wrapper-arg-enum-payload-tail",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     enum E {{ V(R), U }}\n\
                     fn takee(e: E) -> i64 {{ println(\"in-takee\"); match e {{ E.V(r) => r.id, E.U => 0 }} }}\n\
                     fn main() {{\n\
                     \x20   let v = takee(if true {{ E.V(mk(30)) }} else {{ E.U }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "in-takee\ndR30\nv=30\n",
            ),
            (
                "wrapper-arg-drop-bearing-field",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     struct W {{ r: R, n: i64 }}\n\
                     fn takew(w: W) -> i64 {{ println(\"in-takew\"); w.n }}\n\
                     fn main() {{\n\
                     \x20   let v = takew(if true {{ W {{ r: mk(50), n: 5 }} }} else {{ W {{ r: mk(51), n: 6 }} }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "in-takew\ndR50\nv=5\n",
            ),
            (
                "wrapper-arg-shared-tail-stays-with-rc",
                format!(
                    "{hdr}shared struct S {{ id: i64 }}\n\
                     fn takes(s: S) -> i64 {{ println(\"in-takes\"); s.id }}\n\
                     fn main() {{\n\
                     \x20   let v = takes(if true {{ S {{ id: 40 }} }} else {{ S {{ id: 41 }} }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                // A `shared` tail is refcounted; the rc machinery owns its
                // release, so the argument walk must NOT claim it.
                "in-takes\nv=40\n",
            ),
            (
                "wrapper-arg-block-local-shadowed-binding",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                     fn main() {{\n\
                     \x20   let v = one({{ let t = mk(90); let t = mk(91); t }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                // The LAST binding produces the tail; taking the first would
                // classify on an RHS that no longer yields the handed-out value.
                //
                // `mk(90)`'s body was LOST here on all three compiled surfaces
                // until B-2026-08-30-57: the block-tail move retracted the
                // handed-out binding BY NAME, which matched every generation of
                // a shadowed one. The two backends agree again, so this pin and
                // its interpreter twin are byte-identical once more.
                "dR90\nin-one\ndR91\nv=91\n",
            ),
            (
                "wrapper-arg-passthrough-callee-still-defers",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     fn pass(r: R) -> R {{ r }}\n\
                     fn main() {{\n\
                     \x20   let v = pass(if true {{ mk(60) }} else {{ mk(61) }});\n\
                     \x20   println(f\"v={{v.id}}\");\n\
                     }}"
                ),
                // The wrapper redirect must not defeat the passthrough guard:
                // the value travels out of the call, so the RESULT's owner runs
                // the body, once, at its own last use.
                "v=60\ndR60\n",
            ),
            (
                "deep-nest-inner-call-drains-first",
                format!(
                    "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                     fn one(r: R) -> i64 {{ println(\"in-one\"); 1 }}\n\
                     fn main() {{\n\
                     \x20   let v = one(mk(one(mk(1)) + 1));\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "in-one\ndR1\nin-one\ndR2\nv=1\n",
            ),
        ] {
            assert_eq!(run_program(&src).as_deref(), Some(want), "case {label}");
        }
    // B-2026-08-30-38, FIXED — this block pinned the DEFECT when it was
    // filed and now pins the repair. An argument that is a CONTROL-FLOW or
    // BLOCK expression rather than a direct call/literal used to lose its
    // `Drop` body ENTIRELY: the value arrives through the construct's merge
    // block, no argument registrar claimed it, and no frame owned the body
    // at all. Worse than a misordering and better hidden — unanimous on all
    // four surfaces, so no A/B parity gate could see it, and valgrind
    // clean, so the memory channel fired and only the user body went
    // missing.
    //
    // The registrar now redirects a wrapper to a representative tail
    // (`fresh_owned_branch_tail_repr`), so all five lines run one body.
    // `one(mk(6))` and the let-bound `one(e0)` stay as CONTROLS: they
    // worked before the fix, and their spellings are exactly what made a
    // spot-check of this bug come back clean.
    let pin_src = format!(
        "{hdr}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
             fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
             enum P {{ A, B }}\n\
             fn main() {{\n\
             \x20   let a = one(if true {{ mk(1) }} else {{ mk(2) }});\n\
             \x20   println(f\"a={{a}}\");\n\
             \x20   let p = P.A;\n\
             \x20   let b = one(match p {{ P.A => mk(3), P.B => mk(4) }});\n\
             \x20   println(f\"b={{b}}\");\n\
             \x20   let c = one({{ let t = mk(5); t }});\n\
             \x20   println(f\"c={{c}}\");\n\
             \x20   let d = one(mk(6));\n\
             \x20   println(f\"d={{d}}\");\n\
             \x20   let e0 = if true {{ mk(7) }} else {{ mk(8) }};\n\
             \x20   let e = one(e0);\n\
             \x20   println(f\"e={{e}}\");\n\
             }}"
    );
    assert_eq!(
        run_program(&pin_src).as_deref(),
        // Every line runs its body exactly once, at the call's return.
        Some(
            "in-one\ndR1\na=1\nin-one\ndR3\nb=3\nin-one\ndR5\nc=5\n\
                 in-one\ndR6\nd=6\nin-one\ndR7\ne=7\n"
        ),
        "case control-flow-argument-runs-its-drop-body",
    );
}

// ── Prereq.2 user-`impl Drop` dispatch — drop-glue wrapper emission ──
//
// The wrapper `karac_drop_<Type>` is synthesised for every user type
// with a validated `impl Drop`. Its body calls the user-defined
// `Type.drop` method body and, when the type has heap-owning fields,
// hands off to the existing per-struct field-cleanup synthesizer
// (`__karac_drop_struct_<Type>`). Scope-exit invocation of these
// wrappers lands in Prereq.3.

#[test]
fn test_ir_user_drop_wrapper_emitted() {
    let ir = ir_for(
        r#"
struct Foo { x: i64 }
impl Drop for Foo {
    fn drop(mut ref self) {}
}
fn main() {
    let f = Foo { x: 1 };
}
"#,
    );
    assert!(
        ir.contains("@karac_drop_Foo"),
        "expected synthesized drop-wrapper `karac_drop_Foo` in IR; \
             not found in:\n{}",
        ir
    );
    // The user-defined body symbol should be present too — sanity-check
    // that the existing impl-method codegen still emits it. LLVM
    // doesn't quote `Type.method` names (the `.` is accepted bare),
    // so the symbol appears as `@Foo.drop`, not `@"Foo.drop"`.
    assert!(
        ir.contains("@Foo.drop"),
        "expected user-defined `Foo.drop` method symbol in IR; \
             not found in:\n{}",
        ir
    );
}

#[test]
fn test_ir_user_drop_wrapper_calls_user_body() {
    let ir = ir_for(
        r#"
struct Foo { x: i64 }
impl Drop for Foo {
    fn drop(mut ref self) {}
}
fn main() {
    let f = Foo { x: 1 };
}
"#,
    );
    let body = function_body(&ir, "karac_drop_Foo").unwrap_or_else(|| {
        panic!("karac_drop_Foo body not found in IR:\n{}", ir);
    });
    assert!(
        body.contains("call void @Foo.drop("),
        "expected wrapper body to call `@Foo.drop(...)`; body was:\n{}",
        body
    );
}

#[test]
fn test_ir_user_drop_wrapper_composes_field_cleanup() {
    // `Bag` has a heap-owning Vec field, so
    // `emit_struct_drop_synthesis` returns `Some(...)` and the
    // wrapper composes its call after the user-body call.
    let ir = ir_for(
        r#"
struct Bag { items: Vec[i64] }
impl Drop for Bag {
    fn drop(mut ref self) {}
}
fn main() {
    let b = Bag { items: Vec.new() };
}
"#,
    );
    let body = function_body(&ir, "karac_drop_Bag").unwrap_or_else(|| {
        panic!("karac_drop_Bag body not found in IR:\n{}", ir);
    });
    assert!(
        body.contains("call void @Bag.drop("),
        "expected wrapper body to call `@Bag.drop(...)`; body was:\n{}",
        body
    );
    assert!(
        body.contains("call void @__karac_drop_struct_Bag"),
        "expected wrapper body to compose field cleanup via \
             `@__karac_drop_struct_Bag` (Bag has a heap-owning Vec field); \
             body was:\n{}",
        body
    );
}

// ── Prereq.3 user-`impl Drop` dispatch — scope-exit drop call placement ──
//
// When a struct binding has a user `impl Drop`, the let-binding
// registers `CleanupAction::UserDrop` (Prereq.3) instead of the
// existing `CleanupAction::StructDrop` (the existing field-cleanup
// path), so the scope-exit drain emits `call void @karac_drop_<Type>`
// — and exactly that one call, not also the field-cleanup synthesiser
// (the wrapper invokes the field-cleanup synthesiser internally; a
// second call from a stale `StructDrop` action would double-walk
// fields and trigger a double-free for heap-bearing fields).

#[test]
fn test_ir_scope_exit_emits_karac_drop_call() {
    let ir = ir_for(
        r#"
struct Foo { x: i64 }
impl Drop for Foo {
    fn drop(mut ref self) {}
}
fn main() {
    let f = Foo { x: 1 };
}
"#,
    );
    let main_body = function_body(&ir, "main").unwrap_or_else(|| {
        panic!("main body not found in IR:\n{}", ir);
    });
    assert!(
        main_body.contains("call void @karac_drop_Foo("),
        "expected `main` to call `@karac_drop_Foo(...)` at scope exit; \
             body was:\n{}",
        main_body
    );
}

#[test]
fn test_ir_user_drop_replaces_struct_field_cleanup_at_scope_exit() {
    // Bag has a heap-owning Vec field. Without user Drop the
    // scope-exit drain would emit `call @__karac_drop_struct_Bag`
    // directly. With user Drop, the drain emits
    // `call @karac_drop_Bag` ONCE; the wrapper itself dispatches
    // into `__karac_drop_struct_Bag` for field cleanup. The two
    // registrations are mutually exclusive at let-binding time, so
    // main's body must NOT contain a direct `__karac_drop_struct_Bag`
    // call (only the wrapper does, inside its own body).
    let ir = ir_for(
        r#"
struct Bag { items: Vec[i64] }
impl Drop for Bag {
    fn drop(mut ref self) {}
}
fn main() {
    let b = Bag { items: Vec.new() };
}
"#,
    );
    let main_body = function_body(&ir, "main").unwrap_or_else(|| {
        panic!("main body not found in IR:\n{}", ir);
    });
    assert!(
        main_body.contains("call void @karac_drop_Bag("),
        "expected `main` to call `@karac_drop_Bag(...)` at scope exit; \
             body was:\n{}",
        main_body
    );
    assert!(
        !main_body.contains("call void @__karac_drop_struct_Bag("),
        "main must NOT also emit a direct `@__karac_drop_struct_Bag` \
             call — the wrapper handles field cleanup internally; a \
             second call would double-walk the Vec field. body was:\n{}",
        main_body
    );
}

#[test]
fn test_ir_struct_drop_still_fires_without_impl_drop() {
    // Sibling assertion of the above: without user Drop, the
    // existing field-cleanup path stands. Bag's let-binding
    // registers `CleanupAction::StructDrop` whose drain emits
    // `call @__karac_drop_struct_Bag` directly in `main`. This
    // pins down that Prereq.3's tracking change is gated on
    // `drop_method_keys` presence — no regression in the
    // no-impl-Drop case.
    let ir = ir_for(
        r#"
struct Bag { items: Vec[i64] }
fn main() {
    let b = Bag { items: Vec.new() };
}
"#,
    );
    let main_body = function_body(&ir, "main").unwrap_or_else(|| {
        panic!("main body not found in IR:\n{}", ir);
    });
    assert!(
        main_body.contains("call void @__karac_drop_struct_Bag("),
        "expected `main` to call `@__karac_drop_struct_Bag(...)` at \
             scope exit (no user Drop → existing field-cleanup path stays \
             in effect); body was:\n{}",
        main_body
    );
    assert!(
        !main_body.contains("call void @karac_drop_Bag("),
        "main must NOT call `@karac_drop_Bag` when Bag has no \
             `impl Drop` — the wrapper isn't emitted in this case. \
             body was:\n{}",
        main_body
    );
}

// ── phase-7 L938 user-`impl Drop` dispatch for shared structs ──
//
// A `shared struct` with `impl Drop` routes through the RC path
// (`track_rc_var` → `emit_rc_dec`), NOT the value-type
// `CleanupAction::UserDrop` drain. Before L938 the user body never
// fired on a shared binding: a primitive-only shared struct synth'd
// no `__karac_rc_drop_<T>` at all (plain `free`), and the
// `karac_drop_<T>` value-wrapper that *does* carry the body is never
// called for an RC binding. The fix injects the user body at the top
// of the synthesized RC drop fn (which runs only at refcount→0).

#[test]
fn test_ir_shared_struct_user_drop_synth_fn_calls_body() {
    // Primitive-only shared struct: pre-L938 this returned `None`
    // from `emit_shared_struct_rc_drop_fn` (no walkable fields) and
    // `emit_rc_dec` fell back to plain `free`. With a user Drop a
    // synth fn must now exist and call `@Res.drop` before the free.
    let ir = ir_for(
        r#"
shared struct Res { id: i64 }
impl Drop for Res {
    fn drop(mut ref self) {}
}
fn main() {
    let r = Res { id: 7 };
}
"#,
    );
    let body = function_body(&ir, "__karac_rc_drop_Res").unwrap_or_else(|| {
        panic!(
            "expected a synthesized `__karac_rc_drop_Res` (user Drop forces \
                 a drop fn even for a primitive-only shared struct); IR:\n{}",
            ir
        );
    });
    assert!(
        body.contains("call void @Res.drop("),
        "RC drop fn must call the user `@Res.drop(...)` body at refcount→0; \
             body was:\n{}",
        body
    );
    assert!(
        body.contains("@free(") || body.contains("call void @free"),
        "RC drop fn must still free the heap allocation after the user \
             body; body was:\n{}",
        body
    );
}

#[test]
fn test_e2e_shared_struct_user_drop_fires() {
    // Behavioral: the user Drop body runs when the sole reference dies.
    //
    // B-2026-08-09-3 retimed this from `0 7` to `7 0`. `r` is never read,
    // and a never-used binding dies at its own `let` — design.md § Drop
    // ordering, the rule the VALUE tier has always followed here (the
    // same program with a non-shared `struct Res` printed `7 0` before
    // this bug was fixed, and `test_ir_user_drops_nll_placement_never_
    // used_bindings` pins it at the IR level). The old `0 7` was the RC
    // tier's scope-exit drain, i.e. the divergence itself: `--interp`
    // prints `7 0` on both spellings.
    let out = run_program(
        r#"
shared struct Res { id: i64 }
impl Drop for Res {
    fn drop(mut ref self) {
        println(self.id);
    }
}
fn main() {
    let r = Res { id: 7 };
    println(0);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "7\n0",
            "expected the drop body printing `7` at `r`'s own let (never \
                 used), then `0`"
        );
    }
}

#[test]
fn test_e2e_shared_struct_user_drop_recursive_chain_each_node_fires() {
    // Recursive linked-list shape: the iterative self-chain fast
    // path is disabled when a user Drop exists, so each link's
    // refcount→0 dispatches back through `__karac_rc_drop_Node` and
    // fires the body. All three node ids must appear.
    let out = run_program(
        r#"
shared struct Node { val: i64, mut next: Option[Node] }
impl Drop for Node {
    fn drop(mut ref self) {
        println(self.val);
    }
}
fn main() {
    let c = Node { val: 3, next: None };
    let b = Node { val: 2, next: Some(c) };
    let a = Node { val: 1, next: Some(b) };
    println(0);
}
"#,
    );
    if let Some(out) = out {
        for id in ["1", "2", "3"] {
            assert!(
                out.lines().any(|l| l.trim() == id),
                "expected node id `{id}` from a per-link drop body; got:\n{out}"
            );
        }
    }
}

// ── Prereq.5 user-`impl Drop` dispatch — edge cases ──
//
// Drop ordering: when multiple bindings with user Drop coexist in
// the same scope, the cleanup-action stack drains LIFO at scope
// exit per design.md § Drop ordering within a branch. The
// `scope_cleanup_actions` `.iter().rev()` drain in
// `emit_scope_cleanup` implements this; this test pins the
// ordering at the IR level so a future refactor that breaks LIFO
// surfaces here.

#[test]
fn test_ir_user_drops_nll_placement_never_used_bindings() {
    // B-2026-07-21-1 (NLL user-drop placement): a NEVER-USED user-Drop
    // binding dies at its own declaration — "a value whose last use is
    // mid-scope is dropped at that use and does not appear in the
    // end-of-scope stack at all" (design.md § Drop ordering; the
    // interpreter has implemented this since the NLL sub-step). So two
    // never-used bindings `a` then `b` each fire at their own let —
    // @karac_drop_A appears BEFORE @karac_drop_B in the IR (declaration
    // order), NOT the scope-exit LIFO the pre-NLL codegen emitted.
    // Same-statement multi-drop LIFO and mid-scope timing are covered
    // by the E2E `test_e2e_user_drop_nll_timing_and_order`.
    let ir = ir_for(
        r#"
struct A { tag: i64 }
struct B { tag: i64 }
impl Drop for A {
    fn drop(mut ref self) {}
}
impl Drop for B {
    fn drop(mut ref self) {}
}
fn main() {
    let a = A { tag: 1 };
    let b = B { tag: 2 };
}
"#,
    );
    let main_body = function_body(&ir, "main").unwrap_or_else(|| {
        panic!("main body not found in IR:\n{}", ir);
    });
    let a_idx = main_body
        .find("call void @karac_drop_A(")
        .unwrap_or_else(|| {
            panic!(
                "expected @karac_drop_A call in main; body was:\n{}",
                main_body
            )
        });
    let b_idx = main_body
        .find("call void @karac_drop_B(")
        .unwrap_or_else(|| {
            panic!(
                "expected @karac_drop_B call in main; body was:\n{}",
                main_body
            )
        });
    // NLL: each never-used binding drops at its own declaration, so A
    // (declared first) fires first.
    assert!(
        a_idx < b_idx,
        "expected NLL declaration-point drop placement — `@karac_drop_A` \
             should appear before `@karac_drop_B` in main (each never-used \
             binding dies at its own let); A at {}, B at {}; body was:\n{}",
        a_idx,
        b_idx,
        main_body
    );
}

#[test]
fn test_e2e_user_drop_nll_timing_and_order() {
    // B-2026-07-21-1: user `impl Drop` bodies fire at each binding's
    // LIVE-RANGE END (NLL), in LIFO order for drops due at the same
    // statement — matching the interpreter and design.md § Drop
    // ordering. Covers: (1) mid-scope last use — the drop fires BEFORE
    // the scope's next statement; (2) two bindings last-used in the same
    // statement — LIFO (`c:B` before `c:A`), and before the trailing
    // statement; (3) a binding used up to scope end — drop at scope
    // exit. Byte-identical to the interpreter (the run-vs-build
    // divergence this bug filed).
    if let Some(out) = run_program(
        "struct Res { name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { print(f\"c:{self.name}|\"); }\n\
             }\n\
             fn main() {\n\
                 {\n\
                     let r = Res { name: \"mid\".to_string() };\n\
                     print(f\"use:{r.name}|\");\n\
                     print(\"after|\");\n\
                 }\n\
                 print(\"S1|\");\n\
                 let a = Res { name: \"A\".to_string() };\n\
                 let b = Res { name: \"B\".to_string() };\n\
                 print(f\"{a.name}{b.name}|\");\n\
                 print(\"tail|\");\n\
                 let z = Res { name: \"Z\".to_string() };\n\
                 print(f\"end:{z.name}\");\n\
             }",
    ) {
        assert_eq!(out, "use:mid|c:mid|after|S1|AB|c:B|c:A|tail|end:Zc:Z|");
    }
}

/// B-2026-08-09-3 — the RC tier's leg of the same rule. A `shared struct`
/// binding's Drop body used to run at the closing brace while the
/// interpreter (and codegen's own VALUE-struct path, above) ran it at
/// live-range end, so the two backends printed in different orders.
///
/// Three shapes, one program:
///
///   • `a` / `b` — two shared bindings last-used in the SAME statement.
///     Both bodies fire there, LIFO (`c:B` before `c:A`), ahead of the
///     trailing `tail|` — the filed shape.
///
///   • `p` / `q` — the ALIAS shape, and the reason this fix is a
///     retiming rather than a new lifetime rule. `let q = p;` makes two
///     live handles on one object; exactly ONE body runs, at `q`'s last
///     use, not at `p`'s. Firing `p`'s dec early takes the count 2→1,
///     which cannot run a body — the 0 transition still waits for the
///     last holder. If a future change ever frees at a dec instead of
///     decrementing, this leg turns `P|` into a read of freed memory
///     rather than merely reordering output.
///
///   • `z` — used in the final statement, so it stays at scope exit.
///
/// Asserted byte-identical to `--interp` (measured, not derived) at both
/// opt levels.
#[test]
fn test_e2e_shared_user_drop_nll_timing_and_alias() {
    if let Some(out) = run_program(
        "shared struct Res { name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { print(f\"c:{self.name}|\"); }\n\
             }\n\
             fn main() {\n\
                 let a = Res { name: \"A\".to_string() };\n\
                 let b = Res { name: \"B\".to_string() };\n\
                 print(f\"{a.name}{b.name}|\");\n\
                 print(\"tail|\");\n\
                 let p = Res { name: \"P\".to_string() };\n\
                 let q = p;\n\
                 print(f\"{p.name}|\");\n\
                 print(\"mid|\");\n\
                 print(f\"{q.name}|\");\n\
                 print(\"after|\");\n\
                 let z = Res { name: \"Z\".to_string() };\n\
                 print(f\"end:{z.name}\");\n\
             }",
    ) {
        assert_eq!(out, "AB|c:B|c:A|tail|P|mid|P|c:P|after|end:Zc:Z|");
    }
}

/// B-2026-08-09-3, the pin leg: a binding a `defer` body names stays live
/// to scope exit and must NOT be retimed. `compute_block_last_use` pins
/// such a name to the scope-exit sentinel, and the RC tier inherits that
/// unchanged by riding the same map — this test is what says so out loud,
/// since an RC dec moved ahead of a `defer` that reads the object would be
/// a use-after-free rather than a reordering.
#[test]
fn test_e2e_shared_user_drop_defer_pins_to_scope_exit() {
    if let Some(out) = run_program(
        "shared struct Res { name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { print(f\"c:{self.name}|\"); }\n\
             }\n\
             fn main() {\n\
                 let p = Res { name: \"P\".to_string() };\n\
                 defer { print(f\"d:{p.name}|\"); }\n\
                 print(f\"{p.name}|\");\n\
                 print(\"end|\");\n\
             }",
    ) {
        assert_eq!(out, "P|end|d:P|c:P|");
    }
}

#[test]
fn test_ir_tcp_listener_drop_body_calls_tcp_close() {
    let ir = ir_for(
        r#"
fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    println(l.fd);
}
"#,
    );
    let body = function_body(&ir, "TcpListener.drop").unwrap_or_else(|| {
        panic!(
            "@TcpListener.drop body not found in IR; expected hand-rolled \
                 body emitted before emit_user_drop_wrappers. IR:\n{}",
            ir
        )
    });
    assert!(
        body.contains("call i32 @karac_runtime_tcp_close("),
        "expected `@TcpListener.drop` body to call \
             `@karac_runtime_tcp_close(...)`; body was:\n{}",
        body
    );
}

#[test]
fn test_ir_tcp_stream_drop_body_calls_tcp_close() {
    let ir = ir_for(
        r#"
fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let s = l.accept().unwrap();
    println(s.fd);
}
"#,
    );
    let body = function_body(&ir, "TcpStream.drop").unwrap_or_else(|| {
        panic!(
            "@TcpStream.drop body not found in IR; expected hand-rolled \
                 body. IR:\n{}",
            ir
        )
    });
    assert!(
        body.contains("call i32 @karac_runtime_tcp_close("),
        "expected `@TcpStream.drop` body to call \
             `@karac_runtime_tcp_close(...)`; body was:\n{}",
        body
    );
}

#[test]
fn test_ir_main_invokes_tcp_drop_wrapper_at_scope_exit() {
    let ir = ir_for(
        r#"
fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    println(l.fd);
}
"#,
    );
    let main_body = function_body(&ir, "main").unwrap_or_else(|| {
        panic!("main body not found in IR:\n{}", ir);
    });
    // Prereq.3's scope-exit drain emits `call @karac_drop_TcpListener`
    // at scope exit; the wrapper internally calls @TcpListener.drop,
    // which closes the fd. This pins the full pipeline:
    // typechecker drop_method_keys → Prereq.2 wrapper synth →
    // Prereq.3 CleanupAction::UserDrop registration → drain emits
    // the call.
    assert!(
        main_body.contains("call void @karac_drop_TcpListener("),
        "expected `main` to call `@karac_drop_TcpListener(...)` at \
             scope exit (the wrapper closes the fd via @TcpListener.drop); \
             main body was:\n{}",
        main_body
    );
}

// ── Move-suppression for user-Drop bindings (let-rebind) ──
//
// `let g = f;` where `f` has a user `impl Drop` moves the value
// out of `f` into `g`. Without suppression both bindings would
// fire their UserDrop at scope exit, double-closing fds /
// double-calling user Drop bodies. The codegen `stmts.rs`
// let-binding tracking calls `suppress_user_drop_for_var` on the
// source name before tracking the destination, removing the
// source's CleanupAction::UserDrop. These tests pin the
// observable IR shape: exactly one `call @karac_drop_<Type>`
// remains in main.

#[test]
fn test_ir_let_rebind_suppresses_source_user_drop() {
    let ir = ir_for(
        r#"
struct Foo { x: i64 }
impl Drop for Foo {
    fn drop(mut ref self) {}
}
fn main() {
    let f = Foo { x: 1 };
    let g = f;
    println(g.x);
}
"#,
    );
    let main_body = function_body(&ir, "main").unwrap_or_else(|| {
        panic!("main body not found in IR:\n{}", ir);
    });
    let count = main_body.matches("call void @karac_drop_Foo(").count();
    assert_eq!(
        count, 1,
        "expected exactly ONE `@karac_drop_Foo` call in main \
             (source `f` should be move-suppressed; only destination \
             `g` drops at scope exit); got {} calls; body was:\n{}",
        count, main_body
    );
}

// ── Move-suppression for return-by-value of user-Drop bindings ──
//
// `fn make() -> T { let l = T::new(); l }` — `l`'s value moves
// out as the function's return value. Without suppression, the
// function-scope cleanup would fire `l`'s UserDrop (close the
// fd) before the return happens, leaving the caller with a stale
// fd. The fix in `suppress_cleanup_for_tail_return` removes `l`'s
// UserDrop from the scope-cleanup stack right before
// `emit_scope_cleanup` runs; the caller then closes the fd
// exactly once when its own binding goes out of scope.

#[test]
fn test_ir_return_by_value_suppresses_source_user_drop() {
    let ir = ir_for(
        r#"
struct Foo { x: i64 }
impl Drop for Foo {
    fn drop(mut ref self) {}
}
fn make() -> Foo {
    let f = Foo { x: 7 };
    f
}
fn main() {
    let r = make();
    println(r.x);
}
"#,
    );
    let make_body = function_body(&ir, "make").unwrap_or_else(|| {
        panic!("make body not found in IR:\n{}", ir);
    });
    assert!(
        !make_body.contains("call void @karac_drop_Foo("),
        "expected `make` to NOT contain `@karac_drop_Foo` — `f` is \
             moved out as the return value and its UserDrop should be \
             suppressed before scope-exit cleanup. body was:\n{}",
        make_body
    );
    // The caller still drops the value at its own scope exit.
    let main_body = function_body(&ir, "main").unwrap_or_else(|| {
        panic!("main body not found in IR:\n{}", ir);
    });
    assert!(
        main_body.contains("call void @karac_drop_Foo("),
        "expected `main` to drop `r` at scope exit (caller owns \
             the returned value); body was:\n{}",
        main_body
    );
}

#[test]
fn test_ir_explicit_return_suppresses_source_user_drop() {
    // `return f;` is an explicit return that takes the
    // Return-statement path in `compile_expr`. The companion
    // suppression in `src/codegen/exprs.rs`'s `ExprKind::Return`
    // arm pins this case.
    let ir = ir_for(
        r#"
struct Foo { x: i64 }
impl Drop for Foo {
    fn drop(mut ref self) {}
}
fn make() -> Foo {
    let f = Foo { x: 7 };
    return f;
}
fn main() {
    let r = make();
    println(r.x);
}
"#,
    );
    let make_body = function_body(&ir, "make").unwrap_or_else(|| {
        panic!("make body not found in IR:\n{}", ir);
    });
    assert!(
        !make_body.contains("call void @karac_drop_Foo("),
        "expected `make` to NOT contain `@karac_drop_Foo` — \
             explicit `return f` move-suppresses `f`. body was:\n{}",
        make_body
    );
}

// Note: production-shaped `fn make_listener() -> TcpListener {
// let l = TcpListener.bind(...); l }` is the natural Slice 9d
// motivating example, but is NOT currently testable end-to-end:
// TcpListener-as-a-function-return-type fails LLVM module
// verification with "Function return type does not match
// operand type of return inst!" because the codegen's stdlib
// struct-return ABI lowering doesn't yet wire the
// single-i32-field shape through a function's return slot.
// The user-struct case above (`fn make() -> Foo`) exercises the
// same move-suppression mechanism; once the stdlib return-ABI
// gap closes, the TcpListener case will compile and the move-
// suppression machinery already wired here will make it
// correct. Filed as a follow-on in phase-7-codegen.md's
// move-suppression entry.

#[test]
fn test_ir_no_wrapper_without_impl_drop() {
    // No `impl Drop` → no entry in `program.drop_method_keys` →
    // `emit_user_drop_wrappers` synthesizes nothing.
    let ir = ir_for(
        r#"
struct Foo { x: i64 }
fn main() {
    let f = Foo { x: 1 };
}
"#,
    );
    assert!(
        !ir.contains("@karac_drop_Foo"),
        "wrapper `karac_drop_Foo` must NOT be emitted when `Foo` has \
             no `impl Drop`; found in:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_user_method_ref_self_materializes_and_drops() {
    // Slice 3j: a user impl-block method (`ref self`) on a fresh-temp struct
    // receiver (`make_counter().total()`) where the struct owns a `Vec[String]`
    // field. The identifier-keyed user-impl dispatch resolves only
    // Identifier/self receivers, so a call-result receiver hard-errored ("no
    // handler for method ... on non-identifier receiver"). The fresh-temp path
    // recovers the struct type from the `Type.method` callee key, materializes
    // the receiver into a `__urecv_tmp` synth local, passes its address as the
    // `ref self` receiver, and — because `self` is borrowed — drop-tracks the
    // temp so its `Vec[String]` field is freed via `__karac_drop_struct_Counter`
    // at scope exit. Without the materialize the call fails to compile; without
    // the drop the field Vec + its Strings leak (Linux LSan).
    let src = r#"
struct Counter { items: Vec[String], base: i64 }
impl Counter {
    fn total(ref self) -> i64 { return self.base + self.items.len(); }
}
fn make_counter() -> Counter {
    let mut c = Counter { items: Vec.new(), base: 100_i64 };
    c.items.push("a heap field string padded beyond thirty-six bytes ok");
    return c;
}
fn main() {
    println(make_counter().total());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__urecv_tmp"),
        "expected the fresh-temp struct receiver materialized into __urecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("__karac_drop_struct_Counter"),
        "expected the borrowed (`ref self`) temp receiver drop-tracked via \
             __karac_drop_struct_Counter (frees the Vec[String] field); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_user_method_owned_self_materializes_and_drops() {
    // Slice 3j companion: an OWNED-`self` user method on a fresh-temp struct
    // receiver. The user-impl dispatch passes the receiver by shallow value
    // copy and emits NO receiver drop — an owned-`self` method does not drop
    // `self` (proven by LSan: without a caller-side drop the field `Vec` leaks
    // once per call). So the fresh-temp path drop-tracks the temp here too
    // (`__karac_drop_struct_Counter`), exactly as the borrowed-`self` case and
    // the `let`-binding path do — the caller's temp is the sole owner. The
    // receiver materializes into `__urecv_tmp`.
    let src = r#"
struct Counter { items: Vec[String], base: i64 }
impl Counter {
    fn consume(self) -> i64 { return self.base + self.items.len(); }
}
fn make_counter() -> Counter {
    let mut c = Counter { items: Vec.new(), base: 100_i64 };
    c.items.push("a heap field string padded beyond thirty-six bytes ok");
    return c;
}
fn main() {
    println(make_counter().consume());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__urecv_tmp"),
        "expected the fresh-temp struct receiver materialized into __urecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("__karac_drop_struct_Counter"),
        "expected the owned-self temp receiver drop-tracked via \
             __karac_drop_struct_Counter (the method does not drop self); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_shared_struct_method_materializes_and_rc_drops() {
    // Slice 3k: a user impl-block method on a fresh-temp SHARED-STRUCT receiver
    // (`make().count()`). The receiver's obj type is `Shared("Bag")`, whose
    // `method_callee_type_name` arm was missing — so `method_callee_types`
    // never recorded the `Bag.count` key and codegen's `dispatch_key` was
    // `None`, dropping the shared case through to the hard error even after the
    // struct/enum cases compiled. With the arm added, the fresh-temp path
    // recovers `Bag`, materializes the RC-pointer receiver into `__urecv_tmp`,
    // and drop-tracks it as one scope-exit `RcDec` (`track_rc_var`) — the
    // method borrows / shallow-copies `self`, net-zero on the count, so this
    // single dec frees the box via the recursive `__karac_rc_drop_Bag` (which
    // frees the `Vec[String]` field). Without the materialize the call fails to
    // compile; without the dec the whole box + field leaks (Linux LSan).
    let src = r#"
shared struct Bag { items: Vec[String] }
impl Bag {
    fn count(self) -> i64 { self.items.len() }
}
fn make() -> Bag {
    let mut v: Vec[String] = Vec.new();
    v.push("a field payload string padded beyond thirty-six bytes ok");
    Bag { items: v }
}
fn main() {
    println(make().count());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__urecv_tmp"),
        "expected the fresh-temp shared-struct receiver materialized into __urecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("__karac_rc_drop_Bag"),
        "expected the shared-struct temp receiver drop-tracked via a scope-exit \
             RcDec running __karac_rc_drop_Bag (frees the box + Vec[String] field); got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_by_value_aggregate_drops() {
    // B-2026-06-11-4: by-value aggregates leaked their heap fields across
    // several shapes the named-struct drop path didn't cover. This asserts
    // the VALUE is correct (the heap field survives to its use) across all
    // of them; the no-leak / single-free side is `tests/memory_sanitizer.rs
    // ::asan_by_value_aggregate_drops_single_free`. Shapes: a let-bound
    // tuple passed by value, a tuple-to-tuple move, a tuple returned from a
    // fn, a tuple forwarded through two params, a tuple literal arg, a
    // struct literal arg, and a nested-struct field.
    let out = run_program(
        r#"
struct S { k: i64, name: String }
struct Inner { name: String }
struct Outer { id: i64, inner: Inner }
fn show_tup(p: (i64, String)) { println(p.1); }
fn fwd(p: (i64, String)) { show_tup(p); }
fn show_s(s: S) { println(s.name); }
fn show_o(o: Outer) { println(o.inner.name); }
fn mk(n: i64) -> (i64, String) { (n, f"r-{n}") }
fn main() {
    let t = (1i64, f"let-{1}");
    show_tup(t);
    let u = (2i64, f"mv-{2}");
    let w = u;
    println(w.1);
    let r = mk(3i64);
    println(r.1);
    fwd((4i64, f"fwd-{4}"));
    show_tup((5i64, f"lit-{5}"));
    show_s(S { k: 6i64, name: f"slit-{6}" });
    let o = Outer { id: 7i64, inner: Inner { name: f"nest-{7}" } };
    show_o(o);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "let-1\nmv-2\nr-3\nfwd-4\nlit-5\nslit-6\nnest-7\n");
    }
}

#[test]
fn test_e2e_rc_fallback_aggregate_heap_field_drop() {
    // B-2026-06-10-8 value-correctness companion to the ASAN/leak
    // regressions (`tests/memory_sanitizer.rs::asan_rc_fallback_*`): an
    // RC-fallback-boxed tuple and struct, each with a `String` field
    // consumed in one branch then read after, round-trip their heap
    // content through the box. The leak/double-free is the ASAN/LSan
    // story; this guards the observable value across the boxing path
    // (run with the ownership result so `is_rc_fallback_binding` fires).
    let out = run_program_with_ownership(
        r#"
struct Named { id: i64, label: String }
fn sink_t(t: (i64, String)) { }
fn sink_s(n: Named) { }
fn main() {
    let cond: bool = false;
    let t = (1i64, f"tup-{1}");
    if cond { sink_t(t); }
    println(t.1);
    let s = Named { id: 2, label: f"st-{2}" };
    if cond { sink_s(s); }
    println(s.label);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "tup-1\nst-2\n");
    }
}

#[test]
fn test_ir_headerless_cluster_alloc_drops_rc_header() {
    // Phase D: the canonical b2 builder in a type-pure program
    // allocates members WITHOUT the 8-byte rc header — `hl_alloc`
    // (malloc of the twin size, no rc=1 store) replaces `rc_alloc`
    // everywhere in the fn, while the b2 link store and the root's
    // free-walk still engage against the shifted layout. With this,
    // the build loop emits ZERO refcount-related instructions: no
    // header store, no count ops, just malloc + field stores.
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
fn main() { println(build_and_sum(5)); }
"#,
    );
    let body = function_body(&ir, "build_and_sum").expect("fn body");
    assert!(
        body.contains("hl_alloc"),
        "headerless alloc should engage; body:\n{body}"
    );
    assert!(
        !body.contains("rc_alloc") && !body.contains("rc_ptr"),
        "no headered alloc / rc=1 store may remain; body:\n{body}"
    );
    assert!(
        body.contains("b2.link.slot") && body.contains("cw_loop"),
        "b2 link store + free-walk still engage; body:\n{body}"
    );
    // The twin layout is `{ i64, ptr }` (16 bytes): the link GEP
    // against it must address field index 1 of a 2-field struct,
    // never index 2 of the 3-field headered `{ i64, i64, ptr }`.
    assert!(
        body.contains("getelementptr inbounds { i64, ptr }"),
        "member GEPs must use the headerless twin; body:\n{body}"
    );
    assert!(
        !body.contains("getelementptr inbounds { i64, i64, ptr }"),
        "no headered member GEP may remain in the cluster fn; body:\n{body}"
    );
}

#[test]
fn test_e2e_fresh_return_builders_walk_and_drop() {
    // Both C1b shapes end-to-end, repeated: caller walks the
    // returned chain and drops it via the ordinary dec-drop — a
    // transfer miscount is a deterministic UAF (over-dec) or a
    // wrong sum (leak reuses garbage).
    let out = run_program_with_ownership(
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
    while iter < 64 {
        total = total + sum_chain(build_someroot(50));
        total = total + sum_chain(build_rootlink(50));
        iter = iter + 1;
    }
    println(total);
}
"#,
    );
    // 64 * (1275 + 1275) = 163200.
    assert_eq!(out.as_deref(), Some("163200\n"));
}

#[test]
fn test_body_splitting_8p_assignment_to_unknown_name_silently_dropped() {
    // Assignment whose target isn't in `current_names` (here:
    // assignment to a non-existent variable would fail at the
    // typechecker; this test instead uses a field assignment which
    // has a non-Identifier target, demonstrating the
    // non-identifier-target skip path).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             fn driver() with sends(Network) receives(Network) {
                 let h = Hub { count: 0 };
                 h.count = 7;
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // FieldAccess target (h.count = 7) is non-Identifier — the
    // walker drops it. No new store-into-arm-local sites should
    // arise; `h.slot` itself isn't even emitted because struct-
    // literal RHS isn't recognised by slice 8m.
    assert!(
        !body.contains("store i64 7, ptr %h.slot"),
        "field-assign target must not store into any arm-local slot:\n{body}"
    );
}

#[test]
fn test_body_splitting_8r_field_target_compound_assign_silently_dropped() {
    // `h.count += 1;` — non-identifier target (field access). The
    // walker drops the statement; no compound-op result lands in
    // any arm-local slot.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             fn driver() with sends(Network) receives(Network) {
                 let h = Hub { count: 0 };
                 h.count += 1;
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // Struct-literal RHS isn't recognised by the slice-8m let, so
    // `h.slot` itself never emits — but more importantly, no
    // `binop.assign_rhs` would emit even if h.slot existed because
    // the target is a non-identifier expression.
    assert!(
        !body.contains("%binop.assign_rhs"),
        "field-target compound-assign must skip the whole statement:\n{body}"
    );
}

#[test]
fn test_state_destructor_emits_rc_dec_for_shared_struct_field() {
    // A shared-struct captured local (`h: Hub` where `Hub` is a
    // `shared struct`) lowers to a pointer-sized handle in the
    // state struct. The destructor must load the handle,
    // null-guard, and dispatch through the rc_dec machinery.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             shared struct Hub { count: i64 }
             fn driver(h: Hub) { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_state_drop_driver");
    // Load handle.
    assert!(
        body.contains("%h.drop.handle = load ptr"),
        "destructor must load shared-struct handle:\n{body}"
    );
    // Null-guard: the slice-8a uninitialized-reload concern means
    // we must compare the handle against null before dec'ing.
    assert!(
        body.contains("%h.drop.is_null = icmp eq ptr %h.drop.handle, null"),
        "destructor must null-guard the shared-struct handle before rc_dec:\n{body}"
    );
    // The rc_dec branch GEPs the refcount field (Hub heap struct
    // field 0) and decrements — the canonical `emit_rc_dec` IR shape.
    assert!(
        body.contains("%rc_ptr"),
        "destructor must reach rc_dec's refcount GEP via %rc_ptr:\n{body}"
    );
    assert!(
        body.contains("%rc_dec = sub i64 %rc, 1"),
        "destructor must emit the rc -= 1 step:\n{body}"
    );
}

#[test]
fn test_state_destructor_walks_multiple_heap_fields_in_source_order() {
    // Two captured-local heap fields: `items: Vec[i64]` at source
    // pos 0, `name: String` at source pos 1. Both lower to the
    // same `{ptr, i64, i64}` inline layout but the destructor must
    // emit per-field drop IR keyed by the source binding names,
    // and the `items.*` block must precede the `name.*` block
    // (strict source order). This pins both the multi-field
    // walk and the source-order discipline that a future
    // reverse-construction-order Drop hook needs to verify
    // against.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(items: Vec[i64], name: String) { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_state_drop_driver");
    let items_pos = body
        .find("%items.drop.field_ptr")
        .unwrap_or_else(|| panic!("missing %items.drop.field_ptr in destructor:\n{body}"));
    let name_pos = body
        .find("%name.drop.field_ptr")
        .unwrap_or_else(|| panic!("missing %name.drop.field_ptr in destructor:\n{body}"));
    assert!(
        items_pos < name_pos,
        "items drop must precede name drop (source order):\n{body}"
    );
    // Each field gets its own `cap > 0 ? free` pair.
    assert!(
        body.contains("%items.drop.is_heap = icmp sgt i64 %items.drop.cap, 0"),
        "items field needs its cap > 0 compare:\n{body}"
    );
    assert!(
        body.contains("%name.drop.is_heap = icmp sgt i64 %name.drop.cap, 0"),
        "name field needs its cap > 0 compare:\n{body}"
    );
}

#[test]
fn test_ir_defer_drop_interleave_emission_order() {
    // Slice 3's structural pin. The two prior E2E tests rule out
    // "all drops then all defers" (defers read live Vec buffers),
    // but they do NOT distinguish the unified-stack LIFO interleave
    // from the alternative two-phase ordering "all defers then all
    // drops" — both orderings let a defer see its in-scope let-
    // binding alive. This IR test closes the gap by pinning the
    // emission order of defer bodies vs. `@free` calls in
    // `compile_function`'s tail-cleanup block.
    //
    // Source:
    //     let v = Vec.new(); v.push(...); defer { mark_a(); }
    //     let w = Vec.new(); w.push(...); defer { mark_b(); }
    //
    // Push order at the function's top frame:
    //     [FreeVec(v), UserDefer(mark_a), FreeVec(w), UserDefer(mark_b)]
    //
    // Drain order (LIFO) emits IR in this sequence inside main's
    // exit-cleanup block:
    //     1. UserDefer(mark_b) → `call .* @mark_b`
    //     2. FreeVec(w)        → `call .* @free` on w's data ptr
    //     3. UserDefer(mark_a) → `call .* @mark_a`
    //     4. FreeVec(v)        → `call .* @free` on v's data ptr
    //
    // Counterfactual ordering "all defers then all drops" would
    // emit `[@mark_b, @mark_a, @free, @free]`. Observing `@mark_a`
    // appearing AFTER an `@free` (and before the second `@free`)
    // distinguishes the actual unified-LIFO behaviour from that
    // alternative.
    let full_ir = ir_for(
        r#"
fn mark_a() { println("a"); }
fn mark_b() { println("b"); }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    defer { mark_a(); }
    let mut w: Vec[i64] = Vec.new();
    w.push(2_i64);
    defer { mark_b(); }
}
"#,
    );
    // Scope to `@main`'s body so positions are not confused by
    // `@free` calls inside `Vec.push`'s realloc path (which run
    // BEFORE the cleanup block in IR text order, but live in a
    // different function).
    let main_start = full_ir
        .find("define i32 @main")
        .unwrap_or_else(|| panic!("expected `define i32 @main` in IR; got:\n{full_ir}"));
    // `@main` ends at the first `\n}\n` after its opening — every
    // other top-level fn starts on a new line after that.
    let main_body_end_rel = full_ir[main_start..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("expected `@main` body terminator in IR; got:\n{full_ir}"));
    let ir = &full_ir[main_start..main_start + main_body_end_rel];

    let pos_b = ir
        .find("call void @mark_b")
        .unwrap_or_else(|| panic!("expected `call void @mark_b` in main; got:\n{ir}"));
    let pos_a = ir
        .find("call void @mark_a")
        .unwrap_or_else(|| panic!("expected `call void @mark_a` in main; got:\n{ir}"));
    // The cleanup-time `@free` calls take their data pointer from
    // `%cleanup.data*` slots (per `emit_cleanup_action`'s
    // `FreeVecBuffer` arm in `src/codegen/runtime.rs`). Earlier
    // `@free` calls in main's body come from `Vec.push`'s realloc
    // path (`call void @free(ptr %data4)`) and are NOT what we want
    // to compare against — scope to the `%cleanup.data` form.
    let cleanup_free_pat = "call void @karac_free_buf(ptr %cleanup.data";
    let pos_free1 = ir.find(cleanup_free_pat).unwrap_or_else(|| {
            panic!("expected first cleanup-time `@free` (matching `{cleanup_free_pat}`) in main; got:\n{ir}")
        });
    let pos_free2 = {
        let start = pos_free1 + 1;
        let rest = &ir[start..];
        rest.find(cleanup_free_pat)
            .map(|p| start + p)
            .unwrap_or_else(|| panic!("expected a second cleanup-time `@free` in main; got:\n{ir}"))
    };
    // Drain order: mark_b → free(w) → mark_a → free(v). The
    // critical assertion is that mark_a sits BETWEEN the two
    // free calls — that's what rules out "all defers then all
    // drops" (which would put both `mark_*` before either free).
    assert!(
            pos_b < pos_free1,
            "defer B (`mark_b`) should drain before the first cleanup `@free`; got mark_b at {pos_b}, free at {pos_free1}\nmain IR:\n{ir}",
        );
    assert!(
            pos_free1 < pos_a,
            "first cleanup `@free` (FreeVec(w)) should drain before defer A (`mark_a`); got free at {pos_free1}, mark_a at {pos_a}\nmain IR:\n{ir}",
        );
    assert!(
            pos_a < pos_free2,
            "defer A (`mark_a`) should drain before the second cleanup `@free` (FreeVec(v)); got mark_a at {pos_a}, free at {pos_free2}\nmain IR:\n{ir}",
        );
}

/// B-2026-09-01-2 — compiled twin of `tests/interpreter.rs`'s
/// `moving_one_field_out_leaves_the_others_their_drop_bodies`, same programs
/// and expectations.
///
/// This backend was correct on the two divergent rows; the interpreter lost
/// the surviving field's body to a coarse whole-walk disarm that shadowed
/// the precise per-field mask.
///
/// `deep-chain` is the row that matters most here, and it moved twice, each
/// time on both backends at once -- which is what it was pinned to force.
/// B-2026-09-06-46: codegen's move-out disarm stopped DELETING the source's
/// whole field-bodies walker and started masking the moved hop, so `k`, a
/// top-level sibling that never moved, keeps its body. B-2026-09-06-55: the
/// mask became the full PATH, routed through
/// `struct_moved_nested_field_bodies` and the `nested` level of the skip
/// tree, so it lands on `Inner`'s walker instead of `Outer`'s and `q` --
/// the moved hop's sibling one level DOWN -- keeps its body as well. The row
/// reads `dR1 dR3 dR2`, one body per object.
///
/// `three-hops` is what the path record buys that neither earlier form
/// could express: at `o.b.c.r` there is a sibling to lose at EVERY level,
/// and a root-level mask lost both (`dR4` alone, where four are due).
///
/// `discard-the-hop` pins the other direction. `let Outer { k, h: _ } = o`
/// after `o.h.r` moved out still owes `q`'s body and must NOT re-run `r`'s,
/// which `x` owns -- the mask has to narrow the discarded value, not
/// suppress the field. Codegen gets this from the same skip tree; the
/// interpreter needed the paths applied to the discarded clone, and ran
/// `dR1` twice until it did.
#[test]
fn test_e2e_moving_one_field_out_leaves_the_others_their_drop_bodies() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct S3 { a: R, b: R }\n\
             struct Inner { r: R, q: R }\n\
             struct Outer { h: Inner, k: R }\n\
             struct L3 { r: R, q: R }\n\
             struct L2 { c: L3, d: R }\n\
             struct L1 { b: L2, e: R }\n\
             enum E { A(R), Nil }\n\
             struct HasE { e: E, r: R }\n\
             fn mk(i: i64) -> R { return R { id: i }; }\n";
    for (label, body, want) in [
            (
                "mixed wrap, move a",
                "fn f(r: R) -> i64 { let s = S3 { a: r, b: mk(2) }; let x = s.a; return 7; }\n\
                 fn main() { println(f(mk(1))) }",
                "dR2\ndR1\n7\n",
            ),
            (
                "both fresh, move a",
                "fn f() -> i64 { let s = S3 { a: mk(5), b: mk(6) }; let x = s.a; return 7; }\n\
                 fn main() { println(f()) }",
                "dR5\ndR6\n7\n",
            ),
            (
                "move b (control)",
                "fn f(r: R) -> i64 { let s = S3 { a: r, b: mk(8) }; let x = s.b; return 7; }\n\
                 fn main() { println(f(mk(7))) }",
                "dR8\ndR7\n7\n",
            ),
            (
                "no move (control)",
                "fn f(r: R) -> i64 { let s = S3 { a: r, b: mk(4) }; return 7; }\n\
                 fn main() { println(f(mk(3))) }",
                "dR4\ndR3\n7\n",
            ),
            (
                "deep chain masks the moved LEAF, not the whole hop",
                "fn f() -> i64 { let o = Outer { h: Inner { r: mk(1), q: mk(2) }, k: mk(3) }; let x = o.h.r; return 7; }\n\
                 fn main() { println(f()) }",
                "dR1\ndR3\ndR2\n7\n",
            ),
            (
                "deep chain, three hops: every sibling on the way keeps its body",
                "fn f() -> i64 { let o = L1 { b: L2 { c: L3 { r: mk(1), q: mk(2) }, d: mk(3) }, e: mk(4) }; let x = o.b.c.r; return 7; }\n\
                 fn main() { println(f()) }",
                "dR1\ndR4\ndR3\ndR2\n7\n",
            ),
            (
                "deep chain then DISCARD the hop: the moved leaf runs once",
                "fn f() -> i64 { let o = Outer { h: Inner { r: mk(1), q: mk(2) }, k: mk(3) }; let x = o.h.r; let Outer { k, h: _ } = o; return k.id; }\n\
                 fn main() { println(f()) }",
                "dR1\ndR2\ndR3\n3\n",
            ),
            (
                "enum-valued source field",
                "fn f() -> i64 { let s = HasE { e: E.A(mk(4)), r: mk(5) }; let x = s.r; return 7; }\n\
                 fn main() { println(f()) }",
                "dR5\ndR4\n7\n",
            ),
            (
                "single Drop field",
                "fn f() -> i64 { let s = Inner { r: mk(6), q: mk(9) }; let x = s.r; return 7; }\n\
                 fn main() { println(f()) }",
                "dR6\ndR9\n7\n",
            ),
        ] {
            assert_eq!(
                run_program(&format!("{H}{body}\n")),
                Some(want.to_string()),
                "{label}"
            );
        }
}

/// B-2026-09-16-18 — a fresh-temp STRUCT scrutinee's UNBOUND fields run
/// their `Drop` bodies and free their heap.
///
/// The husk of such a temp was owned by nobody, on all four surfaces alike:
/// `match S3 { a: mk(44), b: mk(45) } { S3 { a, .. } => .. }` ran `dR44`
/// alone and `S3 { .. }` ran NOTHING, with valgrind at `-O0` reporting one
/// `name` buffer leaked per unbound field (24 allocs / 20 frees, 12 bytes
/// definitely lost in 4 blocks over the first four cells; 28/28 and zero
/// after). No A/B gate could see it because all four surfaces AGREED — the
/// reason this fixture pins values and not merely parity.
///
/// Two gaps, and both had to move for any cell to change:
///
/// * GAP A — `expr_yields_fresh_owned_temp` matches only `Call` /
///   `MethodCall`, so a struct LITERAL scrutinee never reached the
///   materializer. `temp-call` is the same defect through the arm that DID
///   reach it, which is what shows Gap A was not the whole story.
/// * GAP B — the materializer then declined any type without its OWN
///   `impl Drop`, which is every "merely contains a `Drop` field" struct.
///
/// `temp-all`, `named-control` and `no-drop-fields` are the controls that a
/// naive widening breaks: the first binds BOTH fields (the husk owes
/// nothing, and a second body there would be a double close for a `drop()`
/// that closes a handle), the second has a binding whose own walk already
/// owns the husk, and the third has no `Drop`-bearing field at all.
///
/// `iflet-husk-both-bodies` WAS `iflet-agreed-gap`, PINNED AT `dR68\nz=68\n`
/// — one body where two are due — and B-2026-09-21-1 flipped it. The pin was
/// deliberate, not an oversight: the interpreter carried this ownership in
/// `eval_match` alone, so arming the compiled side by itself measured `dR72`
/// under `--interp` against `dR72 dR73` on the other three, converting an
/// AGREED gap into a run-vs-build DIVERGENCE, which is strictly worse than
/// the gap. `if let`, `while let` and `let ... else` now carry the same
/// ownership in the interpreter (`eval_expr`'s `ExprKind::IfLet` and
/// `ExprKind::WhileLet`, `eval_stmt`'s `StmtKind::LetElse`), the
/// `match_spelling` gate is gone, and all four surfaces run both bodies.
/// Twin: `tests/interpreter.rs`'s
/// `fresh_temp_struct_scrutinee_unbound_fields_run_their_drop_bodies`.
///
/// `guarded-two-arm-per-arm-mask` WAS THAT SECOND GAP AND IS NOW CLOSED
/// (B-2026-09-21-2). This cell pinned `dR81` alone, and the paragraph here
/// explained it as forced: the husk's walker is registered once and fired
/// at the merge block after the phi, one function for every arm, so codegen
/// could only mask the UNION of what the arms bind, and a taken-arm mask in
/// the interpreter alone was measured MORE precise AND divergent.
///
/// The union was not merely imprecise. When it covered every body-bearing
/// field — which two arms naming different fields do between them — the
/// materializer concluded the husk owed nothing and declined outright, so
/// the temp got no bodies walker AND no memory walk: the taken arm's
/// unbound body was lost and its buffer leaked, 3 bytes per call.
///
/// Each arm now stores its own walker in a slot the single fire site loads,
/// so codegen has a per-arm answer and the interpreter masks by the TAKEN
/// arm to agree with it. The drop POINT is untouched — design.md
/// § Temporary Lifetime Rules puts a match scrutinee at "drops at match
/// exit" and B-2026-08-29-28 placed it there deliberately — so only WHICH
/// bodies run changed. Measured after: `dR81 dR80` on all four surfaces,
/// 13 allocs / 13 frees, nothing lost, no invalid access.
///
/// AN ARM THAT BINDS EVERY BODY-BEARING FIELD owes nothing and says so with
/// a no-op walker, not by declining — `guarded-arm2-binds-all` pins that,
/// because declining would have put the lost body and the leak back for
/// every other arm of such a match.
#[test]
fn test_e2e_fresh_temp_struct_scrutinee_unbound_fields_run_their_drop_bodies() {
    const H: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"n{i}\" }; }\n\
             struct S3 { a: R, b: R }\n\
             struct Plain { x: i64, y: i64 }\n\
             fn mks() -> S3 { return S3 { a: mk(51), b: mk(52) }; }\n";
    for (label, cell, want) in [
            // The row's own four cells, verbatim.
            (
                "temp-literal",
                "fn c() -> i64 { match S3 { a: mk(44), b: mk(45) } { S3 { a, .. } => { return a.id; } } }",
                "dR44\ndR45\nz=44\n",
            ),
            (
                "temp-call",
                "fn c() -> i64 { match mks() { S3 { a, .. } => { return a.id; } } }",
                "dR51\ndR52\nz=51\n",
            ),
            (
                "temp-all",
                "fn c() -> i64 { match S3 { a: mk(47), b: mk(48) } { S3 { a, b } => { return a.id + b.id; } } }",
                "dR48\ndR47\nz=95\n",
            ),
            (
                "temp-none",
                "fn c() -> i64 { match S3 { a: mk(49), b: mk(50) } { S3 { .. } => { return 1; } } }",
                "dR50\ndR49\nz=1\n",
            ),
            // Controls.
            (
                "named-control",
                "fn c() -> i64 { let s: S3 = S3 { a: mk(62), b: mk(63) }; match s { S3 { a, .. } => { return a.id; } } }",
                "dR62\ndR63\nz=62\n",
            ),
            (
                "wildcard-field",
                "fn c() -> i64 { match S3 { a: mk(60), b: mk(61) } { S3 { a: _, b } => { return b.id; } } }",
                "dR61\ndR60\nz=61\n",
            ),
            (
                "no-drop-fields",
                "fn c() -> i64 { match Plain { x: 1, y: 2 } { Plain { .. } => { return 7; } } }",
                "z=7\n",
            ),
            (
                "guarded-two-arm-per-arm-mask",
                "fn c() -> i64 { match S3 { a: mk(80), b: mk(81) } { S3 { a, .. } if a.id > 100 => { return a.id; } S3 { b, .. } => { return b.id; } } }",
                "dR81\ndR80\nz=81\n",
            ),
            // Was the pinned agreed gap; closed by B-2026-09-21-2 — see the doc above.
            (
                "iflet-husk-both-bodies",
                "fn c() -> i64 { if let S3 { a, .. } = S3 { a: mk(68), b: mk(69) } { return a.id; } return 0; }",
                "dR68\ndR69\nz=68\n",
            ),
            // B-2026-09-21-2 — the per-arm mask. Measured on all four surfaces
            // through this fixture's own prelude and wrapper and refused unless
            // they agreed, so the tuple and the program its numbers came from
            // are one string rather than two transcriptions.
            (
                "guarded-two-arm-fallthrough",
                "fn c() -> i64 { let v = match S3 { a: mk(89), b: mk(90) } { S3 { a, .. } if a.id > 900 => { a.id } S3 { b, .. } => { b.id } }; return v; }",
                "dR90\ndR89\nz=90\n",
            ),
            (
                "guarded-two-arm-first-taken",
                "fn c() -> i64 { match S3 { a: mk(82), b: mk(83) } { S3 { a, .. } if a.id > 0 => { return a.id; } S3 { b, .. } => { return b.id; } } }",
                "dR82\ndR83\nz=82\n",
            ),
            (
                "guarded-two-arm-wildcard",
                "fn c() -> i64 { match S3 { a: mk(95), b: mk(96) } { S3 { a: _, b } if b.id > 900 => { return b.id; } S3 { a, b: _ } => { return a.id; } } }",
                "dR95\ndR96\nz=95\n",
            ),
            (
                "guarded-three-arm-s3",
                "fn c() -> i64 { match S3 { a: mk(86), b: mk(87) } { S3 { a, .. } if a.id > 900 => { return a.id; } S3 { b, .. } if b.id > 900 => { return b.id; } S3 { .. } => { return 3; } } }",
                "dR87\ndR86\nz=3\n",
            ),
            (
                "guarded-arm2-binds-all",
                "fn c() -> i64 { match S3 { a: mk(99), b: mk(100) } { S3 { a, .. } if a.id > 900 => { return a.id; } S3 { a, b } => { return a.id + b.id; } } }",
                "dR100\ndR99\nz=199\n",
            ),
            (
                "guarded-two-arm-same-field-control",
                "fn c() -> i64 { match S3 { a: mk(84), b: mk(85) } { S3 { a, .. } if a.id > 100 => { return a.id; } S3 { a, .. } => { return a.id + 1; } } }",
                "dR84\ndR85\nz=85\n",
            ),
        ] {
            let src =
                format!("{H}{cell}\nfn main() {{ let z: i64 = c(); println(f\"z={{z}}\"); }}\n");
            assert_eq!(run_program(&src), Some(want.to_string()), "{label}");
        }
}

#[test]
fn test_e2e_borrow_projection_copy_runs_the_drop_body_twice() {
    // B-2026-09-01-4 — the OBSERVABLE this row is about, pinned so the
    // language's current answer is a fact in the test suite rather than an
    // inference from `clone_ref_chain_field_move_rhs`'s existence.
    //
    // `let m = s.r;` over `s: ref S` copies (design.md § "Field projection
    // off a borrow"), so ONE constructed `R` yields TWO values and its
    // `Drop` body runs twice — once for the copy, once for the caller's
    // field. The same line over an OWNED root moves, and runs it once.
    // Both spellings are here because the contrast is the whole point: the
    // read's meaning is set by the root's parameter mode, and nothing at
    // the read shows which one is in force. That invisibility is what
    // `borrow_projection_copy` reports.
    //
    // THIS TEST IS EXPECTED TO CHANGE. design.md commits to promoting the
    // lint to an error once `ref` accepts a place expression, exactly as
    // `v[i]` became `E_INDEX_MOVE_NON_COPY` once `ref v[i]` landed
    // (B-2026-08-26-36 then B-2026-08-26-21). When that happens the
    // `borrowed` half stops compiling, and that is the intended signal —
    // do not "fix" it by deleting the assertion.
    let output = run_program(
            "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(\"d\".to_string() + self.id.to_string()); } }\n\
             struct S { r: R }\n\
             fn borrowed(s: ref S) -> i64 { let m = s.r; return m.id; }\n\
             fn owned(s: S) -> i64 { let m = s.r; return m.id; }\n\
             fn main() {\n\
                 let a = S { r: R { id: 3 } };\n\
                 println(borrowed(a).to_string());\n\
                 let b = S { r: R { id: 7 } };\n\
                 println(owned(b).to_string());\n\
                 println(\"end\");\n\
             }",
        )
        .expect("compile + run failed");
    assert_eq!(
        output, "d3\n3\nd3\n7\nd7\nend\n",
        "the borrowed read copies (two `d3`), the owned read moves (one \
             `d7`); if this ever prints one `d3`, the copy is gone and \
             design.md § \"Field projection off a borrow\" is stale"
    );
}

#[test]
fn test_e2e_file_drop_closes_handle_and_flushes_pending_writes() {
    // F4b verification — no explicit `f.flush()`; the scope-exit
    // FreeFileHandle cleanup action runs `karac_runtime_file_close`,
    // which drops the std::fs::File whose Drop impl flushes
    // pending writes through the kernel. The on-disk contents
    // must match what the user wrote.
    let tmp = std::env::temp_dir().join("karac_e2e_file_f6_drop_flushes.txt");
    let _ = std::fs::remove_file(&tmp);
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
fn main() with writes(FileSystem) {{
    match File.create("{path}") {{
        Ok(f) => {{
            let mut data: Vec[u8] = Vec.new();
            data.push(65u8); data.push(10u8);
            match f.write(data) {{
                Ok(_) => println("wrote"),
                Err(_) => println("err"),
            }}
        }}
        Err(_) => println("create-err"),
    }}
}}
"#
    );
    let out = run_program(&src);
    if let Some(out) = out {
        assert_eq!(out.trim(), "wrote");
        let contents = std::fs::read(&tmp).expect("read tempfile");
        assert_eq!(contents, b"A\n");
    }
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_e2e_generic_parent_drop_field_bodies() {
    // B-2026-08-02-14 — a Drop-carrying field of a GENERIC-mono parent
    // (`Box2[Res]`): the name-keyed field-bodies walk saw only the
    // declared bare param (`item: T`) and stayed silent at owner death
    // (both backends), and the Vec-element leg additionally leaked the
    // element's String buffer under AOT (base struct synthesis read
    // `T` as a scalar). The subst-aware walk fires both bodies and the
    // mono element drop frees the heap — matching the non-generic
    // control byte-for-byte.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Box2[T] { item: T, tag: i64 }
fn main() {
    println("a");
    {
        let b: Box2[Res] = Box2 { item: Res { id: 3, name: f"ggg{3}" }, tag: 1 };
        println(f"tag {b.tag}");
    }
    println("mid");
    {
        let mut v: Vec[Box2[Res]] = Vec.new();
        v.push(Box2 { item: Res { id: 4, name: f"hhhhh{4}" }, tag: 2 });
        println(f"vlen {v.len()}");
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "a\ntag 1\ndrop 3 ggg3\nmid\nvlen 1\ndrop 4 hhhhh4\nend"
        );
    }
}

#[test]
fn test_e2e_truncate_and_reassign_run_displaced_drop_bodies() {
    // B-2026-08-03-2 (class 1, remainder) — the last two positions where a
    // container element is destroyed with no binding to receive it.
    // `truncate(n)` needed a RANGED walk, not the whole-container walker
    // `clear` uses: its SURVIVORS still fire at binding death, so a
    // whole-container walk would double them. `v = w` displaces every old
    // element, so there the whole-container walker is right. Both had the
    // same memory-yes/bodies-no split as clear.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn main() {
    println("truncate:");
    {
        let mut v: Vec[Res] = Vec.new();
        v.push(Res { id: 1, name: f"a{1}" });
        v.push(Res { id: 2, name: f"b{2}" });
        v.truncate(1);
        println(v.len());
    }
    println("truncate0:");
    {
        let mut u: Vec[Res] = Vec.new();
        u.push(Res { id: 3, name: f"c{3}" });
        u.truncate(0);
        println(u.len());
    }
    println("reassign:");
    {
        let mut w: Vec[Res] = Vec.new();
        w.push(Res { id: 4, name: f"d{4}" });
        let mut z: Vec[Res] = Vec.new();
        z.push(Res { id: 5, name: f"e{5}" });
        w = z;
        println(w.len());
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "truncate:\ndrop 2 b2\n1\ndrop 1 a1\ntruncate0:\ndrop 3 c3\n0\n\
                 reassign:\ndrop 4 d4\n1\ndrop 5 e5\nend"
        );
    }
}

#[test]
fn test_e2e_container_clear_runs_element_drop_bodies() {
    // B-2026-08-03-2 (class 1) — `v.clear()` / `m.clear()` destroy every
    // element but ran no destructor. Both arms did all the MEMORY work and
    // none of the body work, so the shapes were vg-clean and silent —
    // parity-equal, invisible to backend diffing and to any sanitizer, and
    // only a fire-count oracle sees them. The third block is a
    // clear-then-reuse control: the cleared element fires, the buffer is
    // reusable, and the replacement fires at scope exit.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn main() {
    println("vecclear:");
    {
        let mut v: Vec[Res] = Vec.new();
        v.push(Res { id: 1, name: f"a{1}" });
        v.push(Res { id: 2, name: f"b{2}" });
        v.clear();
        println(v.len());
    }
    println("mapclear:");
    {
        let mut m: Map[i64, Res] = Map.new();
        m.insert(5, Res { id: 3, name: f"c{3}" });
        m.clear();
        println(m.len());
    }
    println("reuse:");
    {
        let mut w: Vec[Res] = Vec.new();
        w.push(Res { id: 4, name: f"d{4}" });
        w.clear();
        w.push(Res { id: 5, name: f"e{5}" });
        println(w.len());
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "vecclear:\ndrop 1 a1\ndrop 2 b2\n0\nmapclear:\ndrop 3 c3\n0\n\
                 reuse:\ndrop 4 d4\n1\ndrop 5 e5\nend"
        );
    }
}

#[test]
fn test_e2e_nested_call_temp_owned_arg_drop() {
    // B-2026-08-02-28 — a call RESULT consumed directly as another call's
    // owned argument (`use_it(mk(xs))`). The fn-call arm of the owned-arg
    // registrar registered the field-BODIES walk and returned, omitting the
    // memory drop its struct-LITERAL sibling registers for the identical
    // value — so the body printed while the Holder's Vec buffer and its
    // element leaves leaked once per call (asan pin
    // `nested_call_temp_owned_arg_freed` covers that half). Fire count was
    // already right; this pins the ORDER, which the fix also settled: the
    // memory drop is pushed first so the LIFO drain runs the body before
    // the fields it reads are freed. Three shapes — bound outer result,
    // bare-statement discard, and an inner call taking no argument.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Holder { xs: Vec[Res], tag: i64 }
fn mk(v: Vec[Res]) -> Holder { Holder { xs: v, tag: 9 } }
fn mkh() -> Holder {
    let mut v: Vec[Res] = Vec.new();
    v.push(Res { id: 7, name: f"g{7}" });
    Holder { xs: v, tag: 3 }
}
fn use_it(h: Holder) -> i64 { h.tag }
fn main() {
    println("a");
    {
        let mut xs: Vec[Res] = Vec.new();
        xs.push(Res { id: 1, name: f"a{1}" });
        let n = use_it(mk(xs));
        println(n);
    }
    println("b");
    {
        let mut ys: Vec[Res] = Vec.new();
        ys.push(Res { id: 2, name: f"b{2}" });
        use_it(mk(ys));
    }
    println("c");
    {
        let m = use_it(mkh());
        println(m);
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "a\ndrop 1 a1\n9\nb\ndrop 2 b2\nc\ndrop 7 g7\n3\nend"
        );
    }
}

#[test]
fn test_e2e_tuple_binding_container_element_drop() {
    // B-2026-08-03-5 — a TUPLE binding whose element is a CONTAINER of
    // Drop-running values (`let t = (xs, 9)` with `xs: Vec[Res]`). Two
    // independent gaps, both AOT-only, so `karac run` was right and
    // `karac build` was wrong in two ways at once:
    //   * bodies — `emit_tuple_elem_user_drop_bodies_fn` accepted only an
    //     element that was a Path naming a user struct, so `Vec[Res]` read
    //     as the drop-free head "Vec" and the whole walker declined;
    //   * memory — the LLVM-type aggregate drop freed the element Vec's
    //     BUFFER shallowly, leaking every live `Res`'s String.
    // Underneath both sat `infer_arg_elem_te` erasing the generic argument,
    // so the element type was bare `Vec` at every decision point. All three
    // element sources exercised: a named binding, a fresh call, and an
    // annotated binding.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn mkv() -> Vec[Res] {
    let mut v: Vec[Res] = Vec.new();
    v.push(Res { id: 2, name: f"b{2}" });
    v
}
fn main() {
    println("a");
    {
        let mut xs: Vec[Res] = Vec.new();
        xs.push(Res { id: 1, name: f"a{1}" });
        let t = (xs, 9);
        println(t.1);
    }
    {
        let u = (mkv(), 8);
        println(u.1);
    }
    {
        let mut ys: Vec[Res] = Vec.new();
        ys.push(Res { id: 3, name: f"c{3}" });
        let w: (Vec[Res], i64) = (ys, 7);
        println(w.1);
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "a\n9\ndrop 1 a1\n8\ndrop 2 b2\n7\ndrop 3 c3\nend"
        );
    }
}

#[test]
fn test_e2e_tuple_literal_own_drop_source_disarm() {
    // B-2026-08-02-27 — `let r = Res { .. }; let t = (r, 9);`. The
    // consuming-ARG aggregate arms were promoted to the strong
    // `suppress_user_drop_for_var` in B-2026-08-02-22, but the let-RHS
    // sibling `disarm_container_bodies_move_sources` kept the weak
    // container-only form, so `r`'s OWN body stayed armed and fired at
    // r's NLL end over the moved-from slot — printing an EMPTY name —
    // then again through the tuple's element walk. The inline control
    // below (`(Res { .. }, 4)`, no move at all) already fired exactly
    // once, which is what proves the tuple binding owns the body and the
    // strong disarm is safe.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn main() {
    println("a");
    {
        let r = Res { id: 1, name: f"a{1}" };
        let t = (r, 9);
        println(t.1);
    }
    {
        let u = (Res { id: 2, name: f"b{2}" }, 4);
        println(u.1);
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "a\n9\ndrop 1 a1\n4\ndrop 2 b2\nend");
    }
}

#[test]
fn test_e2e_in_loop_decl_rearms_cond_move_drop_flag_each_iteration() {
    // B-2026-09-02-6 — a `cond_move_drop_flags` flag is allocated and set
    // `true` in the function's ENTRY block, which runs ONCE PER CALL. The
    // entry block is the right home for the ALLOCA (it dominates every
    // path) and the wrong home for the `true`: a binding declared in a loop
    // body is re-initialized on every iteration, so it re-owns its value on
    // every iteration. One entry-block arming left the flag `false` for
    // every iteration after the first one that disarmed it, and each later
    // iteration's freshly-declared value silently lost its own `Drop` body.
    //
    // The fix records where an in-loop `let` finished emitting and, when
    // the flag is finally minted at the disarm site, goes back and inserts
    // a second `true` there. That store executes once per iteration and
    // dominates every disarm within it.
    //
    // Four shapes, and between them they lose FIVE bodies before the fix.
    //
    // `a` is the row's repro: the disarm is on iteration 0, so BOTH later
    // iterations lost their own value. Measured pre-fix as
    // `w0 dR90 w1 w2 dR5 a188` against `--interp`'s
    // `w0 dR90 w1 dR91 w2 dR92 dR5 a188`.
    //
    // `b` moves the disarm to the MIDDLE iteration, which pins the two
    // halves separately: iteration 0 is ahead of any disarm and was always
    // right, iteration 2 is behind one and was not. A fix that armed at the
    // top of the loop rather than at the declaration passes `a` and `b`
    // alike, which is why neither is the interesting row.
    //
    // `c` IS: the binding is declared OUTSIDE the loop and assigned inside
    // it. That is a genuine hand-over — `out` holds the caller's value from
    // the assignment onward and must NOT be re-armed on the next iteration.
    // It is byte-identical before and after, and it is the row an
    // over-eager fix breaks by re-arming per iteration instead of per
    // declaration.
    //
    // `d` is the nested spelling, where the inner `let` must re-arm per
    // INNER iteration: the disarm lands on one of four passes and the two
    // passes of the second outer iteration both lost their bodies before.
    let out = run_program(
        r#"
struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }

fn while_first(p: R) -> i64 {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 3 {
        println(f"w{i}");
        let mut out: R = R { id: 90 + i, tag: f"t" };
        if i == 0 { out = p; }
        acc = acc + out.id;
        i = i + 1;
    }
    return acc
}

fn while_middle(p: R) -> i64 {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 3 {
        println(f"m{i}");
        let mut out: R = R { id: 60 + i, tag: f"t" };
        if i == 1 { out = p; }
        acc = acc + out.id;
        i = i + 1;
    }
    return acc
}

fn outside(p: R) -> i64 {
    let mut out: R = R { id: 70, tag: f"t" };
    let mut i: i64 = 0;
    while i < 3 {
        println(f"o{i}");
        if i == 1 { out = p; }
        i = i + 1;
    }
    return out.id
}

fn nested(p: R) -> i64 {
    let mut acc: i64 = 0;
    for a in 0..2 {
        for b in 0..2 {
            println(f"n{a}{b}")
            let mut out: R = R { id: 10 * a + b, tag: f"t" };
            if a == 0 and b == 1 { out = p; }
            acc = acc + out.id;
        }
    }
    return acc
}

fn main() {
    println(f"a{while_first(R { id: 5, tag: f"q" })}");
    println("-");
    println(f"b{while_middle(R { id: 6, tag: f"q" })}");
    println("-");
    println(f"c{outside(R { id: 7, tag: f"q" })}");
    println("-");
    println(f"d{nested(R { id: 8, tag: f"q" })}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            concat!(
                "w0\ndR90\nw1\ndR91\nw2\ndR92\ndR5\na188\n-",
                "\nm0\ndR60\nm1\ndR61\nm2\ndR62\ndR6\nb128\n-",
                "\no0\no1\ndR70\no2\ndR7\nc7\n-",
                "\nn00\ndR0\nn01\ndR1\nn10\ndR10\nn11\ndR11\ndR8\nd29",
            )
        );
    }
}

/// Returning a heap field of an owned by-value struct param must zero that
/// field's `cap` in the SOURCE before the param's `StructDrop`, so the drop
/// skips the moved-out (returned) buffer — otherwise the returned `String`
/// is freed and then handed back (double-free / UAF). Regression for the
/// slice-3c by-value struct field move-out fix (the `FieldAccess` move-out
/// suppressor + `zero_struct_field_move_cap`). E2E coverage lives in
/// `tests/memory_sanitizer.rs::asan_by_value_struct_field_moveout_no_double_free`.
#[test]
fn struct_field_moveout_zeros_source_cap_before_drop() {
    let ir = ir_for(
        "struct Pair { a: String, b: String }\n\
             fn f(p: Pair) -> String { p.a }\n\
             fn main() { println(f(Pair { a: \"x\".to_string(), b: \"y\".to_string() })); }\n",
    );
    let body = function_body(&ir, "f").expect("fn f must be emitted");
    let cap_zero = body.find("sfld.move.cap");
    let drop_call = body.find("@__karac_drop_struct_Pair");
    assert!(
        cap_zero.is_some(),
        "fn f must cap-zero the moved-out field in the source \
             (FieldAccess move-out suppressor)\n--- body ---\n{body}"
    );
    assert!(
        drop_call.is_some(),
        "fn f must drop the callee-owned struct param\n--- body ---\n{body}"
    );
    assert!(
        cap_zero < drop_call,
        "the moved-field cap-zero must precede the struct drop\n--- body ---\n{body}"
    );
}

/// B-2026-08-29-45 — MOVING A BINDING INTO A CONTAINER LITERAL RAN ITS
/// `Drop` BODY TWICE, on every backend.
///
/// Two independent halves, and the row's own triple is what separates
/// them:
///
///   * MOVE-SUPPRESSION. `let m = R { .. }; let v: Vec[R] = [m];` armed the
///     container's element-body walk without retracting `m`'s own
///     ownership, so the body ran at `m`'s NLL death AND through the walk.
///     No param anywhere, so the cause cannot be caller-retains.
///   * CALLER-RETAINS. `fn take(r: R) { let v: Vec[R] = [r]; }` — `r` is a
///     param VIEW whose body the CALLER runs, so the container must arm no
///     walker at all. The tuple, struct-literal and `Some(...)` wraps have
///     handled this since B-2026-08-29-24; the array/`Vec` arm was
///     deliberately left out there rather than half-fixed.
///
/// TWO AST NODES, not one, and this is the trap: `let v: Vec[R] = [m]`
/// parses as `PrefixCollectionLiteral` while `let a: Array[R, 1] = [m]`
/// parses as `ArrayLiteral`. A first cut handled only `ArrayLiteral` and
/// silently fixed the `Array` annotation while leaving the `Vec` one — the
/// row's own repro — still doubling. Both annotations are pinned here for
/// that reason.
///
/// ALL THREE BACKENDS AGREED ON THE WRONG ANSWER, which is why no A/B gate
/// reported this and why the fixture asserts an exact count rather than
/// run==build. It is also not a memory error — the buffer is freed once —
/// so no ASAN fixture can see it either.
///
/// The MIXED literal (`[r, R { id: 2 }]`, one view and one fresh) is
/// deliberately NOT fixed: the tuple walker can mask individual slots
/// because its arity is fixed, and a `Vec`'s is not. Both backends draw
/// that line in the same place, so it stays an agreed gap rather than
/// becoming a divergence.
#[test]
fn e2e_a_binding_moved_into_a_container_literal_drops_once() {
    const PRELUDE: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             struct S { r: R }\n";
    let cases: &[(&str, &str, &str)] = &[
        (
            "local moved into a `Vec` literal",
            "fn take() -> i64 { let m: R = R { id: 4 }; let v: Vec[R] = [m]; return 7; }",
            "dR4\nv=7\n",
        ),
        (
            "local moved into an `Array` literal",
            "fn take() -> i64 { let m: R = R { id: 4 }; let a: Array[R, 1] = [m]; return 7; }",
            "dR4\nv=7\n",
        ),
        (
            "PARAM view moved into a `Vec` literal",
            "fn take2(r: R) -> i64 { let v: Vec[R] = [r]; return 7; }\n\
                 fn take() -> i64 { return take2(R { id: 1 }); }",
            "dR1\nv=7\n",
        ),
        (
            "PARAM view moved into an `Array` literal",
            "fn take2(r: R) -> i64 { let a: Array[R, 1] = [r]; return 7; }\n\
                 fn take() -> i64 { return take2(R { id: 1 }); }",
            "dR1\nv=7\n",
        ),
        // CONTROLS — the shapes that were already correct, and together the
        // reason the two halves above are separable.
        (
            "control: a FRESH element, no named source",
            "fn take() -> i64 { let v: Vec[R] = [R { id: 4 }]; return 7; }",
            "dR4\nv=7\n",
        ),
        (
            "control: the TUPLE wrap of the same local",
            "fn take() -> i64 { let m: R = R { id: 4 }; let t: (R, i64) = (m, 1); return 7; }",
            "dR4\nv=7\n",
        ),
        (
            "control: the `Some(...)` wrap of the same local",
            "fn take() -> i64 { let m: R = R { id: 4 }; let o: Option[R] = Some(m); return 7; }",
            "dR4\nv=7\n",
        ),
        (
            "control: the STRUCT-literal wrap of a param view",
            "fn take2(r: R) -> i64 { let s: S = S { r: r }; return 7; }\n\
                 fn take() -> i64 { return take2(R { id: 1 }); }",
            "dR1\nv=7\n",
        ),
    ];
    for (label, decls, want) in cases {
        let src = format!("{PRELUDE}{decls}\nfn main() {{ println(f\"v={{take()}}\"); }}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "{label}: interpreter");
        if let Some(aot) = run_program(&src) {
            assert_eq!(aot, *want, "{label}: one value, one body");
        }
    }
}

/// B-2026-08-31-21 — A DISCARDED `shared` STRUCT LITERAL ran no `Drop` body
/// on ANY backend and stranded its RC box, while the same literal BOUND was
/// correct — which is what says the discard SITE was the unit, not `shared`
/// types generally.
///
/// A `shared` value is a bare RC pointer, and that is exactly the kind the
/// aggregate registrar returns early on and exactly the kind
/// `materialize_owned_temp` owns. So a discarded `shared` literal fell
/// BETWEEN the two legs at every discard site and got no owner at all.
///
/// THE ALIASING QUESTION THE ROW WAS HELD OPEN FOR IS ANSWERED BY THE
/// MODEL THAT ALREADY EXISTED. The bound spelling fires the body at the
/// refcount 0-transition: `Env::drop_target` hands
/// `invoke_user_drop_if_applicable` an `Arc::strong_count` and it fires at
/// `== 1`. The discard legs use the same test, so "run the body at the
/// discard" is not a new rule — a value reached with one reference is one
/// nothing else can observe. Measured: an ALIASED shared value still runs
/// exactly one body at the end of its scope, and `let _ = a;` over a
/// still-live binding declines in the new legs (the binding holds the other
/// reference) and keeps running through its own path, unchanged.
///
/// THE COUNT MUST BE READ BEFORE THE VALUE IS CLONED, which is the one
/// subtlety: the interpreter's discard walker takes its argument by value
/// and three callers hand it a clone, bumping the `Arc` and defeating the
/// test — the same hazard `Env::drop_target`'s own doc records for `get`.
/// Those callers ask `run_discarded_shared_user_drop` first instead.
#[test]
fn e2e_discarded_shared_literal_runs_its_drop_body() {
    const PRELUDE: &str = "shared struct S { id: i64, name: String }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\"); } }\n\
             shared struct T { id: i64 }\n\
             fn mks(n: i64) -> S { return S { id: n, name: f\"n{n}\" }; }\n";
    let cases: &[(&str, &str, &str)] = &[
            (
                "the row's repro: `let _ =` over a shared literal",
                "fn go() -> i64 { let _ = S { id: 1, name: f\"a\" }; return 7; }\n\
                 fn take() -> i64 { return go(); }",
                "dS1\nv=7\n",
            ),
            (
                "the bare-statement spelling",
                "fn go() -> i64 { S { id: 2, name: f\"b\" }; return 7; }\n\
                 fn take() -> i64 { return go(); }",
                "dS2\nv=7\n",
            ),
            (
                "a no-`else` `if` arm",
                "fn go(n: i64) -> i64 { let _ = if n == 0 { S { id: 11, name: f\"k\" } }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dS11\nv=7\n",
            ),
            (
                "a two-tail `if`",
                "fn go(n: i64) -> i64 { let _ = if n == 0 { S { id: 12, name: f\"m\" } } else { S { id: 13, name: f\"n\" } }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dS12\nv=7\n",
            ),
            (
                "the `match` spelling",
                "fn go(n: i64) -> i64 { let _ = match n { 0 => { S { id: 15, name: f\"q\" } } _ => { S { id: 16, name: f\"r\" } } }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dS15\nv=7\n",
            ),
            (
                "a bare-statement branch",
                "fn go(n: i64) -> i64 { if n == 0 { S { id: 14, name: f\"p\" } }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dS14\nv=7\n",
            ),
            (
                "the value arrives from a CALL",
                "fn go() -> i64 { let _ = mks(7); return 7; }\n\
                 fn take() -> i64 { return go(); }",
                "dS7\nv=7\n",
            ),
            // CONTROLS — the shapes that were already correct, and the ones
            // that pin the last-reference rule.
            (
                "control: the BOUND spelling",
                "fn go() -> i64 { let s = S { id: 3, name: f\"c\" }; return s.id + 4; }\n\
                 fn take() -> i64 { return go(); }",
                "dS3\nv=7\n",
            ),
            (
                "control: an ALIASED binding still runs ONE body",
                "fn go() -> i64 { let a = S { id: 5, name: f\"x\" }; let b = a; return b.id + 2; }\n\
                 fn take() -> i64 { return go(); }",
                "dS5\nv=7\n",
            ),
            (
                "control: `let _ = a;` over a live binding is unchanged",
                "fn go() -> i64 { let a = S { id: 9, name: f\"z\" }; let _ = a; return 7; }\n\
                 fn take() -> i64 { return go(); }",
                "dS9\nv=7\n",
            ),
            (
                "control: a live alias is NOT dropped by a sibling discard",
                "fn go() -> i64 { let a = S { id: 30, name: f\"w\" }; let b = a;\n\
                 let _ = S { id: 31, name: f\"v\" };\n\
                 return b.id - 23; }\n\
                 fn take() -> i64 { return go(); }",
                "dS31\ndS30\nv=7\n",
            ),
            (
                "control: a shared type with NO `impl Drop` stays silent",
                "fn go() -> i64 { let _ = T { id: 21 }; return 7; }\n\
                 fn take() -> i64 { return go(); }",
                "v=7\n",
            ),
        ];
    for (label, decls, want) in cases {
        let src = format!("{PRELUDE}{decls}\nfn main() {{ println(f\"v={{take()}}\"); }}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "{label}: interpreter");
        if let Some(aot) = run_program(&src) {
            assert_eq!(aot, *want, "{label}: body at the RC 0-transition");
        }
    }
}

/// B-2026-09-07-50 — a param READ AFTER a conditional store is RC-FALLBACK
/// PROMOTED (consume, then re-use), and `compile_function`'s param loop
/// boxed it and `continue`d past every registration below — including this
/// family's conditional-store one.
///
/// The statement order is the whole discriminator, and only `c1` draws
/// `perf[rc-fallback]: RC fallback inserted for 'r' (direct re-use after
/// consume)`. Pre-fix `c1` printed `s1` with no `dR1` on the JIT and both
/// AOT lanes against `--interp`'s `s1 dR1`, and lost 20 B in 2 blocks at
/// -O0 (12 allocs / 10 frees) — the value's `String` and its `shared`
/// field's refcount block, i.e. its whole heap.
///
/// `c2` (read BEFORE the store), `c3` (a trailing statement that does not
/// read the param) and `c4` (no trailing statement) are the three controls
/// that isolate it: none promotes, all three were correct throughout. `c5`
/// is the free-function spelling and `c6` a promoted param whose type has
/// no `Drop` of its own but does own heap, so the box's value-drop has to
/// free it without running a body.
#[test]
fn e2e_rc_promoted_param_keeps_its_drop_body_and_heap() {
    let Some(out) = run_program(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R {\n\
             \x20   fn drop(mut ref self) { println(f\"dR{self.id}\") }\n\
             }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             struct P { id: i64, name: String }\n\
             fn mkp(i: i64) -> P { return P { id: i, name: f\"p{i}\" }; }\n\
             struct Box2 { mut xs: Vec[R] }\n\
             impl Box2 {\n\
             \x20   fn after(mut ref self, r: R, k: bool) { if k { self.xs.push(r); } println(f\"s{r.inner.v}\"); }\n\
             \x20   fn before(mut ref self, r: R, k: bool) { println(f\"s{r.inner.v}\"); if k { self.xs.push(r); } }\n\
             \x20   fn trailing(mut ref self, r: R, k: bool) { if k { self.xs.push(r); } println(\"x\"); }\n\
             \x20   fn bare(mut ref self, r: R, k: bool) { if k { self.xs.push(r); } }\n\
             }\n\
             fn ff(v: mut ref Vec[R], r: R, k: bool) { if k { v.push(r); } println(f\"s{r.inner.v}\"); }\n\
             fn pf(v: mut ref Vec[P], p: P, k: bool) { if k { v.push(p); } println(f\"s{p.id}\"); }\n\
             fn main() {\n\
             \x20   let mut b = Box2 { xs: Vec.new() };\n\
             \x20   println(\"c1\");\n\
             \x20   b.after(mk(1), false);\n\
             \x20   println(\"c2\");\n\
             \x20   b.before(mk(2), false);\n\
             \x20   println(\"c3\");\n\
             \x20   b.trailing(mk(3), false);\n\
             \x20   println(\"c4\");\n\
             \x20   b.bare(mk(4), false);\n\
             \x20   println(\"c5\");\n\
             \x20   let mut v: Vec[R] = Vec.new();\n\
             \x20   ff(mut v, mk(5), false);\n\
             \x20   println(\"c6\");\n\
             \x20   let mut w: Vec[P] = Vec.new();\n\
             \x20   pf(mut w, mkp(6), false);\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(
        out,
        "c1\ns1\ndR1\nc2\ns2\ndR2\nc3\nx\ndR3\nc4\ndR4\nc5\ns5\ndR5\nc6\ns6\nend\n"
    );
}

/// B-2026-09-08-13 — a LET-BOUND LOCAL of a SELF-REFERENTIAL struct passed
/// BY VALUE into a callee that never hands it on lost its `Drop` body and
/// leaked its heap.
///
/// `move_declined_copy_struct_arg`'s self-referential arm (B-2026-07-28-3)
/// retracts the caller's cleanup on a MAY-analysis — the callee "receives
/// an ALIAS it may STORE into an owning container" — and the retraction was
/// unconditional, so it fired for a callee that stores nothing either.
/// Nothing then owns the value: the memory authority
/// (`struct_param_memory_stays_with_caller`) leaves the buffer with the
/// caller, and the callee's prologue agrees by listing the param in
/// `caller_retained_aggregate_memory`. The gate now asks the CALLEE
/// (`declined_copy_arg_stays_with_caller`) and stands down only when it
/// provably returns, stores and forwards nothing.
///
/// Cells, and what each one isolates:
///
///   * `c1` is the row's own shape — the defect. Pre-fix: `v10` on the JIT
///     and at both opt levels against `--interp`'s `v10 dN10`, valgrind
///     `3 bytes in 1 blocks` definitely lost (the `tag` buffer).
///   * `c2` is the TEMPORARY spelling, correct throughout: the retraction
///     only matches an `Identifier` argument, so the fresh-temp registrar
///     kept its own cleanup. It also pins the ORDER difference between the
///     two spellings, which is what makes them distinguishable at all.
///   * `c3`/`c4` are the never-passed and by-`ref` controls, correct
///     throughout — they show the defect needs the by-value pass.
///   * `c5` is the METHOD spelling and `c6` the ASSOC-FN spelling, both
///     defective pre-fix for the same reason and both wired here.
///   * `c7` (always returns), `c8` (stores into a `mut ref`) and `c9`
///     (conditionally stores, taking the MISS path) are the hazards this
///     narrowing must NOT admit: restoring the caller's drop for any of
///     them would be a second owner of one value. They were correct before
///     this change and must stay so.
#[test]
fn e2e_declined_copy_arg_keeps_its_owner_when_the_callee_drops_it() {
    let Some(out) = run_program(
        "struct Node { id: i64, next: Option[Node], tag: String }\n\
             impl Drop for Node {\n\
             \x20   fn drop(mut ref self) { println(f\"dN{self.id}\") }\n\
             }\n\
             fn mkn(i: i64) -> Node { return Node { id: i, next: Option.None, tag: f\"t{i}\" }; }\n\
             fn read(n: Node) -> i64 { return n.id; }\n\
             fn peek(n: ref Node) -> i64 { return n.id; }\n\
             fn pass(n: Node) -> Node { return n; }\n\
             fn stash(n: Node, v: mut ref Vec[Node]) { v.push(n); }\n\
             fn cs(n: Node, k: bool, v: mut ref Vec[Node]) { if k { v.push(n); } }\n\
             struct H { z: i64 }\n\
             impl H {\n\
             \x20   fn eat(ref self, n: Node) -> i64 { return n.id + self.z; }\n\
             \x20   fn eatA(n: Node) -> i64 { return n.id; }\n\
             }\n\
             fn main() {\n\
             \x20   println(\"c1\");\n\
             \x20   let c = mkn(10);\n\
             \x20   println(f\"v{read(c)}\");\n\
             \x20   println(\"c2\");\n\
             \x20   println(f\"v{read(mkn(20))}\");\n\
             \x20   println(\"c3\");\n\
             \x20   let e = mkn(30);\n\
             \x20   println(f\"v{e.id}\");\n\
             \x20   println(\"c4\");\n\
             \x20   let g = mkn(40);\n\
             \x20   println(f\"v{peek(g)}\");\n\
             \x20   println(\"c5\");\n\
             \x20   let h = H { z: 1 };\n\
             \x20   let i2 = mkn(50);\n\
             \x20   println(f\"v{h.eat(i2)}\");\n\
             \x20   println(\"c6\");\n\
             \x20   let j = mkn(60);\n\
             \x20   println(f\"v{H.eatA(j)}\");\n\
             \x20   println(\"c7\");\n\
             \x20   let k = mkn(70);\n\
             \x20   let l = pass(k);\n\
             \x20   println(f\"v{l.id}\");\n\
             \x20   println(\"c8\");\n\
             \x20   let mut v1: Vec[Node] = Vec.new();\n\
             \x20   let m = mkn(80);\n\
             \x20   stash(m, mut v1);\n\
             \x20   println(f\"n{v1.len()}\");\n\
             \x20   println(\"c9\");\n\
             \x20   let mut v2: Vec[Node] = Vec.new();\n\
             \x20   let p = mkn(90);\n\
             \x20   cs(p, false, mut v2);\n\
             \x20   println(f\"n{v2.len()}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "c1\nv10\ndN10\n\
             c2\ndN20\nv20\n\
             c3\nv30\ndN30\n\
             c4\nv40\ndN40\n\
             c5\nv51\ndN50\n\
             c6\nv60\ndN60\n\
             c7\nv70\ndN70\n\
             c8\nn1\ndN80\n\
             c9\ndN90\nn0\n\
             end\n"
    );
}

/// B-2026-09-15-5 — a map/set LOOKUP key temporary's user `Drop` body runs at
/// the lookup, which is where its live range ends.
///
/// A lookup BORROWS its key and discards it. The reclaim added by
/// B-2026-08-26-32 / B-2026-09-13-20 / B-2026-09-13-30 resolves a MEMORY walk at
/// each of the five lookup entry points; a type's user `Drop` hook is a SEPARATE
/// `Type.drop` call that every other drop site pairs with its walk, and the key
/// sites emitted the walk alone. So a key's storage was reclaimed exactly once
/// and its body never ran.
///
/// design.md § Drop is what makes the lookup the OWED position rather than merely
/// an early one: destructors fire at a value's live-range end, not at lexical
/// scope end, and "a value whose last use is mid-scope is dropped at that use".
/// A key temporary's last use IS the lookup.
///
/// A BOTH-BACKENDS GAP, not a run/build divergence — measured byte-identical on
/// `karac run --interp` and `karac build` before the fix, which is why an A/B
/// kata could never have caught it and why this fixture is PAIRED instead.
/// Invisible to ASAN and to both ratchet legs too, since storage is freed exactly
/// once either way; only an output comparison sees it.
///
/// THE FIVE CONTROL CELLS ARE WHAT LOCALIZE IT. A fresh temp's body already ran
/// at an ordinary consuming position, at a bare discard, at a binding's
/// live-range end, at container destruction, and for an inserted key — so the
/// machinery worked everywhere except this one position, and the fix belongs at
/// the key sites rather than in any walker. They are also the cells that fail if
/// the fix ever DOUBLES a body.
///
/// THE `dD1` POSITION IN EVERY MAP CELL IS THE LIVE-RANGE RULE, NOT AN ODDITY,
/// and predicting it wrong is easy: the stored element's body fires BEFORE
/// `post`, not at the end of `main`, because the map's own last use is the
/// lookup, so the map dies there too. Every `want` here was measured and then
/// checked against that rule rather than assumed from lexical nesting.
///
/// A TUPLE key needs its own arm and nearly became a regression: the
/// `TypeKind::Path`-keyed leaf walker declines a nameless type, while the
/// interpreter's value-driven walk recurses into tuple elements — so fixing only
/// the named case would have traded a symmetric gap for a run/build divergence,
/// which is strictly worse. Cells 5 and 6 pin both tuple spellings.
///
/// NOT FIXED HERE, and deliberately not asserted: an enum stored in a `Map` or
/// `Set` never runs its user `Drop` body at all — `s.insert(mke(0))` with no
/// lookup anywhere prints nothing, while `Vec[Tg]` is correct. That is a
/// STORED-ELEMENT defect independent of this row's lookup question, filed
/// separately; the lookup half of the enum spelling IS fixed by this change.
#[test]
fn e2e_a_lookup_key_temporarys_user_drop_body_runs_at_the_lookup() {
    let hdr = "#[derive(Hash, Eq, PartialEq)]\n\
                   struct Dk { a: String, b: i64 }\n\
                   impl Drop for Dk { fn drop(mut ref self) { println(f\"dD{self.b}\") } }\n\
                   #[derive(Hash, Eq, PartialEq)]\n\
                   struct Nest { i: Dk, b: i64 }\n\
                   struct Mk { p: String }\n\
                   impl Mk { fn mkd(ref self, n: i64) -> Dk { return Dk { a: f\"heap-{n}\", b: n }; } }\n\
                   impl Mk { fn build(n: i64) -> Dk { return Dk { a: f\"heap-{n}\", b: n }; } }\n\
                   fn mkd(n: i64) -> Dk { return Dk { a: f\"heap-{n}\", b: n }; }\n\
                   fn mkn(n: i64) -> Nest { return Nest { i: mkd(n), b: n }; }\n\
                   fn mkt(n: i64) -> (Dk, i64) { return (mkd(n), n); }\n\
                   fn eat(d: Dk) -> i64 { return d.b; }\n";
    for (label, stmts, want) in [
            (
                "the row's shape: a struct key at Map.get",
                "let mut m: Map[Dk, i64] = Map.new();\n\
                 m.insert(mkd(1), 1);\n\
                 println(\"pre\");\n\
                 match m.get(mkd(2)) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
                 println(\"post\");\n",
                "pre\ndD2\nmiss\ndD1\npost\nend\n",
            ),
            (
                "a Drop-bearing field ONE LEVEL DOWN -- the row's second question",
                "let mut m: Map[Nest, i64] = Map.new();\n\
                 m.insert(mkn(1), 1);\n\
                 println(\"pre\");\n\
                 match m.get(mkn(2)) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
                 println(\"post\");\n",
                "pre\ndD2\nmiss\ndD1\npost\nend\n",
            ),
            (
                "a METHOD-call key, the spelling B-2026-09-13-30 made memory-clean",
                "let mut m: Map[Dk, i64] = Map.new();\n\
                 let g: Mk = Mk { p: f\"x\" };\n\
                 m.insert(g.mkd(1), 1);\n\
                 println(\"pre\");\n\
                 match m.get(g.mkd(2)) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
                 println(\"post\");\n",
                "pre\ndD2\nmiss\ndD1\npost\nend\n",
            ),
            (
                "an ASSOC-FN key: a Path callee, through the same shared resolver",
                "let mut m: Map[Dk, i64] = Map.new();\n\
                 m.insert(Mk.build(1), 1);\n\
                 println(\"pre\");\n\
                 match m.get(Mk.build(2)) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
                 println(\"post\");\n",
                "pre\ndD2\nmiss\ndD1\npost\nend\n",
            ),
            (
                "a TUPLE key from a call -- nameless, so the per-ELEMENT walker",
                "let mut m: Map[(Dk, i64), i64] = Map.new();\n\
                 m.insert(mkt(1), 1);\n\
                 println(\"pre\");\n\
                 match m.get(mkt(2)) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
                 println(\"post\");\n",
                "pre\ndD2\nmiss\ndD1\npost\nend\n",
            ),
            (
                "a TUPLE LITERAL key, fresh by construction",
                "let mut m: Map[(Dk, i64), i64] = Map.new();\n\
                 m.insert((mkd(1), 1), 1);\n\
                 println(\"pre\");\n\
                 match m.get((mkd(2), 2)) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
                 println(\"post\");\n",
                "pre\ndD2\nmiss\ndD1\npost\nend\n",
            ),
            (
                "Set.contains",
                "let mut s: Set[Dk] = Set.new();\n\
                 s.insert(mkd(1));\n\
                 println(\"pre\");\n\
                 if s.contains(mkd(2)) { println(\"hit\"); } else { println(\"miss\"); }\n\
                 println(\"post\");\n",
                "pre\ndD2\nmiss\ndD1\npost\nend\n",
            ),
            (
                "contains_key + remove: THREE bodies -- two key temps and the stored key",
                "let mut m: Map[Dk, i64] = Map.new();\n\
                 m.insert(mkd(1), 1);\n\
                 println(\"pre\");\n\
                 if m.contains_key(mkd(2)) { println(\"has\"); } else { println(\"no\"); }\n\
                 println(\"mid\");\n\
                 m.remove(mkd(1));\n\
                 println(\"post\");\n",
                "pre\ndD2\nno\nmid\ndD1\ndD1\npost\nend\n",
            ),
            (
                "CONTROL: an ordinary consuming position was always correct",
                "println(\"pre\");\n\
                 let r: i64 = eat(mkd(3));\n\
                 println(f\"r{r}\");\n\
                 println(\"post\");\n",
                "pre\ndD3\nr3\npost\nend\n",
            ),
            (
                "CONTROL: a bare discard was always correct",
                "println(\"pre\");\n\
                 mkd(4);\n\
                 println(\"post\");\n",
                "pre\ndD4\npost\nend\n",
            ),
            (
                "CONTROL: a BOUND key -- the body is the binding's, at ITS live-range end",
                "let mut m: Map[Dk, i64] = Map.new();\n\
                 m.insert(mkd(1), 1);\n\
                 println(\"pre\");\n\
                 let k: Dk = mkd(2);\n\
                 match m.get(k) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
                 println(\"post\");\n",
                "pre\nmiss\ndD2\ndD1\npost\nend\n",
            ),
            (
                "CONTROL: insert MOVES its key -- one body, at the map's destruction",
                "let mut m: Map[Dk, i64] = Map.new();\n\
                 println(\"pre\");\n\
                 m.insert(mkd(1), 1);\n\
                 println(\"post\");\n",
                "pre\ndD1\npost\nend\n",
            ),
            (
                "CONTROL: Vec storage was always correct",
                "let mut v: Vec[Dk] = Vec.new();\n\
                 println(\"pre\");\n\
                 v.push(mkd(5));\n\
                 println(\"post\");\n",
                "pre\ndD5\npost\nend\n",
            ),
        ] {
            let src = format!("{hdr}fn main() {{\n{stmts}\nprintln(\"end\");\n}}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
        }
}
