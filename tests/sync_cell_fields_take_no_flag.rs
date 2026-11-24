//! An `Atomic` or `Mutex` field that is not `mut` takes no run-time borrow
//! flag. Its methods take a borrowed `self` (design.md § `sync` types), so
//! every access to it through a handle is a read, and a flag on it could
//! never trip: it only cost an acquire and a release per access. A `mut`
//! one can be replaced through a handle, so it keeps its flag.

use std::process::Command;

fn karac(src: &str, args: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-sync-cell-flags-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(args)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    text
}

const SRC: &str = "sync struct Counter {
    name: String,
    count: Atomic[i64],
    state: Mutex[String],
}
shared struct Log {
    lines: Mutex[Vec[i64]],
    mut items: Vec[i64],
    mut hits: Atomic[i64],
}
fn main() {
    let c = Counter { name: \"a\", count: Atomic.new(0), state: Mutex.new(\"x\") };
    let d = c;
    c.count.fetch_add(1);
    { let g = d.state.lock(); println(g.value); c.count.fetch_add(2); }
    println(d.count.load());
    let b = Log { lines: Mutex.new(Vec.new()), items: Vec.new(), hits: Atomic.new(0) };
    let e = b;
    { let mut g = b.lines.lock(); g.value.push(1); e.hits.fetch_add(1); }
    { let g = e.lines.lock(); println(g.value.len()); }
    println(b.hits.load());
}
";

#[test]
fn a_sync_structs_atomic_and_mutex_fields_take_no_flag() {
    let out = karac(SRC, &["explain", "a.kara.Counter"]);
    assert!(out.contains("a `sync struct`"), "{out}");
    assert!(out.contains("Borrow flags: 0 "), "{out}");
    assert!(
        out.contains("  count: no flag (not `mut`; its methods take a borrowed `self`)"),
        "{out}"
    );
    assert!(
        out.contains("  state: no flag (not `mut`; its methods take a borrowed `self`)"),
        "{out}"
    );
}

#[test]
fn a_shared_structs_mutex_field_takes_none_and_a_mut_atomic_keeps_its_flag() {
    let out = karac(SRC, &["explain", "a.kara.Log"]);
    assert!(out.contains("Borrow flags: 2 "), "{out}");
    assert!(
        out.contains("  lines: no flag (not `mut`; its methods take a borrowed `self`)"),
        "{out}"
    );
    assert!(out.contains("  items: flagged"), "{out}");
    assert!(out.contains("  hits: flagged"), "{out}");
}

#[test]
fn cells_used_through_two_handles_run_without_a_flag() {
    let out = karac(SRC, &["__mir-run", "a.kara"]);
    assert_eq!(out, "x\n3\n1\n1\n");
}
