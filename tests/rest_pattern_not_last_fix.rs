//! E_REST_PATTERN_NOT_LAST names one determined edit, moving the `..` after
//! the last field, and now carries it (fix-coverage table, the in-message
//! batch of 2026-11-24): the pattern binds the same fields and ignores the
//! same rest either way. Each pin applies it with `karac fix` and checks the
//! result clean and printing what the original meant.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn scratch(name: &str) -> PathBuf {
    let n = CALL.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "karac-rest-last-fix-{name}-{}-{n}",
        std::process::id()
    ))
}

struct Mended {
    /// Whether the first check's record for `code` carried an edit.
    fixable: bool,
    /// The first check's JSON, for failure messages.
    first: String,
    /// The text `karac fix` leaves.
    text: String,
    /// Whether that text checks with no diagnostic at all.
    clean: bool,
    /// The fixed check's output, for failure messages.
    after: String,
    /// What the fixed text prints under the MIR interpreter.
    out: String,
}

fn mend(name: &str, code: &str, src: &str) -> Mended {
    let dir = scratch(name);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, src).unwrap();
    let (_, first, _) = karac(&dir, &["check", "--output=json", "cell.kara"]);
    let json: serde_json::Value = serde_json::from_str(&first).expect("one JSON document");
    let fixable = json["diagnostics"].as_array().unwrap().iter().any(|d| {
        d["code"] == code
            && (d.get("replacement").is_some_and(|r| !r.is_null())
                || d.get("fix_diff").is_some_and(|r| !r.is_null()))
    });
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(&file).unwrap();
    let (_, after, after_err) = karac(&dir, &["check", "--output=json", "cell.kara"]);
    let clean = serde_json::from_str::<serde_json::Value>(&after)
        .ok()
        .and_then(|j| j["diagnostics"].as_array().map(|d| d.is_empty()))
        .unwrap_or(false);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    Mended {
        fixable,
        first,
        text,
        clean,
        after: format!("{after}{after_err}"),
        out,
    }
}

fn assert_mended(m: &Mended, fixed: &str, out: Option<&str>) {
    assert!(m.fixable, "no edit on the first record:\n{}", m.first);
    assert_eq!(m.text, fixed);
    assert!(m.clean, "the fixed text does not check clean:\n{}", m.after);
    if let Some(out) = out {
        assert_eq!(m.out, out);
    }
}

const P: &str =
    "struct P { x: i64, y: i64, z: i64 }\n\nfn main() {\n    let p = P { x: 1, y: 2, z: 3 };\n";

#[test]
fn rest_last_fix_moves_the_rest_after_one_field() {
    let m = mend(
        "one",
        "E_REST_PATTERN_NOT_LAST",
        &format!("{P}    let P {{ .., x }} = p;\n    println(x);\n}}\n"),
    );
    assert_mended(
        &m,
        &format!("{P}    let P {{ x, .. }} = p;\n    println(x);\n}}\n"),
        Some("1\n"),
    );
}

#[test]
fn rest_last_fix_moves_the_rest_after_several_fields() {
    let m = mend(
        "several",
        "E_REST_PATTERN_NOT_LAST",
        &format!("{P}    let P {{ .., x, y: b, }} = p;\n    println(x + b);\n}}\n"),
    );
    assert_mended(
        &m,
        &format!("{P}    let P {{ x, y: b, .., }} = p;\n    println(x + b);\n}}\n"),
        Some("3\n"),
    );
}

#[test]
fn rest_last_fix_in_a_match_arm() {
    let m = mend(
        "arm",
        "E_REST_PATTERN_NOT_LAST",
        &format!("{P}    match p {{\n        P {{ .., z }} => println(z),\n    }}\n}}\n"),
    );
    assert_mended(
        &m,
        &format!("{P}    match p {{\n        P {{ z, .. }} => println(z),\n    }}\n}}\n"),
        Some("3\n"),
    );
}
