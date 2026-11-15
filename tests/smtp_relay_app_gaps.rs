//! Gaps the smtp_relay app (kara-katas apps/service/smtp_relay) found porting
//! a Rust-shaped draft: Rust's `*guard` on a `MutexGuard` had no fix
//! (B-2026-11-15-12), `(&line, "")` for a `(Str, Str)` had none either
//! (B-2026-11-15-13), Rust's `const X: &[T] = &[..]` became a `Slice` constant
//! that nothing lowers (B-2026-11-15-14), and a view of a constant, or a
//! binding of a loop item destructured through its `Option`, stayed borrowed
//! as if it pointed into a temporary (B-2026-11-15-15).

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-smtp-gaps-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

/// `karac fix` until it has nothing left to apply, as the Mend loop runs it.
fn fix_to_fixpoint(path: &std::path::Path) {
    for _ in 0..8 {
        let out = karac(&["fix"], path);
        if text(&out).contains("no fixable") {
            return;
        }
    }
    panic!("karac fix did not settle on {}", path.display());
}

fn run_stdout(path: &std::path::Path) -> String {
    let check = karac(&["check"], path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], path);
    assert!(run.status.success(), "{}", text(&run));
    String::from_utf8_lossy(&run.stdout).into_owned()
}

#[test]
fn a_deref_of_a_mutex_guard_is_fixed_to_its_value_field() {
    let path = scratch(
        "guard.kara",
        "import std.sync.Mutex;
struct Lab {
    names: Mutex[Vec[String]],
    best: Mutex[Option[i64]],
}
fn main() {
    let lab = Lab { names: Mutex.new([]), best: Mutex.new(None) };
    lab.names.lock().value.push(\"b\".to_string());
    lab.names.lock().value.push(\"a\".to_string());
    *lab.best.lock() = Some(7);
    let mut all = take(mut *lab.names.lock());
    all.sort();
    println(f\"{all.len()} {all[0]} {lab.names.lock().value.len()}\");
    println(lab.best.lock().value.unwrap());
}
",
    );
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(
        text(&check).contains("unary '*' on a 'MutexGuard': a guard has no `Deref`"),
        "{}",
        text(&check)
    );
    fix_to_fixpoint(&path);
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(src.contains("lab.best.lock().value = Some(7);"), "{src}");
    assert!(src.contains("take(mut lab.names.lock().value)"), "{src}");
    assert_eq!(run_stdout(&path), "2 a 0\n7\n");
}

#[test]
fn a_ref_string_in_a_str_pair_is_fixed_to_as_str() {
    let path = scratch(
        "pair.kara",
        "fn main() {
    let line = \"MAIL FROM:x\".to_string();
    let (verb, arg) = line.split_once(\" \").unwrap_or((ref line, \"\"));
    println(f\"[{verb}] [{arg}]\");
    let one = \"QUIT\".to_string();
    let (v, a) = one.split_once(\" \").unwrap_or((ref one, \"\"));
    println(f\"[{v}] [{a}]\");
}
",
    );
    let check = karac(&["check"], &path);
    assert!(
        text(&check).contains("not as a tuple's element; `.as_str()` views the `String`"),
        "{}",
        text(&check)
    );
    fix_to_fixpoint(&path);
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(src.contains(".unwrap_or((line.as_str(), \"\"))"), "{src}");
    assert!(src.contains(".unwrap_or((one.as_str(), \"\"))"), "{src}");
    assert_eq!(run_stdout(&path), "[MAIL] [FROM:x]\n[QUIT] []\n");
}

#[test]
fn a_slice_constant_of_a_list_literal_is_fixed_to_an_array() {
    let path = scratch(
        "slice-const.kara",
        "const BOXES: Slice[Str] = [\"alice\", \"carol\"];
const MX: Slice[(Str, i64)] = [(\"a\", 10), (\"b\", 20)];
fn main() {
    for b in BOXES {
        println(b);
    }
    let mut total = 0;
    for m in MX.iter() {
        total += m.1;
    }
    println(total);
}
",
    );
    let check = karac(&["check"], &path);
    assert!(
        text(&check).contains("a constant's list literal is an `Array`, not a `Slice`"),
        "{}",
        text(&check)
    );
    fix_to_fixpoint(&path);
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(src.contains("const BOXES: Array[Str, 2] = "), "{src}");
    assert!(src.contains("const MX: Array[(Str, i64), 2] = "), "{src}");
    assert_eq!(run_stdout(&path), "alice\ncarol\n30\n");
}

#[test]
fn views_out_of_a_constant_borrow_nothing() {
    let path = scratch(
        "const-views.kara",
        "const MX: Array[(Str, u16, Str), 3] = [
    (\"b.test\", 20, \"mx2\"),
    (\"b.test\", 10, \"mx1\"),
    (\"a.test\", 10, \"mxa\"),
];
fn kind_of(name: Str) -> Str {
    if name == \"mx1\" { \"busy\" } else { \"plain\" }
}
fn hosts(domain: Str) -> Vec[(u16, Str)] {
    let mut hs: Vec[(u16, Str)] = Vec.new();
    for (d, pref, name) in MX.iter() {
        if d == domain {
            hs.push((pref, name));
        }
    }
    hs.sort();
    hs
}
fn main() {
    let mut kinds: Vec[Str] = Vec.new();
    for (i, (_, _, name)) in MX.iter().enumerate() {
        kinds.push(kind_of(name));
        println(f\"{i} {name}\");
    }
    for (p, n) in hosts(\"b.test\") {
        println(f\"{p} {n}\");
    }
    println(kinds.join(\",\"));
}
",
    );
    assert_eq!(
        run_stdout(&path),
        "0 mx2\n1 mx1\n2 mxa\n10 mx1\n20 mx2\nplain,busy,plain\n"
    );
}

#[test]
fn a_destructured_item_of_a_local_array_does_not_hold_the_loop() {
    let path = scratch(
        "array-items.kara",
        "fn main() {
    let v: Array[(Str, Str), 2] = [(\"a\", \"x\"), (\"b\", \"y\")];
    let mut kept: Vec[Str] = Vec.new();
    for (n, _) in v.iter() {
        kept.push(n);
    }
    println(kept.join(\",\"));
}
",
    );
    assert_eq!(run_stdout(&path), "a,b\n");
}

#[test]
fn a_view_of_a_dropped_string_kept_past_its_loop_is_still_refused() {
    let path = scratch(
        "dangling.kara",
        "fn main() {
    let mut kept: Vec[Str] = Vec.new();
    {
        let owned = [\"a\".to_string(), \"b\".to_string()];
        let pairs: Vec[(Str, i64)] = [(owned[0].as_str(), 1), (owned[1].as_str(), 2)];
        for (n, _) in pairs.iter() {
            kept.push(n);
        }
    }
    println(kept.len());
}
",
    );
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(text(&check).contains("E0516"), "{}", text(&check));
}
