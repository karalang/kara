//! B-2026-11-18-47: over an element of a local the function owns
//! (`let mut s: Vec[Option[V]]`, `match s[0] { Some(v) => v.n += 1, .. }`),
//! a plain binding the arm writes through was E0517 with no fix. Ruled: an
//! index place is a `ref` scrutinee whatever its root, since nothing moves
//! out of an index (core-semantics.md §4.6, amended), so the binding is a
//! `ref` and the write is still E0517, now with the machine-applicable fix
//! `mut ref v`. The fixed program checks and runs the same on both lanes. A
//! binding the arm only reads stays a view and needs nothing; a field of an
//! owned local (`match s.f`) is a legal partial move.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

const SRC: &str = "struct V { n: i64, tags: Vec[String] }\nstruct W { v: V }\nstruct Box2 { items: Vec[Option[V]] }\nfn main() {\n    let mut s: Vec[Option[V]] = [Some(V { n: 1, tags: [] }), None];\n    match s[0] { Some(v) => v.n += 1, None => {} }\n    if let Some(v) = s[0] { v.tags.push(\"a\".to_string()); }\n    let mut w: Vec[W] = [W { v: V { n: 5, tags: [] } }];\n    match w[0] { W { v } => v.n *= 2 }\n    let mut b = Box2 { items: [Some(V { n: 7, tags: [] })] };\n    if let Some(v) = b.items[0] { v.n -= 1; }\n    let mut i = 0;\n    while let Some(v) = s[i] { v.n += 100; i += 1; }\n    match s[0] { Some(ref v) => println(f\"{v.n} {v.tags.len()}\"), None => {} }\n    println(w[0].v.n);\n    if let Some(ref v) = b.items[0] { println(v.n); }\n}\n";
const OUT: &str = "102 1\n10\n6\n";

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

/// Mend: the codes `check --output=json` reports, then `karac fix`, then
/// the fixed program's check and its output on `lane`.
fn mend(lane: &str) -> (Vec<String>, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-index-write-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, SRC).unwrap();
    let p = f.to_str().unwrap();
    let json = String::from_utf8_lossy(&karac(&["check", "--output=json", p]).stdout).into_owned();
    let codes = json
        .split("\"code\":\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap().to_string())
        .collect();
    karac(&["fix", p]);
    let fixed = std::fs::read_to_string(&f).unwrap();
    let check = karac(&["check", p]);
    assert!(
        check.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac(&[lane, p]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    (
        codes,
        fixed,
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

#[test]
fn a_written_binding_over_an_owned_element_is_fixed_to_mut_ref() {
    let (codes, fixed, out) = mend("__mir-run");
    assert_eq!(codes, ["E0517"; 5], "{codes:?}");
    for line in [
        "match s[0] { Some(mut ref v) => v.n += 1",
        "if let Some(mut ref v) = s[0]",
        "match w[0] { W { v: mut ref v } => v.n *= 2 }",
        "if let Some(mut ref v) = b.items[0]",
        "while let Some(mut ref v) = s[i]",
    ] {
        assert!(fixed.contains(line), "{line}\n{fixed}");
    }
    assert_eq!(out, OUT);
}

#[test]
fn a_read_only_binding_over_an_owned_element_is_a_view() {
    let dir = std::env::temp_dir().join(format!(
        "karac-index-read-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(
        &f,
        "struct V { n: i64, s: String }\nfn main() {\n    let s: Vec[Option[V]] = [Some(V { n: 1, s: \"a\".to_string() })];\n    match s[0] { Some(v) => println(f\"{v.n} {v.s}\"), None => {} }\n    if let Some(v) = s[0] { println(v.s.len()); }\n    println(s.len());\n}\n",
    )
    .unwrap();
    let p = f.to_str().unwrap();
    let check = karac(&["check", p]);
    let out = karac(&["__mir-run", p]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1 a\n1\n1\n");
}

#[test]
fn a_field_of_an_owned_local_still_moves() {
    let dir = std::env::temp_dir().join(format!("karac-index-write-field-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(
        &f,
        "struct V { n: i64 }\nstruct S { f: Option[V] }\nfn main() {\n    let s = S { f: Some(V { n: 1 }) };\n    match s.f { Some(v) => { let mut v = v; v.n += 1; println(v.n); } None => {} }\n}\n",
    )
    .unwrap();
    let out = karac(&["__mir-run", f.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "2\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_written_binding_over_an_owned_element_runs_compiled_after_the_fix() {
    assert_eq!(mend("__mir-llvm-run").2, OUT);
}
