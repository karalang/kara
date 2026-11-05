//! `karac check --output=json`'s `public_function_effects` lists the
//! program's public functions only. The conversion traits' methods the effect
//! checker registers itself (`From.from`, `Into.into`, `TryFrom.try_from`,
//! `TryInto.try_into`) were listed for every program with a `pub fn`.

use std::process::Command;

#[test]
fn public_function_effects_lists_only_the_programs_functions() {
    let path = std::env::temp_dir().join(format!("karac-pfe-{}.kara", std::process::id()));
    std::fs::write(
        &path,
        "pub fn hi() with writes(Stdout) {\n    println(\"hi\");\n}\n\nfn main() {\n    hi();\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        doc["public_function_effects"],
        serde_json::json!({"hi": ["writes(Stdout)"]}),
        "{stdout}"
    );
}
