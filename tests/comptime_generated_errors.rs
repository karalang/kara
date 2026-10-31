//! A resolve error in an item a derive generated is the user's to see: the
//! build stops on it, so `karac check` reports it too, at the derive's site, and
//! the MIR front end names it as a diagnostic rather than a Debug struct
//! (B-2026-10-31-5). `examples/protobuf_schema.kara`, whose repeated field
//! number 8 generated `let mut i8 = 0`, runs.

use std::path::PathBuf;
use std::process::{Command, Output};

const RESERVED: &str = r#"comptime fn derive_count(comptime T: Type) -> Vec[Item] {
    let n = T.name();
    [ast.item("impl " + n + " { fn count(self) -> i64 { let i8 = 1; i8 } }")]
}

#[derive(Count)]
struct Widget { id: i64 }

fn main() {
    let w = Widget { id: 7 };
    println(w.count());
}
"#;

fn karac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap()
}

fn file(name: &str, src: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-gen-errors-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let f = d.join(name);
    std::fs::write(&f, src).unwrap();
    f
}

#[test]
fn check_reports_a_resolve_error_in_a_derived_item_at_the_derive() {
    let f = file("reserved.kara", RESERVED);
    let o = karac(&["check", f.to_str().unwrap()]);
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(o.status.code(), Some(1), "{all}");
    assert!(all.contains("error[resolve]"), "{all}");
    assert!(
        all.contains(
            "reserved.kara:7:1: in the code generated here: `i8` is a primitive type name"
        ),
        "{all}"
    );
}

#[test]
fn mir_front_end_names_a_derived_items_resolve_error() {
    let f = file("reserved_run.kara", RESERVED);
    let o = karac(&["__mir-run", f.to_str().unwrap()]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_ne!(o.status.code(), Some(0), "{err}");
    assert!(!err.contains("ResolveError {"), "{err}");
    assert!(
        err.contains("resolve: 7:1: in the code generated here: `i8` is a primitive type name"),
        "{err}"
    );
    assert!(!err.contains("i8_value"), "{err}");
}

#[test]
fn protobuf_schema_example_with_a_repeated_field_eight_runs() {
    let ex = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/protobuf_schema.kara");
    let check = karac(&["check", ex]);
    assert_eq!(
        check.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let o = karac(&["__mir-run", ex]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        out.contains("ratings=[4.5, 5.0, 3.5]") && out.ends_with("ping host=localhost count=7\n"),
        "{out}"
    );
}
