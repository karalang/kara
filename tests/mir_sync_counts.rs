//! Atomic counts for `sync` types in built binaries (core-semantics §6.3,
//! M4 item 10a): a `sync struct` / `sync enum` handle's strong and weak
//! counts change by atomic read-modify-write, and a weak upgrade checks
//! liveness and raises the count in one compare-and-swap, while a plain
//! `shared` type keeps its plain loads and stores. Each program prints the
//! same on the interpreter, built, and under ASan.
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
        let out = run(name, Command::new(&exe));
        assert!(
            out.status.success(),
            "{name} built: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            interp,
            "{name}: the build and the interpreter differ"
        );
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
