//! `unused_variable` (B-2026-11-20-146): a `let`, `for` or pattern binding
//! that nothing names again was accepted without a word. The lint reports it,
//! off by default as the style lints are, and its fix writes `_name`, which
//! design.md § Naming CN-5 reads as unused on purpose and which keeps the
//! binding, so nothing is dropped earlier than before.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every call gets a directory of its own, whatever names two tests pass.
static CALL: AtomicUsize = AtomicUsize::new(0);

fn karac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-unused-variable-{tag}-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn hits(report: &str) -> usize {
    report.matches("warning[unused_variable]").count()
}

/// `src` reports `names` under `-W`, `karac fix -W` writes `want`, and the
/// result checks clean under `-W` and prints what `src` printed.
fn fixed_to(tag: &str, src: &str, names: &[&str], want: &str) {
    let f = scratch(tag, src);
    let p = f.to_str().unwrap();
    let before = karac(&["__mir-run", p]);
    assert!(before.status.success(), "{tag}: run: {}", text(&before));
    let warned = text(&karac(&["check", "-W", "unused_variable", p]));
    assert_eq!(hits(&warned), names.len(), "{tag}: {warned}");
    for name in names {
        assert!(
            warned.contains(&format!(
                "`{name}` is bound and never used; name it `_{name}`"
            )),
            "{tag}: {name}: {warned}"
        );
    }
    let fix = karac(&["fix", "-W", "unused_variable", p]);
    assert!(fix.status.success(), "{tag}: fix: {}", text(&fix));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), want, "{tag}");
    let recheck = karac(&["check", "-W", "unused_variable", p]);
    assert!(recheck.status.success(), "{tag}: {}", text(&recheck));
    assert_eq!(hits(&text(&recheck)), 0, "{tag}: {}", text(&recheck));
    let after = karac(&["__mir-run", p]);
    assert!(after.status.success(), "{tag}: run: {}", text(&after));
    assert_eq!(before.stdout, after.stdout, "{tag}");
}

/// `src` has no `unused_variable` hit under `-W`.
fn silent(tag: &str, src: &str) {
    let f = scratch(tag, src);
    let out = karac(&["check", "-W", "unused_variable", f.to_str().unwrap()]);
    let report = text(&out);
    assert!(out.status.success(), "{tag}: {report}");
    assert_eq!(hits(&report), 0, "{tag}: {report}");
}

const ROW: &str = "fn main() {
    let x = 5;
    let steps: Vec[i64] = [1, 2];
    for (i, step) in steps.iter().enumerate() {
        println(step);
    }
}
";

/// The row's program: nothing by default, as for every style lint.
#[test]
fn unused_variable_is_off_by_default() {
    let f = scratch("default", ROW);
    let out = karac(&["check", f.to_str().unwrap()]);
    let report = text(&out);
    assert!(out.status.success(), "{report}");
    assert!(!report.contains("unused_variable"), "{report}");
    assert!(!report.contains("warning"), "{report}");
}

/// The row's two bindings, a `let` and a `for` tuple's index.
#[test]
fn unused_variable_reports_a_let_and_a_for_binding_and_fix_writes_the_underscore() {
    fixed_to(
        "row",
        ROW,
        &["x", "i"],
        "fn main() {
    let _x = 5;
    let steps: Vec[i64] = [1, 2];
    for (_i, step) in steps.iter().enumerate() {
        println(step);
    }
}
",
    );
}

/// Bindings of `match` arms, `if let`, `while let`, `let … else` and a
/// struct pattern's explicit field.
#[test]
fn unused_variable_reports_pattern_bindings() {
    fixed_to(
        "patterns",
        "struct P {
    x: i64,
    y: i64,
}

enum Shape {
    Dot,
    Circle(i64),
    Rect(i64, i64),
}

fn area(s: Shape) -> i64 {
    match s {
        Dot => 0,
        Circle(r) => 3,
        Rect(w, h) => w,
    }
}

fn first(o: Option[(i64, i64)]) -> i64 {
    let Some((a, b)) = o else {
        return 0;
    };
    a
}

fn main() {
    let P { x: px, y: py } = P { x: 1, y: 2 };
    println(px);
    if let Some(n) = Some(3) {
        println(\"some\");
    }
    let mut stack: Vec[i64] = [1, 2];
    let mut pops = 0;
    while let Some(top) = stack.pop() {
        pops += 1;
    }
    println(pops);
    println(area(Shape.Rect(2, 3)) + area(Shape.Circle(1)) + area(Shape.Dot));
    println(first(Some((4, 5))));
}
",
        &["r", "h", "b", "py", "n", "top"],
        "struct P {
    x: i64,
    y: i64,
}

enum Shape {
    Dot,
    Circle(i64),
    Rect(i64, i64),
}

fn area(s: Shape) -> i64 {
    match s {
        Dot => 0,
        Circle(_r) => 3,
        Rect(w, _h) => w,
    }
}

fn first(o: Option[(i64, i64)]) -> i64 {
    let Some((a, _b)) = o else {
        return 0;
    };
    a
}

fn main() {
    let P { x: px, y: _py } = P { x: 1, y: 2 };
    println(px);
    if let Some(_n) = Some(3) {
        println(\"some\");
    }
    let mut stack: Vec[i64] = [1, 2];
    let mut pops = 0;
    while let Some(_top) = stack.pop() {
        pops += 1;
    }
    println(pops);
    println(area(Shape.Rect(2, 3)) + area(Shape.Circle(1)) + area(Shape.Dot));
    println(first(Some((4, 5))));
}
",
    );
}

/// The fix keeps the binding, so a value with a `Drop` body is dropped at
/// the end of its scope, where it was: `let _ = …` would drop it at once.
#[test]
fn unused_variable_fix_keeps_the_value_alive_to_the_end_of_its_scope() {
    let src = "struct Guard {
    name: String,
}

impl Drop for Guard {
    fn drop(mut ref self) {
        println(f\"drop {self.name}\");
    }
}

fn main() {
    let guard = Guard { name: \"g\".to_string() };
    println(\"body\");
}
";
    let f = scratch("drop", src);
    let p = f.to_str().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&karac(&["__mir-run", p]).stdout),
        "body\ndrop g\n"
    );
    fixed_to(
        "drop-fixed",
        src,
        &["guard"],
        &src.replace("let guard", "let _guard"),
    );
}

/// A name used in a closure, an interpolation, a guard, a later shadowing
/// `let`, or only assigned, is named again; a parameter, a closure
/// parameter, an or-pattern's name, a shorthand field, an `@` binding and a
/// `_` name are exempt.
#[test]
fn unused_variable_leaves_named_and_exempt_bindings_alone() {
    silent(
        "named",
        "struct P {
    x: i64,
    y: i64,
}

fn ignore(a: i64, b: String) -> i64 {
    7
}

fn main() {
    let captured = 7;
    let f = |q: i64| captured + 1;
    println(f(1));
    let g = |unused: i64| 2;
    println(g(1));
    let shown = 9;
    println(f\"{shown}\");
    let mut assigned = 0;
    assigned = 3;
    let _kept = 4;
    let limit = 2;
    let o: Option[i64] = Some(1);
    match o {
        Some(k) if k > limit => println(\"big\"),
        Some(1) | Some(2) => println(\"small\"),
        Some(n @ 3..=9) => println(\"mid\"),
        _ => println(\"none\"),
    }
    let r: Result[i64, i64] = Ok(1);
    match r {
        Ok(v) | Err(v) => println(\"either\"),
    }
    let P { x, y } = P { x: 1, y: 2 };
    let shadowed = 1;
    let shadowed = shadowed + 1;
    println(shadowed);
    println(ignore(1, \"s\".to_string()));
}
",
    );
}

/// A branch's `let` in a `par` block is named by the code after the block.
#[test]
fn unused_variable_reads_a_par_branch_binding_named_after_the_block() {
    silent(
        "par",
        "fn main() {
    let (a, b) = par { 1 + 1, 2 + 2 };
    println(a + b);
}
",
    );
}

/// The lint takes the levels every lint takes: an attribute turns it on or
/// off for one function, and `-D` makes it an error with its `E` code.
#[test]
fn unused_variable_takes_attribute_and_command_line_levels() {
    let src = "#[warn(unused_variable)]
fn warned() {
    let a = 1;
}

#[allow(unused_variable)]
fn allowed() {
    let b = 2;
}

fn plain() {
    let c = 3;
}

fn main() {
    warned();
    allowed();
    plain();
}
";
    let f = scratch("levels", src);
    let p = f.to_str().unwrap();
    let default = text(&karac(&["check", p]));
    assert_eq!(hits(&default), 1, "{default}");
    assert!(default.contains("`a` is bound and never used"), "{default}");
    let warned = text(&karac(&["check", "-W", "unused_variable", p]));
    assert_eq!(hits(&warned), 2, "{warned}");
    assert!(!warned.contains("`b` is bound"), "{warned}");
    let denied = karac(&["check", "-D", "unused_variable", p]);
    let report = text(&denied);
    assert!(!denied.status.success(), "{report}");
    assert!(report.contains("error[unused_variable]"), "{report}");
    let json = text(&karac(&[
        "check",
        "--output=json",
        "-W",
        "unused_variable",
        p,
    ]));
    assert!(json.contains("\"W0324\""), "{json}");
    let explained = text(&karac(&["explain", "W0324"]));
    assert!(explained.contains("unused_variable"), "{explained}");
}
