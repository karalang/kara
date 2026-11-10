//! A `Vec` of literals written through `v.as_slice_mut()` or `mut v[a..b]`
//! is not a constant (B-2026-11-10-2). Both were lowered on a shared
//! borrow, so the literal-table pass, which takes a `Vec` of literals that
//! is only ever borrowed shared for a constant, moved `[9, 8, 3, 1, 2]` into
//! a static: a write through the slice went to a copy, and a sort through
//! it wrote to read-only memory. `as_slice_mut` now borrows mutably; the
//! range view still borrows shared (B-2026-11-10-4), and the pass refuses
//! a borrow made a `mut Slice`. The sequential build always ran that pass;
//! a plain build ran it once kata 1088's fix (B-2026-11-10-1) lowered a
//! build that parallelizes nothing in order.

use std::path::PathBuf;
use std::process::Command;

const WRITTEN: &str = "fn sort_in_place(xs: mut Slice[i64]) {
    xs.sort();
}

fn main() {
    let mut b = [9, 8, 3, 1, 2];
    let mut bs = b.as_slice_mut();
    bs[0] = 7;
    println(f\"{b}\");
    let mut c = [9, 8, 3, 1, 2];
    let mut cs = c.as_slice_mut();
    let h = cs.split_at_mut(2);
    sort_in_place(mut h.1);
    println(f\"{c}\");
    let mut d = [9, 8, 3, 1, 2];
    sort_in_place(mut d[1..4]);
    println(f\"{d}\");
}
";

const PRINTED: &str = "[7, 8, 3, 1, 2]\n[9, 8, 1, 2, 3]\n[9, 1, 3, 8, 2]\n";

#[test]
fn as_slice_mut_borrows_its_vec_to_write() {
    let lowered = karac::mir::lower::build_source(WRITTEN).expect("MIR");
    let main = karac::mir::pretty::pretty_body(&lowered.program.bodies["main"], &lowered.tys);
    assert!(
        !main.contains("as_slice_mut(")
            && main.contains("as_mut_slice(")
            && main.contains("&mut _1"),
        "`b.as_slice_mut()` must take `&mut b`:\n{main}"
    );
}

fn fixture(src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("kata-1088-as-slice-mut-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

#[test]
fn a_vec_written_through_as_slice_mut_keeps_the_writes() {
    let (dir, path) = fixture(WRITTEN);
    let runners: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for runner in runners {
        for par in ["1", "0"] {
            let out = Command::new(env!("CARGO_BIN_EXE_karac"))
                .arg(runner)
                .arg(&path)
                .env("KARAC_AUTO_PAR", par)
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                PRINTED,
                "{runner}, KARAC_AUTO_PAR={par}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
