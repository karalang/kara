//! Review rulings on rows the real-world apps filed.
//!
//! B-2026-11-09-72: `drop(x)` is in the prelude, `fn drop[T](x: own T) {}`:
//! `x` moves in and drops at the call, so a `BufWriter` flushes there.
//!
//! B-2026-11-09-73: `set_read_timeout` on a socket a `Network` fake opened
//! is `Ok(())` and changes nothing (a fake's read never waits); below 1 ms
//! it is still `InvalidInput`, and `ProviderUnavailable` once the override
//! has ended.
//!
//! B-2026-11-09-50: `let v: Vec[Str] = ["a", "b"].to_vec();` keeps what the
//! elements borrow, not the array literal.

mod common;

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn check(src: &str) -> (bool, String) {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-review-rulings-apps-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The program's stdout on the MIR interpreter, run in a scratch directory
/// of its own (the `drop` program writes a file there).
fn run(src: &str) -> String {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "karac-review-rulings-apps-i-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[cfg(feature = "llvm")]
fn compiled(src: &str) -> Option<String> {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!(
        "karac-review-rulings-apps-b-{}-{n}",
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

const DROP: &str = r#"struct Noisy {
    name: String,
}

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

fn main() {
    let path = "karac_drop_probe.txt";
    let f = File.create(path).unwrap();
    let mut w = BufWriter.new(f);
    let _ = w.write_all("hello\n".bytes());
    drop(w);
    println(f"after drop: {fs.read_to_string(path) ?? "?".to_string()}");
    let a = Noisy { name: "a".to_string() };
    let b = Noisy { name: "b".to_string() };
    drop(a);
    println("between");
    let v = [1, 2, 3].to_vec();
    drop(v);
    let _ = fs.remove_file(path);
    println(f"end {b.name}");
}
"#;

const DROP_OUT: &str = "after drop: hello\n\ndrop a\nbetween\nend b\ndrop b\n";

#[test]
fn drop_ends_a_value_at_the_call() {
    let (ok, diags) = check(DROP);
    assert!(ok, "{diags}");
    assert_eq!(run(DROP), DROP_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn drop_ends_a_value_at_the_call_compiled() {
    if let Some(out) = compiled(DROP) {
        assert_eq!(out, DROP_OUT);
    }
}

#[test]
fn a_dropped_value_is_moved() {
    let (ok, diags) = check(
        r#"struct N { name: String }
fn main() {
    let a = N { name: "a".to_string() };
    drop(a);
    println(a.name);
}
"#,
    );
    assert!(!ok);
    assert!(diags.contains(":5:13: value 'a' moved here"), "{diags}");
}

#[test]
fn a_programs_own_drop_function_wins() {
    let src = "fn drop(x: i64) -> i64 {
    x + 1
}

fn main() {
    println(f\"{drop(4)}\");
}
";
    let (ok, diags) = check(src);
    assert!(ok, "{diags}");
    assert_eq!(run(src), "5\n");
}

const TIMEOUT: &str = r#"struct Pong { n: i64 }

impl NetworkProvider for Pong {
    fn connect(mut ref self, addr: Str) -> Result[i64, NetError] {
        self.n += 1;
        Ok(self.n)
    }

    fn read_stream(mut ref self, h: i64, buf: mut Slice[u8]) -> Result[i64, NetError] {
        let reply = "+PONG".bytes();
        for i in 0..reply.len() {
            buf[i] = reply[i];
        }
        Ok(reply.len())
    }

    fn write_stream(mut ref self, h: i64, buf: Slice[u8]) -> Result[i64, NetError] {
        Ok(buf.len())
    }

    fn close(mut ref self, h: i64) {
        println(f"close {h}");
    }
}

fn why(r: Result[(), NetError]) -> String {
    match r {
        Ok(()) => "ok".to_string(),
        Err(NetError.InvalidInput) => "invalid input".to_string(),
        Err(NetError.Unsupported) => "unsupported".to_string(),
        Err(NetError.ProviderUnavailable) => "provider unavailable".to_string(),
        Err(_) => "else".to_string(),
    }
}

fn ping(ms: i64) -> Result[String, NetError] {
    let s = TcpStream.connect("127.0.0.1:6379")?;
    s.set_read_timeout(Some(ms))?;
    let _ = s.write_all("PING\r\n".bytes())?;
    let mut buf = [0u8; 16];
    let n = s.read(mut buf)?;
    Ok(String.from_utf8(buf[0..n].to_vec()).unwrap_or("?".to_string()))
}

fn main() {
    let kept = providers {
        Network => Pong { n: 0 },
    } in {
        match ping(250) {
            Ok(r) => println(f"reply {r}"),
            Err(_) => println("failed"),
        }
        let s = TcpStream.connect("127.0.0.1:6379").unwrap();
        println(f"zero {why(s.set_read_timeout(Some(0)))}");
        println(f"none {why(s.set_read_timeout(None))}");
        s
    };
    println(f"after {why(kept.set_read_timeout(Some(5)))}");
}
"#;

const TIMEOUT_OUT: &str =
    "close 1\nreply +PONG\nzero invalid input\nnone ok\nafter provider unavailable\n";

#[test]
fn a_fakes_socket_accepts_a_read_timeout() {
    let (ok, diags) = check(TIMEOUT);
    assert!(ok, "{diags}");
    assert_eq!(run(TIMEOUT), TIMEOUT_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn a_fakes_socket_accepts_a_read_timeout_compiled() {
    if let Some(out) = compiled(TIMEOUT) {
        assert_eq!(out, TIMEOUT_OUT);
    }
}

const TO_VEC: &str = r#"fn total(v: Vec[Str]) -> i64 {
    let mut n = 0;
    for s in v.iter() {
        n += s.len();
    }
    n
}

fn main() {
    let v: Vec[Str] = ["a", "bc"].to_vec();
    println(f"{v.len()} {v[0]}{v[1]} {total(v)}");
    let s = "x y".to_string();
    let w: Vec[Str] = [s.as_str(), "z"].to_vec();
    println(f"{w[0]}{w[1]}");
}
"#;

#[test]
fn to_vec_of_a_literal_array_keeps_its_elements_borrows() {
    let (ok, diags) = check(TO_VEC);
    assert!(ok, "{diags}");
    assert_eq!(run(TO_VEC), "2 abc 3\nx yz\n");
}

#[cfg(feature = "llvm")]
#[test]
fn to_vec_of_a_literal_array_keeps_its_elements_borrows_compiled() {
    if let Some(out) = compiled(TO_VEC) {
        assert_eq!(out, "2 abc 3\nx yz\n");
    }
}

#[test]
fn to_vec_still_carries_a_temporary_elements_borrow() {
    let (ok, diags) = check(
        r#"fn main() {
    let w: Vec[Str] = [String.from("tmp").as_str(), "z"].to_vec();
    println(f"{w[0]}");
}
"#,
    );
    assert!(!ok);
    assert!(
        diags.contains(":2:24: a reference to a temporary outlives its statement"),
        "{diags}"
    );
    let (ok, diags) = check(
        r#"fn main() {
    let s = String.from("x");
    let arr = [s.as_str(), "z"];
    let w: Vec[Str] = arr.to_vec();
    drop(s);
    println(f"{w[0]}");
}
"#,
    );
    assert!(!ok);
    assert!(
        diags.contains(":5:5: cannot move `s` while `w` still holds a borrow of it"),
        "{diags}"
    );
}
