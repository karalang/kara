//! Found building the http_cache app.
//!
//! B-2026-11-09-91: every bad format spec of an f-string was reported at
//! its closing quote, so two `{x:?}` holes on a line read as one place twice.
//!
//! B-2026-11-09-92: a field named by a structural marker
//! (`struct Directives { private: bool }`) said "Expected identifier, found
//! `:`" at the colon, and a literal of it cascaded past its closing brace.
//!
//! B-2026-11-09-93: a program's own `struct Entry` (a cache entry) lost its
//! own methods to the map entry's dispatch: "no method 'label' on type
//! 'Entry'".
//!
//! B-2026-11-09-94: `opt ?? "" == value` (`??` binds loosest) gave three
//! mismatches, none naming the precedence, and no fix.
//!
//! B-2026-11-09-95: a `ref Str` (an element of `pairs.iter()` over a
//! `Slice[(Str, Str)]`) handed to a `String` parameter had no fix.

mod common;

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn scratch(src: &str) -> std::path::PathBuf {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-http-cache-gaps-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn check(src: &str) -> (bool, String) {
    let path = scratch(src);
    let r = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    r
}

/// The source after `karac fix`, and whether `karac check` then passes.
fn fixed(src: &str) -> (String, bool, String) {
    let path = scratch(src);
    let p = path.to_str().unwrap();
    for _ in 0..3 {
        karac(&["fix", p]);
    }
    let text = std::fs::read_to_string(&path).unwrap();
    let (ok, diags) = karac(&["check", p]);
    let _ = std::fs::remove_file(&path);
    (text, ok, diags)
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[cfg(feature = "llvm")]
fn compiled(src: &str) -> Option<String> {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!(
        "karac-http-cache-gaps-b-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 0).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| Command::new(&exe).current_dir(&dir).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    assert!(out.status.success(), "{:?}", out.status);
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn each_bad_format_spec_is_reported_at_its_own_hole() {
    let (ok, diags) = check(
        "fn main() {
    let a: Option[i64] = Some(3);
    let b: Option[i64] = None;
    println(f\"x {a:?} y {b:?} z {a:Q}\");
}
",
    );
    assert!(!ok);
    assert!(
        diags.contains(":4:20: format spec `?`: `?` is Debug formatting"),
        "{diags}"
    );
    assert!(
        diags.contains(":4:28: format spec `?`: `?` is Debug formatting"),
        "{diags}"
    );
    assert!(diags.contains(":4:36: "), "{diags}");
    assert!(!diags.contains(":4:38:"), "{diags}");
}

#[test]
fn a_structural_marker_as_a_field_name_is_named_once() {
    let src = "struct D {
    private: bool,
    public: bool,
}

fn main() {
    let mut d = D { private: false, public: true };
    d.private = true;
    let e = D { public: false, private: true };
    println(f\"{d.private} {d.public} {e.private}\");
}
";
    let (ok, diags) = check(src);
    assert!(!ok);
    let named = "`private` cannot name a field: it is a structural marker, which `r#` \
                 does not escape, so give the field another name";
    assert!(diags.contains(&format!(":2:5: {named}")), "{diags}");
    assert!(diags.contains(&format!(":7:21: {named}")), "{diags}");
    assert!(diags.contains(&format!(":9:32: {named}")), "{diags}");
    assert!(diags.contains("3 error(s) found"), "{diags}");
}

const USER_ENTRY: &str = "#[derive(Clone)]
struct Entry {
    key: String,
    hits: i64,
}

impl Entry {
    fn label(self) -> String {
        f\"{self.key}:{self.hits}\"
    }

    fn bump(mut ref self) {
        self.hits += 1;
    }
}

fn main() {
    let mut m: Map[String, Vec[Entry]] = Map.new();
    let list = m.entry(\"k\".to_string()).or_insert(Vec.new());
    list.push(Entry { key: \"a\".to_string(), hits: 2 });
    let mut counts: Map[String, i64] = Map.new();
    *counts.entry(\"x\".to_string()).or_insert(0) += 5;
    for e in m.get_mut(\"k\").unwrap().iter_mut() {
        e.bump();
    }
    let e = m.get(\"k\").unwrap()[0].clone();
    println(e.label());
    println(f\"{counts.get(\"x\").unwrap_or(0)}\");
}
";

#[test]
fn a_users_entry_keeps_its_own_methods() {
    let (ok, diags) = check(USER_ENTRY);
    assert!(ok, "{diags}");
    assert_eq!(run(USER_ENTRY), "a:3\n5\n");
}

#[cfg(feature = "llvm")]
#[test]
fn a_users_entry_keeps_its_own_methods_compiled() {
    if let Some(out) = compiled(USER_ENTRY) {
        assert_eq!(out, "a:3\n5\n");
    }
}

#[test]
fn a_comparison_after_nil_coalesce_is_parenthesized() {
    let src = "fn pick(m: Map[String, String], k: Str) -> bool {
    m.get(k).cloned() ?? \"\" == \"yes\"
}

fn main() {
    let mut m: Map[String, String] = Map.new();
    m.insert(\"a\".to_string(), \"yes\".to_string());
    let flag: Option[bool] = None;
    let ok = flag ?? 1 == 1;
    let n: Option[i64] = Some(4);
    println(f\"{pick(m, \"a\")} {pick(m, \"b\")} {ok} {n ?? 0 > 3}\");
}
";
    let (ok, diags) = check(src);
    assert!(!ok);
    assert!(
        diags.contains(":2:5: `??` binds more loosely than `==`: `a ?? b == c` is `a ?? (b == c)`, whose `bool` fallback cannot stand in for the 'String' it unwraps; write `(a ?? b) == c` to compare the value"),
        "{diags}"
    );
    assert!(diags.contains("2 error(s) found"), "{diags}");
    let (text, ok, diags) = fixed(src);
    assert!(ok, "{diags}\n{text}");
    assert!(
        text.contains("    (m.get(k).cloned() ?? \"\") == \"yes\"\n"),
        "{text}"
    );
    // A `bool` payload takes a comparison as its fallback, as written.
    assert!(text.contains("let ok = flag ?? 1 == 1;"), "{text}");
    assert!(text.contains("{(n ?? 0) > 3}"), "{text}");
    assert_eq!(run(&text), "true false true true\n");
}

#[test]
fn a_borrowed_str_where_a_string_is_wanted_gets_to_string() {
    let src = "fn take(s: own String) -> i64 {
    s.len()
}

fn total(pairs: Slice[(Str, Str)]) -> i64 {
    let mut n = 0;
    for (k, v) in pairs.iter() {
        n += take(k) + take(v);
    }
    n
}

fn main() {
    println(f\"{total([(\"ab\", \"c\")])}\");
}
";
    let (text, ok, diags) = fixed(src);
    assert!(ok, "{diags}\n{text}");
    assert!(
        text.contains("n += take(k.to_string()) + take(v.to_string());"),
        "{text}"
    );
    assert_eq!(run(&text), "3\n");
}
