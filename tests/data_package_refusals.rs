//! docs/library/data.md is the post-v1 `kara-data` package (M4c): nothing on
//! it runs in 0.8, so its refusals are the cells.
//!
//! B-2026-11-18-63: `import std.data.{DataFrame};` was told "`DataFrame` is
//! in the prelude, visible without an import, so the import can go", with a
//! machine fix deleting the import, after which every use was E0520 "not in
//! 0.8". The import is now refused for what it is, with no edit.
//!
//! B-2026-11-18-64: `Tensor.zeros([2, 2])` led with E0238 "cannot infer type
//! parameter 'S'" before the E0520 that says `Tensor` is not in 0.8; the
//! refusal now stands alone.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// `check --output=json` on `src`: each diagnostic's (code, message), and
/// whether any carries a fix.
fn check(src: &str) -> (Vec<(String, String)>, bool) {
    let dir = std::env::temp_dir().join(format!(
        "karac-data-refusals-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json", f.to_str().unwrap()])
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "{src} checked clean");
    let json = String::from_utf8_lossy(&out.stdout).into_owned();
    let diags = json
        .split("\"code\":\"")
        .skip(1)
        .map(|d| {
            let code = d.split('"').next().unwrap().to_string();
            let msg = d
                .split("\"message\":\"")
                .nth(1)
                .and_then(|m| m.split("\",\"").next())
                .unwrap_or("")
                .to_string();
            (code, msg)
        })
        .collect();
    (
        diags,
        json.contains("\"fix_diff\"") || json.contains("\"fixes\""),
    )
}

#[test]
fn std_data_import_names_the_package_not_the_prelude() {
    for src in [
        "import std.data.{DataFrame};\nfn main() {}\n",
        "import std.data.{Column, DataFrame};\nfn main() {}\n",
        "import std.data.{Frobnicate};\nfn main() {}\n",
    ] {
        let (diags, fixed) = check(src);
        assert_eq!(diags.len(), 1, "{src}: {diags:?}");
        let (code, msg) = &diags[0];
        assert_eq!(code, "E0112", "{src}: {msg}");
        assert!(msg.contains("`kara-data` package"), "{src}: {msg}");
        assert!(msg.contains("not in 0.8"), "{src}: {msg}");
        assert!(!msg.contains("prelude, visible"), "{src}: {msg}");
        assert!(!msg.contains("did you mean"), "{src}: {msg}");
        assert!(!fixed, "{src}: a fix was offered");
    }
}

#[test]
fn removed_type_call_is_refused_without_an_inference_echo() {
    for (src, ty) in [
        ("fn main() { let t = Tensor.zeros([2, 2]); }\n", "Tensor"),
        ("fn main() { let df = DataFrame.new(); }\n", "DataFrame"),
    ] {
        let (diags, _) = check(src);
        let codes: Vec<&str> = diags.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(codes, ["E0520"], "{src}: {diags:?}");
        assert!(diags[0]
            .1
            .contains(&format!("`{ty}` comes with the `kara-data` package")));
    }
}
