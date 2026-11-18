//! A loop over a constant table (B-2026-11-18-39): an array literal whose
//! every element is a constant is built from a private constant global,
//! and a counting loop that reads one by its index is hinted to unroll
//! in full when it runs at most eight trips, whatever it calls, so each
//! trip's reads fold to constants (kata 1197's `for (dx, dy) in MOVES`
//! with a `VecDeque.push_back` in its body). A longer table keeps the
//! ordinary rules.
#![cfg(feature = "llvm")]

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

/// The body of the function `name` in `ir`.
fn body<'a>(ir: &'a str, name: &str) -> &'a str {
    let at = ir
        .match_indices(&format!("@kara.{name}("))
        .map(|(i, _)| i)
        .find(|&i| {
            ir[..i]
                .rsplit('\n')
                .next()
                .is_some_and(|l| l.starts_with("define"))
        })
        .unwrap_or_else(|| panic!("no function {name} in {ir}"));
    let end = ir[at..].find("\n}\n").map_or(ir.len(), |e| at + e);
    &ir[at..end]
}

/// Whether `name`'s body has a back edge hinted `llvm.loop.unroll.full`.
fn hinted(ir: &str, name: &str) -> bool {
    let full: Vec<&str> = ir
        .lines()
        .filter(|l| l.contains("!\"llvm.loop.unroll.full\""))
        .filter_map(|l| l.split(" = ").next())
        .collect();
    body(ir, name)
        .split("!llvm.loop ")
        .skip(1)
        .filter_map(|t| t.split(|c: char| c != '!' && !c.is_ascii_digit()).next())
        .any(|id| {
            ir.lines()
                .find(|l| l.starts_with(&format!("{id} = ")))
                .is_some_and(|l| {
                    full.iter()
                        .any(|f| l.contains(&format!("{f},")) || l.contains(&format!("{f}}}")))
                })
        })
}

const MOVES: &str = r#"const MOVES: Array[(i64, i64), 8] = [(1, 2), (2, 1), (2, -1), (1, -2), (-1, -2), (-2, -1), (-2, 1), (-1, 2)];

fn spread(x: i64, y: i64) -> i64 {
    let mut queue = VecDeque.new();
    for (dx, dy) in MOVES {
        queue.push_back((x + dx, y + dy));
    }
    let mut total = 0;
    while let Some((a, b)) = queue.pop_front() {
        total = total + a * 3 + b;
    }
    total
}

fn main() {
    println(spread(4, 4));
}
"#;

#[test]
fn mir_unroll_tables_builds_a_constant_literal_from_a_global() {
    let out = ir(MOVES);
    assert!(
        out.contains(
            "private unnamed_addr constant [16 x i64] [i64 1, i64 2, i64 2, i64 1, i64 2, i64 -1"
        ),
        "{out}"
    );
    assert!(body(&out, "spread").contains("@llvm.memcpy"), "{out}");
}

#[test]
fn mir_unroll_tables_hints_a_loop_over_a_table_that_calls() {
    let out = ir(MOVES);
    assert!(hinted(&out, "spread"), "{out}");
}

#[test]
fn mir_unroll_tables_leaves_a_long_table_loop_to_the_usual_rules() {
    let out = ir(
        r#"const PRIMES: Array[i64, 9] = [2, 3, 5, 7, 11, 13, 17, 19, 23];

fn spread(x: i64) -> i64 {
    let mut queue = VecDeque.new();
    for p in PRIMES {
        queue.push_back(x * p);
    }
    let mut total = 0;
    while let Some(a) = queue.pop_front() {
        total = total + a;
    }
    total
}

fn main() {
    println(spread(2));
}
"#,
    );
    assert!(!hinted(&out, "spread"), "{out}");
}
