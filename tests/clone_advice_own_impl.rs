//! A type whose `.clone()` comes from its own `impl Clone` (a program's, or
//! a library hasher's: docs/library/crypto.md) gets "clone it at the move
//! site" on a use after move, not "add `#[derive(Clone)]`" or "has no
//! `.clone()`", and `karac fix`'s `.clone()` re-checks clean.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-clone-advice-{tag}-{}-{}",
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

fn check(path: &PathBuf) -> (bool, String) {
    let out = karac().arg("check").arg(path).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
    )
}

fn advises_clone_and_fix_rechecks(tag: &str, src: &str) {
    let (dir, path) = fixture(tag, src);
    let (ok, err) = check(&path);
    assert!(!ok, "{tag}: check accepted it");
    assert!(
        err.contains("clone 'h' at the move site (`h.clone()`)"),
        "{tag}: no clone advice:\n{err}"
    );
    assert!(
        !err.contains("derive(Clone)") && !err.contains("has no `.clone()`"),
        "{tag}: advice ignores the impl:\n{err}"
    );
    let fix = karac().arg("fix").arg(&path).output().unwrap();
    assert!(
        fix.status.success() || fix.status.code() == Some(1),
        "{tag}: fix crashed"
    );
    let (ok, err) = check(&path);
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(ok, "{tag}: fixed file does not check:\n{fixed}\n{err}");
    assert!(fixed.contains("h.clone()"), "{tag}: fix wrote:\n{fixed}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn own_impl_clone_on_a_drop_struct_is_the_move_site_advice() {
    advises_clone_and_fix_rechecks(
        "user",
        "struct H { h: i64 }\n\
         impl Clone for H {\n    fn clone(self) -> H { H { h: self.h + 1 } }\n}\n\
         impl Drop for H {\n    fn drop(mut ref self) { println(\"d\"); }\n}\n\
         fn eat(h: own H) { println(f\"{h.h}\"); }\n\
         fn main() {\n    let h = H { h: 1 };\n    eat(h);\n    eat(h);\n}\n",
    );
}

#[test]
fn library_hasher_clone_is_the_move_site_advice() {
    advises_clone_and_fix_rechecks(
        "hasher",
        "import std.crypto.sha2.{Sha256};\n\
         import std.encoding.{hex_encode};\n\
         fn eat(h: own Sha256) { println(hex_encode(h.finish())); }\n\
         fn main() {\n    let mut h = Sha256.new();\n    h.update(\"a\".bytes());\n    eat(h);\n    eat(h);\n}\n",
    );
}
