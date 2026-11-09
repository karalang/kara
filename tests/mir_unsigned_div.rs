//! A division or remainder whose dividend the MIR ranges show is never
//! negative and whose divisor is at least 1 is emitted unsigned, which a
//! power of two makes a shift or a mask (kata 540's `(low + high) / 2` and
//! `mid % 2` took a signed division's sign fix-up each,
//! B-2026-11-08-72). One whose operands may be negative keeps the signed
//! values: `-7 / 2` is -3 and `-7 % 2` is -1, here and in the interpreter.

#[cfg(feature = "llvm")]
mod common;

const SRC: &str = r#"
fn single(nums: Vec[i64]) -> i64 {
    let mut low = 0;
    let mut high = nums.len() - 1;
    while low < high {
        let mut mid = (low + high) / 2;
        if mid % 2 == 1 {
            mid -= 1;
        }
        if nums[mid] == nums[mid + 1] {
            low = mid + 2;
        } else {
            high = mid;
        }
    }
    nums[low]
}

fn halves(x: i64, d: i64) -> String {
    f"{x / 2} {x % 2} {x / d} {x % d}"
}

fn counted(n: i64) -> i64 {
    let mut s = 0;
    for i in 0..n {
        s += i / 3 + i % 4;
    }
    s
}

fn main() {
    let nums: Vec[i64] = [1, 1, 2, 3, 3, 4, 4, 8, 8];
    println(f"{single(nums)}");
    println(halves(-7, 2));
    println(halves(7, -2));
    println(halves(-8, -3));
    println(f"{counted(10)} {counted(-5)}");
}
"#;

#[test]
fn mir_divisions_keep_their_signed_values() {
    let r = karac::mir::lower::run_source(SRC).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        r.output, "2\n-3 -1 -3 -1\n3 1 -3 1\n-4 0 2 -2\n25 0\n",
        "stderr: {}",
        r.stderr
    );
}

#[cfg(feature = "llvm")]
#[test]
fn mir_llvm_divisions_the_ranges_bound_are_unsigned() {
    use std::process::Command;
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    // The binary search's `/ 2` and `% 2`, and the counted loop's `/ 3`
    // and `% 4`, are unsigned; `halves`, whose operands may be negative,
    // stays signed.
    let unsigned = |f: &str| {
        let start = ir
            .lines()
            .find(|l| l.starts_with("define") && l.contains(&format!("@kara.{f}(")))
            .and_then(|l| ir.find(l))
            .unwrap_or_else(|| panic!("no `{f}` in:\n{ir}"));
        let end = ir[start..].find("\n}\n").map_or(ir.len(), |e| start + e);
        ir[start..end].matches(" udiv ").count() + ir[start..end].matches(" urem ").count()
    };
    assert_eq!(unsigned("single"), 2, "{ir}");
    assert_eq!(unsigned("counted"), 2, "{ir}");
    assert_eq!(unsigned("halves"), 0, "{ir}");
    let dir = std::env::temp_dir().join(format!("karac-unsigned-div-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    if linked.is_some() {
        let out = Command::new(&exe).output().expect("run");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "2\n-3 -1 -3 -1\n3 1 -3 1\n-4 0 2 -2\n25 0\n"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
