//! design.md §8: `[T]` is never applied at a call site. `Ok[i64, String](3)`
//! parsed and its brackets were dropped without a word, so the `Ok`'s error
//! type stayed unsolved and the refusal named a binding annotation only
//! (B-2026-11-08-23). `karac check` now names the qualified form.

use std::process::{Command, Stdio};

#[test]
fn a_variant_call_with_type_arguments_names_the_qualified_form() {
    let path = std::env::temp_dir().join(format!(
        "karac-variant-type-args-{}.kara",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "fn main() { let x = Ok[i64, String](3); println(x.is_ok()); }\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    let err =
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains(":1:21: Kāra has no call-site type arguments")
            && err.contains("write `Result[i64, String].Ok(..)`"),
        "{err}"
    );
}
