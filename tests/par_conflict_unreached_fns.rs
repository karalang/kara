//! B-2026-11-22-24: a `par` branch conflict (E0418), and a provider use while
//! a task of the same group holds it (E0424), are properties of the function
//! they are written in, so `karac check` reports them in every function of the
//! program's own, not only in what `main` reaches: a file with no `main`, a
//! `pub fn` nothing calls, and a library package.

use std::process::{Command, Output};

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("karac-par-unreached-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn check_file(tag: &str, src: &str) -> String {
    let d = dir(tag);
    std::fs::write(d.join("main.kara"), src).unwrap();
    text(
        &Command::new(env!("CARGO_BIN_EXE_karac"))
            .current_dir(&d)
            .args(["check", "main.kara"])
            .output()
            .unwrap(),
    )
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)
}

const HELPERS: &str = "fn append_log(log: mut ref Vec[String]) { log.push(\"x\".to_string()); }
fn read_log_count(log: Vec[String]) -> i64 { log.len() }
";

const CONFLICT: &str = "fn invalid_aliasing(log: own Vec[String]) {
    let mut log = log;
    par { append_log(mut log), read_log_count(log) };
}
";

#[test]
fn par_conflict_in_a_file_with_no_main_is_reported() {
    let t = check_file("no-main", &format!("{HELPERS}{CONFLICT}"));
    assert!(t.contains("error[E0418]: main.kara:5:5"), "{t}");
}

#[test]
fn par_conflict_in_a_pub_fn_main_does_not_call_is_reported() {
    let t = check_file(
        "pub-fn",
        &format!("{HELPERS}pub {CONFLICT}fn main() {{}}\n"),
    );
    assert!(t.contains("error[E0418]: main.kara:5:5"), "{t}");
}

#[test]
fn par_conflict_reached_from_main_is_reported_once() {
    let t = check_file(
        "called",
        &format!("{HELPERS}{CONFLICT}fn main() {{ invalid_aliasing(Vec.new()); }}\n"),
    );
    assert_eq!(t.matches("error[E0418]").count(), 1, "{t}");
}

#[test]
fn par_without_a_conflict_in_an_unreached_fn_passes() {
    let t = check_file(
        "ok",
        &format!(
            "{HELPERS}fn reads(log: own Vec[String]) -> i64 {{
    let (a, b) = par {{ read_log_count(log), read_log_count(log) }};
    a + b
}}
"
        ),
    );
    assert!(t.contains("All checks passed."), "{t}");
}

#[test]
fn par_conflict_in_a_library_package_is_reported() {
    let d = dir("lib");
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(
        d.join("kara.toml"),
        "[package]\nname = \"loglib\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    std::fs::write(d.join("src/lib.kara"), format!("{HELPERS}pub {CONFLICT}")).unwrap();
    let t = text(
        &Command::new(env!("CARGO_BIN_EXE_karac"))
            .current_dir(&d)
            .arg("check")
            .output()
            .unwrap(),
    );
    assert!(
        t.contains("error[E0418]") && t.contains("lib.kara:5:5"),
        "{t}"
    );
}

#[test]
fn task_provider_conflict_in_an_unreached_fn_is_reported() {
    let t = check_file(
        "task",
        "import std.sync.{TaskGroup}; effect resource Clk: ClkP;
trait ClkP {
    fn now(ref self) -> i64;
    fn bump(mut ref self) {}
}
struct F { t: i64 }
impl ClkP for F {
    fn now(ref self) -> i64 { self.t }
    fn bump(mut ref self) { self.t = self.t + 1; }
}
fn run() -> i64 {
    with_provider[Clk](F { t: 0 }, || {
        let tg = TaskGroup.new();
        let a = tg.spawn(|| { Clk.bump(); Clk.now() });
        let b = tg.spawn(|| { Clk.bump(); Clk.now() });
        a.join() + b.join()
    })
}
fn main() {}
",
    );
    assert!(t.contains("error[E0424]: main.kara:15:17"), "{t}");
}

// Review ruling 2026-11-23: in a function `main` does not reach, only a
// conflict every caller would meet is reported. Two parameters that may or
// may not be one channel, or calls through two function-value parameters,
// rest on what a caller passes, so they are left to the caller's build.

const PRODUCE: &str =
    "import std.sync.{SendError, Sender}; fn produce(tx: Sender[i64], from: i64, to: i64) -> Result[(), SendError[i64]] {
    for i in from..to { tx.send(i)?; }
    Ok(())
}
";

#[test]
fn unreached_par_on_one_channel_binding_is_reported() {
    let t = check_file(
        "one-binding",
        &format!(
            "{PRODUCE}fn one(tx: Sender[i64]) {{
    let (r1, r2) = par {{ produce(tx, 0, 4), produce(tx, 4, 8) }};
    let _ = (r1, r2);
}}
fn main() {{}}
"
        ),
    );
    assert!(t.contains("error[E0418]: main.kara:6:20"), "{t}");
}

#[test]
fn unreached_par_on_two_channel_parameters_is_left_to_callers() {
    let t = check_file(
        "two-params",
        &format!(
            "{PRODUCE}fn two(a: Sender[i64], b: Sender[i64]) {{
    let (r1, r2) = par {{ produce(a, 0, 4), produce(b, 4, 8) }};
    let _ = (r1, r2);
}}
fn main() {{}}
"
        ),
    );
    assert!(t.contains("All checks passed."), "{t}");
}

#[test]
fn unreached_par_through_function_value_parameters_is_left_to_callers() {
    let t = check_file(
        "fn-params",
        "fn both(f: Fn() -> i64 + CrossTask, g: Fn() -> i64 + CrossTask) -> i64 {
    let (a, b) = par { f(), g() };
    a + b
}
fn main() {}
",
    );
    assert!(t.contains("All checks passed."), "{t}");
}

#[test]
fn unreached_par_printing_in_both_branches_is_reported() {
    let t = check_file(
        "stdout",
        "fn noisy() {
    par { println(\"a\"), println(\"b\") };
}
fn main() {}
",
    );
    assert!(t.contains("error[E0418]: main.kara:2:5"), "{t}");
}
