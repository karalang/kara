//! B-2026-11-24-19 and B-2026-11-24-20: `karac check` and `karac run`
//! disagreed on whether a value copies.
//!
//! - An `impl Tok` result whose trait has `Copy` as a supertrait moved at
//!   check (E0500) while the MIR builder copied it; design.md § `impl Trait`
//!   makes it Copy.
//! - An owned `dyn Shape` made from a Copy struct copied at check, which then
//!   accepted a use after `let b = a`, while the MIR move check refused it;
//!   design.md § `dyn Trait` says an owned dyn moves, and is Copy never.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const TOK: &str = "trait Tok: Copy { fn n(self) -> i64; }
impl Tok for i64 { fn n(self) -> i64 { self } }
fn mk() -> impl Tok { 5 }
fn main() { let a = mk(); let b = a; println(b.n() + a.n()); }
";

const SHAPE: &str = "trait Shape { fn area(self) -> i64; }
#[derive(Copy, Clone)]
struct Sq { s: i64 }
impl Shape for Sq { fn area(self) -> i64 { self.s * self.s } }
fn main() { let a: dyn Shape = Sq { s: 2 }; let b = a; println(b.area()); USE }
";

const MOVED: &str = "value 'a' moved here, used again here";

fn cell(src: &str) -> PathBuf {
    static CALL: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "karac-copy-bound-dyn-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, src).unwrap();
    file
}

/// Success, stdout and stderr of `karac <args> <file>`.
fn karac(args: &[&str], file: &PathBuf) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(file)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn an_impl_trait_of_a_copy_supertrait_copies_at_check() {
    let file = cell(TOK);
    let (ok, out, err) = karac(&["check"], &file);
    assert!(ok, "{out}{err}");
    let (ok, out, err) = karac(&["__mir-run"], &file);
    assert!(ok, "{err}");
    assert_eq!(out, "10\n");
}

#[test]
fn an_impl_trait_without_copy_still_moves_at_check() {
    let file = cell(&TOK.replace("trait Tok: Copy", "trait Tok"));
    let (ok, out, err) = karac(&["check"], &file);
    assert!(!ok, "a non-Copy bound moves: {out}{err}");
    assert!(format!("{out}{err}").contains(MOVED), "{out}{err}");
}

#[test]
fn an_owned_dyn_from_a_copy_struct_moves_at_check() {
    let file = cell(&SHAPE.replace("USE", "println(a.area());"));
    let (ok, out, err) = karac(&["check"], &file);
    assert!(!ok, "check accepted a use after move: {out}{err}");
    assert!(format!("{out}{err}").contains(MOVED), "{out}{err}");
    let (ok, _, _) = karac(&["__mir-run"], &file);
    assert!(!ok, "run refuses it too");
}

#[test]
fn an_owned_dyn_used_once_after_the_move_is_accepted() {
    let file = cell(&SHAPE.replace("USE", ""));
    let (ok, out, err) = karac(&["check"], &file);
    assert!(ok, "{out}{err}");
    let (ok, out, err) = karac(&["__mir-run"], &file);
    assert!(ok, "{err}");
    assert_eq!(out, "4\n");
}
