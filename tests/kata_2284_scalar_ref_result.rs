//! Kata 2284's arms pick the index of the smallest name with
//! `(0..n).min_by_key(|i| ref names[i])`. The library's `min_by_key`
//! keeps the best key while the next item replaces the one it was
//! computed from, and the MIR borrow check read the key, a `ref String`,
//! as a borrow of the `i64` item the closure was handed: "write of _9
//! while it is borrowed". A number has no parts, so a reference a call
//! hands back cannot point into one it was lent unless it is a reference
//! to that number (B-2026-11-18-89).

#[test]
fn kata_2284_a_string_key_from_an_index_borrows_the_capture() {
    let src = "fn main() {
    let names = [\"q\".to_string(), \"p\".to_string(), \"r\".to_string()];
    let lo = [0, 1, 2].into_iter().min_by_key(|i| ref names[i]).unwrap();
    let hi = [0, 1, 2].into_iter().max_by_key(|i| ref names[i]).unwrap();
    println(f\"{lo} {hi}\");
}
";
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "1 2\n");
}

#[test]
fn kata_2284_a_number_handed_back_still_borrows_it() {
    let src = "fn pick(x: ref i64) -> ref i64 {
    x
}

fn main() {
    let mut a = 5;
    let r = pick(a);
    a = 6;
    println(f\"{r} {a}\");
}
";
    let Err(err) = karac::mir::lower::run_source(src) else {
        panic!("refused");
    };
    assert!(
        format!("{err:?}").contains("while it is borrowed"),
        "{err:?}"
    );
}
