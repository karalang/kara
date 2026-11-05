//! B-2026-11-04-39: `for i in a..=b` leaves through its header's `i <= b`,
//! and its latch stops only at the counter type's largest value, where the
//! step would overflow. The latch used to test `i == b`, which LLVM folded
//! into the loop's one exit; with `i <= b` gone it could not see that
//! `ans[i]` stays below a `b + 1` long vector, and kata 338's bounds check
//! stayed in the loop. Each program prints what the spec says, the loops
//! that end at the type's largest value included.

use karac::mir::interp::Outcome;

fn run(src: &str) -> Result<(String, i32), String> {
    let r = karac::mir::lower::run_source(src)?;
    match (&r.outcome, r.exit_code()) {
        (_, Some(code)) => Ok((r.output, code)),
        (Outcome::Error(e), None) => Err(format!("run: {e}\n--- output so far\n{}", r.output)),
        _ => Err("no exit code".into()),
    }
}

fn built(src: &str, name: &str) -> String {
    let l = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::pretty::pretty_body(&l.program.bodies[name], &l.tys)
}

const COUNT_BITS: &str = r#"
fn count_bits(n: i64) -> Vec[i64] {
    let mut ans: Vec[i64] = vec![0; n + 1];
    for i in 1..=n {
        ans[i] = ans[i >> 1] + (i & 1);
    }
    return ans;
}

fn main() {
    println(f"{count_bits(5)}");
    println(f"{count_bits(0)}");
}
"#;

#[test]
fn inclusive_range_latch_tests_the_type_max_not_the_bound() {
    assert_eq!(
        run(COUNT_BITS).unwrap(),
        ("[0, 1, 1, 2, 1, 2]\n[0]\n".into(), 0)
    );
    let body = built(COUNT_BITS, "count_bits");
    assert!(body.contains("const 9223372036854775807"), "{body}");
    assert!(body.contains("Le("), "{body}");
}

#[test]
fn inclusive_range_runs_to_each_type_max() {
    let src = r#"
fn main() {
    let mut c = 0;
    for x in 250u8..=255u8 {
        c += 1;
        if x == 255u8 {
            println(f"u8 {c}");
        }
    }
    c = 0;
    for x in 120i8..=127i8 {
        c += 1;
        if x == 127i8 {
            println(f"i8 {c}");
        }
    }
    let top: i64 = 9223372036854775807;
    c = 0;
    for x in top - 2..=top {
        c += 1;
        if x == top {
            println(f"i64 {c}");
        }
    }
    let utop: u64 = 18446744073709551615;
    c = 0;
    for _ in utop - 3..=utop {
        c += 1;
    }
    println(f"u64 {c}");
    c = 0;
    for _ in 5..=4 {
        c += 1;
    }
    for _ in 4..=4 {
        c += 10;
    }
    println(f"short {c}");
    let mut v: Vec[i64] = Vec.new();
    for i in -2..=2 {
        if i == 1 {
            continue;
        }
        v.push(i);
    }
    println(f"{v}");
}
"#;
    assert_eq!(
        run(src).unwrap(),
        (
            "u8 6\ni8 8\ni64 3\nu64 4\nshort 10\n[-2, -1, 0, 2]\n".into(),
            0
        )
    );
}
