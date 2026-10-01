//! B-2026-09-20-43 -- an index-assign rooted at a field of `mut ref self` runs
//! the displaced element's `Drop` body at the store on every backend.

use super::*;

/// B-2026-09-20-43 — `--interp` printed `eight:82 d82`, losing `d81`, while
/// the compiled backends printed the due sequence. Fixed by B-2026-09-20-27's
/// nested-store walk, which takes a `self` root; this pins the flat spelling
/// on both backends. `control:` cells were right before.
#[test]
fn a_store_through_a_self_field_drops_the_displaced_element() {
    let pre = "struct D { id: i64 }
impl Drop for D { fn drop(mut ref self) { println(f\"d{self.id}\"); } }
struct Bag { xs: Vec[D] }
impl Bag {
    fn put(mut ref self, t: D) { self.xs[0] = t; }
    fn put2(mut ref self, i: i64, t: D) { self.xs[i] = t; }
}
";
    let cells: &[(&str, &str, &str)] = &[
        (
            "row-cell",
            "fn eight() { let mut b = Bag { xs: [D { id: 81 }] }; b.put(D { id: 82 }); println(f\"eight:{b.xs[0].id}\"); }
fn main() { eight(); }
",
            "d81\neight:82\nd82\n",
        ),
        (
            "index-by-param",
            "fn main() { let mut b = Bag { xs: [D { id: 1 }, D { id: 2 }] }; b.put2(1, D { id: 3 }); println(f\"x:{b.xs[1].id}\"); }
",
            "d2\nx:3\nd1\nd3\n",
        ),
        (
            "control:flat-root",
            "fn main() { let mut a: Vec[D] = [D { id: 91 }]; a[0] = D { id: 92 }; println(f\"x:{a[0].id}\"); }
",
            "d91\nx:92\nd92\n",
        ),
    ];
    for (label, body, want) in cells {
        let prog = format!("{pre}{body}");
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
