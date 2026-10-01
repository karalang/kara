//! B-2026-09-20-34 -- an index store whose new value reads its container
//! through a SCALAR-typed method call releases the displaced element.

use super::*;

const PRE: &str = "struct S { s: String, k: i64 }\nstruct H { v: Vec[S] }\nimpl S { fn kk(ref self) -> i64 { self.k + 100 } }\nfn cnt(v: ref Vec[S]) -> i64 { v.len() }\n";

/// B-2026-09-20-34 — `a[0] = S { s: f"..", k: a[0].s.len() }` leaked the
/// displaced element's `String` (18 B at `-O0`) where `k: a[0].k + 1` was
/// clean: `expr_cannot_carry_container_heap` answered from declared types and
/// a method's return type is declared nowhere it could read, so the store
/// stood the displaced release down. It now asks the typechecker whether the
/// value is a primitive scalar first. `control:` cells were right before.
#[test]
fn asan_index_store_scalar_method_read_releases_displaced() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "len-of-field",
            "fn main() {\n    let n = 7;\n    let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }]; a[0] = S { s: f\"rep-bbbbbbbbbbbb-{n}\", k: a[0].s.len() }; println(f\"{a[0].k} {a[0].s}\")\n}\n",
            "18 rep-bbbbbbbbbbbb-7\n",
        ),
        (
            "len-of-field-plus-one",
            "fn main() {\n    let n = 7;\n    let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }]; a[0] = S { s: f\"rep-bbbbbbbbbbbb-{n}\", k: a[0].s.len() + 1 }; println(f\"{a[0].k} {a[0].s}\")\n}\n",
            "19 rep-bbbbbbbbbbbb-7\n",
        ),
        (
            "len-of-container",
            "fn main() {\n    let n = 7;\n    let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }]; a[0] = S { s: f\"rep-bbbbbbbbbbbb-{n}\", k: a.len() }; println(f\"{a[0].k} {a[0].s}\")\n}\n",
            "1 rep-bbbbbbbbbbbb-7\n",
        ),
        (
            "user-method",
            "fn main() {\n    let n = 7;\n    let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }]; a[0] = S { s: f\"rep-bbbbbbbbbbbb-{n}\", k: a[0].kk() }; println(f\"{a[0].k} {a[0].s}\")\n}\n",
            "101 rep-bbbbbbbbbbbb-7\n",
        ),
        (
            "free-fn-over-a-borrow",
            "fn main() {\n    let n = 7;\n    let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }]; a[0] = S { s: f\"rep-bbbbbbbbbbbb-{n}\", k: cnt(a) }; println(f\"{a[0].k} {a[0].s}\")\n}\n",
            "1 rep-bbbbbbbbbbbb-7\n",
        ),
        (
            "if-on-a-bool-method",
            "fn main() {\n    let n = 7;\n    let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }]; a[0] = S { s: f\"rep-bbbbbbbbbbbb-{n}\", k: if a[0].s.is_empty() { 0 } else { 5 } }; println(f\"{a[0].k} {a[0].s}\")\n}\n",
            "5 rep-bbbbbbbbbbbb-7\n",
        ),
        (
            "array-container",
            "fn main() {\n    let n = 7;\n    let mut a: Array[S, 1] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }]; a[0] = S { s: f\"rep-bbbbbbbbbbbb-{n}\", k: a[0].s.len() }; println(f\"{a[0].k} {a[0].s}\")\n}\n",
            "18 rep-bbbbbbbbbbbb-7\n",
        ),
        (
            "field-root",
            "fn main() {\n    let n = 7;\n    let mut h = H { v: [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }] }; h.v[0] = S { s: f\"rep-bbbbbbbbbbbb-{n}\", k: h.v.len() }; println(f\"{h.v[0].k} {h.v[0].s}\")\n}\n",
            "1 rep-bbbbbbbbbbbb-7\n",
        ),
        (
            "tuple-element",
            "fn main() {\n    let n = 7;\n    let mut a: Vec[(String, i64)] = [(f\"one-aaaaaaaaaaaa-{n}\", 1)]; a[0] = (f\"rep-bbbbbbbbbbbb-{n}\", a[0].0.len()); println(f\"{a[0].1} {a[0].0}\")\n}\n",
            "18 rep-bbbbbbbbbbbb-7\n",
        ),
        (
            "control:constant",
            "fn main() {\n    let n = 7;\n    let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }]; a[0] = S { s: f\"rep-bbbbbbbbbbbb-{n}\", k: 2 }; println(f\"{a[0].k} {a[0].s}\")\n}\n",
            "2 rep-bbbbbbbbbbbb-7\n",
        ),
        (
            "control:field-read",
            "fn main() {\n    let n = 7;\n    let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }]; a[0] = S { s: f\"rep-bbbbbbbbbbbb-{n}\", k: a[0].k + 1 }; println(f\"{a[0].k} {a[0].s}\")\n}\n",
            "2 rep-bbbbbbbbbbbb-7\n",
        ),
    ];
    for (label, body, want) in cells {
        let prog = format!("{PRE}{body}");
        let lines: Vec<&str> = want.lines().collect();
        assert_clean_asan_run(&prog, &lines, &format!("b2026-09-20-34-{label}"));
    }
}
