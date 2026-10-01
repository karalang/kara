//! B-2026-09-20-39 / B-2026-09-27-121 -- an owned-`self` method that matches
//! its receiver frees a `Vec` or `Array` payload exactly once.

use super::*;

const PRE: &str = "enum G[T] { Y(T), N }
impl[T] G[T] {
    fn show(self) -> i64 { match self { G.Y(v) => { println(f\"v={v}\"); return 1; } G.N => { return 0; } } }
    fn seen(self) -> i64 { match self { G.Y(v) => { println(\"seen\"); return 4; } G.N => { return 0; } } }
    fn keep(self) -> i64 { match self { G.Y(v) => { let w = v; println(\"moved\"); return 3; } G.N => { return 0; } } }
    fn drop_it(self) -> i64 { match self { G.Y(_) => { return 6; } G.N => { return 0; } } }
    fn mlen(self) -> i64 { return 7 }
}
";

/// B-2026-09-20-39 — `match self { G.Y(v) => .. }` over `G[Array[String, 2]]`
/// lost 33 B in 2 blocks at `-O0`: the caller cleared its box's interior drop
/// for an arm that, for an `Array` payload, does not take the interior over.
/// B-2026-09-27-121 — a `Vec` payload leaked its buffer (72 B over four calls)
/// because the arm binding was typed at the erased width. `control:` cells were
/// clean before.
#[test]
fn asan_owned_self_match_frees_a_vec_or_array_payload_once() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "array-string",
            "fn main() { let c: G[Array[String, 2]] = G.Y([f\"ccccccccccd-13-1\", f\"ddddddddddde-13-1\"]); println(f\"self_recv:{c.seen()}\"); }
",
            "seen\nself_recv:4\n",
        ),
        (
            "array-string-print",
            "fn main() { let c: G[Array[String, 2]] = G.Y([f\"a-{1}\", f\"b-{2}\"]); println(f\"m:{c.show()}\"); }
",
            "v=[a-1, b-2]\nm:1\n",
        ),
        (
            "array-string-rebind",
            "fn main() { let c: G[Array[String, 2]] = G.Y([f\"a-{1}\", f\"b-{2}\"]); println(f\"m:{c.keep()}\"); }
",
            "moved\nm:3\n",
        ),
        (
            "array-string-discard",
            "fn main() { let c: G[Array[String, 2]] = G.Y([f\"a-{1}\", f\"b-{2}\"]); println(f\"m:{c.drop_it()}\"); }
",
            "m:6\n",
        ),
        (
            "vec-in-a-loop",
            "fn main() { let a: G[Vec[i64]] = G.Y([1, 2, 3]); a.show(); for i in 0..3 { let g: G[Vec[i64]] = G.Y([i, i]); g.show(); } }
",
            "v=[1, 2, 3]\nv=[0, 0]\nv=[1, 1]\nv=[2, 2]\n",
        ),
        (
            "vec-string-rebind",
            "fn main() { let c: G[Vec[String]] = G.Y([f\"v-{1}\", f\"w-{2}\"]); println(f\"m:{c.keep()}\"); }
",
            "moved\nm:3\n",
        ),
        (
            "concrete-impl-array",
            "enum H[T] { Y(T), N }
impl H[Array[String, 2]] { fn seen(self) -> i64 { match self { H.Y(v) => { println(\"seen\"); return 4; } H.N => { return 0; } } } }
fn main() { let c: H[Array[String, 2]] = H.Y([f\"a-{1}\", f\"b-{2}\"]); println(f\"m:{c.seen()}\"); }
",
            "seen\nm:4\n",
        ),
        (
            "control:no-match",
            "fn main() { let c: G[Array[String, 2]] = G.Y([f\"a-{1}\", f\"b-{2}\"]); println(f\"m:{c.mlen()}\"); }
",
            "m:7\n",
        ),
        (
            "control:string",
            "fn main() { let c: G[String] = G.Y(f\"s-{1}\"); println(f\"m:{c.show()}\"); }
",
            "v=s-1\nm:1\n",
        ),
    ];
    for (label, body, want) in cells {
        let prog = format!("{PRE}{body}");
        let lines: Vec<&str> = want.lines().collect();
        assert_clean_asan_run(&prog, &lines, &format!("b2026-09-20-39-{label}"));
    }
}
