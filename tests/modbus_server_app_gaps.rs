//! Gaps the modbus_server app (kara-katas apps/service/modbus_server)
//! found porting a Rust-shaped draft: `use std::io::{Read, Write};` and
//! `std::net::Shutdown` became imports of names Kāra does not have, with
//! no fix, and `shutdown(Shutdown::Write)` had none either
//! (B-2026-11-16-170); `[[5], be16(n)]` passed as a `Slice[Slice[u8]]`
//! checked but built invalid MIR (B-2026-11-16-171); and Rust's
//! `parts.concat()` over a `Slice[Slice[u8]]` mended to a `join("")` that
//! does not check (B-2026-11-16-172).

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
    let dir = std::env::temp_dir().join(format!("karac-modbus-gaps-{}", std::process::id()));
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
fn rust_io_trait_imports_are_deleted() {
    let path = scratch(
        "io.kara",
        "use std::io::{Read, Write};
use std::net::{TcpListener, Shutdown, TcpStream};

fn main() {
    let l = TcpListener.bind(\"127.0.0.1:0\").unwrap();
    let addr = l.local_addr().unwrap();
    let mut s = TcpStream.connect(addr).unwrap();
    s.shutdown(Shutdown::Write).unwrap();
    let (mut t, _) = l.accept().unwrap();
    let mut buf = [0u8; 4];
    let n = t.read(&mut buf).unwrap();
    println!(\"{}\", n);
}
",
    );
    let check = karac(&["check"], &path);
    let said = text(&check);
    assert!(said.contains("methods of the handle itself"), "{said}");
    assert!(said.contains("`Shutdown` is Rust's alone"), "{said}");
    assert!(
        said.contains("`shutdown(Shutdown::Write)` is Rust's"),
        "{said}"
    );
    let fixed = fix_to_fixpoint(&path);
    assert!(!fixed.contains("std.io"), "{fixed}");
    assert!(!fixed.contains("Shutdown"), "{fixed}");
    assert!(
        fixed.contains("import std.net.{TcpListener, TcpStream};"),
        "{fixed}"
    );
    assert!(fixed.contains("s.shutdown_write().unwrap();"), "{fixed}");
    assert_eq!(run_stdout(&path), "0\n");
}

#[test]
fn a_rust_only_name_last_in_its_group_takes_the_comma_before_it() {
    let path = scratch(
        "last.kara",
        "import std.net.{TcpStream, Shutdown};

fn main() {
    println(\"ok\");
}
",
    );
    let fixed = fix_to_fixpoint(&path);
    assert!(fixed.contains("import std.net.{TcpStream};"), "{fixed}");
}

#[test]
fn arrays_passed_as_a_slice_of_slices_are_viewed() {
    let path = scratch(
        "views.kara",
        "fn be16(v: u16) -> Array[u8, 2] {
    v.to_be_bytes()
}

fn total(parts: Slice[Slice[u8]]) -> i64 {
    let mut t = 0;
    for p in parts.iter() {
        t += p.len();
    }
    t
}

fn main() {
    let v: Vec[u8] = Vec.filled(3, 0u8);
    println(f\"{total([[5], be16(7)])} {total([v, [1]])}\");
}
",
    );
    assert_eq!(run_stdout(&path), "3 4\n");
}

#[test]
fn concat_flattens_a_sequence_of_slices() {
    let path = scratch(
        "concat.kara",
        "fn be16(v: u16) -> Array[u8, 2] {
    v.to_be_bytes()
}

fn pdu(parts: Slice[Slice[u8]]) -> Vec[u8] {
    parts.concat()
}

fn main() {
    let p = pdu([[5], be16(5), be16(0xFF00)]);
    let a: Vec[u8] = Vec.filled(2, 7u8);
    let views: Vec[Slice[u8]] = [a.as_slice(), a.as_slice()];
    let q = views.concat();
    println(f\"{p.len()} {p[0]} {p[1]} {p[2]} {p[3]} {p[4]} {q.len()} {q[3]}\");
}
",
    );
    let check = karac(&["check"], &path);
    let said = text(&check);
    assert!(!said.contains("deprecated"), "{said}");
    assert_eq!(run_stdout(&path), "5 5 0 5 255 0 4 7\n");
}
