//! Review-thread ruling 2026-11-18: `karac check` refuses a package whose
//! `karac-toolchain.toml` the running compiler does not satisfy, as `karac
//! build` does, but the editor only warns: its diagnostics still arrive.

use std::str::FromStr;

use kara_lsp::analysis::diagnostics_for;
use lsp_types::{DiagnosticSeverity, NumberOrString, Uri};

#[cfg(unix)]
#[test]
fn lsp_toolchain_pin_mismatch_is_a_warning_not_a_refusal() {
    let dir = std::env::temp_dir().join(format!("kara-lsp-pin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::write(dir.join("karac-toolchain.toml"), "version = \"=99.0.0\"\n").unwrap();
    let source = "fn main() { let x: i64 = true; }\n";
    std::fs::write(dir.join("src/main.kara"), source).unwrap();
    let main = std::fs::canonicalize(dir.join("src/main.kara")).unwrap();
    let uri = Uri::from_str(&format!("file://{}", main.display())).unwrap();
    let diags = diagnostics_for(source, &uri);
    let pin = diags
        .iter()
        .find(|d| {
            d.code
                == Some(NumberOrString::String(
                    "E_TOOLCHAIN_VERSION_MISMATCH".into(),
                ))
        })
        .unwrap_or_else(|| panic!("no pin warning: {diags:?}"));
    assert_eq!(pin.severity, Some(DiagnosticSeverity::WARNING));
    assert!(
        diags
            .iter()
            .any(|d| d.severity == Some(DiagnosticSeverity::ERROR)),
        "the program's own error must still be reported: {diags:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
