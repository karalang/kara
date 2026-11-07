//! A dotted unit-variant pattern (`Cmp.Lt`) binds nothing, so a `match` of
//! a captured enum with only such arms moves nothing and the closure stays
//! `Fn` (core semantics §4.6, §9.3). `karac check` stopped at the variant;
//! the MIR pipeline's checker took it for a binding that could move, so the
//! closure typed as `OnceFn` and storing it in a `Fn` slot was refused on
//! every backend while `karac check` passed (B-2026-11-07-13).

#[test]
fn a_closure_matching_a_captured_unit_variant_is_repeatable() {
    let src = "enum Cmp { Lt, Ge }
type Test = Fn(i64) -> bool;

fn cmp_of(op: Str) -> Option[Cmp] {
    match op {
        \"<\" => Some(Cmp.Lt),
        \">=\" => Some(Cmp.Ge),
        _ => None,
    }
}

fn make(op: Str, v: i64, out: mut ref Vec[Test]) {
    let Some(cmp) = cmp_of(op) else {
        return;
    };
    let test: Test = |x: i64| {
        match cmp {
            Cmp.Lt => x < v,
            Cmp.Ge => x >= v,
        }
    };
    out.push(test);
}

fn main() {
    let mut ts: Vec[Test] = Vec.new();
    make(\"<\", 3, mut ts);
    make(\">=\", 3, mut ts);
    println(f\"{ts[0](1)} {ts[0](5)} {ts[1](1)} {ts[1](5)}\");
}
";
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "true false false true\n");
}
