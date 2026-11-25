//! Two collections.md promises the library conformance map found broken,
//! each program run on the interpreter and, under `--features llvm`,
//! compiled (conformance/library/collections).
//!
//! - A `Str` view hashes as its `String` does, so `s.as_str().hash(mut h)`
//!   must type-check: it was refused with "no method 'hash'" (B-2026-11-25-28).
//! - `==` on a `Map` or `Set` of a user `BuildHasher` compares contents: it
//!   was false when compiled, even for a table and its clone (B-2026-11-25-29).

mod common;

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-coll-fix-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

/// Exit code, stdout and stderr of `src` on each lane.
fn lanes(name: &str, src: &str) -> Vec<(&'static str, Option<i32>, String, String)> {
    let file = program(name, src);
    let mut cmds = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        cmds.push("__mir-llvm-run");
    }
    let mut out = Vec::new();
    for cmd in cmds {
        let o = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg(cmd)
            .arg(&file)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&o.stderr).to_string();
        if cmd == "__mir-llvm-run"
            && stderr.contains("undefined reference")
            && common::link_or_skip(Err(stderr.clone())).is_none()
        {
            continue;
        }
        out.push((
            cmd,
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).to_string(),
            stderr,
        ));
    }
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
    out
}

fn all_lanes_print(name: &str, src: &str, want: &str) {
    let runs = lanes(name, src);
    assert!(!runs.is_empty());
    for (cmd, code, stdout, stderr) in runs {
        assert_eq!(code, Some(0), "{cmd}: {stderr}");
        assert_eq!(stdout, want, "{cmd}: {stderr}");
    }
}

#[test]
fn str_view_hashes_like_its_string_b_2026_11_25_28() {
    all_lanes_print(
        "strhash",
        r#"fn main() {
    let s = "abc".to_string();
    let mut a = SipHash13BuildHasher {}.build();
    let mut b = SipHash13BuildHasher {}.build();
    s.hash(mut a);
    s.as_str().hash(mut b);
    println(a.finish() == b.finish());
    println(s.as_str() == s);
    println(s.cmp(s.as_str()) == Ordering.Equal);
}
"#,
        "true\ntrue\ntrue\n",
    );
}

#[test]
fn table_of_a_user_hasher_equals_its_clone_b_2026_11_25_29() {
    all_lanes_print(
        "tableeq",
        r#"struct Rec { n: u64 }
impl Hasher for Rec {
    fn write(mut ref self, bytes: Slice[u8]) { for b in bytes { self.n = self.n.wrapping_mul(31).wrapping_add(b as u64); } }
    fn finish(self) -> u64 { self.n }
}
struct RecBuild {}
impl BuildHasher for RecBuild {
    type Hasher = Rec;
    fn build(self) -> Rec { Rec { n: 7 } }
}
fn main() {
    let mut s: Set[i64, RecBuild] = Set.new();
    s.insert(1);
    s.insert(2);
    println(s == s.clone());
    let mut t: Set[i64, RecBuild] = Set.new();
    t.insert(1);
    println(s == t);
    let mut m: Map[String, i64, RecBuild] = Map.new();
    m["a"] = 1;
    println(m == m.clone());
    let mut f: Map[i64, i64, FxBuildHasher] = Map.new();
    f[1] = 1;
    println(f == f.clone());
}
"#,
        "true\nfalse\ntrue\ntrue\n",
    );
}
