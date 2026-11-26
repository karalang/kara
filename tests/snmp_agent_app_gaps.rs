//! Gaps the snmp_agent app (kara-katas apps/service/snmp_agent) found
//! porting a Rust-shaped draft: `thread::spawn(move || ..)` had no fix
//! (B-2026-11-19-80); Rust's alternate form `{:#04x}` had none either, in
//! `format!` or in a hole (B-2026-11-19-81); `SortedMap` had no `lower` or
//! `higher`, so `use std::ops::Bound;` and its neighbour lookups could not
//! be written (B-2026-11-19-82); a `Vec[String]` of literals passed as
//! `Slice[Str]` had no fix (B-2026-11-19-83); `Slice` had no `starts_with` or
//! `ends_with` (B-2026-11-19-84); and a closure that writes a capture and then
//! moves it out checked clean and was refused by `run` (B-2026-11-19-85).

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
    let dir = std::env::temp_dir().join(format!("karac-snmp-gaps-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

/// `karac fix` until it has nothing left to apply, as the Mend loop runs it.
fn fix_to_fixpoint(path: &std::path::Path) -> String {
    let mut text_now = std::fs::read_to_string(path).unwrap();
    for _ in 0..8 {
        let _ = karac(&["fix"], path);
        let now = std::fs::read_to_string(path).unwrap();
        if now == text_now {
            return now;
        }
        text_now = now;
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
fn snmp_thread_spawn_becomes_a_task_group() {
    let src = "use std::thread;

fn work(n: i64) -> i64 {
    n * 2
}

fn main() {
    let h = thread::spawn(move || work(21));
    println(f\"{h.join().unwrap()}\");
}
";
    let path = scratch("spawn.kara", src);
    let out = text(&karac(&["check"], &path));
    assert!(out.contains("`thread::spawn` is Rust"), "{out}");
    let fixed = fix_to_fixpoint(&path);
    assert!(
        fixed.contains("    let group = TaskGroup.new();\n    let h = group.spawn(|| work(21));\n"),
        "{fixed}"
    );
    assert_eq!(run_stdout(&path), "42\n");
}

#[test]
fn snmp_spawn_group_name_avoids_a_binding_in_use() {
    let src = "fn main() {
    let group = 1;
    let h = std::thread::spawn(|| group + 1);
    println(h.join().unwrap());
}
";
    let path = scratch("spawn-name.kara", src);
    let fixed = fix_to_fixpoint(&path);
    assert!(
        fixed
            .contains("    let tasks = TaskGroup.new();\n    let h = tasks.spawn(|| group + 1);\n"),
        "{fixed}"
    );
    assert_eq!(run_stdout(&path), "2\n");
}

#[test]
fn snmp_alternate_form_becomes_a_prefix() {
    let src = "fn main() {
    let tag: u8 = 48;
    let s = format!(\"tag {:#04x} bits {:#010b} oct {:#o}\", tag, tag, tag);
    println(s);
    println(f\"{tag:#04x}\");
}
";
    let path = scratch("alt.kara", src);
    let fixed = fix_to_fixpoint(&path);
    assert!(
        fixed.contains("f\"tag 0x{tag:02x} bits 0b{tag:08b} oct 0o{tag:o}\""),
        "{fixed}"
    );
    assert!(fixed.contains("println(f\"0x{tag:02x}\");"), "{fixed}");
    // What Rust's `{:#04x}`, `{:#010b}` and `{:#o}` print for 48.
    assert_eq!(
        run_stdout(&path),
        "tag 0x30 bits 0b00110000 oct 0o60\n0x30\n"
    );
}

#[test]
fn snmp_bound_lookups_become_sorted_map_neighbours() {
    let src = "use std::ops::Bound;

fn main() {
    let mut m: SortedMap[i64, Str] = SortedMap.new();
    m.insert(10, \"ten\");
    m.insert(20, \"twenty\");
    m.insert(30, \"thirty\");
    let k = 20;
    let a = m.range((Bound::Excluded(k), Bound::Unbounded)).next();
    let b = m.range((Bound::Included(k), Bound::Unbounded)).next();
    let c = m.range((Bound::Unbounded, Bound::Excluded(k))).next_back();
    let d = m.range((Bound::Unbounded, Bound::Included(k))).next_back();
    let e = m.range((Bound::Excluded(30), Bound::Unbounded)).next();
    println(f\"{a} {b} {c} {d} {e}\");
}
";
    let path = scratch("bound.kara", src);
    let out = text(&karac(&["check"], &path));
    assert!(out.contains("`std::ops::Bound` is Rust's"), "{out}");
    let fixed = fix_to_fixpoint(&path);
    assert!(!fixed.contains("Bound"), "{fixed}");
    for want in [
        "let a = m.higher(k);",
        "let b = m.ceiling(k);",
        "let c = m.lower(k);",
        "let d = m.floor(k);",
        "let e = m.higher(30);",
    ] {
        assert!(fixed.contains(want), "{want}:\n{fixed}");
    }
    assert_eq!(
        run_stdout(&path),
        "Some((30, thirty)) Some((20, twenty)) Some((10, ten)) Some((20, twenty)) None\n"
    );
}

#[test]
fn snmp_bound_used_otherwise_keeps_its_line() {
    // A `Bound` that is not in a neighbour lookup: deleting the import
    // would leave it unknown, so the envelope is not offered.
    let src = "use std::ops::Bound;

fn main() {
    let b = Bound::Unbounded;
}
";
    let path = scratch("bound-other.kara", src);
    let out = text(&karac(&["check"], &path));
    assert!(!out.contains("names each neighbour lookup"), "{out}");
}

#[test]
fn snmp_sorted_map_lower_and_higher_walk_string_keys() {
    let src = "fn main() {
    let mut m: SortedMap[String, i64] = SortedMap.new();
    for (i, w) in [\"pear\", \"apple\", \"fig\", \"kiwi\"].iter().enumerate() {
        m.insert(w.to_string(), i);
    }
    let mut k = \"a\".to_string();
    loop {
        match m.higher(k.clone()) {
            Some((n, v)) => {
                println(f\"{n} {v}\");
                k = n.clone();
            }
            None => break,
        }
    }
    println(f\"{m.lower(\"fig\".to_string())} {m.lower(\"apple\".to_string())}\");
}
";
    let path = scratch("walk.kara", src);
    assert_eq!(
        run_stdout(&path),
        "apple 1\nfig 2\nkiwi 3\npear 0\nSome((apple, 1)) None\n"
    );
}

#[test]
fn snmp_string_literal_list_passed_as_str_slice_is_annotated() {
    let src = "fn count(xs: Slice[Str]) -> i64 {
    xs.len()
}

fn main() {
    let many = Vec.filled(3, \"x\");
    let few = [\"a\", \"b\"];
    println(f\"{count(many)} {count(few)}\");
}
";
    let path = scratch("strs.kara", src);
    let out = text(&karac(&["check"], &path));
    assert!(out.contains("declare it `many: Vec[Str]`"), "{out}");
    let fixed = fix_to_fixpoint(&path);
    assert!(
        fixed.contains("let many: Vec[Str] = Vec.filled(3, \"x\");"),
        "{fixed}"
    );
    assert!(
        fixed.contains("let few: Vec[Str] = [\"a\", \"b\"];"),
        "{fixed}"
    );
    assert_eq!(run_stdout(&path), "3 2\n");
}

#[test]
fn snmp_slice_starts_with_and_ends_with() {
    let src = "fn main() {
    let v: Vec[u32] = [1, 3, 6, 1, 2];
    let base: Vec[u32] = [1, 3, 6];
    let tail: Vec[u32] = [1, 2];
    let empty: Vec[u32] = [];
    println(f\"{v.starts_with(base)} {v.starts_with(tail)} {v.ends_with(tail)} {v.ends_with(base)} {v.starts_with(empty)} {base.starts_with(v)}\");
    let s = v[1..3];
    println(f\"{s.starts_with([3])} {s.ends_with([6])}\");
}
";
    let path = scratch("starts.kara", src);
    assert_eq!(
        run_stdout(&path),
        "true false true false true false\ntrue true\n"
    );
}

#[test]
fn snmp_closure_writes_then_returns_its_capture() {
    let src = "import std.sync.{TaskGroup};

struct Agent { n: i64, names: Vec[String] }
impl Agent {
    fn serve(mut ref self) { self.n = self.n + 1; }
}
fn main() {
    let mut agent = Agent { n: 0, names: [] };
    let group = TaskGroup.new();
    let handle = group.spawn(|| {
        agent.serve();
        agent
    });
    let a = handle.join();
    let mut b = Agent { n: 5, names: [\"x\".to_string()] };
    let f = || {
        b.serve();
        b
    };
    let b2 = f();
    println(f\"{a.n} {b2.n} {b2.names.len()}\");
}
";
    let path = scratch("capture.kara", src);
    assert_eq!(run_stdout(&path), "1 6 1\n");
}
