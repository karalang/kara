//! B-2026-09-20-23 -- an `Option`/`Result` argument built around a NAMED
//! local (`take(Some(a))`) runs its payload's `Drop` bodies when the call
//! returns, as the fresh-temp spelling does, rather than at the caller's
//! scope exit.

use super::*;

/// B-2026-09-20-23 — `let a: Array[W1, 1] = [W1 { v: 40 }]; let r =
/// take(Some(a));` printed `r:40 end dW1_40` on every compiled surface against
/// the interpreter's `dW1_40 r:40 end`. Moving `a` into the constructor
/// retracted `a`'s own element-bodies walk while the argument lowered, which
/// shrank the scope frame under the LENGTH both temp drains had saved, so the
/// argument's walk landed below the mark and ran at scope exit. The mark now
/// records which entries were present rather than how many.
///
/// Covers both envelopes, four element shapes, free/method/associated calls,
/// a statement call, two arguments in one expression, a loop and an inner
/// scope. The `control:` cells were correct before and after.
#[test]
fn named_local_optres_argument_runs_its_bodies_at_the_call() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "control:S4-Option-fresh",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[Array[S4, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } }
fn main() { let r = take(Some([S4 { v: 42, tag: f"t{1}" }])); println(f"r:{r}"); println("end") }
"#,
            "dS4_42\nr:42\nend\n",
        ),
        (
            "control:S4-Option-loc",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[Array[S4, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } }
fn main() { let a: Array[S4, 1] = [S4 { v: 42, tag: f"t{1}" }]; let r = take(Some(a)); println(f"r:{r}"); println("end") }
"#,
            "dS4_42\nr:42\nend\n",
        ),
        (
            "control:S4-Result-fresh",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Result[Array[S4, 1], i64]) -> i64 { match o { Ok(x) => { return x[0].v } Err(e) => { return e } } }
fn main() { let r = take(Ok([S4 { v: 42, tag: f"t{1}" }])); println(f"r:{r}"); println("end") }
"#,
            "dS4_42\nr:42\nend\n",
        ),
        (
            "S4-Result-loc",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Result[Array[S4, 1], i64]) -> i64 { match o { Ok(x) => { return x[0].v } Err(e) => { return e } } }
fn main() { let a: Array[S4, 1] = [S4 { v: 42, tag: f"t{1}" }]; let r = take(Ok(a)); println(f"r:{r}"); println("end") }
"#,
            "dS4_42\nr:42\nend\n",
        ),
        (
            "control:W1-Option-fresh",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } }
fn main() { let r = take(Some([W1 { v: 40 }])); println(f"r:{r}"); println("end") }
"#,
            "dW1_40\nr:40\nend\n",
        ),
        (
            "W1-Option-loc",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } }
fn main() { let a: Array[W1, 1] = [W1 { v: 40 }]; let r = take(Some(a)); println(f"r:{r}"); println("end") }
"#,
            "dW1_40\nr:40\nend\n",
        ),
        (
            "control:W1-Result-fresh",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Result[Array[W1, 1], i64]) -> i64 { match o { Ok(x) => { return x[0].v } Err(e) => { return e } } }
fn main() { let r = take(Ok([W1 { v: 40 }])); println(f"r:{r}"); println("end") }
"#,
            "dW1_40\nr:40\nend\n",
        ),
        (
            "W1-Result-loc",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Result[Array[W1, 1], i64]) -> i64 { match o { Ok(x) => { return x[0].v } Err(e) => { return e } } }
fn main() { let a: Array[W1, 1] = [W1 { v: 40 }]; let r = take(Ok(a)); println(f"r:{r}"); println("end") }
"#,
            "dW1_40\nr:40\nend\n",
        ),
        (
            "control:W3-Option-fresh",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[Array[W3, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } }
fn main() { let r = take(Some([W3 { v: 41, b: 0, c: 0 }])); println(f"r:{r}"); println("end") }
"#,
            "dW3_41\nr:41\nend\n",
        ),
        (
            "W3-Option-loc",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[Array[W3, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } }
fn main() { let a: Array[W3, 1] = [W3 { v: 41, b: 0, c: 0 }]; let r = take(Some(a)); println(f"r:{r}"); println("end") }
"#,
            "dW3_41\nr:41\nend\n",
        ),
        (
            "control:W3-Result-fresh",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Result[Array[W3, 1], i64]) -> i64 { match o { Ok(x) => { return x[0].v } Err(e) => { return e } } }
fn main() { let r = take(Ok([W3 { v: 41, b: 0, c: 0 }])); println(f"r:{r}"); println("end") }
"#,
            "dW3_41\nr:41\nend\n",
        ),
        (
            "W3-Result-loc",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Result[Array[W3, 1], i64]) -> i64 { match o { Ok(x) => { return x[0].v } Err(e) => { return e } } }
fn main() { let a: Array[W3, 1] = [W3 { v: 41, b: 0, c: 0 }]; let r = take(Ok(a)); println(f"r:{r}"); println("end") }
"#,
            "dW3_41\nr:41\nend\n",
        ),
        (
            "assoc-loc",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn main() { let a: Array[W1, 1] = [W1 { v: 40 }]; let r = H.a(Some(a)); println(f"r:{r}"); println("end") }
"#,
            "dW1_40\nr:40\nend\n",
        ),
        (
            "control:inner-scope",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } }
fn main() { let a: Array[W1, 1] = [W1 { v: 40 }]; if true { let r = take(Some(a)); println(f"r:{r}"); } println("end") }
"#,
            "dW1_40\nr:40\nend\n",
        ),
        (
            "keep-later",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } }
fn main() { let k = W1 { v: 7 }; let a: Array[W1, 1] = [W1 { v: 40 }]; let r = take(Some(a)); println(f"r:{r}"); println(f"k:{k.v}"); println("end") }
"#,
            "dW1_40\nr:40\nk:7\ndW1_7\nend\n",
        ),
        (
            "loop-loc",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } }
fn main() { let mut i = 0; while i < 2 { let a: Array[W1, 1] = [W1 { v: 40 + i }]; let r = take(Some(a)); println(f"r:{r}"); i = i + 1; } println("end") }
"#,
            "dW1_40\nr:40\ndW1_41\nr:41\nend\n",
        ),
        (
            "method-loc",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn main() { let h = H { n: 1 }; let a: Array[W1, 1] = [W1 { v: 40 }]; let r = h.m(Some(a)); println(f"r:{r}"); println("end") }
"#,
            "dW1_40\nr:41\nend\n",
        ),
        (
            "nested-loc",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } }
fn main() { let a: Array[W1, 1] = [W1 { v: 40 }]; let r = take(Some(a)) + 1; println(f"r:{r}"); println("end") }
"#,
            "dW1_40\nr:41\nend\n",
        ),
        (
            "control:stmt-call",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } }
fn main() { let a: Array[W1, 1] = [W1 { v: 40 }]; take(Some(a)); println("end") }
"#,
            "dW1_40\nend\n",
        ),
        (
            "control:struct-loc",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[S4]) -> i64 { match o { Some(x) => { return x.v } None => { return 0 } } }
fn main() { let a = S4 { v: 42, tag: f"t{1}" }; let r = take(Some(a)); println(f"r:{r}"); println("end") }
"#,
            "dS4_42\nr:42\nend\n",
        ),
        (
            "two-loc",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } }
fn main() { let a: Array[W1, 1] = [W1 { v: 40 }]; let b: Array[W1, 1] = [W1 { v: 50 }]; let r = take(Some(a)) + take(Some(b)); println(f"r:{r}"); println("end") }
"#,
            "dW1_40\ndW1_50\nr:90\nend\n",
        ),
        (
            "control:vec-loc",
            r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W3 { v: i64, b: i64, c: i64 }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
struct S4 { v: i64, tag: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS4_{self.v}") } }
struct S1 { tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS1_{self.tag}") } }
struct H { n: i64 }
impl H { fn m(self, o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v + self.n } None => { return 0 } } }
         fn a(o: Option[Array[W1, 1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } } }
fn take(o: Option[Vec[W1]]) -> i64 { match o { Some(x) => { return x[0].v } None => { return 0 } } }
fn main() { let mut a: Vec[W1] = Vec.new(); a.push(W1 { v: 40 }); let r = take(Some(a)); println(f"r:{r}"); println("end") }
"#,
            "dW1_40\nr:40\nend\n",
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
