//! B-2026-09-20-36 -- an index store `a[i] = p` whose source is read after
//! the store keeps the source's value, on both backends.

use super::*;

const PRE: &str = "struct P { id: i64, s: String }\nimpl Drop for P { fn drop(mut ref self) { println(f\"dP{self.id}\") } }\nstruct Q { id: i64, s: String }\nstruct H { xs: Vec[P] }\nstruct D { k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nstruct W { d: D, s: String }\nenum Ws { Full(P), Empty }\n";

/// B-2026-09-20-36 — `a[0] = p; println(p.s)` printed an empty `p.s` on every
/// compiled surface: the index-assign was the one owning sink
/// B-2026-09-15-16's defensive copy never reached, so the store zeroed the
/// source's caps and lengths. The slot now takes `uam_defensive_copy`'s copy
/// and the source keeps its buffers; only its bodies move. `variant-source-memory`
/// is the sibling sink, whose copy left the source's buffers unowned.
/// `control:` cells were right before; `control:push` and `control:map` still
/// leak their source (separate row) and are here for output only.
#[test]
fn index_store_keeps_a_source_read_after_it() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "vec",
            "fn main() { let mut a: Vec[P] = [P { id: 0, s: f\"zero\" }]; let p = P { id: 4, s: f\"four\" }; a[0] = p; println(f\"p:{p.s}\"); println(f\"a:{a[0].s}\"); println(\"end\") }\n",
            "dP0\np:four\na:four\ndP4\nend\n",
        ),
        (
            "array",
            "fn main() { let mut a: Array[P, 1] = [P { id: 0, s: f\"zero\" }]; let p = P { id: 4, s: f\"four\" }; a[0] = p; println(f\"p:{p.s}\"); println(f\"a:{a[0].s}\"); println(\"end\") }\n",
            "dP0\np:four\na:four\ndP4\nend\n",
        ),
        (
            "nested",
            "fn main() { let mut d: Vec[Vec[P]] = [[P { id: 0, s: f\"zero\" }]]; let p = P { id: 4, s: f\"four\" }; d[0][0] = p; println(f\"p:{p.s}\"); println(\"end\") }\n",
            "dP0\ndP4\np:four\nend\n",
        ),
        (
            "field-root",
            "fn main() { let mut h = H { xs: [P { id: 0, s: f\"zero\" }] }; let p = P { id: 4, s: f\"four\" }; h.xs[0] = p; println(f\"p:{p.s}\"); println(f\"h:{h.xs[0].s}\"); println(\"end\") }\n",
            "dP0\np:four\nh:four\ndP4\nend\n",
        ),
        (
            "no-drop",
            "fn main() { let mut a: Vec[Q] = [Q { id: 0, s: f\"zero\" }]; let q = Q { id: 4, s: f\"four\" }; a[0] = q; println(f\"q:{q.s}\"); println(f\"a:{a[0].s}\"); println(\"end\") }\n",
            "q:four\na:four\nend\n",
        ),
        (
            "mutate-after",
            "fn main() { let mut a: Vec[Q] = [Q { id: 0, s: f\"zero\" }]; let mut q = Q { id: 4, s: f\"four\" }; a[0] = q; q.s.push_str(\"!\"); println(f\"q:{q.s}\"); println(f\"a:{a[0].s}\"); println(\"end\") }\n",
            "q:four!\na:four\nend\n",
        ),
        (
            "field-bodies-reuse",
            "fn main() { let mut a: Vec[W] = [W { d: D { k: 0 }, s: f\"zero\" }]; let w = W { d: D { k: 4 }, s: f\"four\" }; a[0] = w; println(f\"w:{w.s}\"); println(f\"a:{a[0].s}\"); println(\"end\") }\n",
            "dD0\nw:four\na:four\ndD4\nend\n",
        ),
        (
            "string",
            "fn main() { let mut a: Vec[String] = [f\"zero\"]; let s = f\"four\"; a[0] = s; println(f\"s:{s}\"); println(f\"a:{a[0]}\"); println(\"end\") }\n",
            "s:four\na:four\nend\n",
        ),
        (
            "vec-elem",
            "fn main() { let mut a: Vec[Vec[i64]] = [[0]]; let v: Vec[i64] = [4, 5]; a[0] = v; println(f\"v:{v.len()}\"); println(f\"a:{a[0].len()}\"); println(\"end\") }\n",
            "v:2\na:2\nend\n",
        ),
        (
            "variant-source-memory",
            "fn main() { let p = P { id: 1, s: f\"one\" }; let w: Ws = Ws.Full(p); println(f\"p:{p.s}\"); println(\"end\") }\n",
            "dP1\np:one\nend\n",
        ),
        (
            "control:no-reuse",
            "fn main() { let mut a: Vec[P] = [P { id: 0, s: f\"zero\" }]; let p = P { id: 4, s: f\"four\" }; a[0] = p; println(f\"a:{a[0].s}\"); println(\"end\") }\n",
            "dP0\na:four\ndP4\nend\n",
        ),
        (
            "control:field-bodies",
            "fn main() { let mut a: Vec[W] = [W { d: D { k: 0 }, s: f\"zero\" }]; let w = W { d: D { k: 4 }, s: f\"four\" }; a[0] = w; println(f\"a:{a[0].s}\"); println(\"end\") }\n",
            "dD0\na:four\ndD4\nend\n",
        ),
        (
            "control:push",
            "fn main() { let mut v: Vec[P] = []; let p = P { id: 2, s: f\"two\" }; v.push(p); println(f\"p:{p.s}\"); println(f\"v:{v[0].s}\"); println(\"end\") }\n",
            "p:two\nv:two\ndP2\nend\n",
        ),
        (
            "control:map",
            "fn main() { let mut m: Map[i64, P] = Map.new(); let p = P { id: 3, s: f\"three\" }; m.insert(9, p); println(f\"p:{p.s}\"); println(\"end\") }\n",
            "dP3\np:three\nend\n",
        ),
    ];
    for (label, body, want) in cells {
        let prog = format!("{PRE}{body}");
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
