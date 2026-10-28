//! A trait default body copied into two impls keeps the trait's spans in
//! both copies, so `v[0] + v[1]` is `String` concatenation in one copy and a
//! user `Add` in the other at one span. The operator rewrite read the
//! operands' types by span, which holds the last impl's, and the ownership
//! pass suppressed an element move program-wide where any copy borrowed
//! there (B-2026-10-28-12): with the `String` impl last, `check` passed a
//! `Big` copy that moves both elements out of their `Vec`; with it first, a
//! `Copy` `Big` made `check` and `run` refuse the `String` copy.

use std::process::Command;

const HEAD: &str = "struct Big { n: i64 }
impl Add for Big {
    type Output = Big;
    fn add(own self, rhs: own Big) -> Big { return Big { n: self.n + rhs.n }; }
}
impl Big { fn show(ref self) -> String { return f\"{self.n}\"; } }

trait Pair[E: Add[Output = E]] {
    fn items(ref self) -> Vec[E];
    fn both(ref self) -> E {
        let v = self.items();
        let w = v[0] + v[1];
        w
    }
}
";

const BIG: &str = "struct G {}
impl Pair[Big] for G {
    fn items(ref self) -> Vec[Big] { return [Big { n: 1 }, Big { n: 2 }].to_vec(); }
}
";

const TEXT: &str = "struct S {}
impl Pair[String] for S {
    fn items(ref self) -> Vec[String] { return [\"a\".to_string(), \"b\".to_string()].to_vec(); }
}
";

const MAIN: &str = "fn main() {
    println(G {}.both().show());
    println(S {}.both());
}
";

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-copyops-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_both_moves_refused(name: &str, src: &str) {
    let (ok, err) = check(name, src);
    assert!(!ok, "check must refuse:\n{src}");
    for col in [17, 24] {
        let want = format!(
            "main.kara:12:{col}: cannot move a non-`Copy` value out of a collection element"
        );
        assert!(err.contains(&want), "want {want:?} in\n{err}");
    }
}

#[test]
fn an_add_that_moves_elements_in_one_copy_is_refused_with_the_string_impl_last() {
    assert_both_moves_refused("text-last", &format!("{HEAD}{BIG}{TEXT}{MAIN}"));
}

#[test]
fn an_add_that_moves_elements_in_one_copy_is_refused_with_the_string_impl_first() {
    assert_both_moves_refused("text-first", &format!("{HEAD}{TEXT}{BIG}{MAIN}"));
}

#[test]
fn a_copy_add_beside_a_string_concat_checks_and_runs_in_either_order() {
    let head = HEAD.replace(
        "struct Big { n: i64 }",
        "#[derive(Copy, Clone)]\nstruct Big { n: i64 }",
    );
    for (name, impls) in [
        ("copy-text-last", [BIG, TEXT]),
        ("copy-text-first", [TEXT, BIG]),
    ] {
        let src = format!("{head}{}{}{MAIN}", impls[0], impls[1]);
        let (ok, err) = check(name, &src);
        assert!(ok, "{name}: {err}");
        let out = karac::mir::lower::run_source(&src).expect("MIR interpreter");
        assert_eq!(out.output, "3\nab\n", "{name}");
    }
}
