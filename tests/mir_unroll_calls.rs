//! The MIR backend's full-unroll hint on a counting loop that calls
//! (B-2026-10-31-44): a call to a small leaf (kata 36's `box_index`) or to
//! the function itself (kata 37's backtracking over the nine digits, a
//! `<=` guard) is hinted; a call to a kernel with a loop of its own (a
//! benchmark's outer pass loop) is not.
#![cfg(feature = "llvm")]

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

/// Whether the function `name`'s body carries a back edge whose
/// `!llvm.loop` metadata holds `llvm.loop.unroll.full` (the hint itself
/// lives in the module's metadata, after every function).
fn hinted(ir: &str, name: &str) -> bool {
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
    let full: Vec<&str> = ir
        .lines()
        .filter(|l| l.contains("!\"llvm.loop.unroll.full\""))
        .filter_map(|l| l.split(" = ").next())
        .collect();
    ir[at..end]
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

#[test]
fn mir_unroll_calls_hints_a_loop_calling_a_leaf() {
    let out = ir(r#"fn box_index(r: i64, c: i64) -> i64 {
    (r / 3) * 3 + c / 3
}

fn count(r: i64) -> i64 {
    let mut boxes: Array[i64, 9] = [0, 0, 0, 0, 0, 0, 0, 0, 0];
    let mut c = 0;
    while c < 9 {
        let b = box_index(r, c);
        boxes[b] = boxes[b] + 1;
        c = c + 1;
    }
    boxes[0] + boxes[4] * 10
}

fn main() {
    println(count(1));
}
"#);
    assert!(hinted(&out, "count"), "{out}");
}

#[test]
fn mir_unroll_calls_hints_a_self_recursive_digit_loop() {
    let out = ir(r#"fn go(used: mut ref Array[i64, 9], pos: i64) -> bool {
    if pos == 9 {
        return true;
    }
    let mut d = 1;
    while d <= 9 {
        if used[d - 1] == 0 {
            used[d - 1] = pos + 1;
            if go(mut used, pos + 1) {
                return true;
            }
            used[d - 1] = 0;
        }
        d = d + 1;
    }
    false
}

fn main() {
    let mut used: Array[i64, 9] = [0, 0, 0, 0, 0, 0, 0, 0, 0];
    println(go(mut used, 0));
    println(used[8]);
}
"#);
    assert!(hinted(&out, "go"), "{out}");
}

#[test]
fn mir_unroll_calls_leaves_a_pass_loop_calling_a_kernel() {
    let out = ir(r#"fn kernel(n: i64) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < n {
        s = s + i * i;
        i = i + 1;
    }
    s
}

fn main() {
    let mut acc = 0;
    let mut p = 0;
    while p < 30 {
        acc = (acc + kernel(1000 + p)) % 1000000007;
        p = p + 1;
    }
    println(acc);
}
"#);
    assert!(!hinted(&out, "main"), "{out}");
}
