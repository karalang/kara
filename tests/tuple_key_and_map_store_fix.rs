//! Kata 1396 (Design Underground System), three refusals of natural code
//! that `karac fix` could not carry to a clean check:
//!
//! - `let key = (start, station_name);` and then `trips.get(key)`: the
//!   tuple's second element borrows a bare parameter, and the refusal at
//!   the use was a bare "expected '(String, String)', found '(String, ref
//!   String)'" with no remedy, though the same tuple written in place
//!   gets its `own` (B-2026-11-21-67).
//! - `self.names[name] = at` with `name` a bare parameter: the key is
//!   stored, so it must be owned, but only MIR's §3.7 backstop refused it,
//!   with no fix (B-2026-11-21-68).
//! - `self.total[key] = self.total.get(key).unwrap_or(0) + (t - at)`: the
//!   key moves before the value runs, and the hoisting fix spelled `t -
//!   at` as lowering left it, `i32.sub(t, at)`, which `karac fix` refused
//!   to write (B-2026-11-21-69).

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-tuple-key-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

fn fix_check_run(tag: &str, src: &str) -> (String, bool, String) {
    let (dir, path) = fixture(tag, src);
    let fix = karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fix.status.success(),
        "{}\n{fixed}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let check = karac().arg("check").arg(&path).output().unwrap();
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        fixed,
        check.status.success(),
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

fn check_text(tag: &str, src: &str) -> String {
    let (dir, path) = fixture(tag, src);
    let check = karac().arg("check").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&check.stderr).into_owned() + &String::from_utf8_lossy(&check.stdout)
}

const TUPLE_KEY: &str = "struct Trips {
    open: Map[i32, (String, i32)],
    trips: Map[(String, String), (i64, i64)],
}

impl Trips {
    fn check_in(mut ref self, id: i32, station_name: own String, t: i32) {
        self.open[id] = (station_name, t);
    }

    fn check_out(mut ref self, id: i32, station_name: String, t: i32) {
        let (start, at) = self.open.remove(id).unwrap();
        let key = (start, station_name);
        let (total, count) = self.trips.get(key).unwrap_or((0, 0));
        self.trips[key] = (total + t - at, count + 1);
    }
}

fn main() {
    let mut u = Trips { open: Map.new(), trips: Map.new() };
    u.check_in(1, \"a\", 3);
    u.check_out(1, \"b\", 10);
    u.check_in(2, \"a\", 11);
    u.check_out(2, \"b\", 14);
    let (total, count) = u.trips[(\"a\", \"b\")];
    println(f\"{total} {count}\");
}
";

#[test]
fn a_tuple_bound_from_a_bare_parameter_is_refused_with_its_own() {
    let diag = check_text("tuple-diag", TUPLE_KEY);
    assert!(
        diag.contains("`key` is a tuple whose `station_name`"),
        "{diag}"
    );
    let (fixed, clean, out) = fix_check_run("tuple", TUPLE_KEY);
    assert!(
        fixed.contains("let key = (start, station_name.clone());"),
        "{fixed}"
    );
    assert!(clean, "{fixed}");
    assert_eq!(out, "10 2\n");
}

const MAP_STORE: &str = "fn count(names: ref Vec[String]) -> i64 {
    let mut m: Map[String, i64] = Map.new();
    for k in names.iter() {
        m[k] = 1;
    }
    m.len()
}

fn put(m: mut ref Map[String, i64], name: String) {
    m[name] = 2;
}

fn main() {
    let v: Vec[String] = [\"a\", \"b\", \"a\"];
    let mut m: Map[String, i64] = Map.new();
    put(mut m, \"x\");
    println(f\"{count(v)} {m.len()} {m[\"x\"]}\");
}
";

#[test]
fn a_borrowed_key_stored_into_a_map_takes_its_own_or_a_clone() {
    let (fixed, clean, out) = fix_check_run("store", MAP_STORE);
    assert!(fixed.contains("m[k.clone()] = 1;"), "{fixed}");
    assert!(
        fixed.contains("fn put(m: mut ref Map[String, i64], name: own String)"),
        "{fixed}"
    );
    assert!(clean, "{fixed}");
    assert_eq!(out, "2 1 2\n");
}

/// A borrowed key a store only reads through (`m[k] += 1`) is not stored:
/// still accepted as before.
#[test]
fn a_borrowed_key_of_a_compound_store_is_still_accepted() {
    let src = "fn main() {
    let names: Vec[String] = [\"a\", \"b\", \"a\"];
    let mut m: Map[String, i64] = Map.new();
    m[\"a\"] = 0;
    m[\"b\"] = 0;
    for k in names.iter() {
        m[k] += 1;
    }
    println(f\"{m[\"a\"]} {m[\"b\"]}\");
}
";
    let (_, clean, out) = fix_check_run("compound", src);
    assert!(clean);
    assert_eq!(out, "2 1\n");
}

#[test]
fn a_hoisted_value_keeps_its_operators() {
    let src = "struct Totals {
    total: Map[String, i64],
}

impl Totals {
    fn add(mut ref self, start: String, end: String, at: i32, t: i32) {
        let key = f\"{start}->{end}\";
        self.total[key] = self.total.get(key).unwrap_or(0) + (t - at);
    }
}

fn main() {
    let mut s = Totals { total: Map.new() };
    s.add(\"a\", \"b\", 3, 10);
    s.add(\"a\", \"b\", 11, 14);
    println(f\"{s.total[\"a->b\"]}\");
}
";
    let diag = check_text("hoist-diag", src);
    assert!(!diag.contains("i32.sub"), "{diag}");
    let (fixed, clean, out) = fix_check_run("hoist", src);
    assert!(
        fixed.contains("let value = self.total.get(key).unwrap_or(0) + (t - at);"),
        "{fixed}"
    );
    assert!(clean, "{fixed}");
    assert_eq!(out, "10\n");
}
