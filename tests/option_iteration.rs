//! B-2026-11-18-11: an `Option` iterates as zero or one item, as Rust's
//! does: `opt.iter()` views the value, `opt.into_iter()` moves it out, and
//! `impl IntoIterator for Option[T]` lets an `Option` go where an
//! `IntoIterator` does (docs/library/core-types.md, review ruling
//! 2026-11-18). An `Option` is still not an `Iterator`, so `.flatten()` over
//! `Option` items stays refused with its `filter_map` fix
//! (B-2026-11-16-40, pinned in tests/iterators_conformance.rs).

use karac::check_source;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn errors(src: &str) -> Vec<String> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

fn modes() -> Vec<&'static str> {
    let mut m = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        m.push("__mir-llvm-run");
    }
    m
}

fn assert_prints(src: &str, want: &str) {
    static N: AtomicUsize = AtomicUsize::new(0);
    assert!(errors(src).is_empty(), "{src}\n{:#?}", errors(src));
    for mode in modes() {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("karac-optiter-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.kara"), src).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args([mode, "main.kara"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let err = String::from_utf8_lossy(&out.stderr);
        if mode != "__mir-run" && err.contains("runtime archive") {
            continue;
        }
        assert!(out.status.success(), "{mode}: {err}\n{src}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{mode}\n{src}");
    }
}

#[test]
fn an_option_iterates_as_zero_or_one_item() {
    assert_prints(
        "fn main() {
    let root: Option[i32] = Some(3);
    let v: Vec[i32] = root.into_iter().collect();
    println(v);
    let w: Vec[ref i32] = root.iter().collect();
    println(w.len());
    let none: Option[String] = None;
    println(f\"{none.iter().count()} {none.into_iter().count()}\");
    let name: Option[String] = Some(\"ab\".to_string());
    for s in name.iter() {
        println(s.len());
    }
    println(name.unwrap());
}
",
        "[3]\n1\n0 0\n2\nab\n",
    );
}

#[test]
fn an_option_goes_where_an_into_iterator_does() {
    assert_prints(
        "shared struct Node {
    val: i64,
    left: Option[Node],
    right: Option[Node],
}

fn total[C: IntoIterator[Item = i64]](items: own C) -> i64 {
    let mut t = 0;
    for x in items.into_iter() {
        t += x;
    }
    t
}

fn main() {
    let leaf = Node { val: 2, left: None, right: None };
    let t = Node { val: 1, left: Some(leaf), right: None };
    let level: Vec[Node] = [t.left, t.right].into_iter().flat_map(|n| n.into_iter()).collect();
    println(level.len());
    let xs: Vec[Option[i64]] = [Some(1), None, Some(2)];
    let kept: Vec[i64] = xs.into_iter().flat_map(|o| o.into_iter()).collect();
    println(kept);
    let none: Option[i64] = None;
    println(f\"{total(Some(5))} {total(none)}\");
    let mut v: Vec[i64] = [1];
    v.extend(Some(4));
    println(v);
}
",
        "1\n[1, 2]\n5 0\n[1, 4]\n",
    );
}

#[test]
fn flatten_over_options_stays_refused() {
    let es = errors(
        "fn main() {
    let xs: Vec[Option[i64]] = [Some(1), None];
    let f: Vec[i64] = xs.into_iter().flatten().collect();
    println(f);
}
",
    );
    assert_eq!(es.len(), 1, "{es:#?}");
    assert!(es[0].contains("write `.filter_map(|v| v)`"), "{es:#?}");
}
