//! Atomic counts for `sync` types in built binaries (core-semantics §6.3,
//! M4 item 10a): a `sync struct` / `sync enum` handle's strong and weak
//! counts change by atomic read-modify-write, and a weak upgrade checks
//! liveness and raises the count in one compare-and-swap, while a plain
//! `shared` type keeps its plain loads and stores. The strong handles of
//! every `shared` box hold one weak count together (item 10a-2), so a weak
//! release never reads the strong count, and `Atomic[T]` is sequentially
//! consistent atomics (item 10a-3). Each program prints the same on the
//! interpreter, built (on one worker and with the executor's state the
//! pool's, item 10b-2), and under ASan.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long a program may run before it counts as hung.
const DEADLINE: Duration = Duration::from_secs(20);

/// Runs `cmd` to its end, or kills it at [`DEADLINE`] and fails.
fn run(name: &str, mut cmd: Command) -> Output {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > DEADLINE {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!("{name} hung\n{}", String::from_utf8_lossy(&out.stdout));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn scratch(name: &str, tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-mir-sync-counts-{tag}-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

fn karac(file: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_MIR_COROUTINES", "1").args(args).arg(file);
    c
}

/// The program's stdout on the interpreter.
fn stdout_on_interp(name: &str, src: &str) -> String {
    let (dir, file) = scratch(name, "i", src);
    let out = run(name, karac(&file, &["__mir-run"]));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{name} on the interpreter: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The program's stdout on the interpreter and, when the runtime archive
/// links, on the built binary, which must print the same.
fn stdout_on_both(name: &str, src: &str) -> String {
    let interp = stdout_on_interp(name, src);

    // The same path as the interpreter's run, so a panic's location reads
    // the same.
    let (dir, file) = scratch(name, "i", src);
    let exe = dir.join("main");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_MIR_COROUTINES", "1")
        .args(["build", "--mir", file.to_str().unwrap(), "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "{name} does not build: {err}"
    );
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    });
    if linked.is_some() {
        // On one worker, and with the executor's state the pool's
        // (`KARAC_THREADS` above 1), which must print the same.
        for threads in ["1", "2"] {
            let mut cmd = Command::new(&exe);
            cmd.env("KARAC_THREADS", threads);
            let out = run(name, cmd);
            assert!(
                out.status.success(),
                "{name} built, KARAC_THREADS={threads}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8(out.stdout).unwrap(),
                interp,
                "{name}: the build (KARAC_THREADS={threads}) and the interpreter differ"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    interp
}

/// The program's stdout under AddressSanitizer, which must be clean and
/// print `want`; `None` when no ASan-capable `cc` or archive is here.
fn asan_stdout(name: &str, src: &str) -> Option<String> {
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
        return None;
    }
    let (dir, file) = scratch(name, "a", src);
    let out = run(name, karac(&file, &["__mir-llvm-asan"]));
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if out.status.code() == Some(3) && err.contains("Linker failed") {
        common::link_or_skip(Err(err))?;
        return None;
    }
    assert!(out.status.success(), "{name} under ASan: {err}");
    Some(String::from_utf8(out.stdout).unwrap())
}

/// The LLVM IR `karac build --mir` emits for `src`.
fn ir_of(name: &str, tag: &str, src: &str) -> String {
    let (dir, file) = scratch(name, tag, src);
    let ll = dir.join("main.ll");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_MIR_COROUTINES", "1")
        .env("KARAC_MIR_LL", &ll)
        .args(["build", "--mir", file.to_str().unwrap(), "-o"])
        .arg(dir.join("main"))
        .output()
        .unwrap();
    let ir = std::fs::read_to_string(&ll).unwrap_or_else(|_| {
        panic!(
            "{name}: no IR written: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let _ = std::fs::remove_dir_all(&dir);
    ir
}

/// How many atomic read-modify-writes and compare-and-swaps `ir` holds.
fn atomics(ir: &str) -> usize {
    ir.lines()
        .filter(|l| l.contains(" atomicrmw ") || l.contains(" cmpxchg "))
        .count()
}

const HANDLES: &str = r#"sync struct Node {
    id: i64,
    name: String,
}

impl Drop for Node {
    fn drop(mut ref self) {
        println(f"drop {self.id}");
    }
}

sync struct Kid {
    id: i64,
    parent: weak Node,
}

sync enum Shape {
    Dot(i64),
    Named(String, i64),
}

fn area(s: Shape) -> i64 {
    match s {
        Shape.Dot(n) => n,
        Shape.Named(_, n) => n * 2,
    }
}

fn look(k: Kid) {
    match k.parent {
        Option.Some(p) => println(f"kid {k.id} parent {p.name}"),
        Option.None => println(f"kid {k.id} parent gone"),
    }
}

fn main() {
    let kid = {
        let root = Node { id: 1, name: "root" };
        let kid = Kid { id: 2, parent: root };
        let again = root.clone();
        look(kid);
        println(f"again {again.id}");
        kid
    };
    look(kid);
    let k2 = kid.clone();
    look(k2);
    let s = Shape.Named("sq", 21);
    let t = s.clone();
    println(f"area {area(s)} {area(t)} {area(Shape.Dot(5))}");
}
"#;

const HANDLES_OUT: &str =
    "kid 2 parent root\nagain 1\ndrop 1\nkid 2 parent gone\nkid 2 parent gone\narea 42 42 5\n";

/// Clones, drops, a weak field read while its target lives and after it
/// is gone, and a `sync enum`'s clone: the counts end where a plain
/// `shared` program's would, with the `Drop` body run once.
#[test]
fn mir_sync_counts_clone_drop_and_upgrade() {
    assert_eq!(stdout_on_both("handles", HANDLES), HANDLES_OUT);
    if let Some(out) = asan_stdout("handles", HANDLES) {
        assert_eq!(out, HANDLES_OUT);
    }
}

/// A `sync` handle cloned into two tasks: each clone is released once,
/// and the last release runs `Drop`.
#[test]
fn mir_sync_counts_cross_into_tasks() {
    let src = r#"sync struct Config {
    name: String,
    hits: Atomic[i64],
}

impl Drop for Config {
    fn drop(mut ref self) {
        println(f"drop {self.name} {self.hits.load()}");
    }
}

fn work(c: Config, n: i64) -> i64 {
    sleep_ms(10);
    c.hits.fetch_add(n);
    n
}

fn main() {
    let c = Config { name: "cfg", hits: Atomic.new(0) };
    let group = TaskGroup.new();
    let a = { let c1 = c.clone(); group.spawn(|| work(c1, 1)) };
    let b = { let c2 = c.clone(); group.spawn(|| work(c2, 2)) };
    println(f"joined {a.join() + b.join()}");
    println(f"hits {c.hits.load()}");
}
"#;
    let want = "joined 3\nhits 3\ndrop cfg 3\n";
    assert_eq!(stdout_on_both("tasks", src), want);
    if let Some(out) = asan_stdout("tasks", src) {
        assert_eq!(out, want);
    }
}

/// Only a `sync` type pays for atomics: the same program with every
/// `sync` spelled `shared` emits none.
#[test]
fn mir_sync_counts_only_sync_types_count_atomically() {
    let sync = ir_of("ir", "sync", HANDLES);
    assert!(atomics(&sync) > 0, "a `sync` program emitted no atomics");
    assert!(
        sync.contains(" cmpxchg "),
        "a `sync` weak upgrade is not a compare-and-swap"
    );
    let plain = HANDLES
        .replace("sync struct", "shared struct")
        .replace("sync enum", "shared enum");
    assert_eq!(atomics(&ir_of("ir", "plain", &plain)), 0);
}

/// The weak count the strong handles hold together: a weak handle released
/// while its object lives, one to the object itself in its own field (read
/// in its `Drop` body as gone), one that outlives it, and a long chain
/// released by the loop.
#[test]
fn mir_sync_counts_weak_handles_around_the_last_release() {
    let src = r#"shared struct N {
    id: i64,
    mut me: weak N,
    mut next: Option[N],
}

impl Drop for N {
    fn drop(mut ref self) {
        println(f"drop {self.id} self alive {self.me.is_some()}");
    }
}

struct Watch {
    of: weak N,
}

fn main() {
    let w = {
        let a = N { id: 1, me: Option.None, next: Option.None };
        a.me = a;
        {
            let early = Watch { of: a };
            println(f"early {early.of.is_some()}");
        }
        {
            let mut head = N { id: 2, me: Option.None, next: Option.None };
            for i in 3..6 {
                head = N { id: i, me: Option.None, next: Option.Some(head) };
            }
            println(f"chain head {head.id}");
        }
        Watch { of: a }
    };
    println(f"after {w.of.is_some()}");
}
"#;
    let want = "early true\nchain head 5\ndrop 5 self alive false\ndrop 4 self alive false\ndrop 3 self alive false\ndrop 2 self alive false\ndrop 1 self alive false\nafter false\n";
    assert_eq!(stdout_on_both("weak", src), want);
    if let Some(out) = asan_stdout("weak", src) {
        assert_eq!(out, want);
    }
}

const ATOMICS: &str = r#"import std.sync.{Atomic};

sync struct Stats {
    n: Atomic[i64],
    u: Atomic[u32],
    flag: Atomic[bool],
}

fn bump(s: Stats, k: i64) -> i64 {
    sleep_ms(5);
    s.n.fetch_add(k)
}

fn main() {
    let s = Stats { n: Atomic.new(10), u: Atomic.new(4_u32), flag: Atomic.new(false) };
    println(f"{s.n.fetch_add(5)} {s.n.load()}");
    println(f"{s.n.fetch_sub(20)} {s.n.load()}");
    println(f"{s.n.swap(7)} {s.n.load()}");
    match s.n.compare_exchange(7, 9) { Ok(p) => println(f"ok {p}"), Err(a) => println(f"err {a}") }
    match s.n.compare_exchange(7, 11) { Ok(p) => println(f"ok {p}"), Err(a) => println(f"err {a}") }
    println(f"{s.u.fetch_sub(5_u32)} {s.u.load()}");
    s.flag.store(true);
    println(f"{s.flag.load()} {s.flag.swap(false)} {s.flag.load()}");
    match s.flag.compare_exchange(false, true) { Ok(p) => println(f"ok {p}"), Err(a) => println(f"err {a}") }
    let g = TaskGroup.new();
    let a = { let c = s.clone(); g.spawn(|| bump(c, 100)) };
    let b = { let c = s.clone(); g.spawn(|| bump(c, 1000)) };
    let _ = a.join() + b.join();
    println(f"end {s.n.load()}");
}
"#;

/// Every `Atomic[T]` method on `i64`, `u32` and `bool`, and two tasks
/// adding to one counter.
#[test]
fn mir_sync_counts_atomic_methods() {
    let want =
        "10 15\n15 -5\n-5 7\nok 7\nerr 9\n4 4294967295\ntrue true false\nok false\nend 1109\n";
    assert_eq!(stdout_on_both("atomics", ATOMICS), want);
    if let Some(out) = asan_stdout("atomics", ATOMICS) {
        assert_eq!(out, want);
    }
}

/// `Atomic[T]` loads and stores are atomic instructions, not plain ones.
#[test]
fn mir_sync_counts_atomic_cells_are_atomic_instructions() {
    let ir = ir_of("atomic-ir", "a", ATOMICS);
    assert!(ir.contains("load atomic i64"), "no atomic load");
    assert!(ir.contains("store atomic i8"), "no atomic bool store");
    assert!(ir.contains("atomicrmw xchg"), "no atomic swap");
}

/// A task handle's block goes to exactly one side (M4 item 10b-1): the
/// handle frees it after the task finished, the task after the handle
/// dropped while it ran, and a join takes the result; the state word's
/// mark and detach are one atomic exchange each.
#[test]
fn mir_sync_counts_task_handles_free_their_block_once() {
    let src = r#"struct Noisy { name: String }
impl Drop for Noisy { fn drop(mut ref self) { println(f"drop {self.name}"); } }
fn make(n: own String, ms: i64) -> Noisy { sleep_ms(ms); Noisy { name: n } }
fn main() {
    let g = TaskGroup.new();
    {
        let _early = g.spawn(|| make("detached-running", 30));
    }
    let done = g.spawn(|| make("joined", 1));
    let late = g.spawn(|| make("dropped-after-done", 1));
    sleep_ms(10);
    println(f"got {done.join().name}");
    {
        let _l = late;
    }
    println("end");
}
"#;
    let want = "got joined\ndrop joined\ndrop dropped-after-done\nend\ndrop detached-running\n";
    assert_eq!(stdout_on_both("handles-once", src), want);
    if let Some(out) = asan_stdout("handles-once", src) {
        assert_eq!(out, want);
    }
}
