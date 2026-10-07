//! B-2026-10-02-70 -- a `for` loop over a bracketed literal of plain scalar
//! values heap-allocated the literal on every execution.

use super::*;

/// B-2026-10-02-70 — every spelling of a loop over a scalar `Vec` literal
/// that now iterates a stack array: `continue`, `break`, `return` from the
/// body, a non-constant element, tuples, floats, `bool`, `char`, `u8`,
/// `.iter()`, `.iter().enumerate()`, a labeled break out of a nested literal
/// loop, a literal re-evaluated per outer iteration, and a labeled-block
/// break value. Same output as `--interp`.
#[test]
fn e2e_for_over_scalar_vec_literal_iterates_a_stack_array() {
    let src = r#"fn first_neg(k: i64) -> i64 {
    for x in [3, k, -1, 9] {
        if x < 0 {
            return x;
        }
    }
    return 0;
}
fn walk(r: i64, c: i64) -> i64 {
    let mut s = 0;
    for (dr, dc) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
        s = s * 10 + (r + dr) * 3 + (c + dc);
    }
    return s;
}
fn main() {
    let mut t = 0;
    for x in [1, 2, 3, 4, 5] {
        if x == 2 {
            continue;
        }
        if x == 5 {
            break;
        }
        t += x;
    }
    println(f"a {t}");
    println(f"b {first_neg(7)} {first_neg(-4)}");
    println(f"c {walk(2, 3)}");
    let mut f = 0.0;
    for v in [0.5, 1.25, 2.0] {
        f += v;
    }
    println(f"d {f}");
    let mut n = 0;
    for b in [true, false, true] {
        if b {
            n += 1;
        }
    }
    println(f"e {n}");
    let mut cs = "";
    for ch in ['x', 'y', 'z'] {
        cs = f"{cs}{ch}";
    }
    println(f"f {cs}");
    for (i, x) in [10, 20, 30].iter().enumerate() {
        println(f"g {i} {x}");
    }
    let mut acc = 0;
    for x in [1, 2, 3].iter() {
        acc += x;
    }
    println(f"h {acc}");
    let mut p = 0;
    'outer: for a in [1, 2, 3] {
        for b in [10, 20, 30] {
            if b == 30 and a == 2 {
                break 'outer;
            }
            p += a * b;
        }
    }
    println(f"i {p}");
    let mut q = 0;
    for k in 0..3 {
        for x in [k, k * 2, k * 3] {
            q += x;
        }
    }
    println(f"j {q}");
    let mut last = (0, 0, 0);
    for (a, b, c) in [(1, 2, 3), (4, 5, 6)] {
        last = (c, b, a);
    }
    println(f"k {last.0} {last.1} {last.2}");
    let mut u: u8 = 0;
    for x in [1u8, 2u8, 250u8] {
        u = u ^ x;
    }
    println(f"l {u}");
    let w = 'found: {
        for x in [4, 5, 6] {
            if x == 5 {
                break 'found x * 2;
            }
        }
        0
    };
    println(f"m {w}");
}
"#;
    assert_eq!(
        run_program(src),
        Some(
            "a 8\nb -1 -4\nc 7290\nd 3.75\ne 2\nf xyz\ng 0 10\ng 1 20\ng 2 30\nh 6\ni 120\nj 18\nk 6 5 4\nl 249\nm 10\n"
                .to_string()
        )
    );
}

/// B-2026-10-02-70 — the neighbour walk of a grid search allocates nothing.
///
/// Before: `walk` called the heap allocator once per call to build the
/// four-element `Vec` and freed it after the loop, which was 30% of kata
/// 329's benchmark.
#[test]
fn ir_for_over_scalar_vec_literal_allocates_nothing() {
    let ir = ir_for(
        r#"fn walk(r: i64, c: i64) -> i64 {
    let mut s = 0;
    for (dr, dc) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
        s = s * 10 + (r + dr) * 3 + (c + dc);
    }
    return s;
}
fn main() {
    println(f"{walk(2, 3)}");
}
"#,
    );
    let body = extract_fn_ir(&ir, "walk");
    let allocs: Vec<&str> = body
        .lines()
        .filter(|l| l.contains("call") && (l.contains("alloc") || l.contains("free")))
        .collect();
    assert!(
        allocs.is_empty(),
        "walk should not touch the heap, found:\n{}\n\nbody:\n{body}",
        allocs.join("\n")
    );
}
