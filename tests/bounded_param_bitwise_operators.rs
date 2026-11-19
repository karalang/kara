//! B-2026-11-19-125: `& | ^ << >>` and unary `~` on a type parameter
//! bounded by the operator's trait (`T: BitAnd[Output = T]`, `T: Shl[i64,
//! Output = T]`, `T: Not[Output = T]`) were refused "bitwise operator
//! requires integer type, found 'T'": the bitwise path checked for a
//! primitive integer before the bound, though `+ - * / %` on `T: Add` and the
//! method form `a.shl(n)` worked. They now call the bound's impl, as the
//! arithmetic operators do (design.md § Operator traits).
//!
//! B-2026-11-19-129: a `ref T` operand of a `T: Copy` parameter is read as
//! the `T` (core semantics §5.10), so `for x in v.iter() { s = s + x; }`
//! checks on a generic `T: Add[Output = T] + Copy` as it does on `i64`.

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
    let dir = std::env::temp_dir().join(format!("karac-bounded-param-ops-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

fn runs(name: &str, src: &str, want: &str) {
    let path = scratch(name, src);
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{}", text(&run));
    if cfg!(feature = "llvm") {
        let exe = path.with_extension("bin");
        let build = karac(&["build", "-o", exe.to_str().unwrap()], &path);
        assert!(build.status.success(), "{}", text(&build));
        let ran = Command::new(&exe).output().unwrap();
        assert!(ran.status.success(), "{}", text(&ran));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), want, "{}", text(&ran));
    }
}

fn refused(name: &str, src: &str, want: &str) {
    let path = scratch(name, src);
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(text(&check).contains(want), "{}", text(&check));
}

#[test]
fn bitwise_and_shift_operators_call_a_bounded_params_impl() {
    runs(
        "bitwise.kara",
        "fn sh[T: Shl[i64, Output = T]](a: own T, n: i64) -> T { a << n }
fn sr[T: Shr[i64, Output = T]](a: own T, n: i64) -> T { a >> n }
fn band[T: BitAnd[Output = T]](a: own T, b: own T) -> T { a & b }
fn bor[T: BitOr[Output = T]](a: own T, b: own T) -> T { a | b }
fn bxor[T: BitXor[Output = T]](a: own T, b: own T) -> T { a ^ b }
fn bnot[T: Not[Output = T]](a: own T) -> T { ~a }
fn main() {
    let a: i32 = 1;
    println(sh(a, 4));
    println(sr(256, 4));
    println(band(12, 10));
    println(bor(12, 10));
    println(bxor(12, 10));
    let u: u8 = 5;
    println(bnot(u));
}
",
        "16\n16\n8\n14\n6\n250\n",
    );
}

#[test]
fn a_user_bitand_impl_is_called_through_a_generic_bound() {
    runs(
        "user_bitand.kara",
        "struct M { v: i64 }
impl BitAnd for M {
    type Output = M;
    fn bitand(self, rhs: M) -> M { M { v: self.v & rhs.v } }
}
fn band[T: BitAnd[Output = T]](a: own T, b: own T) -> T { a & b }
fn main() {
    let r = band(M { v: 12 }, M { v: 10 });
    println(r.v);
}
",
        "8\n",
    );
}

#[test]
fn a_copy_bounded_ref_operand_reads_as_its_value() {
    runs("copy_ref.kara", "fn sum[T: Add[Output = T] + Copy](v: ref Vec[T], z: T) -> T { let mut s = z; for x in v.iter() { s = s + x; } s }
fn ors[T: BitOr[Output = T] + Copy](v: ref Vec[T], z: T) -> T { let mut s = z; for x in v.iter() { s = s | x; } s }
fn main() {
    println(sum([1, 2, 3], 0));
    let f: Vec[f64] = [1.5, 2.0];
    println(sum(f, 0.5));
    println(ors([1, 2, 4], 8));
}
", "6\n4.0\n15\n");
}

#[test]
fn a_ref_operand_without_copy_is_still_refused() {
    refused(
        "no_copy.kara",
        "fn sum[T: Add[Output = T]](v: ref Vec[T], z: own T) -> T { let mut s = z; for x in v.iter() { s = s + x; } s }
fn main() { println(sum([1, 2, 3], 0)); }
",
        "found 'T' and 'ref T'",
    );
}

#[test]
fn a_bitwise_operator_without_its_bound_names_the_bound() {
    refused(
        "wrong_bound.kara",
        "fn band[T: BitOr[Output = T]](a: own T, b: own T) -> T { a & b }
fn main() { println(band(1, 2)); }
",
        "a type parameter has it through the bound `BitAnd`",
    );
}
