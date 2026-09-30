//! B-2026-09-20-22 -- a `Drop`-declaring struct that carries a `shared`
//! field, bound out of a by-value user-enum param's payload, is a VIEW whose
//! body the caller runs; moving it on (`out = w`, `let m = w`) no longer runs
//! that body a second time in the callee.

use super::*;

/// B-2026-09-20-22 — `match b { Esh.A(w) => { out = w; } .. }` over
/// `struct Wsh { h: Sh }` (`Sh` a `shared struct`) printed
/// `dWsh-OUT dWsh-PAY dWsh-PAY` on every compiled surface against the
/// interpreter's `dWsh-OUT dWsh-PAY`. The payload binding of an owned param is
/// a view whose body the caller's payload walk runs, but the view was recorded
/// only for a copy-supported struct, and a struct with a `shared` field never
/// is, so `out` kept its full drop and ran `w`'s body again at scope exit.
///
/// The `control:` cells were correct before and after.
#[test]
fn shared_field_payload_view_moved_on_runs_its_body_once() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "sh-arm",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
fn sh(b: Esh) -> i64 { let mut out: Wsh = Wsh { h: Sh { s: f"OUT" } }; match b { Esh.A(w) => { out = w; } Esh.B => { } } return 7; }
fn main() { println(f"  r:{sh(Esh.A(Wsh { h: Sh { s: f"PAY" } }))}"); }
"#,
            "  dWsh-OUT\n  dWsh-PAY\n  r:7\n",
        ),
        (
            "sh-ifl",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
fn shi(b: Esh) -> i64 { let mut out: Wsh = Wsh { h: Sh { s: f"OUT" } }; if let Esh.A(w) = b { out = w; } return 7; }
fn main() { println(f"  r:{shi(Esh.A(Wsh { h: Sh { s: f"PAY" } }))}"); }
"#,
            "  dWsh-OUT\n  dWsh-PAY\n  r:7\n",
        ),
        (
            "sh-read-after",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
fn sh(b: Esh) -> i64 { let mut out: Wsh = Wsh { h: Sh { s: f"OUT" } }; match b { Esh.A(w) => { out = w; } Esh.B => { } } println(f"  now:{out.h.s}"); return 7; }
fn main() { println(f"  r:{sh(Esh.A(Wsh { h: Sh { s: f"PAY" } }))}"); }
"#,
            "  dWsh-OUT\n  now:PAY\n  dWsh-PAY\n  r:7\n",
        ),
        (
            "sh-keep-shared",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
fn sh(b: Esh) -> Sh { let mut out: Wsh = Wsh { h: Sh { s: f"OUT" } }; match b { Esh.A(w) => { out = w; } Esh.B => { } } return out.h; }
fn main() { let k = sh(Esh.A(Wsh { h: Sh { s: f"PAY" } })); println(f"  k:{k.s}"); }
"#,
            "  dWsh-OUT\n  dWsh-PAY\n  k:PAY\n",
        ),
        (
            "sx-arm",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
fn sx(b: Esx) -> i64 { let mut out: Wsx = Wsx { h: Sh { s: f"OUT" }, s: f"o" }; match b { Esx.A(w) => { out = w; } Esx.B => { } } return 7; }
fn main() { println(f"  r:{sx(Esx.A(Wsx { h: Sh { s: f"PAY" }, s: f"p" }))}"); }
"#,
            "  dWsx-OUT-o\n  dWsx-PAY-p\n  r:7\n",
        ),
        (
            "rb-then-assign",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
enum E2 { A(Wsh, i64), B }
fn mk(s: String) -> Wsh { return Wsh { h: Sh { s: s } } }
fn eat(w: Wsh) { println(f"  ate:{w.h.s}") }
fn f(b: Esh) -> i64 { let mut out = mk(f"OUT"); match b { Esh.A(w) => { let m = w; out = m; } Esh.B => { } } return 7; }
fn main() { println(f"  r:{f(Esh.A(mk(f"PAY")))}"); }
"#,
            "  dWsh-OUT\n  dWsh-PAY\n  r:7\n",
        ),
        (
            "rb-only",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
enum E2 { A(Wsh, i64), B }
fn mk(s: String) -> Wsh { return Wsh { h: Sh { s: s } } }
fn eat(w: Wsh) { println(f"  ate:{w.h.s}") }
fn f(b: Esh) -> i64 { match b { Esh.A(w) => { let m = w; println(f"  m:{m.h.s}"); } Esh.B => { } } return 7; }
fn main() { println(f"  r:{f(Esh.A(mk(f"PAY")))}"); }
"#,
            "  m:PAY\n  dWsh-PAY\n  r:7\n",
        ),
        (
            "sx-rb",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
enum E2 { A(Wsh, i64), B }
fn mk(s: String) -> Wsh { return Wsh { h: Sh { s: s } } }
fn eat(w: Wsh) { println(f"  ate:{w.h.s}") }
fn f(b: Esx) -> i64 { let mut out = Wsx { h: Sh { s: f"OUT" }, s: f"o" }; match b { Esx.A(w) => { let m = w; out = m; } Esx.B => { } } return 7; }
fn main() { println(f"  r:{f(Esx.A(Wsx { h: Sh { s: f"PAY" }, s: f"p" }))}"); }
"#,
            "  dWsx-OUT-o\n  dWsx-PAY-p\n  r:7\n",
        ),
        (
            "tuple-variant",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
enum E2 { A(Wsh, i64), B }
fn mk(s: String) -> Wsh { return Wsh { h: Sh { s: s } } }
fn eat(w: Wsh) { println(f"  ate:{w.h.s}") }
fn f(b: E2) -> i64 { let mut out = mk(f"OUT"); match b { E2.A(w, k) => { out = w; println(f"  k:{k}"); } E2.B => { } } return 7; }
fn main() { println(f"  r:{f(E2.A(mk(f"PAY"), 3))}"); }
"#,
            "  dWsh-OUT\n  k:3\n  dWsh-PAY\n  r:7\n",
        ),
        (
            "two-assign",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
enum E2 { A(Wsh, i64), B }
fn mk(s: String) -> Wsh { return Wsh { h: Sh { s: s } } }
fn eat(w: Wsh) { println(f"  ate:{w.h.s}") }
fn f(b: Esh, c: Esh) -> i64 { let mut out = mk(f"OUT"); match b { Esh.A(w) => { out = w; } Esh.B => { } } match c { Esh.A(v) => { out = v; } Esh.B => { } } return 7; }
fn main() { println(f"  r:{f(Esh.A(mk(f"P1")), Esh.A(mk(f"P2")))}"); }
"#,
            "  dWsh-OUT\n  dWsh-P2\n  dWsh-P1\n  r:7\n",
        ),
        (
            "named-arg",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
enum E2 { A(Wsh, i64), B }
fn mk(s: String) -> Wsh { return Wsh { h: Sh { s: s } } }
fn eat(w: Wsh) { println(f"  ate:{w.h.s}") }
fn f(b: Esh) -> i64 { let mut out = mk(f"OUT"); match b { Esh.A(w) => { out = w; } Esh.B => { } } return 7; }
fn main() { let e = Esh.A(mk(f"PAY")); println(f"  r:{f(e)}"); println("end"); }
"#,
            "  dWsh-OUT\n  r:7\n  dWsh-PAY\nend\n",
        ),
        (
            "method",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
enum E2 { A(Wsh, i64), B }
fn mk(s: String) -> Wsh { return Wsh { h: Sh { s: s } } }
fn eat(w: Wsh) { println(f"  ate:{w.h.s}") }
struct H { k: i64 }
impl H { fn f(ref self, b: Esh) -> i64 { let mut out = mk(f"OUT"); match b { Esh.A(w) => { out = w; } Esh.B => { } } return 7; } }
fn main() { let h = H { k: 1 }; println(f"  r:{h.f(Esh.A(mk(f"PAY")))}"); }
"#,
            "  dWsh-OUT\n  dWsh-PAY\n  r:7\n",
        ),
        (
            "control:pl-arm",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
fn pl(b: Epl) -> i64 { let mut out: Wpl = Wpl { a: f"OUT" }; match b { Epl.A(w) => { out = w; } Epl.B => { } } return 7; }
fn main() { println(f"  r:{pl(Epl.A(Wpl { a: f"PAY" }))}"); }
"#,
            "  dWpl-OUT\n  dWpl-PAY\n  r:7\n",
        ),
        (
            "control:sh-inline",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
fn shc() -> i64 { let mut out: Wsh = Wsh { h: Sh { s: f"OUT" } }; out = Wsh { h: Sh { s: f"PAY" } }; return 7; }
fn main() { println(f"  r:{shc()}"); }
"#,
            "  dWsh-OUT\n  dWsh-PAY\n  r:7\n",
        ),
        (
            "control:sh-local",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
fn shl() -> i64 { let mut out: Wsh = Wsh { h: Sh { s: f"OUT" } }; let w = Wsh { h: Sh { s: f"PAY" } }; out = w; return 7; }
fn main() { println(f"  r:{shl()}"); }
"#,
            "  dWsh-OUT\n  dWsh-PAY\n  r:7\n",
        ),
        (
            "control:sh-param",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
fn shp(w: Wsh) -> i64 { let mut out: Wsh = Wsh { h: Sh { s: f"OUT" } }; out = w; return 7; }
fn main() { println(f"  r:{shp(Wsh { h: Sh { s: f"PAY" } })}"); }
"#,
            "  dWsh-OUT\n  dWsh-PAY\n  r:7\n",
        ),
        (
            "control:sh-opt",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
fn sho(b: Option[Wsh]) -> i64 { let mut out: Wsh = Wsh { h: Sh { s: f"OUT" } }; match b { Option.Some(w) => { out = w; } Option.None => { } } return 7; }
fn main() { println(f"  r:{sho(Option.Some(Wsh { h: Sh { s: f"PAY" } }))}"); }
"#,
            "  dWsh-OUT\n  dWsh-PAY\n  r:7\n",
        ),
        (
            "control:sh-arm-local",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
fn main() { let b = Esh.A(Wsh { h: Sh { s: f"PAY" } }); let mut out: Wsh = Wsh { h: Sh { s: f"OUT" } }; match b { Esh.A(w) => { out = w; } Esh.B => { } } println("end"); }
"#,
            "  dWsh-OUT\n  dWsh-PAY\nend\n",
        ),
        (
            "control:sh-notaken",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
fn sh(b: Esh) -> i64 { let mut out: Wsh = Wsh { h: Sh { s: f"OUT" } }; match b { Esh.A(w) => { out = w; } Esh.B => { } } return 7; }
fn main() { println(f"  r:{sh(Esh.B)}"); }
"#,
            "  dWsh-OUT\n  r:7\n",
        ),
        (
            "control:sx-notaken",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
fn sx(b: Esx) -> i64 { let mut out: Wsx = Wsx { h: Sh { s: f"OUT" }, s: f"o" }; match b { Esx.A(w) => { out = w; } Esx.B => { } } return 7; }
fn main() { println(f"  r:{sx(Esx.B)}"); }
"#,
            "  dWsx-OUT-o\n  r:7\n",
        ),
        (
            "control:read-only",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
enum E2 { A(Wsh, i64), B }
fn mk(s: String) -> Wsh { return Wsh { h: Sh { s: s } } }
fn eat(w: Wsh) { println(f"  ate:{w.h.s}") }
fn f(b: Esh) -> i64 { match b { Esh.A(w) => { println(f"  w:{w.h.s}"); } Esh.B => { } } return 7; }
fn main() { println(f"  r:{f(Esh.A(mk(f"PAY")))}"); }
"#,
            "  w:PAY\n  dWsh-PAY\n  r:7\n",
        ),
        (
            "control:consume",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
enum E2 { A(Wsh, i64), B }
fn mk(s: String) -> Wsh { return Wsh { h: Sh { s: s } } }
fn eat(w: Wsh) { println(f"  ate:{w.h.s}") }
fn f(b: Esh) -> i64 { match b { Esh.A(w) => { eat(w); } Esh.B => { } } return 7; }
fn main() { println(f"  r:{f(Esh.A(mk(f"PAY")))}"); }
"#,
            "  ate:PAY\n  dWsh-PAY\n  r:7\n",
        ),
        (
            "control:ret-w",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
enum E2 { A(Wsh, i64), B }
fn mk(s: String) -> Wsh { return Wsh { h: Sh { s: s } } }
fn eat(w: Wsh) { println(f"  ate:{w.h.s}") }
fn f(b: Esh) -> Wsh { match b { Esh.A(w) => { return w; } Esh.B => { return mk(f"NB"); } } }
fn main() { let g = f(Esh.A(mk(f"PAY"))); println(f"  g:{g.h.s}"); }
"#,
            "  g:PAY\n  dWsh-PAY\n",
        ),
        (
            "control:share-keep",
            r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { println(f"  dWsh-{self.h.s}") } }
struct Wpl { a: String }
impl Drop for Wpl { fn drop(mut ref self) { println(f"  dWpl-{self.a}") } }
enum Esh { A(Wsh), B }
enum Epl { A(Wpl), B }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { println(f"  dWsx-{self.h.s}-{self.s}") } }
enum Esx { A(Wsx), B }
enum E2 { A(Wsh, i64), B }
fn mk(s: String) -> Wsh { return Wsh { h: Sh { s: s } } }
fn eat(w: Wsh) { println(f"  ate:{w.h.s}") }
fn f(b: Esh) -> Sh { let mut out = mk(f"OUT"); let mut keep = Sh { s: f"K" }; match b { Esh.A(w) => { keep = w.h; println("got") } Esh.B => { } } return keep; }
fn main() { let k = f(Esh.A(mk(f"PAY"))); println(f"  k:{k.s}"); }
"#,
            "  dWsh-OUT\ngot\n  dWsh-PAY\n  k:PAY\n",
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
