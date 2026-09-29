//! B-2026-09-29-78: i128/u128 arithmetic beside a suffixless literal computed at i64.

use super::*;

/// B-2026-09-29-78 — an `i128`/`u128` operand beside a suffixless literal was
/// computed at i64: the literal lowers at the default i64 width and the binop's
/// width harmonization truncated the WIDE side to it. `b + 1` over
/// `b: i128 = 10^20` printed 7766279631452241921, `b * 2` trapped a
/// spurious overflow, and `b.wrapping_add(1)` failed module verification.
#[test]
fn e2e_i128_arith_with_an_unsuffixed_literal_runs_at_128_bits() {
    let Some(out) = run_program(
        r#"fn id(x: i128) -> i128 { x }
fn five() -> i128 { 5 }

fn main() {
    println("start")
    let b: i128 = 100000000000000000000i128;
    let u: u128 = 200000000000000000000u128;
    println("add " + (b + 1).to_string())
    println("radd " + (1 + b).to_string())
    println("sub " + (b - 1).to_string())
    println("mul " + (b * 2).to_string())
    println("div " + (b / 3).to_string())
    println("mod " + (b % 7).to_string())
    println("and " + (b & 255).to_string())
    println("or " + (b | 1).to_string())
    println("xor " + (b ^ 1).to_string())
    println("shr " + (b >> 1).to_string())
    println("shl " + (b << 1).to_string())
    println("gt " + (b > 5).to_string())
    println("lt " + (b < 5).to_string())
    println("eq " + (b == 5).to_string())
    println("ne " + (b != 5).to_string())
    let mut v = b;
    v += 1;
    println("pluseq " + v.to_string())
    v -= 2;
    println("minuseq " + v.to_string())
    v *= 2;
    println("muleq " + v.to_string())
    println("wadd " + b.wrapping_add(1).to_string())
    println("wmul " + b.wrapping_mul(10).to_string())
    println("wsub " + b.wrapping_sub(1).to_string())
    println("sadd " + b.saturating_add(1).to_string())
    println("smul " + b.saturating_mul(2).to_string())
    match b.checked_add(1) {
        Some(x) => println("cadd " + x.to_string()),
        None => println("cadd none"),
    }
    println("min " + b.min(5).to_string())
    println("max " + b.max(5).to_string())
    println("pow " + b.pow(1).to_string())
    println("id " + id(b + 1).to_string())
    println("five " + (five() + b).to_string())
    let neg: i128 = -b;
    println("neg " + (neg - 1).to_string())
    println("uadd " + (u + 1).to_string())
    println("usub " + (u - 1).to_string())
    println("ugt " + (u > 1).to_string())
    println("udiv " + (u / 3).to_string())
    let mut i: i128 = 0;
    let mut n = 0;
    while i < b {
        i = i + 2000000000000000000;
        n += 1;
    }
    println("loop " + n.to_string())
    let arr = [b, b + 1];
    println("arr " + arr[1].to_string())
    let t = (b + 2, 1);
    println("tup " + t.0.to_string())
    let c = if b > 0 { b - 3 } else { 0 };
    println("if " + c.to_string())
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"start
add 100000000000000000001
radd 100000000000000000001
sub 99999999999999999999
mul 200000000000000000000
div 33333333333333333333
mod 2
and 0
or 100000000000000000001
xor 100000000000000000001
shr 50000000000000000000
shl 200000000000000000000
gt true
lt false
eq false
ne true
pluseq 100000000000000000001
minuseq 99999999999999999999
muleq 199999999999999999998
wadd 100000000000000000001
wmul 1000000000000000000000
wsub 99999999999999999999
sadd 100000000000000000001
smul 200000000000000000000
cadd 100000000000000000001
min 5
max 100000000000000000000
pow 100000000000000000000
id 100000000000000000001
five 100000000000000000005
neg -100000000000000000001
uadd 200000000000000000001
usub 199999999999999999999
ugt true
udiv 66666666666666666666
loop 50
arr 100000000000000000001
tup 100000000000000000002
if 99999999999999999997
"#,
        "b_2026_09_29_78_p"
    );
}

/// B-2026-09-29-78 — the `if` / `match` merge truncated an `i128` branch to
/// its suffixless-literal sibling's i64 (`if c { b - 3 } else { 0 }`).
#[test]
fn e2e_i128_branch_and_arm_merge_beside_a_literal_keeps_128_bits() {
    let Some(out) = run_program(
        r#"fn pick(b: i128, k: i64) -> i128 {
    match k {
        0 => 0,
        1 => b + 1,
        _ => -1,
    }
}

fn pk(b: i128, c: bool) -> i128 {
    if c { 1 } else { b }
}

fn ret_lit() -> i128 {
    7
}

fn main() {
    println("start")
    let b: i128 = 100000000000000000000i128;
    println(pick(b, 0).to_string())
    println(pick(b, 1).to_string())
    println(pick(b, 2).to_string())
    println(pk(b, true).to_string())
    println(pk(b, false).to_string())
    let c = if b > 0 { b - 3 } else { 0 };
    println(c.to_string())
    let d = if b < 0 { 0 } else { b - 3 };
    println(d.to_string())
    let u: u128 = 340282366920938463463374607431768211455u128;
    let e = if u > 0 { u } else { 0 };
    println(e.to_string())
    let f: u128 = match 3 { 1 => 5, _ => u - 1 };
    println(f.to_string())
    println((ret_lit() + b).to_string())
    let o: Option[i128] = if b > 0 { Some(b) } else { None };
    match o { Some(z) => println(z.to_string()), None => println("n") }
    let w: i128 = b.wrapping_add(-1i128);
    println(w.to_string())
    let x: i128 = (-b).wrapping_mul(3);
    println(x.to_string())
    let y: u128 = u.wrapping_add(1);
    println(y.to_string())
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"start
0
100000000000000000001
-1
1
100000000000000000000
99999999999999999997
99999999999999999997
340282366920938463463374607431768211455
340282366920938463463374607431768211454
100000000000000000007
100000000000000000000
99999999999999999999
-300000000000000000000
0
"#,
        "b_2026_09_29_78_r"
    );
}

/// B-2026-09-29-78 — a literal pattern on an `i128` scrutinee compared the low
/// words (`2^64` matched the `0` arm), and a loop accumulator `acc * 2` trapped
/// at i64.
#[test]
fn e2e_i128_literal_patterns_and_mixed_ops_keep_128_bits() {
    let Some(out) = run_program(
        r#"fn cls(x: i128) -> String {
    match x {
        0 => "zero".to_string(),
        1..=9 => "small".to_string(),
        100000000000000000000i128 => "big".to_string(),
        _ => "other".to_string(),
    }
}

fn clu(x: u128) -> String {
    match x {
        0 => "zero".to_string(),
        340282366920938463463374607431768211455u128 => "max".to_string(),
        _ => "other".to_string(),
    }
}

fn main() {
    println("start")
    let b: i128 = 100000000000000000000i128;
    println(cls(b))
    println(cls(b + 1))
    println(cls(18446744073709551616i128))
    println(cls(3))
    println(clu(340282366920938463463374607431768211455u128))
    println(clu(18446744073709551615u128))
    let s = b.abs() + 1;
    println(s.to_string())
    let mut acc: i128 = 1;
    for i in 0..70 {
        acc = acc * 2;
    }
    println(acc.to_string())
    let x: i128 = if b != 0 { 7 } else { 8 };
    println((x + b).to_string())
    let v: Vec[i128] = vec![b, 1, 2];
    let mut sum: i128 = 0;
    for e in v { sum = sum + e + 1; }
    println(sum.to_string())
    let o: Option[i128] = Some(b + 5);
    match o { Some(z) => println(z.to_string()), None => println("n") }
    let h = (b >> 64) + 1;
    println(h.to_string())
    println((b / 1000000007).to_string())
    println((-b % 1000000007).to_string())
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"start
big
other
other
small
max
other
100000000000000000001
1180591620717411303424
100000000000000000007
100000000000000000006
100000000000000000005
6
99999999300
-4900
"#,
        "b_2026_09_29_78_q"
    );
}
