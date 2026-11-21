//! Found writing kata 2812 (Find the Safest Path in a Grid).
//!
//! B-2026-11-21-201: LeetCode's Rust solution, carried by `karac fix`, fills
//! a table with `Vec.filled(n, Vec.filled(n, -1))`, stores
//! `dist[r][c] = dist[i][j] + 1`, pushes `(dist[0][0], 0, 0)` onto a
//! `PriorityQueue` and returns the popped part from a function returning
//! `i32`. design.md §8 Literals leaves the `-1` open until that return
//! fixes it, but two places settled it to `i64` first: a place read into
//! a tuple (`(x, 0)`, `(dist[0], 0)`) and arithmetic stored by an
//! assignment (`dist[1] = dist[0] + 1`). The return was then refused as a
//! narrowing, with no fix.

fn check(src: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-literal-open-tuple-{}-{}.kara",
        std::process::id(),
        src.len()
    ));
    std::fs::write(&path, src).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

const BINDING_IN_TUPLE: &str = "fn f() -> i32 {
    let x = -1;
    let t = (x, 0);
    return t.0;
}

fn main() {
    println(f\"{f()}\");
}
";

const ELEMENT_IN_PUSHED_TUPLE: &str = "fn f(n: i64) -> i32 {
    let dist = Vec.filled(n, -7);
    let mut heap = PriorityQueue.max_first();
    heap.push((dist[0], 0));
    if let Some((d, _)) = heap.pop() {
        return d;
    }
    0
}

fn main() {
    println(f\"{f(3)}\");
}
";

const ASSIGNED_SUM: &str = "fn f(n: i64) -> i32 {
    let mut dist = Vec.filled(n, Vec.filled(n, -1));
    dist[0][0] = 4;
    dist[1][0] = dist[0][0] + 1;
    let mut heap = PriorityQueue.max_first();
    heap.push((dist[1][0], 1, 0));
    while let Some((d, _, _)) = heap.pop() {
        return d;
    }
    0
}

fn main() {
    println(f\"{f(3)}\");
}
";

#[test]
fn a_binding_read_into_a_tuple_keeps_its_literal_open() {
    let out = check(BINDING_IN_TUPLE);
    assert!(out.contains("All checks passed"), "{out}");
    assert_eq!(run(BINDING_IN_TUPLE), "-1\n");
}

#[test]
fn an_element_read_into_a_pushed_tuple_keeps_its_literal_open() {
    let out = check(ELEMENT_IN_PUSHED_TUPLE);
    assert!(out.contains("All checks passed"), "{out}");
    assert_eq!(run(ELEMENT_IN_PUSHED_TUPLE), "-7\n");
}

#[test]
fn arithmetic_stored_by_an_assignment_keeps_its_literal_open() {
    let out = check(ASSIGNED_SUM);
    assert!(out.contains("All checks passed"), "{out}");
    assert_eq!(run(ASSIGNED_SUM), "5\n");
}

#[test]
fn an_unfixed_literal_in_a_tuple_still_defaults() {
    let src = "fn main() {\n    let x = -1;\n    let t = (x, 0);\n    let big = t.0 * 4000000000;\n    println(f\"{big}\");\n}\n";
    let out = check(src);
    assert!(out.contains("All checks passed"), "{out}");
    assert_eq!(run(src), "-4000000000\n");
}

#[test]
fn places_in_a_tuple_lent_to_ref_slots_take_the_slots_type() {
    // An open element in a tuple lent part by part to `ref i64` slots is
    // fixed by its slot, as a binding lent to a `ref` parameter is.
    let src = "fn g(p: (ref i64, ref i64)) -> i64 {
    let (x, y) = p;
    x + y
}

fn main() {
    let a = [1, 2];
    let b = [10, 20];
    println(g((a[0], b[0])));
    let f = |(x, y): (ref i64, ref i64)| x * y;
    println(f((a[1], b[1])));
}
";
    let out = check(src);
    assert!(out.contains("All checks passed"), "{out}");
    assert_eq!(run(src), "11\n40\n");
}

#[test]
fn a_fill_stored_into_by_arithmetic_is_fixed_by_a_collect() {
    // Kata 2055's shape: the assignment leaves the fill open, and the
    // `collect` into a `Vec[i32]` return fixes it, as Rust infers it.
    let src = "fn spans(n: i64, queries: Vec[Vec[i32]]) -> Vec[i32] {
    let mut prefix = Vec.filled(n + 1, 0);
    for i in 0..n {
        prefix[i + 1] = prefix[i] + 2;
    }
    queries.iter().map(|q| prefix[q[1] as i64] - prefix[q[0] as i64]).collect()
}

fn main() {
    println(spans(5, [[1, 4], [2, 3]]));
}
";
    let out = check(src);
    assert!(out.contains("All checks passed"), "{out}");
    assert_eq!(run(src), "[6, 2]\n");
}
