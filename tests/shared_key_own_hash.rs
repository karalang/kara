//! B-2026-11-18-53: on mir-llvm a `shared` type used as a `Set` or `Map`
//! key was hashed field by field, never by its own `Hash` impl: the hasher
//! saw through the handle to the body before looking for the impl, which
//! is recorded for the `shared` type. Two keys its `Eq` calls equal but
//! whose other fields differ then hashed apart, so a set kept duplicates
//! (50 entries where the interpreter has 5), and a key holding a field with
//! no structural hash (a `Set`) failed to build ("feeding a `Set[shared M]`
//! to a hasher is not lowered yet").

use std::process::Command;

fn run(backend: &str, src: &str, tag: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(backend)
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{backend}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const SRC: &str = r#"shared struct M { v: i64, tag: String, kids: Set[M] }

impl Hash for M {
    fn hash[H: Hasher](self, hasher: mut ref H) {
        hasher.write_i64(self.v);
    }
}

impl PartialEq for M {
    fn eq(self, other: M) -> bool { self.v == other.v }
}

impl Eq for M {}

fn main() {
    let mut s: Set[M] = Set.new();
    let mut m: Map[M, i64] = Map.new();
    let mut i = 0;
    while i < 50 {
        s.insert(M { v: i % 5, tag: f"t{i}", kids: Set.new() });
        m[M { v: i % 7, tag: f"m{i}", kids: Set.new() }] = i;
        i += 1;
    }
    println(f"{s.len()} {m.len()} {s.contains(M { v: 3, tag: "other", kids: Set.new() })}");
}
"#;

#[test]
fn shared_key_hashes_by_its_own_impl_on_every_lane() {
    assert_eq!(run("__mir-run", SRC, "skh-interp"), "5 7 true\n");
    #[cfg(feature = "llvm")]
    assert_eq!(run("__mir-llvm-run", SRC, "skh-llvm"), "5 7 true\n");
}
