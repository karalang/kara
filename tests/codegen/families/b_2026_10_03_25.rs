//! B-2026-10-03-25 -- a bare `Vec.sort()` over an element type with no
//! integer fast path inlines its natural-order comparator into the
//! monomorphized merge sort instead of calling it through
//! `karac_vec_sort_by`'s function pointer.

use super::*;

/// B-2026-10-03-25 — every element shape that reaches the `karac_cmp_<T>`
/// family (integer tuples at sizes that take each merge-sort path, `String`
/// tuples, `F64` with a NaN and both zeros, nested `Vec`, a derived-`Ord`
/// struct with a `u8` field, and an enum) sorts to the interpreter's order
/// through the monomorphized path. The expected text is `--interp`'s output.
#[test]
fn e2e_bare_sort_family_elements_match_interp() {
    let out = run_program(
        r#"#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    hi: u8,
    lo: i64,
    name: String,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Shape {
    Dot,
    Line(i64),
    Box(u32, i32),
}

fn next(seed: mut ref i64) -> i64 {
    seed = (seed * 1103515245 + 12345) % 2147483648;
    return seed / 65536;
}

fn hash_ints(v: ref Vec[(i64, i64)]) -> i64 {
    let mut h: i64 = 17;
    for p in v {
        h = (h * 31 + p.0 + 1000000) % 1000000007;
        h = (h * 31 + p.1 + 1000000) % 1000000007;
    }
    return h;
}

fn main() {
    let mut st: i64 = 7;
    for card in [3, 100000] {
        for n in [0, 1, 33, 1000, 6000] {
            let mut v: Vec[(i64, i64)] = Vec.new();
            for _ in 0..n {
                let a = next(mut st) % card - card / 2;
                let b = next(mut st) % 7 - 3;
                v.push((a, b));
            }
            v.sort();
            let mut ok = true;
            for i in 1..v.len() {
                if v[i - 1] > v[i] {
                    ok = false;
                }
            }
            println(f"ii card={card} n={n} sorted={ok} h={hash_ints(v)}");
        }
    }
    let mut asc: Vec[(i64, i64)] = Vec.new();
    for i in 0..10000 {
        asc.push((i / 3, 0 - i));
    }
    let mut desc = asc.clone();
    desc.reverse();
    asc.sort();
    desc.sort();
    println(f"asc==desc {asc == desc} h={hash_ints(asc)}");
    let zero = 0.0;
    let raw: Vec[f64] = vec![3.5, -0.0, 0.0, zero / zero, -1.0, 1.0e10, -1.0e10, 2.0];
    let mut f: Vec[F64] = raw.iter().map(F64.from).collect();
    f.sort();
    let mut fo = "";
    for x in f {
        fo = fo + f"{x.value} ";
    }
    println(fo);
    let mut ft: Vec[(F64, i64)] = Vec.new();
    ft.push((F64.from(1.5), 2));
    ft.push((F64.from(1.5), 1));
    ft.push((F64.from(-2.0), 9));
    ft.push((F64.from(-0.0), 0));
    ft.push((F64.from(0.0), -1));
    ft.sort();
    let mut fto = "";
    for x in ft {
        fto = fto + f"({x.0.value},{x.1}) ";
    }
    println(fto);
    let mut s: Vec[(String, i64)] = Vec.new();
    for i in 0..300 {
        let k = next(mut st) % 40;
        s.push((f"k{k}", i % 5));
    }
    s.sort();
    let mut sh: i64 = 0;
    for e in s {
        sh = (sh * 31 + e.0.len() * 10 + e.1) % 1000000007;
    }
    println(f"str-tuples h={sh} first={s[0].0},{s[0].1} last={s[299].0},{s[299].1}");
    let mut nv: Vec[Vec[i64]] = vec![vec![3, 1], vec![], vec![3], vec![-1, 9, 9], vec![3, 1, 0]];
    nv.sort();
    println(f"{nv}");
    let mut ks: Vec[Key] = Vec.new();
    for i in 0..2000 {
        let hi = (next(mut st) % 3 + 250) as u8;
        let lo = next(mut st) % 11 - 5;
        ks.push(Key { hi: hi, lo: lo, name: f"n{i % 4}" });
    }
    ks.sort();
    let mut kh: i64 = 0;
    for k in ks {
        kh = (kh * 1000003 + (k.hi as i64) * 97 + (k.lo + 5) * 7 + k.name.len()) % 1000000007;
    }
    println(f"struct h={kh} first={ks[0].hi},{ks[0].lo},{ks[0].name}");
    let mut es: Vec[Shape] = vec![Shape.Box(1, -1), Shape.Line(4), Shape.Dot, Shape.Line(-4), Shape.Box(4000000000, 0), Shape.Box(1, -2)];
    es.sort();
    let mut eo = "";
    for e in es {
        match e {
            Shape.Dot => { eo = eo + "D "; }
            Shape.Line(x) => { eo = eo + f"L{x} "; }
            Shape.Box(a, b) => { eo = eo + f"B{a},{b} "; }
        }
    }
    println(eo);
}
"#,
    );
    let Some(out) = out else { return };
    assert_eq!(out, "ii card=3 n=0 sorted=true h=17\nii card=3 n=1 sorted=true h=32016340\nii card=3 n=33 sorted=true h=906393946\nii card=3 n=1000 sorted=true h=621801907\nii card=3 n=6000 sorted=true h=989513765\nii card=100000 n=0 sorted=true h=17\nii card=100000 n=1 sorted=true h=30489028\nii card=100000 n=33 sorted=true h=906245708\nii card=100000 n=1000 sorted=true h=44915288\nii card=100000 n=6000 sorted=true h=519534254\nasc==desc true h=55864032\n-10000000000 -1 -0 0 2 3.5 10000000000 NaN \n(-2,9) (-0,0) (0,-1) (1.5,1) (1.5,2) \nstr-tuples h=53791323 first=k0,2 last=k9,4\n[[], [-1, 9, 9], [3], [3, 1], [3, 1, 0]]\nstruct h=201342121 first=250,-5,n0\nD L-4 L4 B1,-2 B1,-1 B4000000000,0 \n");
}

/// B-2026-10-03-25 — the path itself: no call to the runtime callback sort
/// survives for `Vec[(i64, i64)].sort()`. Before, the tuple's comparator was
/// called through a function pointer on every comparison, which made the
/// bare `sort()` slower than the `sort_by(|a, b| a.cmp(b))` spelling of the
/// same order (kata 336: 859 ms against 731 ms).
#[test]
fn bare_tuple_sort_emits_no_runtime_sort_call() {
    let src = r#"
fn main() {
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut i: i64 = 0;
    while i < 5000 {
        v.push(((i * 37 + 11) % 5000, i % 3));
        i = i + 1;
    }
    v.sort();
    println(v[0].0);
}
"#;
    let mut parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    let ir =
        karac::codegen::compile_to_ir(&parsed.program, Some(&ownership), None).expect("codegen");
    let runtime_sort_calls = ir
        .lines()
        .filter(|l| l.contains("@karac_vec_sort_by(") && l.contains(" call "))
        .count();
    assert_eq!(
        runtime_sort_calls, 0,
        "a bare tuple sort() must not reach the function-pointer runtime sort"
    );
}
