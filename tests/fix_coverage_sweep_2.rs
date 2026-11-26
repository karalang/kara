//! The second round of the corpus fix-coverage sweep (review thread,
//! 2026-11-16): the legacy `Stdio.Piped` rewrite that pruning hid, a
//! library type named without its import, the did-you-mean that renamed
//! `Pool[T]` to a primitive or a local, an escaping closure that captures a
//! bare parameter, and `assert_eq` on a type without `Debug`. Each test runs
//! `karac fix` and checks the program it leaves.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

/// Writes `src`, runs `karac fix`, and returns the fixed text and the
/// output of `karac check` on it.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-fixcov2-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let fix = karac(&["fix", p]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let after = std::fs::read_to_string(&f).unwrap();
    let check = karac(&["check", p]);
    let text = String::from_utf8_lossy(&check.stdout).into_owned()
        + &String::from_utf8_lossy(&check.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    (after, text)
}

/// B-2026-11-16-186: the rewrite looked for `Stdio.piped` among the impls,
/// which the library pruning had dropped because the program never spells
/// `piped`.
#[test]
fn legacy_stdio_variants_become_constructors() {
    let (after, check) = fixed(
        "stdio",
        "import std.process.{Command, Stdio};
fn main() {
    let quiet = true;
    let c = Command.new(\"true\")
        .stdout(Stdio.Piped)
        .stdin(if quiet { Stdio.Null } else { Stdio.Inherit });
    let _ = c;
}
",
    );
    assert!(after.contains(".stdout(Stdio.piped())"), "{after}");
    assert!(
        after.contains("if quiet { Stdio.null() } else { Stdio.inherit() }"),
        "{after}"
    );
    assert!(check.contains("All checks passed"), "{check}");
}

/// B-2026-11-16-187: `Pool` written as a type with an argument is not
/// `bool`, nor the local `pool`; it is `std.pool`'s, and the fix imports it.
#[test]
fn an_unimported_library_type_gets_its_import() {
    let (after, check) = fixed(
        "pool",
        "struct Conn { id: i64 }
fn connect() -> Result[Conn, String] { Ok(Conn { id: 1 }) }
fn closed(e: PoolError[String]) -> bool {
    match e {
        PoolError.PoolClosed => true,
        _ => false,
    }
}
fn main() {
    let pool: Pool[Conn, String] = Pool.new(connect, 1, 2);
    println(f\"{closed(PoolError.PoolClosed)}\");
    let _ = pool;
}
",
    );
    assert!(after.contains("import std.pool.{Pool};\n"), "{after}");
    assert!(after.contains("import std.pool.{PoolError};\n"), "{after}");
    assert!(after.contains("let pool: Pool[Conn, String]"), "{after}");
    assert!(!after.contains("bool[Conn"), "{after}");
    assert!(check.contains("All checks passed"), "{check}");
}

/// B-2026-11-16-191: a capitalised name is offered a primitive only in
/// another case, and a type is never offered a value binding.
#[test]
fn an_undefined_type_is_not_renamed_to_a_primitive_or_a_local() {
    let dir = std::env::temp_dir().join(format!("karac-fixcov2-typo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(
        &f,
        "fn main() {
    let pool = 1;
    let a: Bool = true;
    let b: Pooll[i64] = pool;
}
",
    )
    .unwrap();
    let out = karac(&["check", f.to_str().unwrap()]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        text.contains("undefined type 'Bool', did you mean 'bool'?"),
        "{text}"
    );
    assert!(text.contains("undefined type 'Pooll'"), "{text}");
    assert!(!text.contains("'Pooll', did you mean"), "{text}");
}

/// B-2026-11-16-188: an escaping closure that captured a bare parameter
/// was refused E0515, and `karac fix` took the parameter `own`. Since
/// B-2026-11-26-74 (review ruling 2026-11-26, core-semantics §5.7, §9.3)
/// such a closure is a view result borrowing the argument: `make` checks
/// as written, and a temporary argument is the caller's E0516, bound first.
#[test]
fn an_escaping_capture_of_a_bare_parameter_borrows_the_argument() {
    let make = "fn make(p: String, base: i64) -> Fn(i64) -> i64 {
    |n| p.len() + base + n
}
";
    let check = |tag: &str, main: &str| {
        let dir = std::env::temp_dir().join(format!("karac-fixcov2-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("main.kara");
        std::fs::write(&f, format!("{make}{main}")).unwrap();
        let out = karac(&["check", f.to_str().unwrap()]);
        let _ = std::fs::remove_dir_all(&dir);
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
    };
    let temp = check(
        "escape-temp",
        "fn main() {\n    let f = make(\"abc\".to_string(), 1);\n    println(f\"{f(2)}\");\n}\n",
    );
    assert!(temp.contains("E0516"), "{temp}");
    assert!(
        temp.contains("Bind the expression to a variable first"),
        "{temp}"
    );
    let bound = check(
        "escape-bound",
        "fn main() {\n    let s = \"abc\".to_string();\n    let f = make(s, 1);\n    println(f\"{f(2)}\");\n}\n",
    );
    assert!(bound.contains("All checks passed"), "{bound}");
    // `karac fix` binds the temporary (B-2026-11-26-80).
    let (after, check) = fixed(
        "escape-fix",
        &format!("{make}fn main() {{\n    let f = make(\"abc\".to_string(), 1);\n    println(f\"{{f(2)}}\");\n}}\n"),
    );
    assert!(
        after.contains("    let p = \"abc\".to_string();\n    let f = make(p, 1);\n"),
        "{after}"
    );
    assert!(check.contains("All checks passed"), "{check}");
}

/// B-2026-11-16-189: `assert_eq` on the program's own type without `Debug`
/// derives it.
#[test]
fn assert_eq_operands_derive_debug() {
    let (after, check) = fixed(
        "debug",
        "#[derive(PartialEq)]
enum E {
    A(i64),
    B,
}
fn main() {
    assert_eq(E.A(1), E.A(1));
    assert_ne(E.A(1), E.B);
    println(\"ok\");
}
",
    );
    assert_eq!(after.matches("#[derive(Debug)]\n").count(), 1, "{after}");
    assert!(check.contains("All checks passed"), "{check}");
}

/// B-2026-11-16-190: two imports of one library module are one import, so
/// the items both reach are spliced once.
#[test]
fn two_imports_of_one_library_module_splice_its_items_once() {
    let (_, check) = fixed(
        "twoimports",
        "import std.pool.{Pool};
import std.pool.{PoolError};
fn main() {
    let e: PoolError[String] = PoolError.Timeout;
    let _ = e;
    println(\"ok\");
}
",
    );
    assert!(check.contains("All checks passed"), "{check}");
}
