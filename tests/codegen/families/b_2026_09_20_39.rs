//! B-2026-09-20-39 / B-2026-09-27-121 -- a method on a GENERIC impl block
//! (`impl[T] G[T]`) sees its receiver's whole type argument, so a `Vec` or
//! `Array` payload bound by `match self` is typed, printed and freed as itself.

use super::*;

const PRE: &str = "enum G[T] { Y(T), N }
impl[T] G[T] {
    fn show(self) -> i64 { match self { G.Y(v) => { println(f\"v={v}\"); return 1; } G.N => { return 0; } } }
    fn rshow(ref self) -> i64 { match self { G.Y(v) => { println(f\"r={v}\"); return 2; } G.N => { return 0; } } }
    fn keep(self) -> i64 { match self { G.Y(v) => { let w = v; println(\"moved\"); return 3; } G.N => { return 0; } } }
}
";

/// B-2026-09-20-39 / B-2026-09-27-121 — the impl's `T` reached the monomorph
/// as a head NAME only (`Vec`, `Array`), so the arm binding fell to the erased
/// one-word width: a `Vec` payload printed raw bytes, an `Array` payload its
/// box pointer, and every `Vec` (and every `Array`) instantiation shared one
/// symbol. Each cell reads the same on the interpreter and compiled.
/// `control:` cells were right before.
#[test]
fn generic_impl_method_sees_the_whole_type_argument() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "vec-in-a-loop",
            "fn main() { let a: G[Vec[i64]] = G.Y([1, 2, 3]); a.show(); for i in 0..3 { let g: G[Vec[i64]] = G.Y([i, i]); g.show(); } }
",
            "v=[1, 2, 3]\nv=[0, 0]\nv=[1, 1]\nv=[2, 2]\n",
        ),
        (
            "five-instantiations",
            "fn main() {
    let a: G[Vec[i64]] = G.Y([1, 2, 3]);
    let b: G[Vec[String]] = G.Y([f\"x-{1}\"]);
    let c: G[Array[i64, 3]] = G.Y([4, 5, 6]);
    let d: G[Array[String, 2]] = G.Y([f\"p-{1}\", f\"q-{2}\"]);
    let e: G[Array[i64, 2]] = G.Y([7, 8]);
    println(f\"{a.show()} {b.show()} {c.show()} {d.show()} {e.show()}\");
}
",
            "v=[1, 2, 3]\nv=[x-1]\nv=[4, 5, 6]\nv=[p-1, q-2]\nv=[7, 8]\n1 1 1 1 1\n",
        ),
        (
            "array-string-owned-self",
            "fn main() { let c: G[Array[String, 2]] = G.Y([f\"a-{1}\", f\"b-{2}\"]); println(f\"m:{c.show()}\"); }
",
            "v=[a-1, b-2]\nm:1\n",
        ),
        (
            "array-i64-ref-self",
            "fn main() { let c: G[Array[i64, 2]] = G.Y([1, 2]); println(f\"m:{c.rshow()}\"); println(f\"m:{c.show()}\"); }
",
            "r=[1, 2]\nm:2\nv=[1, 2]\nm:1\n",
        ),
        (
            "vec-string-rebind",
            "fn main() { let c: G[Vec[String]] = G.Y([f\"v-{1}\"]); println(f\"m:{c.rshow()}\"); println(f\"m:{c.keep()}\"); }
",
            "r=[v-1]\nm:2\nmoved\nm:3\n",
        ),
        (
            "concrete-impl-array",
            "enum H[T] { Y(T), N }
impl H[Array[String, 2]] { fn show(self) -> i64 { match self { H.Y(v) => { println(f\"h={v}\"); return 4; } H.N => { return 0; } } } }
fn main() { let c: H[Array[String, 2]] = H.Y([f\"a-{1}\", f\"b-{2}\"]); println(f\"m:{c.show()}\"); }
",
            "h=[a-1, b-2]\nm:4\n",
        ),
        (
            "control:string",
            "fn main() { let c: G[String] = G.Y(f\"s-{1}\"); println(f\"m:{c.rshow()}\"); println(f\"m:{c.show()}\"); }
",
            "r=s-1\nm:2\nv=s-1\nm:1\n",
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
