//! Two `par` conflicts the MIR effect pass missed (design.md §12; the
//! §12 sentence-map audit, conformance/design/effects/SENTENCE_MAP.md).
//!
//! B-2026-11-24-44: a native method of a built-in type that runs a
//! program impl on the values it holds (`xs.sort()` the elements' `cmp`,
//! `xs.clone()` their `clone`, `xs.dedup()` their `eq`, a `Map` / `Set` key
//! operation the key's `hash`) was charged none of that impl's effects, so
//! two branches whose `cmp` or `hash` printed were accepted, and the
//! automatic parallelizer could run them at once.
//!
//! B-2026-11-24-45: a function declaring a keyed resource
//! (`writes(UserDb[i])`, the deferred parameterized form) was charged
//! nothing for it: the clause was dropped whole. It is the whole resource
//! now, so two branches writing it conflict.

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn karac(dir: &PathBuf, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn cell(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-par-native-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

fn conflict(tag: &str, src: &str, want: &str) {
    let dir = cell(tag, src);
    let (ok, out) = karac(&dir, &["check"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "{tag}: accepted");
    assert!(out.contains("E0418"), "{tag}: {out}");
    assert!(out.contains(want), "{tag}: {out}");
}

fn runs(tag: &str, src: &str, expected: &str) {
    let dir = cell(tag, src);
    let (ok, out) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: check: {out}");
    let (ok, out) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{tag}: run: {out}");
    assert_eq!(out, expected, "{tag}");
}

const MAIN: &str = "fn main() { let (a, b) = par { f(1), f(2) }; println(a + b); }\n";

const P: &str = "struct P { v: i64 }
impl PartialEq for P { fn eq(self, o: P) -> bool { LOUD self.v == o.v } }
impl Eq for P { }
impl PartialOrd for P { fn partial_cmp(self, o: P) -> Option[Ordering] { Some(self.cmp(o)) } }
impl Ord for P { fn cmp(self, o: P) -> Ordering { LOUD self.v.cmp(o.v) } }
impl Clone for P { fn clone(self) -> P { LOUD P { v: self.v } } }
impl Hash for P { fn hash[H: Hasher](self, h: mut ref H) { LOUD h.write_i64(self.v); } }
";

const BODIES: [(&str, &str); 5] = [
    ("sort", "fn f(k: i64) -> i64 { let mut v: Vec[P] = [P { v: k }, P { v: 0 }]; v.sort(); v[0].v }\n"),
    ("clone", "fn f(k: i64) -> i64 { let v: Vec[P] = [P { v: k }]; let w = v.clone(); w.len() as i64 }\n"),
    ("dedup", "fn f(k: i64) -> i64 { let mut v: Vec[P] = [P { v: k }, P { v: k }]; v.dedup(); v.len() as i64 }\n"),
    ("map", "fn f(k: i64) -> i64 { let mut m: Map[P, i64] = Map.new(); m.insert(P { v: k }, k); m.len() as i64 }\n"),
    ("set", "fn f(k: i64) -> i64 { let s: Set[P] = Set.new(); if s.contains(P { v: k }) { 1 } else { 0 } }\n"),
];

#[test]
fn par_branch_is_charged_the_impls_a_native_runs_on_what_it_holds() {
    for (tag, body) in BODIES {
        let loud = P.replace("LOUD", "println(\"x\");");
        conflict(tag, &format!("{loud}{body}{MAIN}"), "writes(Stdout)");
    }
}

#[test]
fn par_branch_over_quiet_impls_a_native_runs_is_apart() {
    let want = ["0\n", "2\n", "2\n", "2\n", "0\n"];
    for ((tag, body), want) in BODIES.iter().zip(want) {
        let quiet = P.replace("LOUD ", "");
        runs(
            &format!("{tag}-quiet"),
            &format!("{quiet}{body}{MAIN}"),
            want,
        );
    }
}

#[test]
fn keyed_resource_is_its_whole_resource() {
    let src = "effect resource UserDb[id: u64];
fn save(i: u64) -> u64 with writes(UserDb[i]) { i }
fn main() { let (a, b) = par { save(1), save(2) }; println(a + b); }
";
    conflict("keyed", src, "writes(UserDb)");
    let apart = "effect resource UserDb[id: u64];
effect resource Other;
fn save(i: u64) -> u64 with writes(UserDb[i]) { i }
fn other(i: u64) -> u64 with writes(Other) { i }
fn main() { let (a, b) = par { save(1), other(2) }; println(a + b); }
";
    runs("keyed-apart", apart, "3\n");
}
