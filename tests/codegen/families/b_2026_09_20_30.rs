//! B-2026-09-20-30 -- a `Result[shared]` or generic enum bound from a USER
//! METHOD's by-value return is released like the free-call twin.

use super::*;

const PRE: &str = "shared struct Sh { n: i64 }
impl Drop for Sh { fn drop(mut ref self) { println(f\"dSh{self.n}\") } }
enum Box2[T] { V(T), N }
struct Maker { k: i64 }
impl Maker {
    fn make(ref self) -> Box2[Sh] { Box2.V(Sh { n: self.k }) }
    fn res(ref self) -> Result[Sh, i64] { Ok(Sh { n: self.k }) }
    fn pass(ref self, b: Box2[Sh]) -> Box2[Sh] { b }
    fn rpass(ref self, r: Result[Sh, i64]) -> Result[Sh, i64] { r }
    fn me(ref self) -> Maker { Maker { k: self.k + 10 } }
}
fn freemk(k: i64) -> Box2[Sh] { Box2.V(Sh { n: k }) }
fn freeres(k: i64) -> Result[Sh, i64] { Ok(Sh { n: k }) }
fn retm(m: ref Maker) -> Box2[Sh] { let b = m.make(); b }
";

/// B-2026-09-20-30 — `let mb: Box2[Sh] = mk.make()` over `shared struct Sh`
/// lost `Sh`'s `Drop` body and leaked its box on every compiled surface while
/// the byte-identical free call `freemk(1)` was right: the `Result[shared]` /
/// generic-enum let registrar asked `rhs_is_fresh_inline_enum`, which admits
/// `Call` and sent `MethodCall` to its `_ => false` tail. A user method's
/// by-value return, and a block / `if` / `unsafe` tail of one, now register on
/// the free call's terms. `control:` cells were right before.
#[test]
fn method_result_generic_enum_is_released() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "annotated",
            "fn main() {
    let mk = Maker { k: 2 }; let mb: Box2[Sh] = mk.make(); println(\"x\")
}
",
            "x\ndSh2\n",
        ),
        (
            "unannotated",
            "fn main() {
    let mk = Maker { k: 2 }; let mb = mk.make(); println(\"x\")
}
",
            "x\ndSh2\n",
        ),
        (
            "matched",
            "fn main() {
    let mk = Maker { k: 2 }; let mb = mk.make(); match mb { Box2.V(s) => println(f\"v{s.n}\"), Box2.N => println(\"n\") }; println(\"x\")
}
",
            "v2\nx\ndSh2\n",
        ),
        (
            "result",
            "fn main() {
    let mk = Maker { k: 6 }; let r: Result[Sh, i64] = mk.res(); println(\"x\")
}
",
            "x\ndSh6\n",
        ),
        (
            "result-matched",
            "fn main() {
    let mk = Maker { k: 6 }; let r = mk.res(); match r { Ok(s) => println(f\"ok{s.n}\"), Err(e) => println(\"e\") }; println(\"x\")
}
",
            "ok6\nx\ndSh6\n",
        ),
        (
            "handed-back",
            "fn main() {
    let mk = Maker { k: 8 }; let b = mk.pass(freemk(9)); println(\"x\")
}
",
            "x\ndSh9\n",
        ),
        (
            "result-handed-back",
            "fn main() {
    let mk = Maker { k: 8 }; let r = mk.rpass(freeres(9)); println(\"x\")
}
",
            "x\ndSh9\n",
        ),
        (
            "chained-receiver",
            "fn main() {
    let mk = Maker { k: 1 }; let b = mk.me().make(); println(\"x\")
}
",
            "x\ndSh11\n",
        ),
        (
            "in-a-loop",
            "fn main() {
    let mk = Maker { k: 40 }; for i in 0..2 { let b = mk.make(); println(f\"i{i}\") }; println(\"x\")
}
",
            "i0\ndSh40\ni1\ndSh40\nx\n",
        ),
        (
            "result-in-a-loop",
            "fn main() {
    let mk = Maker { k: 41 }; for i in 0..2 { let r = mk.res(); println(f\"i{i}\") }; println(\"x\")
}
",
            "i0\ndSh41\ni1\ndSh41\nx\n",
        ),
        (
            "block-tail",
            "fn main() {
    let mk = Maker { k: 56 }; let b: Box2[Sh] = { mk.make() }; println(\"x\")
}
",
            "x\ndSh56\n",
        ),
        (
            "if-tail",
            "fn main() {
    let mk = Maker { k: 57 }; let c = true; let b: Box2[Sh] = if c { mk.make() } else { Box2.N }; println(\"x\")
}
",
            "x\ndSh57\n",
        ),
        (
            "result-if-tail",
            "fn main() {
    let mk = Maker { k: 58 }; let c = true; let r: Result[Sh, i64] = if c { mk.res() } else { Err(0) }; println(\"x\")
}
",
            "x\ndSh58\n",
        ),
        (
            "unsafe-tail",
            "fn main() {
    // Safety: no raw pointers are touched.\n    let b: Box2[Sh] = unsafe { freemk(53) }; println(\"x\")
}
",
            "x\ndSh53\n",
        ),
        (
            "control:free-call",
            "fn main() {
    let ca: Box2[Sh] = freemk(1); println(\"x\")
}
",
            "x\ndSh1\n",
        ),
        (
            "control:returned",
            "fn main() {
    let mk = Maker { k: 30 }; let b = retm(mk); println(\"x\")
}
",
            "x\ndSh30\n",
        ),
    ];
    for (label, body, want) in cells {
        let prog = format!("{PRE}{body}");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&prog);
        assert!(
            interp_errs.is_empty(),
            "[{label}] interp errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "[{label}] interpreter");
        let Some(aot) = run_program(&prog) else {
            continue;
        };
        assert_eq!(aot, *want, "[{label}] AOT");
    }
}
