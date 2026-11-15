//! Gaps the ninep_server app (kara-katas apps/service/ninep_server) found
//! porting a Rust-shaped draft: `&format!(..)` was fixed to a refused borrow of
//! a temporary (B-2026-11-15-70), Rust's `impl fmt::Display` stopped the parse
//! (B-2026-11-15-71), `Err("..".into())` and `"..".into()` for an
//! `impl From[Str]` found no target (B-2026-11-15-72), a `MutexGuard` passed
//! or read where its value was meant had no fix (B-2026-11-15-73), Rust's
//! `let (sock, _) = listener.accept()` had none either (B-2026-11-15-74),
//! `Mutex.into_inner` was missing (B-2026-11-15-75), and a slice's
//! `try_into()` to an array said nothing useful (B-2026-11-15-76).

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
    let dir = std::env::temp_dir().join(format!("karac-ninep-gaps-{}", std::process::id()));
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
fn a_rust_display_impl_is_rewritten_to_to_string() {
    let path = scratch(
        "display.rs.kara",
        "enum E {
    Msg(String),
    Bad,
}

impl std::fmt::Display for E {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            E::Msg(m) => write!(f, \"msg {}\", m),
            E::Bad => f.write_str(\"bad\"),
        }
    }
}

struct P {
    x: i64,
}

impl fmt::Display for P {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, \"P<{}>\", self.x)
    }
}

fn main() {
    let p = P { x: 3 };
    println!(\"{} {} {}\", E::Msg(\"a\".to_string()), E::Bad, p);
}
",
    );
    let check = karac(&["check"], &path);
    assert!(
        text(&check).contains("is Rust's -- Kāra's `Display` is `fn to_string(self) -> String`"),
        "{}",
        text(&check)
    );
    fix_to_fixpoint(&path);
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(src.contains("fn to_string(self) -> String"), "{src}");
    assert_eq!(run_stdout(&path), "msg a bad P<3>\n");
}

#[test]
fn a_display_body_that_writes_twice_is_reported_with_no_fix() {
    let path = scratch(
        "display_twice.rs.kara",
        "struct P {
    x: i64,
}

impl Display for P {
    fn fmt(&self, f: &mut Formatter) -> Result {
        write!(f, \"a\")?;
        write!(f, \"{}\", self.x)
    }
}

fn main() {
    println(f\"{P { x: 1 }}\");
}
",
    );
    let check = karac(&["check"], &path);
    let out = text(&check);
    assert!(
        out.contains("building the text where the body writes to the formatter"),
        "{out}"
    );
    let fix = karac(&["fix", "--dry-run"], &path);
    assert!(!text(&fix).contains("fix(es)"), "{}", text(&fix));
}

#[test]
fn a_borrow_of_a_temporary_is_fixed_by_dropping_the_borrow() {
    let path = scratch(
        "temporary.rs.kara",
        "fn main() {
    let mut t = String::new();
    let s = \"abc\".to_string();
    t += &s;
    t += &format!(\"x{}\", 1);
    t += &s.to_uppercase();
    t += &\"!\";
    println!(\"{}\", t);
}
",
    );
    fix_to_fixpoint(&path);
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(!src.contains("ref f\""), "{src}");
    assert_eq!(run_stdout(&path), "abcx1ABC!\n");
}

#[test]
fn into_takes_its_target_from_an_err_slot_and_a_str_impl() {
    let path = scratch(
        "into.rs.kara",
        "enum E {
    Msg(String),
}

impl From<&str> for E {
    fn from(s: &str) -> Self {
        E::Msg(s.to_string())
    }
}

fn check(x: i64) -> Result<i64, E> {
    if x > 1 {
        return Err(\"too big\".into());
    }
    if x < 0 {
        let e: E = \"negative\".into();
        return Err(e);
    }
    if x == 1 {
        let r: Result<i64, E> = Err(\"one\".into());
        return r;
    }
    Err(\"zero\".into())
}

fn main() {
    for x in [2, -1, 1, 0] {
        match check(x) {
            Ok(_) => println!(\"ok\"),
            Err(E::Msg(m)) => println!(\"{}\", m),
        }
    }
}
",
    );
    fix_to_fixpoint(&path);
    assert_eq!(run_stdout(&path), "too big\nnegative\none\nzero\n");
}

#[test]
fn a_guard_passed_or_read_for_its_value_is_fixed_to_the_value_field() {
    let path = scratch(
        "guard_value.rs.kara",
        "use std::sync::Mutex;

struct S {
    n: i64,
    log: Vec<String>,
}

fn bump(s: &mut S) {
    s.n += 1;
}

fn main() {
    let m = Mutex::new(S { n: 0, log: Vec::new() });
    {
        let mut g = m.lock().unwrap();
        bump(&mut g);
    }
    m.lock().unwrap().log.push(\"x\".to_string());
    let n = m.lock().n;
    let s = m.into_inner().unwrap();
    println!(\"{} {} {}\", n, s.n, s.log.len());
}
",
    );
    fix_to_fixpoint(&path);
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(!src.contains("unwrap"), "{src}");
    assert!(src.contains("bump(mut g.value)"), "{src}");
    assert!(src.contains("m.lock().value.n"), "{src}");
    assert!(src.contains("m.into_inner();"), "{src}");
    assert_eq!(run_stdout(&path), "1 1 1\n");
}

#[test]
fn rusts_accept_pair_is_fixed_to_the_stream() {
    let path = scratch(
        "accept.rs.kara",
        "use std::net::{TcpListener, TcpStream};

fn main() {
    let listener = TcpListener::bind(\"127.0.0.1:0\").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut c = TcpStream::connect(&addr).unwrap();
    let (mut s, _) = listener.accept().unwrap();
    let _ = c.write_all(b\"hi\");
    let mut buf = [0u8; 2];
    s.read_exact(&mut buf).unwrap();
    println!(\"{} {}\", buf[0], buf[1]);
}
",
    );
    fix_to_fixpoint(&path);
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(src.contains("let s = listener.accept()"), "{src}");
    assert_eq!(run_stdout(&path), "104 105\n");
}

#[test]
fn an_accept_pair_that_binds_the_address_is_reported_with_no_fix() {
    let path = scratch(
        "accept_addr.kara",
        "import std.net.TcpListener;
fn main() {
    let listener = TcpListener.bind(\"127.0.0.1:0\").unwrap();
    let (s, addr) = listener.accept().unwrap();
    println(f\"{addr}\");
    drop(s);
}
",
    );
    let check = karac(&["check"], &path);
    let out = text(&check);
    assert!(
        out.contains("Kāra's `accept` gives the `TcpStream` alone"),
        "{out}"
    );
    let fix = karac(&["fix", "--dry-run"], &path);
    assert!(text(&fix).contains("no fixable"), "{}", text(&fix));
}

#[test]
fn a_slice_try_into_an_array_says_what_to_write() {
    let path = scratch(
        "try_into.kara",
        "fn word(b: Slice[u8]) -> u16 {
    u16.from_le_bytes(b[..2].try_into().unwrap())
}
fn main() {
    println(word([1, 2]));
}
",
    );
    let check = karac(&["check"], &path);
    let out = text(&check);
    assert!(
        out.contains("write the array from the slice's elements, `[b[0], b[1]]`"),
        "{out}"
    );
}
