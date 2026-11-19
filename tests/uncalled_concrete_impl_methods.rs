//! B-2026-11-19-1: a method of an impl at concrete type arguments
//! (`impl Pr[bool]` of `struct Pr[T] { x: T }`) that nothing calls is
//! built by `karac check` with `self` the written target, so its fields
//! have the impl's types. It was built with the struct's own parameter, so
//! `self.x` read as a move of a non-`Copy` `T` (E0516) and `if self.x` as a
//! view stored where its borrow is not kept (E0509), until a call anywhere
//! built it at its real type.

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-uncalled-concrete-impl-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

const IMPLS: &str = "trait Score { fn score(self) -> i64; }
struct Pr[T] { x: T }
struct Pair[A, B] { a: A, b: B }
enum Two[T] { One(T), Zero }
impl Score for Pr[bool] { fn score(self) -> i64 { if self.x { 7 } else { 0 } } }
impl Pr[i64] { fn req(self) -> i64 { self.x } }
impl Pr[String] {
    fn size(self) -> i64 { self.x.len() }
    fn take(own self) -> String { self.x }
}
impl Pair[i64, String] { fn sum(self) -> i64 { self.a + self.b.len() } }
impl Two[String] {
    fn get(self) -> i64 {
        match self {
            Two.One(s) => s.len(),
            Two.Zero => 0,
        }
    }
}
";

#[test]
fn uncalled_methods_of_concrete_impls_check_clean() {
    let path = scratch("uncalled.kara", &format!("{IMPLS}fn main() {{}}\n"));
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    assert!(!text(&check).contains("error"), "{}", text(&check));
}

#[test]
fn called_methods_of_concrete_impls_run_as_before() {
    let path = scratch(
        "called.kara",
        &format!(
            "{IMPLS}fn main() {{
    println(Pr {{ x: true }}.score());
    println(Pr {{ x: 3 }}.req());
    let p = Pr {{ x: \"abc\" }};
    println(p.size());
    println(p.take());
    println(Pair {{ a: 2, b: \"xy\" }}.sum());
    println(Two.One(\"four\").get());
}}
"
        ),
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "7\n3\n3\nabc\n4\n4\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_real_move_in_an_uncalled_concrete_impl_is_still_refused() {
    let path = scratch(
        "bad.kara",
        "struct Pr[T] { x: T }
impl Pr[String] { fn bad(self) -> String { self.x } }
fn main() {}
",
    );
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(
        text(&check).contains(":2:44: cannot move a non-`Copy` value out of a borrowed place"),
        "{}",
        text(&check)
    );
}
