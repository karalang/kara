//! B-2026-09-20-25 -- a struct field read (and store) through an `Array` or
//! `Vec` index inside a TUPLE element (`a.0[0].n`) lowers.

use super::*;

const PRE: &str = r#"struct P { n: i64 }
struct Q { n: i64, s: String }
struct R { n: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.n}") } }
struct Bag { t: (Array[P, 2], i64) }
"#;

/// B-2026-09-20-25 — `a.0[0].n` over `(Array[P, 1], i64)` passed `karac check`,
/// printed `r:5` under `--interp`, and failed `karac build` with "cannot
/// resolve field 'n' on this receiver". The container-element type resolver
/// knew a container reached through a binding, an index and a named field, and
/// not one held in a tuple element. Every non-`control:` cell failed to BUILD
/// before the fix; the store cell additionally needs B-2026-09-30-83's array
/// element place.
#[test]
fn field_through_container_in_tuple_element_lowers() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "array-first",
            r#"fn main() { let a: (Array[P, 1], i64) = ([P { n: 5 }], 7); println(f"r:{a.0[0].n}") }"#,
            "r:5\n",
        ),
        (
            "array-second-element",
            r#"fn main() { let a: (i64, Array[P, 2]) = (7, [P { n: 5 }, P { n: 8 }]); println(f"r:{a.1[1].n}") }"#,
            "r:8\n",
        ),
        (
            "vec",
            r#"fn main() { let mut v: Vec[P] = Vec.new(); v.push(P { n: 6 }); let a: (Vec[P], i64) = (v, 7); println(f"r:{a.0[0].n}") }"#,
            "r:6\n",
        ),
        (
            "depth-two",
            r#"fn main() { let a: (Array[(i64, P), 1], i64) = ([(3, P { n: 5 })], 7); println(f"r:{a.0[0].1.n}") }"#,
            "r:5\n",
        ),
        (
            "drop-element",
            r#"fn main() { let a: (Array[R, 1], i64) = ([R { n: 5 }], 7); println(f"r:{a.0[0].n}"); println("end") }"#,
            "r:5\ndR5\nend\n",
        ),
        (
            "field-rooted",
            r#"fn main() { let b = Bag { t: ([P { n: 5 }, P { n: 6 }], 7) }; println(f"r:{b.t.0[1].n}") }"#,
            "r:6\n",
        ),
        (
            "generic-caller",
            r#"fn g[T](a: ref (Array[T, 1], i64)) -> i64 { return a.1 } fn main() { let a: (Array[P, 1], i64) = ([P { n: 5 }], 7); println(f"r:{g(a)} {a.0[0].n}") }"#,
            "r:7 5\n",
        ),
        (
            "loop-index",
            r#"fn main() { let a: (i64, Array[P, 3]) = (7, [P { n: 1 }, P { n: 2 }, P { n: 3 }]); let mut s = 0; let mut i = 0; while i < 3 { s = s + a.1[i].n; i = i + 1; } println(f"r:{s}") }"#,
            "r:6\n",
        ),
        (
            "string-field",
            r#"fn main() { let a: (Array[Q, 1], i64) = ([Q { n: 5, s: f"hi{1}" }], 7); println(f"r:{a.0[0].n} {a.0[0].s}") }"#,
            "r:5 hi1\n",
        ),
        (
            "string-method",
            r#"fn main() { let a: (Array[Q, 1], i64) = ([Q { n: 5, s: f"hi{1}" }], 7); let k = a.0[0].s.len(); println(f"r:{k}") }"#,
            "r:3\n",
        ),
        (
            "store",
            r#"fn main() { let mut a: (Array[P, 2], i64) = ([P { n: 5 }, P { n: 6 }], 3); a.0[1].n = 9; println(f"r:{a.0[0].n} {a.0[1].n} {a.1}") }"#,
            "r:5 9 3\n",
        ),
        (
            "control:no-tuple",
            r#"fn main() { let a: Array[P, 1] = [P { n: 5 }]; println(f"r:{a[0].n}") }"#,
            "r:5\n",
        ),
        (
            "control:no-container",
            r#"fn main() { let a: (P, i64) = (P { n: 5 }, 7); println(f"r:{a.0.n}") }"#,
            "r:5\n",
        ),
        (
            "control:named-field",
            r#"struct B2 { a: Array[P, 1], k: i64 } fn main() { let b = B2 { a: [P { n: 5 }], k: 7 }; println(f"r:{b.a[0].n}") }"#,
            "r:5\n",
        ),
    ];
    for (label, body, want) in cells {
        let prog = format!("{PRE}{body}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&prog);
        assert!(
            interp_errs.is_empty(),
            "[{label}] interp errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "[{label}] interpreter");
        let Some(aot) = run_program(&prog) else {
            continue;
        };
        assert_eq!(aot, *want, "[{label}] AOT");
    }
}
