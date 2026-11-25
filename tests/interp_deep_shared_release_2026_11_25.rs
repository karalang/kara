//! B-2026-11-25-27: `karac run --interp` aborted with a Rust stack overflow
//! (exit 134) dropping a 1,000,000-node `shared` chain, since the
//! interpreter's `release` recursed through `drop_adt` once per level; the
//! compiled glue already looped (B-2026-11-16-311). The interpreter now
//! releases such a chain in a loop, and the order of `Drop` bodies is the
//! recursion's (design.md §16, conformance/design/targets/
//! deep_shared_chain_drops_take_no_frames).

use std::path::PathBuf;
use std::process::Command;

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-deep-release-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

fn interp(tag: &str, src: &str) -> (Option<i32>, String, String) {
    let (dir, path) = fixture(tag, src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn interp_drops_a_million_deep_shared_chain() {
    let src = "shared struct Node { v: i64, next: Option[Node] }\n\
               fn main() {\n    let mut head: Option[Node] = None;\n    \
               for i in 0..1000000 { head = Some(Node { v: i, next: head }); }\n    \
               match head { Some(n) => println(n.v), None => println(\"none\") }\n}\n";
    let (code, out, err) = interp("chain", src);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, "999999\n", "{err}");
}

#[test]
fn interp_deep_release_keeps_the_drop_order() {
    // A node whose last-dropped field is the handle loops; one with a
    // droppable field after it in drop order recurses. Both keep the
    // recursion's order: each node's later fields, then the rest of the
    // chain, then its earlier fields.
    let src = "struct Tag { n: i64 }\n\
               impl Drop for Tag { fn drop(mut ref self) { println(f\"tag {self.n}\"); } }\n\
               shared struct N { next: Option[N], t: Tag }\n\
               shared struct P { t: Tag, next: Option[P], u: Tag }\n\
               shared enum L { Nil, Cons(L, Tag) }\n\
               fn main() {\n    let mut h: Option[N] = None;\n    \
               for i in 0..3 { h = Some(N { next: h, t: Tag { n: i } }); }\n    \
               let mut p: Option[P] = None;\n    \
               for i in 0..3 { p = Some(P { t: Tag { n: 10 + i }, next: p, u: Tag { n: 100 + i } }); }\n    \
               let mut l = L.Nil;\n    \
               for i in 0..3 { l = L.Cons(l, Tag { n: 20 + i }); }\n    \
               println(\"end\");\n}\n";
    let (code, out, err) = interp("order", src);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        out,
        "end\ntag 22\ntag 21\ntag 20\ntag 102\ntag 101\ntag 100\ntag 10\ntag 11\ntag 12\n\
         tag 2\ntag 1\ntag 0\n",
        "{err}"
    );
}
