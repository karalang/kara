//! B-2026-09-20-17 -- a whole rebind of a callee-owned `Option`/`Result`
//! tuple payload (`Some(t) => { let u = t; return u.0; }`) is the payload
//! under another name, so a part taken off it leaves the payload walk.

use super::*;

/// B-2026-09-20-17 — `Some(t) => { let u = t; return u.0; }` over a by-value
/// `Option[(H, H)]` ran element 0's body twice on all four surfaces
/// (`dH5 dH6 got:5 dH5 end` for a due `dH6 got:5 dH5 end`). Both backends
/// asked which parts the arm takes off `t` alone, and `let u = t` takes
/// nothing off `t`, so the walk stayed whole while the caller owned `u.0`.
/// They now follow whole rebinds (`binding_use::optres_arm_moved_tuple_paths`
/// for codegen, the caller mask's `escaping_param_payload_part_paths_impl`
/// for the interpreter).
///
/// The mask is static, so a rebind is followed only when the arm cannot leave
/// before its move and takes no owning rebind part on some paths only. The
/// `control:` cells were correct before the fix and are where following the
/// rebind unconditionally lost a body (`taken-under-if-not-taken`, and
/// `early-exit-taken` on the interpreter). Every other cell doubled a body on
/// both backends before the fix. Each is checked against the interpreter and
/// against the hand-derived sequence, since the backends agreed while wrong.
#[test]
fn rebind_of_boxed_payload_then_projected_runs_each_body_once() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "projected",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> H { match o { Option.Some(t) => { let u = t; return u.0; } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.id}");
    println("end");
}
"#,
            "dH6\ngot:5\ndH5\nend\n",
        ),
        (
            "result-head",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Result[(H, H), i64]) -> H { match o { Result.Ok(t) => { let u = t; return u.0; } Result.Err(_) => { return mkh(0); } } }
fn main() {
    let got = eat(Result.Ok((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.id}");
    println("end");
}
"#,
            "dH6\ngot:5\ndH5\nend\n",
        ),
        (
            "two-rebinds",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> H { match o { Option.Some(t) => { let u = t; let w = u; return w.0; } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.id}");
    println("end");
}
"#,
            "dH6\ngot:5\ndH5\nend\n",
        ),
        (
            "tail",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> H { match o { Option.Some(t) => { let u = t; u.0 } Option.None => mkh(0), } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.id}");
    println("end");
}
"#,
            "dH6\ngot:5\ndH5\nend\n",
        ),
        (
            "other-element",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> H { match o { Option.Some(t) => { let u = t; return u.1; } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.id}");
    println("end");
}
"#,
            "dH5\ngot:6\ndH6\nend\n",
        ),
        (
            "nested-part",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, (H, H))]) -> H { match o { Option.Some(t) => { let u = t; return u.1.0; } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, (H { id: 6, s: "bbbbbbbbbbbb" }, H { id: 7, s: "cccccccccccc" })))); println(f"got:{got.id}");
    println("end");
}
"#,
            "dH5\ndH7\ngot:6\ndH6\nend\n",
        ),
        (
            "read-then-take",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> H { match o { Option.Some(t) => { let u = t; println(f"r{u.0.id}"); return u.1; } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" }))); println(f"got:{got.id}");
    println("end");
}
"#,
            "r5\ndH5\ngot:6\ndH6\nend\n",
        ),
        (
            "scalar-read-in-loop",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> H { match o { Option.Some(t) => { let u = t; let mut i = 0; while i < 2 { println(f"i{u.1.id}"); i = i + 1; } return u.0; } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" }))); println(f"got:{got.id}");
    println("end");
}
"#,
            "i6\ni6\ndH6\ngot:5\ndH5\nend\n",
        ),
        (
            "named-argument",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> H { match o { Option.Some(t) => { let u = t; return u.0; } Option.None => { return mkh(0); } } }
fn main() {
    let a = Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })); let got = eat(a); println(f"got:{got.id}");
    println("end");
}
"#,
            "dH6\ngot:5\ndH5\nend\n",
        ),
        (
            "control:taken-under-if-not-taken",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)], c: bool) -> H { match o { Option.Some(t) => { let u = t; if c { return u.0; } return mkh(9); } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })), false); println(f"got:{got.id}");
    println("end");
}
"#,
            "dH5\ndH6\ngot:9\ndH9\nend\n",
        ),
        (
            "control:early-exit-taken",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)], c: bool) -> H { match o { Option.Some(t) => { let u = t; if c { return mkh(9); } return u.0; } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })), true); println(f"got:{got.id}");
    println("end");
}
"#,
            "dH5\ndH6\ngot:9\ndH9\nend\n",
        ),
        (
            "control:rebind-returned-whole",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> (H, H) { match o { Option.Some(t) => { let u = t; return u; } Option.None => { return (mkh(0), mkh(1)); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" }))); println(f"got:{got.0.id}");
    println("end");
}
"#,
            "got:5\ndH5\ndH6\nend\n",
        ),
        (
            "control:rebind-forwarded",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn sink(u: (H, H)) -> i64 { return u.0.id } fn eat(o: Option[(H, H)]) -> i64 { match o { Option.Some(t) => { let u = t; return sink(u); } Option.None => { return 0; } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got}");
    println("end");
}
"#,
            "dH5\ndH6\ngot:5\nend\n",
        ),
    ];
    for (label, prog, want) in cells {
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(prog);
        assert!(
            interp_errs.is_empty(),
            "[{label}] interp errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "[{label}] interpreter");
        let Some(aot) = run_program(prog) else {
            continue;
        };
        assert_eq!(aot, *want, "[{label}] AOT");
    }
}
