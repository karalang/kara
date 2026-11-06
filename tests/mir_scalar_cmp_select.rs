//! An integer `cmp` computes its `Ordering` without a branch: both
//! compares, `(a > b) - (a < b)` as an `i8`, then one switch on it
//! (B-2026-11-06-1). Lowered as a branch per compare, a two-key `sort_by`
//! comparator kept a branch per element in the sort's partition loop that
//! LLVM could not turn into a select, and kata 354 ran 1.3x its time on
//! x86. The pins: every ordering over the extremes of `i64`, `u64`, `i8`,
//! `i128`, `u128`, `char` and `bool` prints the same under both backends,
//! and `order`'s IR has both compares ahead of its one switch.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

const SRC: &str = r#"fn sign(o: Ordering) -> i64 {
    match o {
        Ordering.Less => -1,
        Ordering.Equal => 0,
        Ordering.Greater => 1,
    }
}
fn order(a: i64, b: i64) -> Ordering {
    a.cmp(b)
}
fn main() {
    let xs = [i64.MIN, -3, 0, 3, i64.MAX];
    let mut line = "";
    for a in xs {
        for b in xs {
            line = line + f"{sign(order(a, b))} ";
        }
    }
    println(line);
    let us: Array[u64, 3] = [0, 9, u64.MAX];
    let mut u = "";
    for a in us {
        for b in us {
            u = u + f"{sign(a.cmp(b))} ";
        }
    }
    println(u);
    let cs = ['a', 'z'];
    let bs = [false, true];
    let small: Array[i8, 2] = [-128, 127];
    let mut c = "";
    for a in cs {
        for b in cs {
            c = c + f"{sign(a.cmp(b))} ";
        }
    }
    for a in bs {
        for b in bs {
            c = c + f"{sign(a.cmp(b))} ";
        }
    }
    for a in small {
        for b in small {
            c = c + f"{sign(a.cmp(b))} ";
        }
    }
    println(c);
    let wide: Array[i128, 3] = [i128.MIN, 0, i128.MAX];
    let uwide: Array[u128, 3] = [0, 5, u128.MAX];
    let mut w = "";
    for a in wide {
        for b in wide {
            w = w + f"{sign(a.cmp(b))} ";
        }
    }
    for a in uwide {
        for b in uwide {
            w = w + f"{sign(a.cmp(b))} ";
        }
    }
    println(w);
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut i = 0;
    while i < 20 {
        v.push(((i * 7) % 4, (i * 5) % 6));
        i += 1;
    }
    v.sort_by(|a, b| if a.0 != b.0 { a.0.cmp(b.0) } else { b.1.cmp(a.1) });
    let mut s = "";
    for p in v {
        s = s + f"{p.0}{p.1} ";
    }
    println(s);
}
"#;

const EXPECTED: &str = "0 -1 -1 -1 -1 1 0 -1 -1 -1 1 1 0 -1 -1 1 1 1 0 -1 1 1 1 1 0 \n\
                        0 -1 -1 1 0 -1 1 1 0 \n\
                        0 -1 1 0 0 -1 1 0 0 -1 1 0 \n\
                        0 -1 -1 1 0 -1 1 1 0 0 -1 -1 1 0 -1 1 1 0 \n\
                        04 02 02 00 00 15 15 13 13 11 24 24 22 20 20 35 35 33 31 31 \n";

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-scalar-cmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

#[test]
fn an_integer_cmp_compares_both_ways_before_one_switch() {
    let d = dir();
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let interp = karac()
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        out(&interp),
        EXPECTED,
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = karac()
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    let ir = std::fs::read_to_string(&ll).unwrap_or_else(|_| panic!("no IR\n{why}"));
    let body: Vec<&str> = ir
        .lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains("@kara.order(")))
        .take_while(|l| *l != "}")
        .collect();
    assert!(!body.is_empty(), "no `order` in the IR\n{ir}");
    let text = body.join("\n");
    let at = |s: &str| body.iter().position(|l| l.contains(s));
    let switch = at("switch ").unwrap_or_else(|| panic!("no switch\n{text}"));
    for c in ["icmp sgt", "icmp slt"] {
        let ahead = at(c).is_some_and(|i| i < switch);
        assert!(ahead, "{c} is not ahead of the switch\n{text}");
    }
    let switches = body.iter().filter(|l| l.contains("switch ")).count();
    assert_eq!(switches, 1, "{text}");
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
    assert_eq!(out(&ran), EXPECTED);
    let _ = std::fs::remove_dir_all(&d);
}
