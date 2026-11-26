//! `#[derive(Serialize, Deserialize)]` refuses a field whose type, or a
//! part of it, has no impl in the library: `i128`, `u128` and `Result[T,
//! E]`, alone or inside `Vec` / `Option` (B-2026-11-25-49). `Array[T, N]`
//! was refused too until the library implemented it (B-2026-11-26-69).
//! `karac check` accepted them and every backend then stopped at "no method
//! 'serialize' on type 'i128'"; a direct `Json.serialize` of one was
//! already refused. Field types the library does implement still derive.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-serde-library-field-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn program(field: &str) -> String {
    format!(
        "import std.json.{{Json}};\n#[derive(Serialize, Deserialize)]\nstruct A {{ x: {field} }}\nfn main() {{ let t = Json.serialize(A {{ x: Default.default() }}); println(t.len()); }}\n"
    )
}

#[test]
fn serde_derive_refuses_a_field_without_a_library_impl() {
    for (field, missing) in [
        ("i128", "i128"),
        ("u128", "u128"),
        ("Result[i64, String]", "Result"),
        ("Vec[i128]", "i128"),
        ("Option[u128]", "u128"),
    ] {
        let (ok, text) = check(field, &program(field));
        assert!(!ok, "{field}: accepted");
        assert!(
            text.contains(&format!(
                "the library has no Serialize impl for '{missing}'"
            )),
            "{field}: {text}"
        );
    }
}

#[test]
fn serde_derive_accepts_fields_the_library_implements() {
    for field in [
        "u64",
        "i64",
        "f64",
        "String",
        "Vec[i64]",
        "Option[String]",
        "SortedMap[String, i64]",
        "SortedSet[i64]",
        "(i64, bool)",
        "Map[String, Vec[u8]]",
        "Array[i64, 2]",
        "Array[Array[String, 2], 3]",
        "Vec[Array[u8, 4]]",
    ] {
        let src = format!(
            "import std.json.{{Json}};\n#[derive(Serialize, Deserialize)]\nstruct A {{ x: {field} }}\nfn main() {{ println(1); }}\n"
        );
        let (ok, text) = check(field, &src);
        assert!(ok, "{field}: {text}");
    }
}
