//! The compiled-code fixes from the x86 Callgrind sweep of the katas
//! pushed since 11-08 (kata 2063, 2042, 2194, 2208, 2099), each pinned by
//! its answers, which are the same on the interpreter, the built binary
//! and ASan, and by the shape of the built IR:
//! - `f64.total_cmp` / `f32.total_cmp` compare in line, NaN and -0.0
//!   included (B-2026-11-16-8);
//! - `s.contains(t)` with a one-byte needle and a short haystack is a byte
//!   scan, and a hoisted `String` literal static is a constant global
//!   (B-2026-11-16-9);
//! - `i64.parse` of a word asks the runtime only when its first byte
//!   could start a number, and gives the same errors (B-2026-11-16-10);
//! - `String.from_utf8` of a fresh `Vec` adopts its buffer when every byte
//!   is ASCII, and copies otherwise (B-2026-11-16-11);
//! - `x.cmp(y)` on integers picks its variant with selects, not a switch,
//!   so a sort comparator with a `then` tie break does not branch
//!   (B-2026-11-16-12).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const PROG: &str = r#"import std.cmp.{F64}; enum Dir { North, East, South, West }

fn name(o: Ordering) -> String {
    (match o { Ordering.Less => "lt", Ordering.Equal => "eq", Ordering.Greater => "gt" }).to_string()
}

fn turn(n: i64) -> Dir {
    match n % 4 { 0 => Dir.North, 1 => Dir.East, 2 => Dir.South, _ => Dir.West }
}

fn dname(d: Dir) -> String {
    (match d { Dir.North => "N", Dir.East => "E", Dir.South => "S", Dir.West => "W" }).to_string()
}

fn main() {
    let nan = 0.0 / 0.0;
    let fs = [-1.0, -0.0, 0.0, 2.5, nan, -nan];
    let mut line = "".to_string();
    for a in fs.iter() {
        for b in fs.iter() {
            line.push_str(name(a.total_cmp(b)).as_str());
            line.push(' ');
        }
    }
    println(line);
    let x: f32 = -0.0;
    let y: f32 = 0.0;
    let z: f32 = 1.0e30;
    println(f"{name(x.total_cmp(y))} {name(y.total_cmp(x))} {name(z.total_cmp(y))} {name(x.total_cmp(x))}");
    let mut v: Vec[F64] = Vec.new();
    for f in [3.0, -0.0, nan, 0.0, -2.0] {
        v.push(F64.from(f));
    }
    v.sort();
    println(f"{v.len()} {v[0].value} {v[1].value} {v[2].value} {v[3].value}");
    let mut hits = 0;
    for c in "a quick brown fox jumps over the lazy dog".chars() {
        if "aeiou".contains(c.to_string()) {
            hits += 1;
        }
    }
    let long = "abcdefghijklmnopqrstuvwxyz0123456789";
    println(f"{hits} {long.contains("z")} {long.contains("!")} {"".contains("a")} {"ab".contains("")}");
    for s in ["abc", "  12", "+3", "\t5", "é", "-", "7x", "", "99"] {
        match i64.parse(s) {
            Ok(n) => println(f"ok {n}"),
            Err(e) => println(f"err {e}"),
        }
    }
    let ascii: Vec[u8] = [104u8, 105u8].to_vec();
    let wide: Vec[u8] = [99u8, 97u8, 102u8, 195u8, 169u8].to_vec();
    let bad: Vec[u8] = [97u8, 255u8].to_vec();
    println(f"{String.from_utf8(ascii).unwrap()} {String.from_utf8(wide).unwrap()} {String.from_utf8(bad).is_err()}");
    println(f"{String.from_utf8([65u8, 66u8].to_vec()).unwrap().len()}");
    let nums = [5, 3, 5, 1, 3, 5];
    let mut order: Vec[i64] = (0..6).collect();
    order.sort_by(|a, b| nums[b].cmp(nums[a]).then(a.cmp(b)));
    println(f"{order}");
    let os = [Ordering.Less, Ordering.Equal, Ordering.Greater];
    let mut t = "".to_string();
    for a in os.iter() {
        for b in os.iter() {
            t.push_str(name(a.then(*b)).as_str());
            t.push(' ');
        }
    }
    println(t);
    let mut ds = "".to_string();
    for i in 0..9 {
        ds.push_str(dname(turn(i)).as_str());
    }
    println(f"{ds} {name('a'.cmp('b'))} {name(true.cmp(false))} {name(7u8.cmp(7u8))}");
}
"#;

const WANT: &str = "eq lt lt lt lt lt gt eq lt lt lt lt gt gt eq lt lt lt gt gt gt eq lt lt gt gt gt gt eq eq gt gt gt gt eq eq \n\
lt gt gt eq\n\
5 -2.0 -0.0 0.0 3.0\n\
11 true false false true\n\
err invalid digit found in string\n\
ok 12\n\
ok 3\n\
ok 5\n\
err invalid digit found in string\n\
err invalid digit found in string\n\
err invalid digit found in string\n\
err cannot parse a number from an empty string\n\
ok 99\n\
hi café true\n\
2\n\
[0, 2, 5, 1, 4, 3]\n\
lt lt lt lt eq gt gt gt gt \n\
NESWNESWN lt gt eq\n";

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

#[test]
fn mir_kata_sweep_fixes_answer_alike_and_inline() {
    let d = std::env::temp_dir().join(format!("karac-kata-sweep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let file = d.join("main.kara");
    std::fs::write(&file, PROG).unwrap();
    let file = file.to_str().unwrap();
    let interp = karac().args(["__mir-run", file]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        WANT,
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = karac()
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })
    .is_none()
    {
        return;
    }
    let ran = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&ran.stdout), WANT);
    let ir = std::fs::read_to_string(&ll).unwrap();
    for label in ["contains.scan:", "dec.first:", "utf8.head:", "utf8.copy:"] {
        assert!(
            ir.lines()
                .any(|l| l.split_whitespace().next() == Some(label)),
            "no {label} in the IR"
        );
    }
    assert!(
        !ir.contains("karac_mir_total_cmp"),
        "total_cmp still calls the runtime"
    );
    assert!(
        ir.lines()
            .any(|l| l.starts_with("@kara.static.") && l.contains("= constant { ptr, i64, i64 }")),
        "the hoisted literal is not a constant global"
    );
    let cmp: Vec<&str> = ir
        .lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains("@kara.main.c0(")))
        .take_while(|l| *l != "}")
        .collect();
    assert!(!cmp.is_empty(), "no comparator in the IR");
    let cmp = cmp.join("\n");
    assert!(!cmp.contains("switch "), "the comparator switches:\n{cmp}");
    assert!(
        cmp.contains("select i1"),
        "the comparator has no select:\n{cmp}"
    );
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_none() {
        let asan = karac().args(["__mir-llvm-asan", file]).output().unwrap();
        let err = String::from_utf8_lossy(&asan.stderr);
        // A stale archive or KARAC_REQUIRE_RUNTIME_ARCHIVE=1 fails here (B-2026-11-21-10).
        if !(asan.status.code() == Some(3)
            && err.contains("Linker failed")
            && common::link_or_skip(Err(err.to_string())).is_none())
        {
            assert!(asan.status.success(), "{err}");
            assert_eq!(String::from_utf8_lossy(&asan.stdout), WANT);
        }
    }
    let _ = std::fs::remove_dir_all(&d);
}
