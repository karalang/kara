//! `karac fmt` keeps a type's effect argument (B-2026-10-31-31). The parser
//! stores `with E` apart from the type arguments, and the formatter printed
//! only the type arguments, so `EventBus[with writes(OrderDb)]` came back as a
//! bare `EventBus`: a different type, and a program `karac check` refused
//! checked clean after formatting.

fn formatted(src: &str) -> String {
    karac::format_source(src).expect("formats")
}

#[test]
fn fmt_keeps_an_effect_argument_alone_and_after_type_arguments() {
    let src = "struct Bus[with E] { handlers: Vec[Fn(i64) with E] }\n\
               impl[with E] Bus[with E] { fn n(self) -> i64 { 0 } }\n\
               fn a(b: Bus[with writes(Stdout)]) -> i64 { b.n() }\n\
               struct S[T, with E] { v: Vec[T], f: Vec[Fn(T) with E] }\n\
               fn b(s: S[i64, with writes(Stdout)]) -> i64 { s.v.len() }\n\
               fn main() {}\n";
    let out = formatted(src);
    for want in [
        "impl[with E] Bus[with E] {",
        "fn a(b: Bus[with writes(Stdout)]) -> i64 {",
        "fn b(s: S[i64, with writes(Stdout)]) -> i64 {",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    assert_eq!(formatted(&out), out, "formatting is idempotent");
}
