//! B-2026-09-20-19 -- a payload part moved into a local of the arm's own
//! frame and handed out as the arm's last act (`let x = t.0; ..; return x`)
//! leaves the caller's walk exactly as `return t.0` does.

use super::*;

/// B-2026-09-20-19 — `Some(t) => { let x = t.0; println("mid"); return x; }`
/// over a by-value `Option[(T, T)]` ran element 0's body twice interpreted at
/// both payload widths (`mid dH5 dH6 got:5 dH5 end` for a due `mid dH6 got:5
/// dH5 end`), and at the INLINE width the compiled side lost element 1's body
/// (`mid got:5 dR5 end`). The caller-side channel that masks a part the callee
/// hands out (`fn_escaping_param_payload_part_paths`, consulted by the
/// interpreter and by codegen's inline-width caller walk alike) knew
/// `return t.0` and not the same part through a local; it now follows an
/// immutable top-level `let x = <part>` whose hand-out is the arm's only exit.
///
/// The `control:` cells were correct before and after: a conditional exit
/// before the hand-out, and a local that dies in the frame. Every other cell
/// was wrong on at least one backend before the fix.
#[test]
fn part_moved_into_arm_local_and_returned_runs_each_body_once() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "boxed",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> H { match o { Option.Some(t) => { let x = t.0; println("mid"); return x; } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" }))); println(f"got:{got.id}");
    println("end");
}
"#,
            "mid\ndH6\ngot:5\ndH5\nend\n",
        ),
        (
            "inline",
            r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkh(i: i64) -> R { return R { id: i } }
fn eat(o: Option[(R, R)]) -> R { match o { Option.Some(t) => { let x = t.0; println("mid"); return x; } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((R { id: 5 }, R { id: 6 }))); println(f"got:{got.id}");
    println("end");
}
"#,
            "mid\ndR6\ngot:5\ndR5\nend\n",
        ),
        (
            "boxed-tail",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> H { match o { Option.Some(t) => { let x = t.0; println("mid"); x } Option.None => mkh(0), } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" }))); println(f"got:{got.id}");
    println("end");
}
"#,
            "mid\ndH6\ngot:5\ndH5\nend\n",
        ),
        (
            "boxed-result-head",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Result[(H, H), i64]) -> H { match o { Result.Ok(t) => { let x = t.1; println("mid"); return x; } Result.Err(_) => { return mkh(0); } } }
fn main() {
    let got = eat(Result.Ok((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" }))); println(f"got:{got.id}");
    println("end");
}
"#,
            "mid\ndH5\ngot:6\ndH6\nend\n",
        ),
        (
            "boxed-nested-two-locals",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, (H, H))]) -> H { match o { Option.Some(t) => { let a = t.1.0; let b = t.1.1; println(f"n:{b.id}"); return a; } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, (H { id: 6, s: "bbbbbbbbbbbb" }, H { id: 7, s: "cccccccccccc" })))); println(f"got:{got.id}");
    println("end");
}
"#,
            "n:7\ndH7\ndH5\ngot:6\ndH6\nend\n",
        ),
        (
            "boxed-read-then-returned",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> H { match o { Option.Some(t) => { let x = t.0; println(f"r{x.id}"); return x; } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" }))); println(f"got:{got.id}");
    println("end");
}
"#,
            "r5\ndH6\ngot:5\ndH5\nend\n",
        ),
        (
            "boxed-through-rebind",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> H { match o { Option.Some(t) => { let u = t; let x = u.0; return x; } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" }))); println(f"got:{got.id}");
    println("end");
}
"#,
            "dH6\ngot:5\ndH5\nend\n",
        ),
        (
            "boxed-named-argument",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> H { match o { Option.Some(t) => { let x = t.0; println("mid"); return x; } Option.None => { return mkh(0); } } }
fn main() {
    let a = Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })); let got = eat(a); println(f"got:{got.id}");
    println("end");
}
"#,
            "mid\ndH6\ngot:5\ndH5\nend\n",
        ),
        (
            "inline-named-argument",
            r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkh(i: i64) -> R { return R { id: i } }
fn eat(o: Option[(R, R)]) -> R { match o { Option.Some(t) => { let x = t.0; println("mid"); return x; } Option.None => { return mkh(0); } } }
fn main() {
    let a = Option.Some((R { id: 5 }, R { id: 6 })); let got = eat(a); println(f"got:{got.id}");
    println("end");
}
"#,
            "mid\ndR6\ngot:5\ndR5\nend\n",
        ),
        (
            "control:early-exit-taken",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)], c: bool) -> H { match o { Option.Some(t) => { let x = t.0; if c { return mkh(9); } return x; } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })), true); println(f"got:{got.id}");
    println("end");
}
"#,
            "dH5\ndH6\ngot:9\ndH9\nend\n",
        ),
        (
            "control:local-dies-in-frame",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: "zzzzzzzzzzzz" } }
fn eat(o: Option[(H, H)]) -> H { match o { Option.Some(t) => { let x = t.0; println(f"r{x.id}"); return mkh(9); } Option.None => { return mkh(0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" }))); println(f"got:{got.id}");
    println("end");
}
"#,
            "r5\ndH5\ndH6\ngot:9\ndH9\nend\n",
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
