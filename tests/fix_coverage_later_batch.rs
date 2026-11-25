//! The fix-coverage table's later batch (2026-11-24): diagnostics whose
//! edit is determined now carry it, and the package-version messages name
//! the form to write. A union's where-clause bounds nothing and every union
//! field is writable, so deleting either means the same program; a layout
//! block is deferred and MIR never reads it, so deleting it does not change
//! what the program does.

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
        "karac-later-batch-fix-{name}-{}-{n}",
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

#[test]
fn later_batch_union_where_clause_is_deleted() {
    let m = mend(
        "where",
        "E_UNION_WHERE_FORBIDDEN",
        "#[repr(C)]\nunion Bits where i32: Copy {\n    a: i32,\n    b: f32,\n}\n\nfn main() {}\n",
    );
    assert_mended(
        &m,
        "#[repr(C)]\nunion Bits {\n    a: i32,\n    b: f32,\n}\n\nfn main() {}\n",
        None,
    );
}

#[test]
fn later_batch_union_field_mut_is_deleted() {
    let m = mend(
        "mut",
        "E_UNION_FIELD_MUT_FORBIDDEN",
        "#[repr(C)]\nunion Bits {\n    mut a: i32,\n    b: f32,\n}\n\nfn main() {}\n",
    );
    assert_mended(
        &m,
        "#[repr(C)]\nunion Bits {\n    a: i32,\n    b: f32,\n}\n\nfn main() {}\n",
        None,
    );
}

const LAYOUT: &str =
    "layout particles: Vec[Particle] {\n    group position { x, y }\n    group cold { mass }\n}";

#[test]
fn later_batch_layout_block_is_deleted() {
    let m = mend(
        "layout",
        "E_DEFERRED_LAYOUT_BLOCK",
        &format!(
            "struct Particle {{ x: f64, y: f64, mass: f64 }}\n\n{LAYOUT}\n\nfn main() {{\n    let p = Particle {{ x: 1.0, y: 2.0, mass: 3.0 }};\n    println(p.x + p.y + p.mass);\n}}\n"
        ),
    );
    assert_mended(
        &m,
        "struct Particle { x: f64, y: f64, mass: f64 }\n\n\n\nfn main() {\n    let p = Particle { x: 1.0, y: 2.0, mass: 3.0 };\n    println(p.x + p.y + p.mass);\n}\n",
        Some("6.0\n"),
    );
}

#[test]
fn later_batch_documented_layout_block_keeps_the_message_alone() {
    // A doc comment in front would be left on the next item, so no edit.
    let m = mend(
        "layout-doc",
        "E_DEFERRED_LAYOUT_BLOCK",
        &format!(
            "struct Particle {{ x: f64, y: f64, mass: f64 }}\n\n/// Hot and cold.\n{LAYOUT}\n\nfn main() {{}}\n"
        ),
    );
    assert!(!m.fixable, "{}", m.first);
}

#[test]
fn later_batch_toolchain_version_messages_name_the_form() {
    use karac::karac_toolchain::parse_toolchain_file;
    let path = Path::new("karac-toolchain.toml");
    let err = parse_toolchain_file(path, "targets = []\n").unwrap_err();
    assert_eq!(err.code(), "E_TOOLCHAIN_MISSING_VERSION");
    assert!(err.to_string().contains("version = \"0.8\""), "{err}");
    let err = parse_toolchain_file(path, "version = \"latest\"\n").unwrap_err();
    assert_eq!(err.code(), "E_TOOLCHAIN_INVALID_VERSION_CONSTRAINT");
    assert!(err.to_string().contains("\">=0.8, <1.0\""), "{err}");
}

#[test]
fn later_batch_install_version_message_names_the_form() {
    let err = karac::install_spec::parse_install_spec("my_tool@latest").unwrap_err();
    assert_eq!(err.code(), "E_INSTALL_INVALID_VERSION");
    assert!(err.to_string().contains("`<name>@1.0`"), "{err}");
}
