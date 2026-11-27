//! B-2026-11-26-97: `p.read_volatile()` / `p.write_volatile(v)` and the
//! stdlib's `volatile_read` / `volatile_write` intrinsics are volatile
//! accesses in compiled code. MIR carries them as the `VolatileRead` /
//! `VolatileWrite` casts, and the backend emits one `load volatile` or
//! `store volatile` per access (a volatile `llvm.memcpy` for a struct), so
//! three writes to one register stay three writes. The interpreter's
//! behaviour is unchanged: the same values, read and written as plain ones.

#[cfg(feature = "llvm")]
mod common;

#[cfg(feature = "llvm")]
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const SRC: &str = "struct Pair { a: i64, b: i64 }
fn poke(p: *mut i32) {
    // Safety: `p` points at `main`'s live `x`.
    unsafe {
        p.write_volatile(1);
        p.write_volatile(2);
        p.write_volatile(3);
    }
}
fn peek(p: *const i32) -> i32 {
    // Safety: `p` points at `main`'s live `x`.
    unsafe { p.read_volatile() + p.read_volatile() }
}
fn pair(p: *mut Pair) -> i64 {
    // Safety: `p` points at `main`'s live `s`.
    unsafe {
        p.write_volatile(Pair { a: 4, b: 5 });
        let q = p.read_volatile();
        q.a + q.b
    }
}
fn main() {
    let mut x: i32 = 0;
    let p = ptr.mut(x);
    poke(p);
    let r = peek(ptr.const(x));
    let mut s: Pair = Pair { a: 0, b: 0 };
    let t = pair(ptr.mut(s));
    println(f\"{x} {r} {t} {s.a}\");
}
";

/// The last of the three writes stays, twice that is 6, and 4 + 5.
const EXPECTED: &str = "3 6 9 4\n";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "karac-volatile-access-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

#[test]
fn volatile_reads_and_writes_keep_their_values_interpreted() {
    let d = dir("interp");
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let o = karac()
        .args(["run", "--interp"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), EXPECTED);
}

/// The body of `@kara.<name>` in `ll`.
#[cfg(feature = "llvm")]
fn body(ll: &Path, name: &str) -> String {
    let ir = std::fs::read_to_string(ll).unwrap();
    let head = format!("@kara.{name}(");
    let mut lines = ir.lines();
    while let Some(l) = lines.next() {
        if l.starts_with("define") && l.contains(&head) {
            return lines
                .take_while(|l| *l != "}")
                .collect::<Vec<_>>()
                .join("\n");
        }
    }
    panic!("no {head} in the IR");
}

#[cfg(feature = "llvm")]
#[test]
fn volatile_reads_and_writes_are_volatile_in_compiled_code() {
    let d = dir("build");
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let ll = d.join("main.ll");
    let bin = d.join("main");
    let o = karac()
        .env("KARAC_MIR_LL", &ll)
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&o.stderr).to_string();
    if common::link_or_skip(if o.status.success() { Ok(()) } else { Err(why) }).is_none() {
        return;
    }
    let run = Command::new(&bin).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), EXPECTED);

    let count = |s: &str, pat: &str| s.matches(pat).count();
    let poke = body(&ll, "poke");
    assert_eq!(count(&poke, "store volatile i32"), 3, "{poke}");
    assert!(poke.contains("align 4"), "{poke}");
    let peek = body(&ll, "peek");
    assert_eq!(count(&peek, "load volatile i32"), 2, "{peek}");
    let pair = body(&ll, "pair");
    assert_eq!(count(&pair, "@llvm.memcpy"), 2, "{pair}");
    assert_eq!(count(&pair, "i1 true)"), 2, "{pair}");
}
