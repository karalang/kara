//! Kata 2057: a mutation of the position arm wrote
//! `nums.iter().enumerate().rev().position(..)`, as Rust does, and the check
//! refused it with "no method 'rev' on type 'IterEnumerate'". Only the
//! collections' own iterators were double-ended, so `map(..).rev()`,
//! `filter(..).rev()` and `zip(..).rev()` were refused too. The adapters
//! now walk from the back when what they wrap can, and `enumerate` and
//! `zip`, which need to know how many items are left, ask the new
//! `ExactSizeIterator`.

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn kata_2057_enumerate_rev_numbers_items_from_the_front() {
    let src = "fn main() {
    let v = [3, 1, 2, 6];
    let a: Vec[(i64, i64)] = v.iter().enumerate().rev().collect();
    println(a);
    println(v.iter().enumerate().rev().find(|(i, x)| i % 2 == 0));
    let mut it = v.iter().enumerate();
    let _ = it.next();
    println(it.next_back());
    println(it.len());
    let s: Vec[String] = [\"a\".to_string(), \"b\".to_string()].to_vec();
    let g: Vec[(i64, String)] = s.into_iter().enumerate().rev().collect();
    println(g);
}
";
    assert_eq!(
        run(src),
        "[(3, 6), (2, 2), (1, 1), (0, 3)]\nSome((2, 2))\nSome((3, 6))\n2\n[(1, b), (0, a)]\n"
    );
}

#[test]
fn kata_2057_map_filter_cloned_rev() {
    let src = "fn main() {
    let v = [3, 1, 2, 6];
    let a: Vec[i64] = v.iter().map(|x| x * 2).rev().collect();
    let b: Vec[i64] = v.iter().filter(|x| x % 2 == 0).rev().cloned().collect();
    let c: Vec[i64] = v.iter().cloned().rev().collect();
    let d: Vec[(i64, i64)] = v.iter().map(|x| x + 1).enumerate().rev().collect();
    println(f\"{a} {b} {c} {d}\");
    println(v.iter().map(|x| x + 1).rev().position(|x| x == 4));
}
";
    assert_eq!(
        run(src),
        "[12, 4, 2, 6] [6, 2] [6, 2, 1, 3] [(3, 7), (2, 3), (1, 2), (0, 4)]\nSome(3)\n"
    );
}

/// The longer side is trimmed first, so the back pairs what the front does.
#[test]
fn kata_2057_zip_rev_trims_the_longer_side() {
    let src = "fn main() {
    let v = [3, 1, 2, 6];
    let w = [10, 20];
    let c: Vec[(i64, i64)] = v.iter().zip(w.iter()).rev().collect();
    let d: Vec[(i64, i64)] = w.iter().zip(v.iter()).rev().collect();
    println(f\"{c} {d} {v.iter().zip(w.iter()).len()}\");
}
";
    assert_eq!(run(src), "[(1, 20), (3, 10)] [(20, 1), (10, 3)] 2\n");
}
