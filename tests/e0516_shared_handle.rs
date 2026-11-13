//! An E0516 whose borrow lends a `shared` value (the probe's LRU cache:
//! `Some(e) => { let e2 = e; self.unlink(e2); .. }` over `self.index.get(k)`
//! for a `shared struct Ent`). The help named `.clone()` and "`*r` for a
//! `Copy` value"; for a shared handle `*r` is the copy, a second counted
//! handle that holds no borrow. The help now says so, and `karac fix`
//! writes `let e2 = *e;`.

use std::process::{Command, Stdio};

const LRU: &str = "shared struct Ent {
    key: String,
    mut val: i64,
}

struct Cache {
    index: Map[String, Ent],
    order: Vec[Ent],
}

impl Cache {
    fn touch(mut ref self, e: Ent) {
        self.order.push(e);
    }

    fn get(mut ref self, k: Str) -> Option[i64] {
        match self.index.get(k) {
            Some(e) => {
                let e2 = e;
                self.touch(e2);
                Some(e2.val)
            }
            None => None,
        }
    }
}

fn main() {
    let mut c = Cache { index: Map.new(), order: Vec.new() };
    let a = Ent { key: \"a\".to_string(), val: 7 };
    c.index.insert(\"a\".to_string(), a);
    println(c.get(\"a\").unwrap_or(0));
    println(c.get(\"b\").unwrap_or(0));
    c.order[0].val = 9;
    println(c.index.get(\"a\").map(|e| e.val).unwrap_or(0));
    println(c.order.len());
}
";

fn karac(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
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
fn e0516_over_a_shared_handle_says_star_is_a_counted_copy_and_fix_writes_it() {
    let dir = std::env::temp_dir().join(format!("karac-e0516-shared-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), LRU).unwrap();
    let (ok, said) = karac(&dir, &["check", "main.kara"]);
    assert!(!ok, "{said}");
    assert!(said.contains("[E0516]"), "{said}");
    assert!(
        said.contains("`Ent` is `shared`, so `*r` of a `ref Ent` is a second counted handle"),
        "{said}"
    );
    assert!(!said.contains("`*r` for a `Copy` value"), "{said}");
    let (ok, said) = karac(&dir, &["fix", "main.kara"]);
    assert!(ok, "{said}");
    let mended = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    assert!(mended.contains("let e2 = *e;"), "{mended}");
    let (ok, said) = karac(&dir, &["check", "main.kara"]);
    assert!(ok, "{said}\n{mended}");
    // The handle is the map's own value: a write through the copy pushed to
    // `order` is seen through the map.
    let (ok, out) = karac(&dir, &["__mir-run", "main.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert_eq!(out, "7\n0\n9\n1\n");
}

/// A borrow of a plain struct keeps the general help.
#[test]
fn e0516_over_a_plain_value_keeps_the_copy_help() {
    let src = "#[derive(Clone)]
struct Ent {
    val: i64,
}

struct Cache {
    index: Map[String, Ent],
    hits: i64,
}

impl Cache {
    fn hit(mut ref self) {
        self.hits += 1;
    }

    fn get(mut ref self, k: Str) -> Option[i64] {
        match self.index.get(k) {
            Some(e) => {
                let e2 = e;
                self.hit();
                Some(e2.val)
            }
            None => None,
        }
    }
}

fn main() {
    let mut c = Cache { index: Map.new(), hits: 0 };
    c.index.insert(\"a\".to_string(), Ent { val: 7 });
    println(c.get(\"a\").unwrap_or(0));
}
";
    let dir = std::env::temp_dir().join(format!("karac-e0516-plain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let (ok, said) = karac(&dir, &["check", "main.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "{said}");
    assert!(said.contains("[E0516]"), "{said}");
    assert!(said.contains("`*r` for a `Copy` value"), "{said}");
    assert!(!said.contains("counted handle"), "{said}");
}
