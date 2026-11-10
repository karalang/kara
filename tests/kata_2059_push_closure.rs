//! Kata 2059's map arm kept its three operations in a
//! `Vec[Fn(i64, i64) -> i64]`. Pushed one at a time, `ops.push(|x, n| x + n)`
//! was refused "type '?T0' does not implement trait Add": the closure was
//! inferred alone, its parameters with no type, though the element type
//! names them, as a closure passed for a function parameter takes them from
//! the parameter. It is now checked against the element.

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn kata_2059_closure_pushed_into_fn_vec_takes_the_element_types() {
    let src = "fn main() {
    let mut ops: Vec[Fn(i64, i64) -> i64] = Vec.new();
    ops.push(|x, n| x + n);
    ops.push(|x, n| x - n);
    ops.push(|x, n| x ^ n);
    let k = 10;
    ops.push(|x, _| x * k);
    let mut out: Vec[i64] = Vec.new();
    for op in ops.iter() {
        out.push(op(6, 3));
    }
    let mut names: Vec[Fn(String) -> i64] = Vec.new();
    names.push(|s| s.len());
    let w = names[0](\"abc\".to_string());
    println(f\"{out} {w}\");
}
";
    assert_eq!(run(src), "[9, 3, 5, 60] 3\n");
}
