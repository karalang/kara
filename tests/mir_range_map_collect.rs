//! `(a..b).map(|i| ..).collect()` into a `Vec` (B-2026-11-15-16) lowers to
//! the counting loop it stands for: the closure literal is called on each
//! value of the range and its result pushed, so neither the library's
//! `Range.map` nor `IterMap.collect` runs. Through the library each item
//! cost about 28 instructions in a built binary, against 4.4 for the push
//! loop, because the closure went through an erased `Fn` with a heap
//! environment that LLVM reloaded every item. The pins: the range's edges
//! (empty, `..=` ending at the type's largest value, a reversed `..=`),
//! a closure that writes a captured variable, owned elements, a capture
//! with a `Drop` that runs once at its scope's end, and a nested
//! map/collect, on the interpreter, the built binary and ASan; and the
//! built IR naming no `map` or `collect` body.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const EDGES: &str = r#"struct Noisy { n: i64 }
impl Drop for Noisy { fn drop(mut ref self) { println(f"drop {self.n}"); } }

fn grid(seed: mut ref i64) -> Vec[Vec[i64]] {
    (0..3).map(|_| (0..4).map(|j| { *seed = *seed * 7 % 11; *seed + j }).collect()).collect()
}

fn main() {
    let e: Vec[i64] = (5..5).map(|i| i * 2).collect();
    let r: Vec[i64] = (3..=1).map(|i| i).collect();
    println(f"{e.len()} {r.len()}");
    let a: Vec[u8] = (250u8..=255u8).map(|i| i).collect();
    println(f"{a.len()} {a[0]} {a[5]}");
    let big: Vec[i64] = (9223372036854775805..=9223372036854775807).map(|i| i % 10).collect();
    println(f"{big}");
    let mut c = 0;
    let b: Vec[i64] = (0..4).map(|i| { c += i; c }).collect();
    println(f"{b} {c}");
    let s = "ab".to_string();
    let d = Noisy { n: 1 };
    let v: Vec[i64] = (0..3).map(|i| s.len() + i + d.n).collect();
    println(f"{v} {s}");
    let w: Vec[String] = (1..=3).map(|i| f"x{i}").collect();
    println(f"{w}");
    let mut seed = 3;
    let g = grid(mut seed);
    println(f"{g} {seed}");
}
"#;

const WANT: &str = "0 0\n6 250 255\n[5, 6, 7]\n[0, 1, 3, 6] 6\n[3, 4, 5] ab\n\
[x1, x2, x3]\n[[10, 5, 8, 12], [8, 2, 9, 8], [2, 4, 12, 7]] 4\ndrop 1\n";

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

#[test]
fn mir_range_map_collect_runs_the_closure_in_a_counting_loop() {
    let d = std::env::temp_dir().join(format!("karac-range-map-collect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let file = d.join("main.kara");
    std::fs::write(&file, EDGES).unwrap();
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
    for name in ["].map[", "].collect["] {
        assert!(
            !ir.lines()
                .any(|l| l.starts_with("define") && l.contains(name)),
            "the library's {name} is still emitted"
        );
    }
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

/// The same into a `String` when the closure answers a `char`
/// (B-2026-11-16-7, kata 1573's `(0..n).map(|_| .. '1' ..).collect()`):
/// each char is pushed, a wide one as its UTF-8 bytes.
const CHARS: &str = r#"fn main() {
    let a: String = (0..5).map(|i| if i % 2 == 0 { 'a' } else { 'é' }).collect();
    println(f"{a} {a.len()}");
    let e: String = (3..3).map(|_| 'x').collect();
    println(f"[{e}]");
    let mut k = 0;
    let b: String = (1..=4).map(|i| { k += i; if k > 5 { '😀' } else { 'z' } }).collect();
    println(f"{b} {k}");
}
"#;

const CHARS_WANT: &str = "aéaéa 7\n[]\nzz😀😀 10\n";

#[test]
fn mir_range_map_collect_into_a_string_pushes_each_char() {
    let d = std::env::temp_dir().join(format!("karac-range-map-chars-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let file = d.join("main.kara");
    std::fs::write(&file, CHARS).unwrap();
    let file = file.to_str().unwrap();
    let interp = karac().args(["__mir-run", file]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        CHARS_WANT,
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
    assert_eq!(String::from_utf8_lossy(&ran.stdout), CHARS_WANT);
    let ir = std::fs::read_to_string(&ll).unwrap();
    for name in ["].map[", "].collect["] {
        assert!(
            !ir.lines()
                .any(|l| l.starts_with("define") && l.contains(name)),
            "the library's {name} is still emitted"
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}
