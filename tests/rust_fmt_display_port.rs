//! B-2026-11-15-36: a Rust `impl fmt::Display` was a `karac fix` dead end.
//! The impl itself is now rewritten to Kāra's `to_string` (3e72c72f48);
//! what was left was the `use std::fmt;` line, which became `import
//! std.fmt;`, an unknown module with no fix. Kāra's `Display` is in the
//! prelude and has no `Formatter` or `fmt::Result`, so the fix deletes the
//! line, and both spellings of the port mend to a program that runs.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

const BODY: &str = "struct Cell {
    col: char,
    row: i64,
}

impl IMPL for Cell {
    fn fmt(&self, f: &mut FORMATTER) -> RESULT {
        write!(f, \"{}{}\", self.col, self.row)
    }
}

fn main() {
    let c = Cell { col: 'a', row: 3 };
    println(f\"{c}\");
}
";

fn mended(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-rustfmt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    for _ in 0..4 {
        let _ = karac(&["fix", p]);
    }
    let after = std::fs::read_to_string(&f).unwrap();
    let check = karac(&["check", p]);
    assert!(
        check.status.success(),
        "{after}\n{}",
        String::from_utf8_lossy(&check.stdout)
    );
    let run = karac(&["run", "--interp", p]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "a3\n", "{after}");
    let _ = std::fs::remove_dir_all(&dir);
    after
}

#[test]
fn a_rust_display_impl_over_the_fmt_module_mends_and_runs() {
    let src = format!(
        "use std::fmt;\n\n{}",
        BODY.replace("IMPL", "fmt::Display")
            .replace("FORMATTER", "fmt::Formatter")
            .replace("RESULT", "fmt::Result")
    );
    let after = mended("module", &src);
    assert!(!after.contains("fmt"), "{after}");
    assert!(after.starts_with("struct Cell"), "{after}");
}

#[test]
fn a_rust_display_impl_over_imported_names_mends_and_runs() {
    let src = format!(
        "use std::fmt::{{Display, Formatter, Result}};\n\n{}",
        BODY.replace("IMPL", "Display")
            .replace("FORMATTER", "Formatter")
            .replace("RESULT", "Result")
    );
    let after = mended("names", &src);
    assert!(!after.contains("Formatter"), "{after}");
    assert!(after.starts_with("struct Cell"), "{after}");
}
