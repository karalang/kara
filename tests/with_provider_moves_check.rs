//! `with_provider[R](p, f)` moves `p` into the call (core semantics §14.4),
//! so `karac check` reports a later use of `p` as E0500, as `karac run`
//! and `karac build` already refused it (B-2026-11-01-33).

use std::process::{Command, Stdio};

fn check(src: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-with-provider-moves-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn with_provider_moves_check_refuses_a_later_use() {
    let json = check(
        "effect resource Clk: ClkP;
trait ClkP { fn now(ref self) -> i64; }
struct F { t: Vec[i64] }
impl ClkP for F { fn now(ref self) -> i64 { self.t[0] } }
fn main() {
    let p = F { t: vec![1] };
    let r = with_provider[Clk](p, || Clk.now());
    println(r);
    println(p.t[0]);
}
",
    );
    assert!(json.contains("\"code\":\"E0500\""), "{json}");
    assert!(json.contains("\"line\":9"), "{json}");
}
