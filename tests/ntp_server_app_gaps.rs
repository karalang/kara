//! Gaps the ntp_server app (kara-katas apps/service/ntp_server) found
//! porting a Rust-shaped draft: `with_provider::<Clock, _>(..)`, Rust's
//! turbofish after a function name, stopped the parse with no fix
//! (B-2026-11-17-119); `Atomic[i64].new(0)` and `Mutex[i64].new(0)`, the
//! qualified constructors, had "no method 'new'" (B-2026-11-17-120);
//! `use std::sync::atomic::{AtomicI64, Ordering};` was an unknown module
//! with no fix (B-2026-11-17-121); `Arc` in `use std::sync::{Arc, Mutex};`
//! was an unknown name with no fix (B-2026-11-17-122); and a `u64` handed to
//! `Duration.ns`, as Rust's `Duration::from_nanos` takes it, had no fix
//! (B-2026-11-17-123).

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
    let dir = std::env::temp_dir().join(format!("karac-ntp-gaps-{}", std::process::id()));
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
fn ntp_function_turbofish_is_fixed() {
    let path = scratch(
        "turbofish.kara",
        "struct Fixed {}

impl ClockProvider for Fixed {
    fn now(self) -> i64 { 7 }
    fn monotonic_ns(self) -> i64 { 7 }
    fn wall_ns(self) -> i64 { 7_000_000_000 }
}

fn first[T: Clone](xs: Slice[T]) -> T {
    xs[0].clone()
}

fn main() {
    let n = first::<i64>([4, 5]);
    with_provider::<Clock, _>(Fixed {}, || {
        println(f\"{n} {SystemTime.now().since_epoch().as_secs()}\");
    });
}
",
    );
    let check = karac(&["check"], &path);
    let said = text(&check);
    assert!(
        said.contains("Kāra names the resource in brackets"),
        "{said}"
    );
    assert!(
        said.contains("after a function name is Rust's turbofish"),
        "{said}"
    );
    let fixed = fix_to_fixpoint(&path);
    assert!(fixed.contains("with_provider[Clock](Fixed {}"), "{fixed}");
    assert!(fixed.contains("let n = first([4, 5]);"), "{fixed}");
    assert_eq!(run_stdout(&path), "4 7\n");
}

#[test]
fn ntp_qualified_atomic_and_mutex_constructors() {
    let path = scratch(
        "qualified.kara",
        "import std.sync.{Atomic, Mutex};

fn main() {
    let a = Atomic[u64].new(5);
    let b = Atomic[bool].new(true);
    let m = Mutex[i64].new(4);
    a.fetch_add(2);
    println(f\"{a.load()} {b.load()} {m.lock().value}\");
}
",
    );
    assert_eq!(run_stdout(&path), "7 true 4\n");
    // The pin is checked: an atomic of a type no atomic holds, and a value
    // of the wrong type, are refused as the unqualified forms are.
    let bad = scratch(
        "qualified_bad.kara",
        "import std.sync.{Atomic, Mutex};

fn main() {
    let a = Atomic[String].new(\"x\");
    let m = Mutex[i64].new(\"x\");
}
",
    );
    let said = text(&karac(&["check"], &bad));
    assert!(said.contains("is not a valid atomic type"), "{said}");
    assert!(
        said.contains("cannot construct `Mutex[String]` as `Mutex[i64]`"),
        "{said}"
    );
}

#[test]
fn ntp_rust_atomics_become_atomic() {
    let path = scratch(
        "atomics.kara",
        "use std::sync::atomic::{AtomicI64, AtomicBool, Ordering};

struct Counter {
    n: AtomicI64,
    on: AtomicBool,
}

fn main() {
    let c = Counter { n: AtomicI64::new(1), on: AtomicBool::new(false) };
    c.n.fetch_add(4, Ordering::SeqCst);
    c.on.store(true, Ordering::SeqCst);
    println!(\"{} {}\", c.n.load(Ordering::SeqCst), c.on.load(Ordering::Relaxed));
}
",
    );
    let said = text(&karac(&["check"], &path));
    assert!(said.contains("`AtomicI64` is `Atomic[i64]`"), "{said}");
    let fixed = fix_to_fixpoint(&path);
    assert!(fixed.contains("import std.sync.{Atomic};"), "{fixed}");
    assert!(fixed.contains("n: Atomic[i64],"), "{fixed}");
    assert!(fixed.contains("Atomic[bool].new(false)"), "{fixed}");
    assert!(!fixed.contains("Ordering"), "{fixed}");
    assert_eq!(run_stdout(&path), "5 true\n");
}

#[test]
fn ntp_rust_arc_is_written_away() {
    let path = scratch(
        "arc.kara",
        "use std::sync::{Arc, Mutex, TaskGroup};

struct Tally {
    n: i64,
}

fn bump(t: Arc<Mutex<Tally>>, by: i64) {
    t.lock().unwrap().n += by;
}

fn main() {
    let tally = Arc::new(Mutex::new(Tally { n: 1 }));
    let group = TaskGroup::new();
    let t = tally.clone();
    let u = Arc::clone(&tally);
    group.spawn(move || bump(t, 10));
    group.spawn(move || bump(u, 100));
    drop(group);
    println!(\"{}\", tally.lock().unwrap().n);
}
",
    );
    let said = text(&karac(&["check"], &path));
    assert!(said.contains("Kāra has no `Arc`"), "{said}");
    let fixed = fix_to_fixpoint(&path);
    assert!(
        fixed.contains("import std.sync.{Mutex, TaskGroup};"),
        "{fixed}"
    );
    assert!(
        fixed.contains("fn bump(t: Mutex[Tally], by: i64)"),
        "{fixed}"
    );
    assert!(
        fixed.contains("let tally = Mutex.new(Tally { n: 1 });"),
        "{fixed}"
    );
    assert!(fixed.contains("let t = ref tally;"), "{fixed}");
    assert!(fixed.contains("let u = ref tally;"), "{fixed}");
    assert_eq!(run_stdout(&path), "111\n");
}

#[test]
fn ntp_duration_from_nanos_of_a_u64() {
    let path = scratch(
        "duration.kara",
        "use std::time::Duration;

fn main() {
    let frac: u32 = 16384;
    let d = Duration::from_nanos(frac as u64 * 1_000_000_000 >> 16);
    let e = Duration::from_millis(frac as u64);
    println!(\"{} {}\", d, e);
}
",
    );
    let fixed = fix_to_fixpoint(&path);
    assert!(
        fixed.contains("Duration.ns((frac as u64 * 1_000_000_000 >> 16) as i64)"),
        "{fixed}"
    );
    assert!(
        fixed.contains("Duration.ms((frac as u64) as i64)"),
        "{fixed}"
    );
    assert_eq!(run_stdout(&path), "250ms 16.384s\n");
}
