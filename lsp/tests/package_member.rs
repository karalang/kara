//! B-2026-11-05-15: kara-lsp checked every document as a file of its own, so
//! `import util;` in `src/main.kara` of a package got a false "unknown
//! module" error and every multi-file package showed red in the editor. A
//! document inside a package's `src/` is now checked with its package, as
//! `karac check <file>` checks it, with the editor's buffer in place of the
//! file on disk.

use std::str::FromStr;

use kara_lsp::analysis::{diagnostics_for, file_path};
use lsp_types::Uri;

fn package(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kara-lsp-pkg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::write(dir.join("src/util.kara"), "pub fn f() -> i64 { 1 }\n").unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import util;\nfn main() { println(util.f()); }\n",
    )
    .unwrap();
    plain(std::fs::canonicalize(dir).unwrap())
}

/// `path` without the `\\?\` prefix Windows' `canonicalize` puts on it.
fn plain(path: std::path::PathBuf) -> std::path::PathBuf {
    match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) => std::path::PathBuf::from(rest),
        None => path,
    }
}

/// The `file:` URI of `path`: `file:///tmp/x` on Unix, `file:///C:/x` for
/// Windows' `C:\x`, with a space percent-encoded.
fn uri_of(path: &std::path::Path) -> Uri {
    let mut s = path.display().to_string().replace('\\', "/");
    if !s.starts_with('/') {
        s.insert(0, '/');
    }
    let s = s.replace(' ', "%20");
    Uri::from_str(&format!("file://{s}")).unwrap()
}

#[test]
fn a_package_member_sees_its_sibling_modules() {
    let dir = package("ok");
    let main = dir.join("src/main.kara");
    let text = std::fs::read_to_string(&main).unwrap();
    let diags = diagnostics_for(&text, &uri_of(&main));
    assert!(diags.is_empty(), "{diags:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_package_member_is_checked_from_its_unsaved_buffer() {
    let dir = package("buf");
    let main = dir.join("src/main.kara");
    // The buffer differs from the disk: the error is on line 3 only there.
    let text = "import util;\nfn main() {\n    let s: String = util.f();\n    println(s);\n}\n";
    let diags = diagnostics_for(text, &uri_of(&main));
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert!(
        diags[0].message.contains("expected 'String', found 'i64'"),
        "{diags:#?}"
    );
    assert_eq!(diags[0].range.start.line, 2, "{diags:#?}");
    // A wrong sibling name is still an error.
    let text = "import utils;\nfn main() { println(utils.f()); }\n";
    let diags = diagnostics_for(text, &uri_of(&main));
    assert!(!diags.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_lone_file_still_cannot_import_a_user_module() {
    let dir = std::env::temp_dir().join(format!("kara-lsp-lone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("lone.kara");
    let text = "import util;\nfn main() { println(util.f()); }\n";
    std::fs::write(&file, text).unwrap();
    let diags = diagnostics_for(text, &uri_of(&file));
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("belongs to no package")),
        "{diags:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_uris_decode_to_paths() {
    let u = Uri::from_str("file:///home/a%20b/src/main.kara").unwrap();
    assert_eq!(
        file_path(&u).unwrap(),
        std::path::PathBuf::from("/home/a b/src/main.kara")
    );
    let u = Uri::from_str("file://localhost/x.kara").unwrap();
    assert_eq!(file_path(&u).unwrap(), std::path::PathBuf::from("/x.kara"));
    let u = Uri::from_str("untitled:Untitled-1").unwrap();
    assert!(file_path(&u).is_none());
}
