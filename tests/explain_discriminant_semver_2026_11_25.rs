//! B-2026-11-25-17, design.md § Semver lock: `karac explain` on a function
//! that calls `.discriminant()` on a C-like enum with no `#[repr]` says the
//! value is a local property, not a semver contract. A `#[repr(intN)]`
//! enum's discriminants are part of its API, so its call gets no note.

use std::path::{Path, PathBuf};
use std::process::Command;

fn file(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-disc-semver-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn explain(path: &Path, item: &str, json: bool) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.arg("explain");
    if json {
        cmd.arg("--format=json");
    }
    let out = cmd
        .arg(format!("{}.{item}", path.display()))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn explain_discriminant_without_repr_says_local() {
    // conformance/design/ffi/explain_discriminant_without_repr_says_local.kara
    let cell = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("conformance/design/ffi/explain_discriminant_without_repr_says_local.kara");
    let path = file("cell", &std::fs::read_to_string(cell).unwrap());
    let text = explain(&path, "code", false);
    assert!(text.contains("\nSemver:\n"), "{text}");
    assert!(
        text.contains("`.discriminant()` on `Plain`, which has no `#[repr]`"),
        "{text}"
    );
    assert!(text.contains("not a semver contract"), "{text}");
    let json = explain(&path, "code", true);
    assert!(
        json.contains(
            "\"local_discriminants\":[{\"enum\":\"Plain\",\"line\":6,\"semver\":\"local\"}]"
        ),
        "{json}"
    );
}

#[test]
fn explain_discriminant_with_repr_has_no_semver_note() {
    let path = file(
        "repr",
        "#[repr(u8)]\nenum R { A, B }\nfn code() -> u8 { R.B.discriminant() }\nfn main() { println(f\"{code()}\"); }\n",
    );
    let text = explain(&path, "code", false);
    assert!(!text.contains("Semver:"), "{text}");
    assert!(explain(&path, "code", true).contains("\"local_discriminants\":[]"));
}

#[test]
fn explain_discriminant_note_names_only_the_items_own_calls() {
    let path = file(
        "method",
        "enum Plain { A, B }\nstruct S { p: Plain }\nimpl S {\n    fn tag(self) -> u32 { self.p.discriminant() }\n}\nfn other() -> i64 { 1 }\nfn main() { let s = S { p: Plain.A }; println(f\"{s.tag()} {other()}\"); }\n",
    );
    let text = explain(&path, "S.tag", false);
    assert!(
        text.contains("m.kara:4  `.discriminant()` on `Plain`"),
        "{text}"
    );
    assert!(!explain(&path, "other", false).contains("Semver:"));
}
