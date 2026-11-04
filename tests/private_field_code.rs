//! B-2026-11-04-46 — design.md §4 Visibility: "A non-`pub` type in a `pub`
//! signature is `E0221 PrivateTypeInPublicSignature`. Direct access to an
//! invisible item is `E0111 PrivateItemAccess`." A field refused across a
//! directory (B-2026-11-04-22) was coded E0221.

use std::path::Path;
use std::process::Command;

fn check(dir: &Path, main: &str) -> (bool, String) {
    std::fs::write(dir.join("src/main.kara"), main).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .current_dir(dir)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_private_field_from_another_directory_is_e0111() {
    let dir = std::env::temp_dir().join(format!("karac-field-code-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/db")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"fc\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/db/c.kara"),
        "pub struct S { pub a: i64, private secret: i64 }\n\
         pub fn mk() -> S { S { a: 1, secret: 2 } }\n",
    )
    .unwrap();
    for main in [
        "import db.c.{mk};\nfn main() { let s = mk(); println(s.secret); }\n",
        "import db.c.{S};\nfn main() { let s = S { a: 1, secret: 2 }; println(s.a); }\n",
    ] {
        let (ok, out) = check(&dir, main);
        assert!(!ok, "{main}: {out}");
        assert!(out.contains("error[E0111]"), "{main}: {out}");
        assert!(!out.contains("E0221"), "{main}: {out}");
        assert!(
            out.contains("field 'secret' of struct 'S' is `private`"),
            "{main}: {out}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
