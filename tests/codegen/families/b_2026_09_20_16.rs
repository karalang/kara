//! B-2026-09-20-16 -- a whole-payload view of a callee-owned by-value
//! `Option`/`Result` param handed out of the FRAME (returned, as the body's
//! tail, or inside the aggregate the frame returns) takes the payload's
//! `Drop` bodies with it.

use super::*;

/// B-2026-09-20-16 — `fn eat(o: Option[(H, H)]) -> (H, H) { match o {
/// Some(t) => { return t; } .. } }` ran both parts' bodies twice on every
/// compiled surface (`dH5 dH6 got:5 dH5 dH6 end`): the boxed payload's bodies
/// live on the param's own walk (B-2026-09-10-9), which stayed armed while the
/// caller received both parts. The walk now stands down where a view of the
/// payload leaves the frame, and only on that path.
///
/// Every `returned` cell below doubled before the fix; the three controls
/// (`forwarded`, `forwarded-tail`, `borrowed`) were correct before and after.
/// The forwarded ones are the boundary the row warned about: handing the view
/// to another call is equally a whole-payload take, and standing the walk
/// down there loses both bodies, because the callee's by-value tuple param
/// registers none of its own. Each cell is checked against the interpreter
/// and against the literal expected sequence.
#[test]
fn whole_payload_view_returned_out_of_the_frame_runs_each_body_once() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "ret",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> (H, H) { match o { Option.Some(t) => { return t; } Option.None => { return mk(); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.0.id}");
    println("end");
}
"#,
            "got:5\ndH5\ndH6\nend\n",
        ),
        (
            "ret-nb",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> (H, H) { match o { Option.Some(t) => return t, Option.None => return mk(), } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.0.id}");
    println("end");
}
"#,
            "got:5\ndH5\ndH6\nend\n",
        ),
        (
            "tail",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> (H, H) { match o { Option.Some(t) => t, Option.None => mk(), } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.0.id}");
    println("end");
}
"#,
            "got:5\ndH5\ndH6\nend\n",
        ),
        (
            "tailblk",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> (H, H) { match o { Option.Some(t) => { t } Option.None => { mk() } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.0.id}");
    println("end");
}
"#,
            "got:5\ndH5\ndH6\nend\n",
        ),
        (
            "rebind",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> (H, H) { match o { Option.Some(t) => { let u = t; return u; } Option.None => { return mk(); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.0.id}");
    println("end");
}
"#,
            "got:5\ndH5\ndH6\nend\n",
        ),
        (
            "res",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Result[(H, H), i64]) -> (H, H) { match o { Result.Ok(t) => { return t; } Result.Err(_) => { return mk(); } } }
fn main() {
    let got = eat(Result.Ok((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.0.id}");
    println("end");
}
"#,
            "got:5\ndH5\ndH6\nend\n",
        ),
        (
            "loop",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> (H, H) { match o { Option.Some(t) => { return t; } Option.None => { return mk(); } } }
fn main() {
    let mut i = 0;
    while i < 2 {
        let got = eat(Option.Some((H { id: 5 + i, s: "aaaaaaaaaaaa" }, H { id: 7 + i, s: "bbbbbbbbbbbb" })));
        println(f"got:{got.0.id}");
        i = i + 1;
    }
    println("end");
}
"#,
            "got:5\ndH5\ndH7\ngot:6\ndH6\ndH8\nend\n",
        ),
        (
            "named",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> (H, H) { match o { Option.Some(t) => { return t; } Option.None => { return mk(); } } }
fn main() {
    let a = Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" }));
    let got = eat(a);
    println(f"got:{got.0.id}");
    println("end");
}
"#,
            "got:5\ndH5\ndH6\nend\n",
        ),
        (
            "cond-true",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)], c: bool) -> (H, H) { match o { Option.Some(t) => { if c { return t; } println(f"kept{t.0.id}"); return mk(); } Option.None => { return mk(); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })), true);
    println(f"got:{got.0.id}");
    println("end");
}
"#,
            "got:5\ndH5\ndH6\nend\n",
        ),
        (
            "ret-after-use",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> (H, H) { match o { Option.Some(t) => { println(f"saw{t.1.id}"); return t; } Option.None => { return mk(); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.0.id}");
    println("end");
}
"#,
            "saw6\ngot:5\ndH5\ndH6\nend\n",
        ),
        (
            "ret-match",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> (H, H) { return match o { Option.Some(t) => t, Option.None => mk(), }; }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.0.id}");
    println("end");
}
"#,
            "got:5\ndH5\ndH6\nend\n",
        ),
        (
            "wrap-some",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> Option[(H, H)] { match o { Option.Some(t) => { return Option.Some(t); } Option.None => { return Option.None; } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    match got { Option.Some(g) => println(f"got:{g.0.id}"), Option.None => println("none"), }
    println("end");
}
"#,
            "got:5\ndH5\ndH6\nend\n",
        ),
        (
            "wrap-some-tail",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> Option[(H, H)] { match o { Option.Some(t) => Option.Some(t), Option.None => Option.None, } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    match got { Option.Some(g) => println(f"got:{g.0.id}"), Option.None => println("none"), }
    println("end");
}
"#,
            "got:5\ndH5\ndH6\nend\n",
        ),
        (
            "wrap-tup",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> ((H, H), i64) { match o { Option.Some(t) => { return (t, 1); } Option.None => { return (mk(), 0); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.1}");
    println("end");
}
"#,
            "got:1\ndH5\ndH6\nend\n",
        ),
        (
            "wrap-tup-tail",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> ((H, H), i64) { match o { Option.Some(t) => (t, 1), Option.None => (mk(), 0), } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.1}");
    println("end");
}
"#,
            "got:1\ndH5\ndH6\nend\n",
        ),
        (
            "fwd",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> i64 { match o { Option.Some(t) => { return sink(t); } Option.None => { return 0; } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got}");
    println("end");
}
"#,
            "dH5\ndH6\ngot:11\nend\n",
        ),
        (
            "fwd-tail",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> i64 { match o { Option.Some(t) => sink(t), Option.None => 0, } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got}");
    println("end");
}
"#,
            "dH5\ndH6\ngot:11\nend\n",
        ),
        (
            "borrow",
            r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk() -> (H, H) { return (H { id: 0, s: "zzzzzzzzzzzz" }, H { id: 1, s: "zzzzzzzzzzzz" }); }
fn sink(t: (H, H)) -> i64 { return t.0.id + t.1.id }
fn eat(o: Option[(H, H)]) -> (H, H) { match o { Option.Some(t) => { println(f"saw{t.0.id}"); return mk(); } Option.None => { return mk(); } } }
fn main() {
    let got = eat(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, H { id: 6, s: "bbbbbbbbbbbb" })));
    println(f"got:{got.0.id}");
    println("end");
}
"#,
            "saw5\ndH5\ndH6\ngot:0\ndH0\ndH1\nend\n",
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
