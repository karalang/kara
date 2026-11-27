//! The gaps porting apps/service/radius_server found (B-2026-11-27-40 to
//! -45), each pinned on the interpreter and, where it runs, compiled:
//!
//! - -40: Rust's `mut self` on a method that hands `self` back is fixed to
//!   `own self`; one that only writes it still gets `mut ref self`.
//! - -41: `Arc<T>` in a struct field becomes `ref T`, unless the file fills
//!   the field with an `Arc::new(..)`, which keeps it owned.
//! - -42: a `u64` left at `set_read_timeout(Some(x))` once
//!   `Duration::from_millis(x)` is mended away gets `as i64`.
//! - -43: a `Str` or `Slice` argument of a `mut ref self` call on a struct
//!   with a `ref` field is not kept by the receiver when it has no slot of
//!   that type; one it has a slot for still is (E0516).
//! - -44: a `for` over an `Array` in a function that suspends ran into
//!   "index is held across a suspension point".
//! - -45: `Some(ref v)` with `v: Vec[u8]` at an `Option[Slice[u8]]` built
//!   invalid MIR.

use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-radius-gaps-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

fn karac(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn run(tag: &str, mode: &str, src: &str) -> String {
    let (dir, _) = scratch(&format!("{tag}-{mode}"), src);
    let out = karac(&dir, &[mode, "main.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `karac fix` to a fixpoint, then the fixed text and its run.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let (dir, file) = scratch(&format!("{tag}-fix"), src);
    let out = karac(&dir, &["fix", "main.kara"]);
    let text = std::fs::read_to_string(&file).unwrap();
    let run = karac(&dir, &["__mir-run", "main.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "fix: {}\n{text}\nrun: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&run.stderr)
    );
    (text, String::from_utf8_lossy(&run.stdout).into_owned())
}

#[test]
fn fix_mut_self_handed_back_is_own_self() {
    let src = r#"
struct Port { n: i64, log: Vec<String> }

impl Port {
    fn serve(mut self) -> Port {
        while self.n < 3 {
            self.n += 1;
            self.log.push(format!("n {}", self.n));
        }
        self
    }

    fn bump(mut self) {
        self.n += 10;
    }
}

fn main() {
    let p = Port { n: 0, log: Vec::new() };
    let mut p = p.serve();
    p.bump();
    println!("{} {}", p.log.join(","), p.n);
}
"#;
    let (text, out) = fixed("mutself", src);
    assert!(text.contains("fn serve(own self) -> Port"), "{text}");
    assert!(text.contains("fn bump(mut ref self)"), "{text}");
    assert_eq!(out, "n 1,n 2,n 3 13\n");
}

#[test]
fn fix_arc_struct_field_is_a_ref_unless_filled_with_arc_new() {
    let src = r#"
use std::sync::{Arc, Mutex};
use std::thread;

struct Counter { n: i64 }

struct Worker {
    name: String,
    state: Arc<Mutex<Counter>>,
}

struct Owner {
    total: Arc<Mutex<i64>>,
}

impl Worker {
    fn run(self) -> String {
        self.state.lock().unwrap().n += 1;
        self.name.clone()
    }
}

fn main() {
    let state = Arc::new(Mutex::new(Counter { n: 0 }));
    let a = Worker { name: "a".to_string(), state: Arc::clone(&state) };
    let b = Worker { name: "b".to_string(), state: Arc::clone(&state) };
    let ta = thread::spawn(move || a.run());
    let tb = thread::spawn(move || b.run());
    println!("{} {}", ta.join().unwrap(), tb.join().unwrap());
    println!("{}", state.lock().unwrap().n);
    let o = Owner { total: Arc::new(Mutex::new(5)) };
    *o.total.lock().unwrap() += 1;
    println!("{}", *o.total.lock().unwrap());
}
"#;
    let (text, out) = fixed("arcfield", src);
    assert!(text.contains("state: ref Mutex[Counter],"), "{text}");
    assert!(text.contains("total: Mutex[i64],"), "{text}");
    assert_eq!(out, "a b\n2\n6\n");
}

#[test]
fn fix_u64_millis_at_set_read_timeout_gets_as_i64() {
    let src = r#"
use std::net::UdpSocket;
use std::time::Duration;

const WAIT: u64 = 20;

fn main() {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let quiet = true;
    let wait = if quiet { WAIT } else { 5000 };
    sock.set_read_timeout(Some(Duration::from_millis(wait))).unwrap();
    let mut buf = [0u8; 16];
    match sock.recv_from(&mut buf) {
        Ok(_) => println!("a datagram"),
        Err(_) => println!("timed out"),
    }
}
"#;
    let (text, out) = fixed("timeout", src);
    assert!(
        text.contains("set_read_timeout(Some(wait as i64))"),
        "{text}"
    );
    assert_eq!(out, "timed out\n");
}

const VIEW_ARGS: &str = r#"
import std.sync.{Mutex};

struct Counter { n: i64 }

struct Port {
    state: ref Mutex[Counter],
    log: Vec[String],
}

impl Port {
    fn note(mut ref self, who: Str, data: Slice[u8]) -> i64 {
        self.log.push(f"{who}: {data.len()}");
        self.state.lock().value.n += 1;
        data.len()
    }

    fn serve(own self) -> Port {
        let buf = [1u8, 2, 3];
        let who = "nas".to_string();
        let n = self.note(who, buf[..2]);
        self.log.push(f"n {n}");
        self
    }
}

fn main() {
    let state = Mutex.new(Counter { n: 0 });
    let p = Port { state: state, log: Vec.new() };
    let p = p.serve();
    println(p.log.join(","));
    println(state.lock().value.n);
}
"#;

#[test]
fn interp_view_args_of_a_ref_field_receiver_are_not_kept() {
    assert_eq!(run("views", "__mir-run", VIEW_ARGS), "nas: 2,n 2\n1\n");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_view_args_of_a_ref_field_receiver_are_not_kept() {
    assert_eq!(run("views", "__mir-llvm-run", VIEW_ARGS), "nas: 2,n 2\n1\n");
}

#[test]
fn check_a_view_the_receiver_can_keep_still_outlives_its_value() {
    let src = r#"
import std.sync.{Mutex};

struct Names {
    keep: Vec[Str],
    m: ref Mutex[i64],
}

impl Names {
    fn add(mut ref self, s: Str) {
        self.keep.push(s);
        self.m.lock().value += 1;
    }
}

fn main() {
    let m = Mutex.new(0);
    let mut n = Names { keep: Vec.new(), m: m };
    {
        let s = "abc".to_string();
        n.add(s);
    }
    println(n.keep.len());
}
"#;
    let (dir, _) = scratch("keeps", src);
    let mir = karac(&dir, &["__mir-run", "main.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&mir.stderr);
    assert!(!mir.status.success() && text.contains("E0516"), "{text}");
}

const ARRAY_FOR: &str = r#"
import std.net.UdpSocket;

fn main() {
    let a = UdpSocket.bind("127.0.0.1:0").unwrap();
    let b = UdpSocket.bind("127.0.0.1:0").unwrap();
    let to = a.local_addr().unwrap().to_string();
    let words: Array[(Str, i64), 3] = [("x", 1), ("yy", 2), ("zzz", 3)];
    for (w, n) in words {
        b.send_to(w.bytes(), to).unwrap();
        let mut buf = [0u8; 16];
        let (got, _) = a.recv_from(mut buf).unwrap();
        println(f"{w} {n} {got}");
    }
}
"#;

const ARRAY_FOR_WANT: &str = "x 1 1\nyy 2 2\nzzz 3 3\n";

#[test]
fn interp_for_over_an_array_across_a_suspension() {
    assert_eq!(run("arrayfor", "__mir-run", ARRAY_FOR), ARRAY_FOR_WANT);
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_for_over_an_array_across_a_suspension() {
    assert_eq!(run("arrayfor", "__mir-llvm-run", ARRAY_FOR), ARRAY_FOR_WANT);
}

#[cfg(feature = "llvm")]
#[test]
fn asan_for_over_an_array_across_a_suspension() {
    assert_eq!(
        run("arrayfor", "__mir-llvm-asan", ARRAY_FOR),
        ARRAY_FOR_WANT
    );
}

const REF_VEC_SLICE: &str = r#"
struct Attr { t: u8, v: Vec[u8] }

fn int_of(v: Option[Slice[u8]]) -> u32 {
    match v {
        Some(b) if b.len() == 4 => u32.from_be_bytes([b[0], b[1], b[2], b[3]]),
        _ => 0,
    }
}

fn main() {
    let a = Attr { t: 6, v: [0u8, 0, 1, 2].to_vec() };
    println(int_of(Some(ref a.v)));
    let w = [0u8, 0, 0, 9].to_vec();
    println(int_of(Some(ref w)));
    println(int_of(Some(w)));
    println(a.t);
}
"#;

const REF_VEC_SLICE_WANT: &str = "258\n9\n9\n6\n";

#[test]
fn interp_ref_vec_in_an_option_slice_slot_is_viewed() {
    assert_eq!(
        run("refvec", "__mir-run", REF_VEC_SLICE),
        REF_VEC_SLICE_WANT
    );
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_ref_vec_in_an_option_slice_slot_is_viewed() {
    assert_eq!(
        run("refvec", "__mir-llvm-run", REF_VEC_SLICE),
        REF_VEC_SLICE_WANT
    );
}

#[cfg(feature = "llvm")]
#[test]
fn asan_ref_vec_in_an_option_slice_slot_is_viewed() {
    assert_eq!(
        run("refvec", "__mir-llvm-asan", REF_VEC_SLICE),
        REF_VEC_SLICE_WANT
    );
}
