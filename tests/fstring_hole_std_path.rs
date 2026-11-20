//! B-2026-11-20-108: a `std.` path into one of Kāra's library modules,
//! written inside an f-string hole, is refused with the import form and
//! fixed as it is outside a hole (library/time-random-env.md § RandomSource).

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-hole-std-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

#[test]
fn std_path_in_fstring_hole_names_the_import_form() {
    let (dir, path) = fixture(
        "check",
        "fn main() {\n    println(f\"{std.uuid.v4().len()}\");\n}\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("`std.uuid.v4` is a path into Kāra's `std.uuid` module"),
        "{err}"
    );
    assert!(err.contains("p.kara:2:16"), "{err}");
    assert!(!err.contains("undefined name 'std'"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn std_path_in_fstring_hole_is_fixed_with_its_import() {
    let (dir, path) = fixture(
        "fix",
        "fn main() {\n    let x = std.random.next_u64();\n    println(f\"{x > 0} {std.uuid.v4().len()}\");\n}\n",
    );
    let out = karac().arg("fix").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        fixed,
        "import std.random;\nimport std.uuid;\nfn main() {\n    let x = random.next_u64();\n    println(f\"{x > 0} {uuid.v4().len()}\");\n}\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn std_path_in_fstring_hole_offers_no_second_import() {
    let (dir, path) = fixture(
        "imported",
        "import std.uuid;\nfn main() {\n    println(f\"{std.uuid.v4().len()}\");\n}\n",
    );
    let out = karac().arg("fix").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        fixed,
        "import std.uuid;\nfn main() {\n    println(f\"{uuid.v4().len()}\");\n}\n"
    );
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "36\n");
    let _ = std::fs::remove_dir_all(&dir);
}
