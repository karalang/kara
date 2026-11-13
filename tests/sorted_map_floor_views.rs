//! `SortedMap.floor` and `ceiling` hand out the entry as views,
//! `Option[(ref K, ref V)]`, as `first` and `last` do
//! (docs/library/collections.md; B-2026-11-13-25). They were typed
//! `Option[(K, V)]`, so a program written to the doc was refused; on the MIR
//! pipeline a clone-view patch let `*k` read a clone (B-2026-10-12-28).
//! The same output on the interpreter and a compiled build.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn karac(mode: &str, src: &str) -> Output {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-floor-views-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

const VIEWS: &str = r#"fn show(o: Option[(ref String, ref Vec[i64])]) -> String {
    match o {
        Some((k, v)) => f"{k}:{v.len()}",
        None => "none".to_string(),
    }
}

fn main() {
    let mut m: SortedMap[String, Vec[i64]] = SortedMap.new();
    m.insert("b".to_string(), [1, 2].to_vec());
    m.insert("d".to_string(), [3].to_vec());
    println(show(m.floor("c".to_string())));
    println(show(m.ceiling("c".to_string())));
    println(show(m.floor("a".to_string())));
    println(show(m.ceiling("e".to_string())));
    let e: Option[(ref String, ref Vec[i64])] = m.floor("z".to_string());
    println(show(e));
    if let Some((k, v)) = m.ceiling("b".to_string()) {
        println(f"{k} {v[0] + v[1]}");
    }
    let (k, v) = m.floor("d".to_string()).unwrap();
    println(f"{k} {v.len()}");
    let mut n: SortedMap[i64, i64] = SortedMap.new();
    for i in 0..5 {
        n.insert(i * 10, i);
    }
    match n.floor(25) {
        Some((k, v)) => println(k + v),
        None => println(-1),
    }
    let c = n.ceiling(25).map(|(k, _)| *k).unwrap_or(0);
    println(c);
    println(m.len());
}
"#;

const WANT: &str = "b:2\nd:1\nnone\nnone\nd:1\nb 3\nd 1\n22\n30\n2\n";

fn runs(mode: &str) {
    let out = karac(mode, VIEWS);
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT, "{mode}");
}

#[test]
fn floor_and_ceiling_are_views_under_check() {
    let out = karac("check", VIEWS);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn interp_floor_and_ceiling_views() {
    runs("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_floor_and_ceiling_views() {
    runs("__mir-llvm-run");
}
