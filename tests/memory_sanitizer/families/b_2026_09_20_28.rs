//! B-2026-09-20-28 -- a nested store releases the displaced element at any
//! depth and through a field root, not only at `d[i][j]`.

use super::*;

/// B-2026-09-20-28 — `d[0][0][0] = x` and `h.xs[0][0] = x` leaked the displaced
/// element's `String` (one block at `-O0`): the displaced-element drop lowered
/// only a bare-name inner object, so an `Index` under an `Index` and a
/// `FieldAccess` under an `Index` were declined. Every printed line reads the
/// stored value, so a value freed at the store cannot pass as one leaked.
#[test]
fn asan_nested_store_releases_the_displaced_element_at_any_depth() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "depth-3",
            "fn main() { let n = 7; let mut d: Vec[Vec[Vec[(String, i64)]]] = [[[(f\"one-aaaaaaaaaaaa-{n}\", 1)]]]; d[0][0][0] = (f\"replaced-bbbbbbbbbbbb-{n}\", 2); println(f\"r:{d[0][0][0].1}:{d[0][0][0].0.len()}\") }
",
            "r:2:23",
        ),
        (
            "depth-4",
            "fn main() { let n = 7; let mut d: Vec[Vec[Vec[Vec[(String, i64)]]]] = [[[[(f\"one-aaaaaaaaaaaa-{n}\", 1)]]]]; d[0][0][0][0] = (f\"replaced-bbbbbbbbbbbb-{n}\", 2); println(f\"r:{d[0][0][0][0].1}:{d[0][0][0][0].0.len()}\") }
",
            "r:2:23",
        ),
        (
            "field-root",
            "struct H { xs: Vec[Vec[(String, i64)]] }
fn main() { let n = 7; let mut h = H { xs: [[(f\"one-aaaaaaaaaaaa-{n}\", 1)]] }; h.xs[0][0] = (f\"replaced-bbbbbbbbbbbb-{n}\", 2); println(f\"r:{h.xs[0][0].1}:{h.xs[0][0].0.len()}\") }
",
            "r:2:23",
        ),
        (
            "field-under-an-index",
            "struct H { xs: Vec[(String, i64)] }
fn main() { let n = 7; let mut d: Vec[H] = [H { xs: [(f\"one-aaaaaaaaaaaa-{n}\", 1)] }]; d[0].xs[0] = (f\"replaced-bbbbbbbbbbbb-{n}\", 2); println(f\"r:{d[0].xs[0].1}:{d[0].xs[0].0.len()}\") }
",
            "r:2:23",
        ),
        (
            "self-field-root",
            "struct H { xs: Vec[Vec[(String, i64)]] }
impl H { fn set(mut ref self, n: i64) { self.xs[0][0] = (f\"replaced-bbbbbbbbbbbb-{n}\", 2); } }
fn main() { let n = 7; let mut h = H { xs: [[(f\"one-aaaaaaaaaaaa-{n}\", 1)]] }; h.set(n); println(f\"r:{h.xs[0][0].1}:{h.xs[0][0].0.len()}\") }
",
            "r:2:23",
        ),
        (
            "array-depth-3",
            "fn main() { let n = 7; let mut d: Array[Array[Array[(String, i64), 1], 1], 1] = [[[(f\"one-aaaaaaaaaaaa-{n}\", 1)]]]; d[0][0][0] = (f\"replaced-bbbbbbbbbbbb-{n}\", 2); println(f\"r:{d[0][0][0].1}:{d[0][0][0].0.len()}\") }
",
            "r:2:23",
        ),
        (
            "struct-element",
            "struct W { s: String, k: i64 }
fn main() { let n = 7; let mut d: Vec[Vec[Vec[W]]] = [[[W { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }]]]; d[0][0][0] = W { s: f\"replaced-bbbbbbbbbbbb-{n}\", k: 2 }; println(f\"r:{d.len()}\") }
",
            "r:1",
        ),
        (
            "enum-element",
            "enum E { A(String), B }
fn main() { let n = 7; let mut d: Vec[Vec[Vec[E]]] = [[[E.A(f\"one-aaaaaaaaaaaa-{n}\")]]]; d[0][0][0] = E.A(f\"replaced-bbbbbbbbbbbb-{n}\"); println(f\"r:{d.len()}\") }
",
            "r:1",
        ),
        (
            "in-a-loop",
            "fn main() { let n = 7; let mut d: Vec[Vec[Vec[(String, i64)]]] = [[[(f\"one-aaaaaaaaaaaa-{n}\", 1), (f\"two-aaaaaaaaaaaa-{n}\", 1)]]]; for i in 0..2 { d[0][0][i] = (f\"replaced-bbbbbbbbbbbb-{i}\", 2); } println(f\"r:{d[0][0][1].1}:{d[0][0][1].0.len()}\") }
",
            "r:2:23",
        ),
        (
            "mut-ref-param",
            "fn set(d: mut ref Vec[Vec[Vec[(String, i64)]]], n: i64) { d[0][0][0] = (f\"replaced-bbbbbbbbbbbb-{n}\", 2); }
fn main() { let n = 7; let mut d: Vec[Vec[Vec[(String, i64)]]] = [[[(f\"one-aaaaaaaaaaaa-{n}\", 1)]]]; set(mut d, n); println(f\"r:{d[0][0][0].1}:{d[0][0][0].0.len()}\") }
",
            "r:2:23",
        ),
        (
            "control:bare-string-element",
            "fn main() { let n = 7; let mut d: Vec[Vec[Vec[String]]] = [[[f\"one-aaaaaaaaaaaa-{n}\"]]]; d[0][0][0] = f\"replaced-bbbbbbbbbbbb-{n}\"; println(d[0][0][0]) }
",
            "replaced-bbbbbbbbbbbb-7",
        ),
    ];
    for (label, src, want) in cells {
        assert_clean_asan_run(src, &[want], &format!("b2026-09-20-28-{label}"));
    }
}
