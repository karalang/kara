//! Prefix collection literals (`Vec[a, b]`, `Set[a]`, `Map[k: v]`,
//! `Array[v; n]`, `VecDeque[..]`, `SortedMap[..]`) are deprecated
//! (design.md § Collection literals, B-2026-11-16-255): each warns, and the
//! fix rewrites it to a spelling that builds the same collection, so the
//! fixed program prints what the original did and warns no more.

use karac::{check_source, run_playground, PlaygroundDiagnostic};

/// The prefix-literal warnings `karac check` reports for `src` (the
/// playground's run keeps only errors).
fn prefix_warnings(src: &str) -> Vec<PlaygroundDiagnostic> {
    check_source(src)
        .into_iter()
        .filter(|d| d.message.starts_with("use of deprecated prefix literal"))
        .collect()
}

/// `src` with every prefix-literal fix applied.
fn fixed(src: &str) -> String {
    let mut fixes: Vec<_> = prefix_warnings(src)
        .into_iter()
        .flat_map(|d| d.fixes)
        .collect();
    fixes.sort_by_key(|f| std::cmp::Reverse(f.offset));
    let mut out = src.to_string();
    for f in fixes {
        out.replace_range(f.offset..f.offset + f.length, &f.replacement);
    }
    out
}

/// `src` warns `warnings` times, all with a fix; the fixed text is
/// `want`, warns no more, and prints what `src` printed.
fn fixes_to(src: &str, warnings: usize, want: &str) {
    let ws = prefix_warnings(src);
    assert_eq!(ws.len(), warnings, "{src}\n{ws:#?}");
    assert!(ws.iter().all(|w| !w.fixes.is_empty()), "{ws:#?}");
    let out = fixed(src);
    assert_eq!(out, want);
    let (before, after) = (run_playground(src), run_playground(&out));
    assert!(after.ok, "{out}\n{:#?}", after.diagnostics);
    assert_eq!(before.stdout, after.stdout, "{out}");
    assert!(prefix_warnings(&out).is_empty(), "{out}");
}

#[test]
fn prefix_vec_literals_fix_to_the_list_literal() {
    fixes_to(
        "fn main() {\n    let n = 3;\n    let a = Vec[1, 2];\n    let b = Vec[0; n];\n    \
         let c: Vec[i64] = Vec[];\n    println(f\"{a} {b} {c} {Vec[4, 5].len()}\");\n}\n",
        4,
        "fn main() {\n    let n = 3;\n    let a = [1, 2];\n    let b = Vec.filled(n, 0);\n    \
         let c: Vec[i64] = Vec.new();\n    println(f\"{a} {b} {c} {[4, 5].len()}\");\n}\n",
    );
}

#[test]
fn prefix_set_and_map_literals_fix_to_from_or_the_literal() {
    fixes_to(
        "fn main() {\n    let s = Set[3, 1];\n    let t: Set[i64] = Set[2];\n    \
         let m = Map[\"a\": 1, \"b\": 2];\n    let o = SortedMap[\"b\": 2, \"a\": 1];\n    \
         let e: Map[String, i64] = Map[];\n    \
         println(f\"{s.len()} {t.contains(2)} {m.len()} {o} {e.len()}\");\n}\n",
        5,
        "fn main() {\n    let s = Set.from([3, 1]);\n    let t: Set[i64] = [2];\n    \
         let m = [\"a\": 1, \"b\": 2];\n    let o = SortedMap.from([(\"b\", 2), (\"a\", 1)]);\n    \
         let e: Map[String, i64] = Map.new();\n    \
         println(f\"{s.len()} {t.contains(2)} {m.len()} {o} {e.len()}\");\n}\n",
    );
}

#[test]
fn prefix_array_literals_fix_where_the_array_is_expected() {
    fixes_to(
        "fn main() {\n    let z = Array[7; 3];\n    let a: Array[i64, 2] = Array[1, 2];\n    \
         let d: VecDeque[i64] = VecDeque[4, 5];\n    println(f\"{z[2]} {a[1]} {d.len()}\");\n}\n",
        3,
        "fn main() {\n    let z = [7; 3];\n    let a: Array[i64, 2] = [1, 2];\n    \
         let d: VecDeque[i64] = [4, 5];\n    println(f\"{z[2]} {a[1]} {d.len()}\");\n}\n",
    );
    // With nothing expecting an array, `[1, 2]` would be a `Vec`: the
    // warning stands without a fix.
    let ws =
        prefix_warnings("fn main() {\n    let a = Array[1, 2];\n    println(f\"{a[0]}\");\n}\n");
    assert_eq!(ws.len(), 1, "{ws:#?}");
    assert!(ws[0].fixes.is_empty(), "{ws:#?}");
    assert!(ws[0].message.contains("Array[T, N]"), "{ws:#?}");
}

#[test]
fn prefix_literal_warning_is_silenced_by_allow_deprecated() {
    let ws = prefix_warnings(
        "#[allow(deprecated)]\nfn main() {\n    let a = Vec[1, 2];\n    println(f\"{a}\");\n}\n",
    );
    assert!(ws.is_empty(), "{ws:#?}");
}
