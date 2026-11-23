//! B-2026-11-23-8: a trait default body copied into several impls keeps
//! the trait's spans in every copy, and the typechecker's completed argument
//! list of a method call (defaults filled in) was kept by span, so every
//! copy of `self.inner().go()` took the defaults of the `go` the LAST impl's
//! copy calls: `LoudIn.go(self; k: i64 = 10)` ran with `QuietIn.go`'s 200.
//! Each copy reads its own call's list now, whichever impl is written last.

use std::process::Command;

const WALK: &str = "trait Walk {
    type In;

    fn inner(self) -> Self.In;

    fn run(self) -> i64 {
        self.inner().go()
    }
}
";

const LOUD: &str = "struct LoudIn { n: i64 }

impl LoudIn {
    fn go(self; k: i64 = 10) -> i64 { self.n + k }
}

struct A { n: i64 }

impl Walk for A {
    type In = LoudIn;
    fn inner(self) -> LoudIn { LoudIn { n: self.n } }
}
";

/// `go` with another value for the same default.
const QUIET: &str = "struct QuietIn { n: i64 }

impl QuietIn {
    fn go(self; k: i64 = 200) -> i64 { self.n + k }
}

struct B { n: i64 }

impl Walk for B {
    type In = QuietIn;
    fn inner(self) -> QuietIn { QuietIn { n: self.n } }
}
";

/// `go` with one more defaulted parameter than `LoudIn.go` has.
const WIDE: &str = "struct QuietIn { n: i64 }

impl QuietIn {
    fn go(self; k: i64 = 200, j: i64 = 3000) -> i64 { self.n + k + j }
}

struct B { n: i64 }

impl Walk for B {
    type In = QuietIn;
    fn inner(self) -> QuietIn { QuietIn { n: self.n } }
}
";

const MAIN: &str = "fn main() {
    println(A { n: 1 }.run());
    println(B { n: 1 }.run());
}
";

fn run(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-copyfills-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("run")
        .arg("--interp")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn default_copy_fills_each_copy_takes_its_own_callees_default_value() {
    assert_eq!(
        run("ab", &format!("{WALK}\n{LOUD}\n{QUIET}\n{MAIN}")),
        "11\n201\n"
    );
    assert_eq!(
        run("ba", &format!("{WALK}\n{QUIET}\n{LOUD}\n{MAIN}")),
        "11\n201\n"
    );
}

#[test]
fn default_copy_fills_each_copy_takes_its_own_callees_default_count() {
    assert_eq!(
        run("wide-ab", &format!("{WALK}\n{LOUD}\n{WIDE}\n{MAIN}")),
        "11\n3201\n"
    );
    assert_eq!(
        run("wide-ba", &format!("{WALK}\n{WIDE}\n{LOUD}\n{MAIN}")),
        "11\n3201\n"
    );
}

/// One impl leaves a default out and the other's callee has none to fill:
/// the copy with nothing to fill keeps the arguments as written.
#[test]
fn default_copy_fills_a_copy_whose_callee_has_no_defaults_is_left_alone() {
    let plain = "struct PlainIn { n: i64 }

impl PlainIn {
    fn go(self) -> i64 { self.n }
}

struct C { n: i64 }

impl Walk for C {
    type In = PlainIn;
    fn inner(self) -> PlainIn { PlainIn { n: self.n } }
}
";
    let main = "fn main() {\n    println(A { n: 1 }.run());\n    println(C { n: 5 }.run());\n}\n";
    assert_eq!(
        run("plain-ac", &format!("{WALK}\n{LOUD}\n{plain}\n{main}")),
        "11\n5\n"
    );
    assert_eq!(
        run("plain-ca", &format!("{WALK}\n{plain}\n{LOUD}\n{main}")),
        "11\n5\n"
    );
}
