//! B-2026-09-20-42 -- a `ref` binding extends the live range of the binding it
//! borrows from, so the borrowed container's element `Drop` body runs after
//! the last read through the borrow, not at the borrow's `let`.

use super::*;

const PRE: &str = "struct D { id: i64 }
impl Drop for D { fn drop(mut ref self) { println(f\"d{self.id}\"); } }
fn mid(n: i64) { println(f\"m{n}\"); }
";

/// B-2026-09-20-42 — `d`'s range ended at its last SYNTACTIC mention (`ref
/// d[0]`), so `d31` printed before the read through `e`, on the interpreter and
/// compiled alike. Each cell reads the same on both. `control:` cells were
/// right before.
#[test]
fn a_ref_binding_keeps_the_borrowed_container_alive() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "row-cell",
            "fn main() { { let mut d: Vec[Vec[D]] = [[D { id: 31 }]]; let e: ref Vec[D] = ref d[0]; println(f\"x:{e[0].id}\"); } println(\"end\"); }
",
            "x:31\nd31\nend\n",
        ),
        (
            "read-between-calls",
            "fn main() { let d: Vec[Vec[D]] = [[D { id: 1 }]]; let e: ref Vec[D] = ref d[0]; mid(1); println(f\"x:{e[0].id}\"); mid(2); }
",
            "m1\nx:1\nd1\nm2\n",
        ),
        (
            "read-inside-an-if",
            "fn main() { let d: Vec[Vec[D]] = [[D { id: 3 }]]; let e: ref Vec[D] = ref d[0]; if e.len() > 0 { println(f\"in-if:{e[0].id}\"); } mid(4); }
",
            "in-if:3\nd3\nm4\n",
        ),
        (
            "for-over-the-borrow",
            "fn main() { let d: Vec[Vec[D]] = [[D { id: 4 }], [D { id: 5 }]]; let e: ref Vec[D] = ref d[1]; mid(5); for x in e { println(f\"for:{x.id}\"); } mid(6); }
",
            "m5\nfor:5\nd4\nd5\nm6\n",
        ),
        (
            "borrow-read-in-the-tail",
            "fn main() { let v = { let d: Vec[Vec[D]] = [[D { id: 7 }]]; let e: ref Vec[D] = ref d[0]; mid(7); e[0].id }; println(f\"v:{v}\"); }
",
            "m7\nd7\nv:7\n",
        ),
        (
            "control:shadowed-borrower-is-not-a-borrow",
            "fn main() { let d: Vec[Vec[D]] = [[D { id: 2 }]]; let e: ref Vec[D] = ref d[0]; let e = 8; println(f\"e:{e}\"); mid(3); }
",
            "d2\ne:8\nm3\n",
        ),
        (
            "control:direct-use-after",
            "fn main() { let d: Vec[Vec[D]] = [[D { id: 41 }]]; let e: ref Vec[D] = ref d[0]; println(f\"x:{e[0].id}\"); println(f\"len:{d.len()}\"); }
",
            "x:41\nlen:1\nd41\n",
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
