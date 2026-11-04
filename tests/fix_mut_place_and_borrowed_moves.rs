//! Three `karac fix` edits found porting the mini_tsdb app.
//!
//! B-2026-11-04-1: `let s = mut place;` writes a call argument's marker
//! where a borrow is wanted, and was reported as "'mut' is a reserved
//! keyword and cannot be used as an identifier" with no fix. It now says a
//! borrow for writing is `mut ref place`, and the fix inserts the `ref`.
//!
//! B-2026-11-04-2: `items.into_iter()` on a bare `Vec[Item]` parameter,
//! with `Item` not clonable, got the fix `items.clone().into_iter()`, which
//! does not check either and asks for `#[derive(Clone)]` on `Item`. A `Vec`
//! clones only when its elements do, so the fix now takes the parameter
//! `own`, as it already did for a type with no `.clone()`.
//!
//! B-2026-11-04-3: `let s = v[i].field;` with `s` only read got the fix
//! that clones the field, where `let s = v[i];` already got the borrow. A
//! field of an element now borrows the same way.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn check_err(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-msg-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let check = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&check.stderr).into_owned()
}

fn fix_until_clean(name: &str, src: &str) -> (String, String) {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    let file = path.to_str().unwrap().to_string();
    std::fs::write(&path, src).unwrap();
    for _ in 0..3 {
        let _ = karac(&["fix", &file]);
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac(&["check", &file]);
    assert!(
        check.status.success(),
        "{name}: {}\n{fixed}",
        String::from_utf8_lossy(&check.stderr)
    );
    let _ = std::fs::remove_file(&path);
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    (fixed, out)
}

#[test]
fn a_let_of_mut_place_gets_mut_ref() {
    let src = "fn main() {
    let mut v = [1, 2].to_vec();
    let first = mut v[0];
    first += 10;
    println(f\"{v[0]} {v[1]}\");
}
";
    let err = check_err("mut-place", src);
    assert!(
        err.contains("`mut place` is the marker a call argument takes; a borrow for writing is `mut ref place`"),
        "{err}"
    );
    assert!(!err.contains("reserved keyword"), "{err}");
    let (fixed, out) = fix_until_clean("mut-place", src);
    assert!(fixed.contains("let first = mut ref v[0];"), "{fixed}");
    assert_eq!(out, "11 2\n");
}

#[test]
fn into_iter_on_a_bare_vec_of_unclonable_items_takes_it_own() {
    let src = "struct Item {
    name: String,
}

fn total(items: Vec[Item]) -> i64 {
    let mut n = 0;
    for it in items.into_iter() {
        n += it.name.len();
    }
    n
}

fn main() {
    println(total(vec![Item { name: \"ab\".to_string() }, Item { name: \"c\".to_string() }]));
}
";
    let (fixed, out) = fix_until_clean("into-iter-own", src);
    assert!(fixed.contains("fn total(items: own Vec[Item])"), "{fixed}");
    assert!(!fixed.contains(".clone()"), "{fixed}");
    assert!(!fixed.contains("derive"), "{fixed}");
    assert_eq!(out, "3\n");
}

#[test]
fn a_read_only_field_of_an_element_is_borrowed() {
    let src = "struct Series {
    samples: Vec[i64],
}

fn first_len(s: Vec[Series]) -> i64 {
    let samples = s[0].samples;
    samples.len()
}

fn main() {
    println(first_len(vec![Series { samples: vec![3, 4] }]));
}
";
    let err = check_err("field-of-element", src);
    assert!(
        err.contains("the binding is only read, so the fix borrows the field"),
        "{err}"
    );
    let (fixed, out) = fix_until_clean("field-of-element", src);
    assert!(fixed.contains("let samples = ref s[0].samples;"), "{fixed}");
    assert_eq!(out, "2\n");
}

#[test]
fn a_field_of_self_s_element_is_borrowed_unless_that_field_is_written() {
    let src = "struct Db {
    series: Map[String, Vec[i64]],
    out: Vec[String],
}

impl Db {
    fn report(mut ref self, key: Str) {
        let s = self.series[key];
        self.out.push(f\"{s.len()}\");
    }

    fn grow(mut ref self, key: Str) {
        let s = self.series[key];
        self.series.insert(\"z\".to_string(), Vec.new());
        self.out.push(f\"{s.len()}\");
    }
}

fn main() {
    let mut db = Db { series: Map.new(), out: Vec.new() };
    db.series.insert(\"a\".to_string(), vec![1, 2]);
    db.report(\"a\");
    db.grow(\"a\");
    println(db.out.join(\",\"));
}
";
    let (fixed, out) = fix_until_clean("self-field-element", src);
    assert!(
        fixed.contains("let s = ref self.series[key];\n        self.out"),
        "{fixed}"
    );
    assert!(
        fixed.contains("let s = self.series[key].clone();"),
        "{fixed}"
    );
    assert_eq!(out, "2,2\n");
}
