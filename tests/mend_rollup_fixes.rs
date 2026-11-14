//! B-2026-11-14-8: five refusals named their rewrite but carried no edit, so `karac
//! fix` left the program refused. Each probe cell (probe-core/mend) is
//! checked, fixed to a fixed point, checked again and run on the MIR
//! interpreter against its expected output.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-mend-rollup-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

/// `check` refuses; `karac fix`, run to a fixed point, makes the program
/// pass with `edit` in it, and it prints `out`.
fn fixed_and_runs(tag: &str, src: &str, edit: &str, out: &str) {
    let (dir, path) = fixture(tag, src);
    let first = karac().arg("check").arg(&path).output().unwrap();
    assert!(!first.status.success(), "{tag}: check must refuse:\n{src}");
    for _ in 0..4 {
        karac().arg("fix").arg(&path).output().unwrap();
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        check.status.success(),
        "{tag}: check must pass after `karac fix`; source now:\n{fixed}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(
        fixed.contains(edit),
        "{tag}: expected `{edit}` in:\n{fixed}"
    );
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        out,
        "{tag}: fixed program's output; source:\n{fixed}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mend_method_typo_is_renamed() {
    fixed_and_runs(
        "typo-method",
        "fn main() { let v = [1, 2, 3]; println(f\"{v.lenn()}\"); }\n",
        "v.len()",
        "3\n",
    );
    fixed_and_runs(
        "typo-method-arg",
        "fn main() { let v = [1, 2, 3]; println(v.containz(2)); }\n",
        "v.contains(2)",
        "true\n",
    );
}

#[test]
fn mend_debug_spec_is_display() {
    fixed_and_runs(
        "debug-fmt",
        "fn main() { let x = 3; println(f\"{x:?}\"); }\n",
        "f\"{x}\"",
        "3\n",
    );
    fixed_and_runs(
        "debug-fmt-width",
        "fn main() { let x = 3; println(f\"[{x:>3?}]\"); }\n",
        "{x:>3}",
        "[  3]\n",
    );
}

#[test]
fn mend_escaping_return_drops_the_word() {
    fixed_and_runs(
        "escaping-return",
        "fn mk(n: i64) -> escaping Fn(i64) -> i64 { |x| x + n }\n\
         fn main() { println(f\"{mk(1)(5)}\"); }\n",
        "fn mk(n: i64) -> Fn(i64) -> i64",
        "6\n",
    );
}

#[test]
fn mend_nested_fn_on_one_line_moves_up() {
    fixed_and_runs(
        "nested-fn",
        "fn main() { fn add(a: i64, b: i64) -> i64 { a + b } println(f\"{add(1, 2)}\"); }\n",
        "fn add(a: i64, b: i64) -> i64 { a + b }\n\nfn main() {",
        "3\n",
    );
}

#[test]
fn mend_box_new_in_a_hole_is_its_argument() {
    fixed_and_runs(
        "box-hole",
        "fn main() { let b = 5; println(f\"{Box.new(b)}\"); }\n",
        "f\"{b}\"",
        "5\n",
    );
    // The probe's cell: `Box[T]` payloads, then a recursive enum, which
    // `karac fix` makes `shared` (B-2026-11-14-1).
    fixed_and_runs(
        "box-dyn-field",
        "enum T { Leaf(i64), Node(Box[T], Box[T]) }\n\
         fn s(t: T) -> i64 { match t { T.Leaf(n) => n, T.Node(a, b) => s(a) + s(b) } }\n\
         fn main() { println(f\"{s(T.Node(Box.new(T.Leaf(1)), Box.new(T.Leaf(2))))}\"); }\n",
        "T.Node(T.Leaf(1), T.Leaf(2))",
        "3\n",
    );
}
