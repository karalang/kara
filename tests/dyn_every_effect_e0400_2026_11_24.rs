//! B-2026-11-24-23: a public function calling through a `dyn` value whose
//! type writes no `with` clause has every effect. Its E0400 named the
//! internal `__dyn_every.T.t` for `blocks` (the one verb with no resource),
//! and which effect the first finding named changed from run to run, as
//! the check walked the set in hash order. One finding, naming the call,
//! the same on every run.

use std::process::{Command, Stdio};

const SRC: &str = "pub trait T { fn t(self) -> i64; }
struct P {}
impl T for P { fn t(self) -> i64 { println(\"t\"); 1 } }
pub struct H { h: dyn T }
pub fn call(x: ref H) -> i64 with writes(Stdout) { x.h.t() }
fn main() { println(call(H { h: P {} })); }
";

fn check(dir: &std::path::Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "cell.kara"])
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    assert!(!out.status.success());
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn dyn_every_effect_e0400_names_the_call_once_and_stably() {
    let dir = std::env::temp_dir().join(format!("karac-dyn-every-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), SRC).unwrap();
    let first = check(&dir);
    assert_eq!(first.matches("error[E0400]").count(), 1, "{first}");
    assert!(!first.contains("__dyn_every"), "{first}");
    assert!(
        first.contains(
            "performs allocates(*) but does not declare it (via a call of `T.t` at line 5"
        ),
        "{first}"
    );
    // A fresh process hashes under a fresh key; the text must not move.
    for _ in 0..4 {
        assert_eq!(check(&dir), first);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
