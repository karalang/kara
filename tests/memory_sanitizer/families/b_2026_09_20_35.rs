//! B-2026-09-20-35 -- an index store whose value reads a heap field out of
//! the same container releases the displaced element.

use super::*;

const PRE: &str = "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}:{self.s.len()}\") } }\nstruct H { v: Vec[S] }\n";

/// B-2026-09-20-35 — `a[0] = S { s: a[1].s, k: 2 }` leaked the displaced
/// element's `String` (28 B at `-O0`): the RHS names the container, so the
/// store stood the displaced release down, although the field read had been
/// cloned at the read and shared nothing with the element. A field or tuple
/// read the clone log records now clears the release. The displaced bodies
/// stay off for that clearance, as the interpreter skips them for any RHS that
/// names the container. `control:` cells were right before.
#[test]
fn asan_index_store_reading_a_cloned_field_releases_displaced() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "self-field",
            "fn main() { let n = 7; let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaaaaaaaaaaaa-{n}\", k: 1 }, S { s: f\"two-bbbbbbbb-{n}\", k: 5 }]; a[0] = S { s: a[0].s, k: 2 }; println(f\"r:{a[0].k}:{a[0].s}:{a[1].s}\") }\n",
            "r:2:one-aaaaaaaaaaaaaaaaaaaaaa-7:two-bbbbbbbb-7\n",
        ),
        (
            "other-field",
            "fn main() { let n = 7; let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaaaaaaaaaaaa-{n}\", k: 1 }, S { s: f\"two-bbbbbbbb-{n}\", k: 5 }]; a[0] = S { s: a[1].s, k: 2 }; println(f\"r:{a[0].k}:{a[0].s}:{a[1].s}\") }\n",
            "r:2:two-bbbbbbbb-7:two-bbbbbbbb-7\n",
        ),
        (
            "tuple-self",
            "fn main() { let n = 7; let mut a: Vec[(String, i64)] = [(f\"one-aaaaaaaaaaaaaaaaaaaaaa-{n}\", 1), (f\"two-bbbbbbbb-{n}\", 5)]; a[0] = (a[0].0, 3); println(f\"r:{a[0].1}:{a[0].0}:{a[1].0}\") }\n",
            "r:3:one-aaaaaaaaaaaaaaaaaaaaaa-7:two-bbbbbbbb-7\n",
        ),
        (
            "tuple-other",
            "fn main() { let n = 7; let mut a: Vec[(String, i64)] = [(f\"one-aaaaaaaaaaaaaaaaaaaaaa-{n}\", 1), (f\"two-bbbbbbbb-{n}\", 5)]; a[0] = (a[1].0, 3); println(f\"r:{a[0].1}:{a[0].0}:{a[1].0}\") }\n",
            "r:3:two-bbbbbbbb-7:two-bbbbbbbb-7\n",
        ),
        (
            "nested-field",
            "fn main() { let n = 7; let mut a: Vec[U] = [U { t: T { s: f\"one-aaaaaaaaaaaaaaaaaaaaaa-{n}\" }, k: 1 }, U { t: T { s: f\"two-bbbbbbbb-{n}\" }, k: 5 }]; a[0] = U { t: T { s: a[1].t.s }, k: 2 }; println(f\"r:{a[0].k}:{a[0].t.s}:{a[1].t.s}\") }\n",
            "r:2:two-bbbbbbbb-7:two-bbbbbbbb-7\n",
        ),
        (
            "field-root",
            "fn main() { let n = 7; let mut h = H { v: [S { s: f\"one-aaaaaaaaaaaaaaaaaaaaaa-{n}\", k: 1 }, S { s: f\"two-bbbbbbbb-{n}\", k: 5 }] }; h.v[0] = S { s: h.v[1].s, k: 2 }; println(f\"r:{h.v[0].k}:{h.v[0].s}:{h.v[1].s}\") }\n",
            "r:2:two-bbbbbbbb-7:two-bbbbbbbb-7\n",
        ),
        (
            "mut-ref-param",
            "fn set(a: mut ref Vec[S]) { a[0] = S { s: a[1].s, k: 2 }; }\nfn main() { let n = 7; let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaaaaaaaaaaaa-{n}\", k: 1 }, S { s: f\"two-bbbbbbbb-{n}\", k: 5 }]; set(mut a); println(f\"r:{a[0].k}:{a[0].s}:{a[1].s}\") }\n",
            "r:2:two-bbbbbbbb-7:two-bbbbbbbb-7\n",
        ),
        (
            "loop",
            "fn main() { let n = 7; let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaaaaaaaaaaaa-{n}\", k: 1 }, S { s: f\"two-bbbbbbbb-{n}\", k: 5 }]; for i in 0..3 { a[0] = S { s: a[1].s, k: i }; }; println(f\"r:{a[0].k}:{a[0].s}:{a[1].s}\") }\n",
            "r:2:two-bbbbbbbb-7:two-bbbbbbbb-7\n",
        ),
        (
            "control:fresh",
            "fn main() { let n = 7; let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaaaaaaaaaaaa-{n}\", k: 1 }, S { s: f\"two-bbbbbbbb-{n}\", k: 5 }]; a[0] = S { s: f\"x-{n}\", k: 2 }; println(f\"r:{a[0].k}:{a[0].s}:{a[1].s}\") }\n",
            "r:2:x-7:two-bbbbbbbb-7\n",
        ),
        (
            "control:clone",
            "fn main() { let n = 7; let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaaaaaaaaaaaa-{n}\", k: 1 }, S { s: f\"two-bbbbbbbb-{n}\", k: 5 }]; a[0] = S { s: a[1].s.clone(), k: 2 }; println(f\"r:{a[0].k}:{a[0].s}:{a[1].s}\") }\n",
            "r:2:two-bbbbbbbb-7:two-bbbbbbbb-7\n",
        ),
    ];
    for (label, body, want) in cells {
        let prog = format!("{PRE}{body}");
        let lines: Vec<&str> = want.lines().collect();
        assert_clean_asan_run(&prog, &lines, &format!("b2026-09-20-35-{label}"));
    }
}
