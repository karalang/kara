//! Core semantics §14.5 when the provider is bound out of sight: a function
//! called under a caller's `with_provider` (or in a test under
//! `#[with_provider]`) runs with that provider, so a callback it makes that
//! uses the resource and hands to an `escaping` parameter would keep a view
//! of the provider. The check refused one only when the closure was written
//! inside the `with_provider` itself (B-2026-10-30-7); outside an override
//! the same callback uses the static default and is fine.

use std::process::Command;

fn karac_in(dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-pescape-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    dir
}

const PLAIN: &str = r#"struct Mute {}
impl StdoutProvider for Mute {
    fn write(mut ref self, s: Str) { }
}
struct Bus[with E] { hs: Vec[Fn() with E] }
impl[with E] Bus[with E] {
    fn reg(mut ref self, h: escaping Fn() with E) { self.hs.push(h); }
    fn run(ref self) with E { for h in self.hs.iter() { h(); } }
}
fn audit() {
    let mut bus = Bus { hs: Vec.new() };
    bus.reg(|| println("tick"));
    bus.run();
}
fn main() {
    audit();

}
"#;

const OVERRIDDEN: &str = r#"struct Mute {}
impl StdoutProvider for Mute {
    fn write(mut ref self, s: Str) { }
}
struct Bus[with E] { hs: Vec[Fn() with E] }
impl[with E] Bus[with E] {
    fn reg(mut ref self, h: escaping Fn() with E) { self.hs.push(h); }
    fn run(ref self) with E { for h in self.hs.iter() { h(); } }
}
fn audit() {
    let mut bus = Bus { hs: Vec.new() };
    bus.reg(|| println("tick"));
    bus.run();
}
fn main() {
    audit();
    with_provider[Stdout](Mute {}, || audit());
}
"#;

#[test]
fn a_stored_callback_is_refused_under_a_callers_override_only() {
    let dir = scratch("bus");
    std::fs::write(dir.join("plain.kara"), PLAIN).unwrap();
    std::fs::write(dir.join("over.kara"), OVERRIDDEN).unwrap();
    let (ok, _, err) = karac_in(&dir, &["check", "plain.kara"]);
    assert!(ok, "no override: {err}");
    let (ok, _, err) = karac_in(&dir, &["check", "over.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "a callback that prints to the fake was stored");
    assert!(err.contains("a caller of `audit` binds"), "{err}");
    assert!(err.contains("`Stdout`"), "{err}");
    assert_eq!(
        karac::mir::lower::run_source(PLAIN)
            .expect("MIR interpreter")
            .output,
        "tick\n"
    );
}

#[test]
fn a_stored_callback_is_refused_under_a_test_with_provider() {
    let dir = scratch("test");
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        r#"struct FakeClock { t: i64 }
impl ClockProvider for FakeClock { fn now(ref self) -> i64 { self.t } }
struct Bus[with E] { hs: Vec[Fn() -> i64 with E] }
impl[with E] Bus[with E] {
    fn reg(mut ref self, h: escaping Fn() -> i64 with E) { self.hs.push(h); }
    fn run(ref self) -> i64 with E { let mut s = 0; for h in self.hs.iter() { s = s + h(); } s }
}
fn audit() -> i64 {
    let mut bus = Bus { hs: Vec.new() };
    bus.reg(|| Clock.now());
    bus.run()
}
fn main() {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main_test.kara"),
        r#"#[with_provider(Clock, FakeClock { t: 42 })]
test "stored clock callback" {
    assert_eq(audit(), 42);
}
"#,
    )
    .unwrap();
    let (ok, out, err) = karac_in(&dir, &["test"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "the test ran: {out}");
    assert!(out.contains("\"phase\":\"provider_escape\""), "{out}{err}");
    assert!(out.contains("a caller of `audit` binds"), "{out}{err}");
}
