//! files, processes, CLI, environment, sockets, time -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer io_runtime::
//!
//! New fixtures about files, processes, CLI, environment, sockets, time belong in this file.

use super::*;

/// B-2026-09-07-22 — the MEMORY half of the method and assoc spellings:
/// one owner and one free per object on both legs of the branch, on both
/// call legs.
#[test]
fn asan_mixed_path_hop_on_a_method() {
    assert_clean_asan_run(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             fn fwd(r: R) -> R { return r; }\n\
             struct Hold { n: i64 }\n\
             impl Hold { fn pick2(ref self, r: R, k: bool) -> R { if k { return mk(90); } return fwd(r); } }\n\
             impl R { fn picka2(r: R, k: bool) -> R { if k { return mk(91); } return fwd(r); } }\n\
             impl Hold { fn pick(ref self, r: R, k: bool) -> R { if k { return mk(92); } return r; } }\n\
             fn main() {\n\
               let h = Hold { n: 0 };\n\
               println(\"method_hop_handback\"); let a = h.pick2(mk(1), false); println(f\"  v={a.inner.v}\");\n\
               println(\"method_hop_dies\"); let b = h.pick2(mk(2), true); println(f\"  v={b.id}\");\n\
               println(\"assoc_hop_handback\"); let c = R.picka2(mk(3), false); println(f\"  v={c.inner.v}\");\n\
               println(\"assoc_hop_dies\"); let d = R.picka2(mk(4), true); println(f\"  v={d.id}\");\n\
               println(\"method_nohop_handback\"); let e = h.pick(mk(5), false); println(f\"  v={e.inner.v}\");\n\
               println(\"named_into_method_hop\"); let g = mk(6); let n = h.pick2(g, false); println(f\"  v={n.inner.v}\");\n\
               println(\"end\");\n\
             }\n",
            &[
                "method_hop_handback",
                "  v=1",
                "  dR1",
                "method_hop_dies",
                "  dR2",
                "  v=90",
                "  dR90",
                "assoc_hop_handback",
                "  v=3",
                "  dR3",
                "assoc_hop_dies",
                "  dR4",
                "  v=91",
                "  dR91",
                "method_nohop_handback",
                "  v=5",
                "  dR5",
                "named_into_method_hop",
                "  v=6",
                "  dR6",
                "end"
            ],
            "mixed_path_hop_on_a_method",
        );
}

/// B-2026-09-07-15 — the MEMORY half: the same program under ASAN + LSan,
/// where the pre-fix build double-freed the object handed back through the
/// hop. One owner and one free per object on BOTH legs of the branch, which
/// is the property the per-path flag exists to hold.
#[test]
fn asan_mixed_path_hand_back_through_a_hop() {
    assert_clean_asan_run(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             struct P { id: i64, name: String, xs: Vec[i64] }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"  dP{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             fn mkp(i: i64) -> P { return P { id: i, name: f\"p{i}\", xs: [i] }; }\n\
             fn f(r: R) -> R { return r; }\n\
             fn pf(p: P) -> P { return p; }\n\
             fn mvia(r: R, c: bool) -> R { if c { return f(r); } return mk(90); }\n\
             fn cvia(r: R, c: bool) -> R { if c { return r; } return mk(91); }\n\
             fn pvia(p: P, c: bool) -> P { if c { return pf(p); } return mkp(92); }\n\
             fn main() {\n\
               println(\"hop_handback\"); let a = mk(1); let z = mvia(a, true); println(f\"  v={z.inner.v}\");\n\
               println(\"hop_dies_inside\"); let b = mk(2); let y = mvia(b, false); println(f\"  v={y.id}\");\n\
               println(\"hop_handback_fresh\"); let w = mvia(mk(3), true); println(f\"  v={w.inner.v}\");\n\
               println(\"hop_dies_fresh\"); let x = mvia(mk(4), false); println(f\"  v={x.id}\");\n\
               println(\"nohop_handback\"); let c = mk(5); let u = cvia(c, true); println(f\"  v={u.inner.v}\");\n\
               println(\"nohop_dies_inside\"); let d = mk(6); let t = cvia(d, false); println(f\"  v={t.id}\");\n\
               println(\"copyable_hop_handback\"); let e = mkp(7); let s = pvia(e, true); println(f\"  v={s.id}\");\n\
               println(\"copyable_hop_dies\"); let g = mkp(8); let r = pvia(g, false); println(f\"  v={r.id}\");\n\
               println(\"end\");\n\
             }\n",
            &[
                "hop_handback",
                "  v=1",
                "  dR1",
                "hop_dies_inside",
                "  dR2",
                "  v=90",
                "  dR90",
                "hop_handback_fresh",
                "  v=3",
                "  dR3",
                "hop_dies_fresh",
                "  dR4",
                "  v=90",
                "  dR90",
                "nohop_handback",
                "  v=5",
                "  dR5",
                "nohop_dies_inside",
                "  dR6",
                "  v=91",
                "  dR91",
                "copyable_hop_handback",
                "  v=7",
                "  dP7",
                "copyable_hop_dies",
                "  dP8",
                "  v=92",
                "  dP92",
                "end"
            ],
            "mixed_path_hand_back_through_a_hop",
        );
}

/// B-2026-09-07-3, the VIA-CALL half — the MEMORY side of
/// `test_e2e_free_fn_mixed_path_hand_back_through_a_hop_owns_its_argument`.
///
/// A MIXED-PATH callee handing its by-value param back through ONE HOP
/// (`if k { return mk(92); } return fwd(r); }`) stranded 19 bytes in 2
/// blocks on the DIES-INSIDE leg: the caller stood down through
/// `fn_returns_param_via_call` while the callee registered nothing, because
/// `fn_conditionally_returns_param_bare` declined a leaf that reaches the
/// param through a call. Two frames each deferring to the other. It took
/// `6ef13bb` (per-path memory ownership for the declined-copy class) and
/// `99bd72d` (the one-hop leaf test plus the per-path disarm) together.
///
/// The A/B string caught the lost `Drop` body; only LSan catches the bytes,
/// which is why the leg is pinned here as well.
///
/// Floored because this class hides under DCE exactly as its method/assoc
/// sibling above does: an allocation whose only consumer hands it straight
/// back is what LLVM deletes at -O2, and a collapsed program reads clean
/// with nothing left to free.
#[test]
fn asan_free_fn_mixed_path_hand_back_through_a_hop_owns_its_argument() {
    assert_clean_asan_run_min_allocs(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             fn fwd(r: R) -> R { return r; }\n\
             struct Hold { n: i64 }\n\
             impl Hold { fn pv(ref self, r: R, k: bool) -> R { if k { return mk(90); } return fwd(r); } }\n\
             impl R { fn av(r: R, k: bool) -> R { if k { return mk(91); } return fwd(r); } }\n\
             fn fv(r: R, k: bool) -> R { if k { return mk(92); } return fwd(r); }\n\
             fn c1() { let z = fv(mk(11), false); println(f\"c1={z.id}\"); }\n\
             fn c2() { let z = fv(mk(12), true); println(f\"c2={z.id}\"); }\n\
             fn c3() { let a = mk(13); let z = fv(a, false); println(f\"c3={z.id}\"); }\n\
             fn c4() { let a = mk(14); let z = fv(a, true); println(f\"c4={z.id}\"); }\n\
             fn c5() { let h = Hold { n: 1 }; let z = h.pv(mk(15), false); println(f\"c5={z.id}\"); }\n\
             fn c6() { let h = Hold { n: 1 }; let z = h.pv(mk(16), true); println(f\"c6={z.id}\"); }\n\
             fn c7() { let z = R.av(mk(17), false); println(f\"c7={z.id}\"); }\n\
             fn c8() { let z = R.av(mk(18), true); println(f\"c8={z.id}\"); }\n\
             fn main() { c1(); c2(); c3(); c4(); c5(); c6(); c7(); c8(); println(\"end\"); }\n",
            &[
                "c1=11", "dR11", "dR12", "c2=92", "dR92", "c3=13", "dR13", "dR14", "c4=92",
                "dR92", "c5=15", "dR15", "dR16", "c6=90", "dR90", "c7=17", "dR17", "dR18",
                "c8=91", "dR91", "end",
            ],
            "free_fn_mixed_path_hand_back_through_a_hop",
            40,
        );
}

/// The over-fire direction of B-2026-08-09-20, and the reason the new drops
/// null their slot: `karac_runtime_file_close` reconstructs the `Box` and
/// drops it, so it is NOT idempotent — two owners means a double free, not a
/// harmless second call.
///
/// Three shapes that each hand the handle to a second candidate owner:
/// the struct crosses a function return (B-2026-08-09-17's own pin, here in
/// a loop so a double free has 40 chances to land), an element is `pop`ped
/// out (the `Some(g)` binding owns it and the `0..len` drain must not
/// revisit it), and a field is read into a fresh binding while the struct is
/// still live.
#[test]
fn asan_file_handed_to_a_second_owner_is_closed_exactly_once() {
    let path = file_fixture_path("file_second_owner");
    assert_clean_asan_run(
        &format!(
            r#"
struct Holder {{ f: File }}
fn make(path: String) -> Holder with reads(FileSystem) panics {{
    match File.open(path) {{
        Ok(fh) => Holder {{ f: fh }},
        Err(_) => panic("open-failed"),
    }}
}}
fn main() with reads(FileSystem) writes(FileSystem) panics {{
    let mut buf: Array[u8, 4] = [0u8; 4];
    let mut i: i64 = 0i64;
    let mut n: i64 = 0i64;
    while i < 40i64 {{
        // (a) moved through a return, then dropped by the caller's binding.
        let h = make("{path}");
        match h.f.read(mut buf) {{ Ok(_) => {{ n = n + 1i64; }} Err(_) => {{}} }}
        // (b) popped out — ownership moves to the `Some` binding.
        let mut hs: Vec[File] = Vec.new();
        match File.open("{path}") {{ Ok(f) => {{ hs.push(f); }} Err(_) => {{}} }}
        match hs.pop() {{
            Some(g) => {{ match g.read(mut buf) {{ Ok(_) => {{ n = n + 1i64; }} Err(_) => {{}} }} }}
            None => {{}}
        }}
        // (c) field read into a fresh binding while the struct is still live.
        let h2 = make("{path}");
        let g2 = h2.f;
        match g2.read(mut buf) {{ Ok(_) => {{ n = n + 1i64; }} Err(_) => {{}} }}
        i = i + 1;
    }}
    println(n.to_string());
}}
"#
        ),
        &["120"],
        "file_second_owner_closed_once",
    );
}

/// B-2026-08-25-9. A TEMPORARY passed as the path argument to a
/// `#[compiler_builtin]` fs entry point was never freed.
///
/// The runtime side borrows — `karac_runtime_file_open` / `_fs_read_to_
/// string` / `_fs_write` all take `(ptr, len)` and copy into a
/// PathBuf/String on the Rust side — so nothing downstream ever owned the
/// caller's buffer. Under the OWNED signature the front end treated the
/// temporary as moved into the callee and emitted no release; the callee
/// never took ownership. ~47 bytes per call, on both the owned and the
/// `ref String` signature (B-2026-08-24-24 was leak-NEUTRAL here).
///
/// HERMETIC BY DESIGN: the path does not exist and every call returns
/// `Err`. That is deliberate — the leak is in ARGUMENT handling, which
/// happens whether or not the open succeeds, so the fixture needs no
/// filesystem state and cannot flake on a sandbox that forbids writes.
///
/// Covers BOTH lowering paths, which needed separate fixes, and both
/// temporary shapes (a function return and a `.clone()`):
///
/// - `fs.read_to_string(..)` — the lowercase ambient-alias path, compiled
///   in `compile_ambient_resource_method`.
/// - `File.open(..)` — the capitalized associated-call path, compiled in
///   `compile_file_constructor`.
///
/// The plain BINDING argument (`f(p)`) is here as the counter-case: it
/// must NOT be freed, and `p` is used after every call. Freeing it would
/// be a use-after-free rather than a leak, which is the failure mode this
/// fix could plausibly have introduced.
#[test]
fn asan_builtin_fs_call_frees_temporary_path_argument() {
    assert_clean_asan_run(
        r#"
fn mk() -> String { "no-such-file-b20260825-9.txt" }
fn main() {
    let p = "no-such-file-b20260825-9.txt";
    let mut n: i64 = 0;
    // temporaries: must be freed by the caller
    match fs.read_to_string(mk()) { Ok(s) => { n = n + s.len(); } Err(e) => { n = n + 1; } }
    match fs.read_to_string(p.clone()) { Ok(s) => { n = n + s.len(); } Err(e) => { n = n + 1; } }
    match File.open(mk()) { Ok(f) => { n = n + 2; } Err(e) => { n = n + 1; } }
    match File.open(p.clone()) { Ok(f) => { n = n + 2; } Err(e) => { n = n + 1; } }
    // binding: must NOT be freed — `p` is still live below
    match fs.read_to_string(p) { Ok(s) => { n = n + s.len(); } Err(e) => { n = n + 1; } }
    println(f"n={n} p={p.len()}");
}
"#,
        &["n=5 p=28"],
        "builtin-fs-temp-path-arg",
    );
}

/// B-2026-08-25-2 — `std.cli`'s method bodies under ASAN.
///
/// This module's bodies had never been AOT-compiled before the usage gate
/// landed, so every heap path in them is newly exercised machine code. The
/// module is also the one that produced B-2026-08-25-15 (a borrowed match
/// payload stored into an owned field, double-freeing on the compiled side
/// only) and B-2026-08-25-13 (four borrowed-String-into-owned-field stores
/// that shipped because the baked-module typecheck errors were discarded).
/// Both were memory corruption in exactly this code, found only by running
/// it — so shipping its bodies without an ASAN run would be repeating the
/// mistake that made those two rows expensive.
///
/// The builder chain allocates a `String` per hop and pushes owned entries
/// into three `Vec`s; `parse()` then walks argv, builds the help text by
/// repeated concatenation, and drops the whole tree. `help_text()` is
/// called explicitly because it is the heaviest String producer in the
/// module and the one whose two `Parser` self-copies the typechecker
/// reports as ambiguous.
#[test]
fn asan_stdlib_cli_builder_and_parse_are_clean() {
    let src = r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let p = Parser.new("greet")
            .about("greets someone")
            .version("2.1")
            .arg("--name", Arg.string().required().help("who to greet"))
            .flag("--loud", 'l', "shout it");
        match p.parse() {
            Ok(args) => { println("parsed"); }
            Err(e) => { println(f"err {e.message}"); }
        }
        let h = p.help_text();
        println(f"len {h.len() > 0}");
        i = i + 1;
    }
}
"#;
    assert_clean_asan_run(
        src,
        &[
            "err missing required argument",
            "len true",
            "err missing required argument",
            "len true",
            "err missing required argument",
            "len true",
        ],
        "stdlib-cli-builder-and-parse",
    );
}
