//! `corpus/diagnostics/<code>/`: for each diagnostic code the corpus emits,
//! the smallest program that produces it (B-2026-11-16-303), which is the
//! example behind `karac explain <code>` (B-2026-11-16-296).
//!
//! Each directory is named for its code in lower case. The test checks that
//! `karac check --output=json` still reports that code for the program, and
//! that `karac explain` gives the code a description of its own rather than
//! the "no class page yet" stub. The corpus runner runs the same entries on
//! every backend.

use std::process::{Command, Stdio};

fn karac(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn diagnostics_corpus_programs_produce_their_code_and_are_explained() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/diagnostics");
    let mut dirs: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let mut bad = Vec::new();
    for dir in &dirs {
        let code = dir.file_name().unwrap().to_str().unwrap().to_uppercase();
        let json = karac(dir, &["check", "--output=json", "source.kara"]);
        if !json.contains(&format!("\"code\":\"{code}\"")) {
            bad.push(format!(
                "{code}: `karac check` no longer reports it:\n{json}"
            ));
        }
        let explained = karac(dir, &["explain", &code]);
        if explained.contains("no class page yet") || !explained.contains(&code) {
            bad.push(format!(
                "{code}: `karac explain` has no description:\n{explained}"
            ));
        }
    }
    // A void run (the directory moved, nothing matched) must not pass.
    assert!(
        dirs.len() >= 80,
        "only {} entries under {root:?}",
        dirs.len()
    );
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

/// B-2026-11-16-297: a code for a later-track or removed construct is not
/// described by the class it is filed under.
#[test]
fn diagnostics_corpus_later_track_codes_get_the_scope_paragraph() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for code in [
        "E_ARCHIVED",
        "E_DEFERRED_M4A",
        "E_TRY_BLOCK_NOT_IMPLEMENTED_YET",
        "E_TAIT_NOT_IMPLEMENTED_YET",
        "E0520",
    ] {
        let out = karac(dir, &["explain", code]);
        assert!(
            out.contains("not in the language this compiler implements"),
            "{code}: {out}"
        );
        assert!(
            !out.contains("Operand, argument, or return-value type")
                && !out.contains("Method-call dispatch found no matching impl"),
            "{code} still prints an unrelated class paragraph: {out}"
        );
        let json = karac(dir, &["explain", "--format=json", code]);
        assert!(
            json.contains("not in the language this compiler implements"),
            "{code}: {json}"
        );
    }
}

/// B-2026-11-16-297: a symbolic code with no prose of its own says that its
/// class paragraph is the class's, not the code's.
#[test]
fn diagnostics_corpus_symbolic_code_frames_its_class_paragraph() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = karac(dir, &["explain", "E_UNION_REQUIRES_REPR"]);
    assert!(
        out.contains("filed under the TYPE_MISMATCH class, which gathers many"),
        "{out}"
    );
    let e0216 = karac(dir, &["explain", "E0216"]);
    assert!(e0216.contains("labeled arguments"), "{e0216}");
    assert!(!e0216.contains("Operand, argument"), "{e0216}");
}

/// B-2026-11-17-77: `Point.make()` where two traits `Point` implements each
/// declare `make` is ambiguous (corpus/diagnostics/e0233), but an inherent
/// `make` is found first, and a trait named with the expected type picks its
/// own impl.
#[test]
fn diagnostics_corpus_inherent_assoc_fn_is_not_ambiguous() {
    let dir = tempfile_dir("assoc_inherent");
    std::fs::write(
        dir.join("main.kara"),
        "trait Origin { fn make() -> Self; }
trait Unit { fn make() -> Self; }
struct Point { x: i64 }
impl Origin for Point { fn make() -> Point { Point { x: 0 } } }
impl Unit for Point { fn make() -> Point { Point { x: 1 } } }
impl Point { fn make() -> Point { Point { x: 2 } } }
fn main() {
    let a: Point = Unit.make();
    let b = Point.make();
    println(f\"{a.x} {b.x}\");
}
",
    )
    .unwrap();
    let out = karac(&dir, &["check", "--output=json", "main.kara"]);
    assert!(!out.contains("\"code\":\"E0233\""), "{out}");
    assert!(!out.contains("\"severity\":\"error\""), "{out}");
}

fn tempfile_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac_diag_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
