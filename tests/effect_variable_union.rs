//! Several function values at one effect variable's positions in a call
//! give the union of their sets (design.md § Effect variables, "How it is
//! solved"), and the call is charged the union. The checker used to refuse
//! such a call as E0406 when the values' sets differed (B-2026-11-25-30);
//! that code is retired. A value bound later, in another function through
//! a parameter whose type fixed the set, must still fit: that is E0404.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

/// `karac <cmd> cell.kara` on `src`: success, and stdout + stderr.
fn karac(cmd: &str, src: &str) -> (bool, String) {
    let n = CALL.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("karac-effect-var-union-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([cmd, "cell.kara"])
        .current_dir(Path::new(&dir))
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

const PRELUDE: &str = "effect resource Log;
effect resource Db;

pub fn write_log() with writes(Log) writes(Stdout) { println(\"log\"); }
pub fn read_db() with reads(Db) writes(Stdout) { println(\"db\"); }

pub fn pipe[with E](a: Fn() with E, b: Fn() with E) with E {
    a();
    b();
}
";

fn program(run_sig: &str, call: &str) -> String {
    format!("{PRELUDE}\npub fn run() {run_sig} {{\n    {call}\n}}\n\nfn main() {{ run(); }}\n")
}

#[test]
fn effect_var_union_two_values_with_different_sets_are_accepted() {
    let src = program(
        "with writes(Log) reads(Db) writes(Stdout)",
        "pipe(|| write_log(), || read_db())",
    );
    let (ok, out) = karac("check", &src);
    assert!(ok, "the union {{writes(Log), reads(Db)}} solves E:\n{out}");
    assert!(!out.contains("E0406"), "{out}");
    let (ok, out) = karac("__mir-run", &src);
    assert!(ok, "{out}");
    assert_eq!(out, "log\ndb\n");
}

#[test]
fn effect_var_union_does_not_depend_on_argument_order() {
    let src = program(
        "with writes(Log) reads(Db) writes(Stdout)",
        "pipe(|| read_db(), || write_log())",
    );
    let (ok, out) = karac("check", &src);
    assert!(ok, "{out}");
    // Named functions as the values, too.
    let src = program(
        "with writes(Log) reads(Db) writes(Stdout)",
        "pipe(read_db, write_log)",
    );
    let (ok, out) = karac("check", &src);
    assert!(ok, "{out}");
}

#[test]
fn effect_var_union_is_charged_to_the_caller() {
    // `run` declares only one half of the union: the call is charged both.
    let src = program(
        "with writes(Log) writes(Stdout)",
        "pipe(|| write_log(), || read_db())",
    );
    let (ok, out) = karac("check", &src);
    assert!(!ok, "{out}");
    assert!(
        out.contains("E0400") && out.contains("reads(Db)"),
        "the undeclared half of the union is reported:\n{out}"
    );
    assert!(!out.contains("E0406"), "{out}");
}

#[test]
fn effect_var_union_through_a_method_with_its_own_variable() {
    let src = format!(
        "{PRELUDE}
struct Runner {{ n: i64 }}

impl Runner {{
    fn both[with E](self, a: Fn() with E, b: Fn() with E) with E {{
        a();
        b();
    }}
}}

pub fn run() with writes(Log) reads(Db) writes(Stdout) {{
    let r = Runner {{ n: 1 }};
    r.both(|| write_log(), || read_db());
}}

fn main() {{ run(); }}
"
    );
    let (ok, out) = karac("check", &src);
    assert!(ok, "{out}");
    assert!(!out.contains("E0406"), "{out}");
}

const BUS: &str = "effect resource Log;
effect resource Db;

pub fn write_log() with writes(Log) {}
pub fn read_db() with reads(Db) {}

struct Bus[with E] {
    handlers: Vec[Fn() with E],
}

impl[with E] Bus[with E] {
    fn register(mut ref self, h: escaping Fn() with E) {
        self.handlers.push(h);
    }
    fn emit(self) with E {
        for h in self.handlers { h(); }
    }
}
";

#[test]
fn effect_var_union_later_binding_in_another_function_must_fit() {
    // The control: `add_reader` sees the bus through a parameter whose type
    // fixes the set at `{writes(Log)}`, so a `reads(Db)` handler does not
    // fit. That is E0404, naming the extra effect and the type argument.
    let src = format!(
        "{BUS}
fn add_reader(bus: mut ref Bus[with writes(Log)]) {{
    bus.register(|| read_db());
}}

fn main() {{
    let mut bus = Bus {{ handlers: Vec.new() }};
    bus.register(|| write_log());
    add_reader(mut bus);
    bus.emit();
}}
"
    );
    let (ok, out) = karac("check", &src);
    assert!(!ok, "{out}");
    assert!(
        out.contains("E0404") && out.contains("reads(Db)") && out.contains("type argument"),
        "{out}"
    );

    // The same registration inside the function that built the bus joins
    // the union instead.
    let src = format!(
        "{BUS}
fn main() {{
    let mut bus = Bus {{ handlers: Vec.new() }};
    bus.register(|| write_log());
    bus.register(|| read_db());
    bus.emit();
}}
"
    );
    let (ok, out) = karac("check", &src);
    assert!(ok, "{out}");
}

#[test]
fn effect_var_union_e0406_is_retired() {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["explain", "E0406"])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("This code is retired"), "{text}");
    assert!(text.contains("E0404"), "{text}");
}
