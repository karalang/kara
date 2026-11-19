//! The docs/library pass of 2026-11-19 found six places where the checker
//! disagreed with a documented signature or promise:
//!
//! - B-2026-11-19-115 (secret.md § Access): `Secret.ct_eq` was accepted on any `T`.
//!   It is defined only for `Secret[String]` and `Secret[Vec[u8]]`, and is
//!   refused on any other payload with both named.
//! - B-2026-11-19-116 (collections.md § Entry): `or_insert_with` / `and_modify`
//!   take a `OnceFn`, so a closure that gives a capture away fits.
//! - B-2026-11-19-117 (concurrency.md § Semaphore): `Semaphore.release` (and
//!   `Arena.rewind_to`) returned the type `Unit`, not `()`.
//! - B-2026-11-19-118 (io.md § Standard streams, time-random-env.md § Environment):
//!   `stdout`/`stderr` `print`/`println` and `env.set` take `Str`.
//! - B-2026-11-19-119 (io.md § HTTP): `header(name)` borrows the name as a `Str`.
//! - B-2026-11-19-120 (io.md § Standard streams): `StdinLines` can be named.
//!
//! Each program is checked and run under the interpreter here; the
//! compiled `ct_eq` is pinned by its IR in `tests/mir_secret_ct_eq_ir.rs`.

use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

fn karac(src: &str, args: &[&str], stdin: &str) -> Output {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-libsig-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("main.kara")
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut i = child.stdin.take().unwrap();
        i.write_all(stdin.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// Checks `src` clean, then runs it under the interpreter and returns stdout.
fn checks_and_runs(src: &str, stdin: &str) -> String {
    let c = karac(src, &["check"], "");
    let err = String::from_utf8_lossy(&c.stderr);
    assert!(c.status.success() && !err.contains("error"), "{err}");
    let r = karac(src, &["__mir-run"], stdin);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    String::from_utf8_lossy(&r.stdout).into_owned()
}

#[test]
fn secret_ct_eq_on_string_and_byte_vec_compares_bytes() {
    let out = checks_and_runs(
        r#"import std.secret.{Secret};
fn main() {
    let a = Secret.new("hunter2".to_string());
    let b = Secret.new("hunter2".to_string());
    let c = Secret.new("hunter3".to_string());
    let d = Secret.new("hunt".to_string());
    println(a.ct_eq(b));
    println(a.ct_eq(c));
    println(a.ct_eq(d));
    let x: Secret[Vec[u8]] = Secret.new([1u8, 2, 3].to_vec());
    let y: Secret[Vec[u8]] = Secret.new([1u8, 2, 3].to_vec());
    let z: Secret[Vec[u8]] = Secret.new([1u8, 2, 4].to_vec());
    println(x.ct_eq(y));
    println(x.ct_eq(z));
}
"#,
        "",
    );
    assert_eq!(out, "true\nfalse\nfalse\ntrue\nfalse\n");
}

#[test]
fn secret_ct_eq_on_any_other_payload_is_refused_naming_both_types() {
    let out = karac(
        r#"import std.secret.{Secret};
struct Q { a: i64 }
fn main() {
    println(Secret.new(Q { a: 1 }).ct_eq(Secret.new(Q { a: 1 })));
    println(Secret.new(1).ct_eq(Secret.new(2)));
    let v: Secret[Vec[i64]] = Secret.new(Vec.new());
    println(v.ct_eq(Secret.new(Vec.new())));
    println(Secret.new(true).ct_eq(Secret.new(false)));
}
"#,
        &["check"],
        "",
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    for t in [
        "Secret[Q]",
        "Secret[i64]",
        "Secret[Vec[i64]]",
        "Secret[bool]",
    ] {
        assert!(
            err.contains(&format!("no method 'ct_eq' on type '{t}'")),
            "{t} not refused:\n{err}"
        );
    }
    assert_eq!(
        err.matches("defined only for Secret[String] and Secret[Vec[u8]]")
            .count(),
        4,
        "{err}"
    );
}

#[test]
fn entry_or_insert_with_and_and_modify_take_a_once_fn() {
    let out = checks_and_runs(
        r#"fn main() {
    let mut m: Map[String, Vec[String]] = Map.new();
    let v = Vec.new();
    m.entry("a".to_string()).or_insert_with(|| v).push("x".to_string());
    let w = vec_of("y");
    m.entry("a".to_string()).and_modify(|e| { let w2 = w; e.push(w2[0].clone()); });
    m.entry("b".to_string()).or_insert_with(Vec.new).push("z".to_string());
    let mut c: Map[String, i64] = Map.new();
    c.entry("k".to_string()).and_modify(|n| { *n += 1; }).or_insert(1);
    c.entry("k".to_string()).and_modify(|n| { *n += 1; }).or_insert(1);
    println(m["a"].len());
    println(m["a"][1]);
    println(m["b"][0]);
    println(c["k"]);
}
fn vec_of(s: Str) -> Vec[String] { let mut v = Vec.new(); v.push(s.to_string()); v }
"#,
        "",
    );
    assert_eq!(out, "2\ny\nz\n2\n");
}

#[test]
fn semaphore_release_and_arena_rewind_to_return_unit() {
    let out = checks_and_runs(
        r#"fn r(s: ref Semaphore) -> () {
    s.release()
}
fn main() {
    let s = Semaphore.new(0);
    r(s);
    match s.acquire(10) { Ok(()) => println("got"), Err(_) => println("none") }
    let a: Arena[i64] = Arena.new();
    let cp = a.high_water_mark();
    a.push(1);
    let u: () = a.rewind_to(cp);
    println(a.len());
}
"#,
        "",
    );
    assert_eq!(out, "got\n0\n");
}

#[test]
fn stdout_stderr_print_and_env_set_take_str() {
    let out = checks_and_runs(
        r#"fn f(s: Str) with writes(Env, Stdout, Stderr) {
    stdout.println(s);
    stdout.print(s);
    stdout.println("");
    stderr.println(s);
    stderr.print(s);
    env.set(s, s);
}
fn main() {
    f("KARA_LIBSIG_X");
    let owned = "own".to_string();
    stdout.println(owned);
    stdout.println(owned);
    env.set("KARA_LIBSIG_Y", owned);
    match env.var("KARA_LIBSIG_X") { Ok(v) => println(v), Err(_) => println("unset") }
    match env.var("KARA_LIBSIG_Y") { Ok(v) => println(v), Err(_) => println("unset") }
    stdout.println(f"{owned}!");
}
"#,
        "",
    );
    assert_eq!(
        out,
        "KARA_LIBSIG_X\nKARA_LIBSIG_X\nown\nown\nKARA_LIBSIG_X\nown\nown!\n"
    );
}

#[test]
fn request_header_borrows_its_name_as_str() {
    let out = checks_and_runs(
        r#"fn h(r: ref Request, n: Str) -> Option[String] { r.header(n) }
fn h2(r: ref Request, n: String) -> Option[String] { r.header(n) }
fn main() {
    let r = Request.new("GET", "/x").with_header("Accept", "text/plain");
    let n = "accept".to_string();
    println(h(r, "ACCEPT") ?? "none");
    println(h2(r, n) ?? "none");
    println(n);
    println(r.header("missing") ?? "none");
}
"#,
        "",
    );
    assert_eq!(out, "text/plain\ntext/plain\naccept\nnone\n");
}

#[test]
fn stdin_lines_type_can_be_named() {
    let out = checks_and_runs(
        r#"fn f() -> StdinLines with writes(Stdin) blocks { stdin.lines() }
fn main() {
    let mut n = 0;
    for line in f() {
        match line { Ok(l) => { n += l.len(); }, Err(_) => {} }
    }
    println(n);
}
"#,
        "ab\ncde\n",
    );
    assert_eq!(out, "5\n");
}
