//! `own` in parameters, receivers and function types (core semantics
//! amendment 8, D5, step 1). `own T` is accepted and means what a bare `T`
//! means today; the spelling is recorded so `karac fmt` keeps it and the
//! later flip of a bare `T` to borrowed can tell the two apart.

use karac::ast::{ImplItem, Item, TraitItem, TypeKind};

const SRC: &str = "\
struct S { n: i64 }

impl S {
    fn take(own self) -> i64 { self.n }
    fn peek(ref self) -> i64 { self.n }
}

trait Eat {
    fn eat(own self, other: own S) -> i64;
}

fn apply(x: own S, f: Fn(own S, i64) -> i64) -> i64 { f(x, 1) }

fn main() {
    let s = S { n: 41 };
    println(apply(s, |t: S, k: i64| t.take() + k));
}
";

#[test]
fn own_is_recorded_on_params_receivers_and_fn_types() {
    let r = karac::parse(SRC);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let mut seen = 0;
    for item in &r.program.items {
        match item {
            Item::Function(f) if f.name == "apply" => {
                assert!(f.params[0].is_own, "`x: own S`");
                assert!(!f.params[1].is_own, "`f: Fn(..)` is bare");
                let TypeKind::FnType { own_params, .. } = &f.params[1].ty.kind else {
                    panic!("`f` is a function type");
                };
                assert_eq!(own_params, &vec![true, false]);
                seen += 1;
            }
            Item::ImplBlock(b) => {
                for it in &b.items {
                    if let ImplItem::Method(m) = it {
                        assert_eq!(m.self_is_own, m.name == "take", "{}", m.name);
                        seen += 1;
                    }
                }
            }
            Item::TraitDef(t) => {
                let TraitItem::Method(m) = &t.items[0] else {
                    panic!("`eat` is a method");
                };
                assert!(m.self_is_own && m.params[0].is_own);
                seen += 1;
            }
            _ => {}
        }
    }
    assert_eq!(seen, 4, "apply, take, peek, eat");
}

#[test]
fn karac_fmt_keeps_own() {
    let out = karac::format_source(SRC).expect("formats");
    for want in [
        "fn take(own self)",
        "fn peek(ref self)",
        "fn eat(own self, other: own S)",
        "fn apply(x: own S, f: Fn(own S, i64) -> i64)",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
}

/// Until the flip, `own T` and a bare `T` are the same parameter.
#[test]
fn own_runs_like_a_bare_parameter() {
    let dir = std::env::temp_dir().join(format!("karac-own-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("own.kara");
    std::fs::write(
        &path,
        SRC.replace(
            "trait Eat {\n    fn eat(own self, other: own S) -> i64;\n}\n",
            "",
        ),
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", "--interp", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "karac run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n");
}
