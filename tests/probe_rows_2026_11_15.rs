//! Probe rows of 2026-11-15.
//!
//! - B-2026-11-15-18 (PV1): a `FileSystem` fake whose handle method uses a
//!   resource overridden where one of its files is used is refused by the
//!   check (core semantics §14.11), also when that override is nested inside
//!   the `FileSystem` one; it used to print to the real stdout.
//! - B-2026-11-15-19 (TR1): `std.random.next_u64()` names the import form,
//!   and `karac fix` writes the import with the shortened call.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-probe-1115-{tag}-{}-{}",
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

const FAKE: &str = "struct P {}
impl FileSystemProvider for P {
  fn open(mut ref self, path: Str, opts: ref OpenOptions) -> Result[i64, IoError] { Ok(1) }
  fn close(mut ref self, h: i64) { println(\"closed\"); }
}
struct Cap { s: String }
impl StdoutProvider for Cap { fn write(mut ref self, s: Str) { self.s.push_str(s); } fn flush(mut ref self) {} }
";

#[test]
fn fake_close_printing_under_a_nested_stdout_override_is_refused() {
    let src = format!(
        "{FAKE}fn main() {{
  with_provider[FileSystem](P {{}}, || {{
    with_provider[Stdout](Cap {{ s: String.new() }}, || {{ let _f = File.create(\"a\").unwrap(); }});
  }});
}}
"
    );
    let (dir, path) = fixture("nested", &src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("`P.close` uses `Stdout`, which is overridden where this file is used"),
        "{err}"
    );
    assert_eq!(err.matches("`P.close` uses `Stdout`").count(), 1, "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fake_close_printing_with_no_stdout_override_runs() {
    let src = format!(
        "{FAKE}fn main() {{
  with_provider[FileSystem](P {{}}, || {{ let _f = File.create(\"a\").unwrap(); }});
  println(\"done\");
}}
"
    );
    let (dir, path) = fixture("plain", &src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "closed\ndone\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn std_module_path_names_the_import_and_fix_writes_it() {
    for (tag, src, want, out_shape) in [
        (
            "random",
            "fn main() {\n    let n = std.random.next_u64();\n    println(n >= 0u64);\n}\n",
            "`import std.random;` and then `random.next_u64`",
            "true\n",
        ),
        (
            "uuid",
            "fn main() {\n    let id = std.uuid.v4();\n    println(id.len());\n}\n",
            "`import std.uuid;` and then `uuid.v4`",
            "36\n",
        ),
    ] {
        let (dir, path) = fixture(tag, src);
        let out = karac().arg("check").arg(&path).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(want), "{tag}: {err}");
        karac().arg("fix").arg(&path).output().unwrap();
        let fixed = std::fs::read_to_string(&path).unwrap();
        let check = karac().arg("check").arg(&path).output().unwrap();
        assert!(
            check.status.success(),
            "{tag}: {fixed}\n{}",
            String::from_utf8_lossy(&check.stderr)
        );
        assert!(
            fixed.starts_with(&format!("import std.{tag};\n")),
            "{fixed}"
        );
        let run = karac().arg("__mir-run").arg(&path).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), out_shape, "{tag}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    // Already imported: the fix only shortens the call.
    let src = "import std.random;\nfn main() {\n    let n = std.random.next_u64();\n    println(n >= 0u64);\n}\n";
    let (dir, path) = fixture("imported", src);
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert_eq!(fixed.matches("import std.random;").count(), 1, "{fixed}");
    assert!(fixed.contains("let n = random.next_u64();"), "{fixed}");
    let _ = std::fs::remove_dir_all(&dir);
}
