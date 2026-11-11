//! `std.cmp.Reverse[T]` (library/core-types.md § Reverse, the review
//! ruling of 2026-11-11 on B-2026-11-10-41): a struct with one field,
//! `value`, ordered by `T`'s order reversed. It orders a heap's item, a
//! sort key or a sorted table's key wholly or in part, on every lane.
//!
//! A part with its own `cmp` (a `Reverse`, or any hand-written `Ord`)
//! orders a tuple, array, `Vec`, `Option` or derived-`Ord` struct holding
//! it, as the derived `cmp` does: `sort`, `sort_by_key`, `binary_search`,
//! `is_sorted` and sorted tables compared the holder's parts structurally
//! instead (B-2026-11-11-60).
//!
//! `karac fix` carries Rust's spelling over: `use std::cmp::Reverse;`
//! becomes `import std.cmp.Reverse;` where a use stays (an unimported use
//! gets the import), `Reverse(x)` becomes `Reverse { value: x }` in an
//! expression and in a pattern, and `r.0` becomes `r.value`. The fixes
//! that drop a heap's whole-item `Reverse` (kata 1057) and rewrite a
//! whole-`Reverse` sort key (kata 1058) are unchanged.

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn karac_at(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-std-cmp-reverse-{name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Checks clean and prints `stdout` on every MIR lane this build has.
fn runs(name: &str, src: &str, stdout: &str) {
    let dir = scratch(name);
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let check = karac_at(&dir, &["check", "cell.kara"]);
    assert!(check.status.success(), "{}", text(&check));
    let lanes: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"]
    } else {
        &["__mir-run"]
    };
    for lane in lanes {
        let run = karac_at(&dir, &[lane, "cell.kara"]);
        assert!(run.status.success(), "{lane}: {}", text(&run));
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            stdout,
            "{lane}: {}",
            text(&run)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every check's diagnostics, the text `karac fix` leaves, whether that
/// text checks clean, and what it prints under the MIR interpreter.
fn mend(name: &str, src: &str) -> (String, String, bool, String) {
    let dir = scratch(name);
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let mut first = String::new();
    for _ in 0..6 {
        first += &text(&karac_at(&dir, &["check", "cell.kara"]));
        let before = std::fs::read_to_string(&file).unwrap();
        karac_at(&dir, &["fix", "cell.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let fixed = std::fs::read_to_string(&file).unwrap();
    let clean = karac_at(&dir, &["check", "cell.kara"]).status.success();
    let out =
        String::from_utf8_lossy(&karac_at(&dir, &["__mir-run", "cell.kara"]).stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    (first, fixed, clean, out)
}

#[test]
fn reverse_orders_heaps_sort_keys_and_comparisons() {
    runs(
        "reverse_orders_heaps_sort_keys_and_comparisons",
        r#"import std.cmp.Reverse;

fn main() {
    let mut pq: PriorityQueue[(Reverse[i64], String)] = PriorityQueue.max_first();
    pq.push((Reverse { value: 3 }, "c".to_string()));
    pq.push((Reverse { value: 1 }, "a".to_string()));
    pq.push((Reverse { value: 1 }, "b".to_string()));
    pq.push((Reverse { value: 2 }, "z".to_string()));
    while not pq.is_empty() {
        let (Reverse { value: s }, n) = pq.pop().unwrap();
        print(f"{s}{n} ");
    }
    println("");
    let mut w: PriorityQueue[(i64, Reverse[String])] = PriorityQueue.max_first();
    w.push((5, Reverse { value: "b".to_string() }));
    w.push((5, Reverse { value: "a".to_string() }));
    w.push((7, Reverse { value: "x".to_string() }));
    while let Some((s, r)) = w.pop() {
        print(f"{s}{r.value} ");
    }
    println("");
    let mut v = [2, 3, 1].to_vec();
    v.sort_by_key(|x| Reverse { value: x });
    let mut names = ["b", "a", "c"].to_vec();
    names.sort_by_key(|n| (Reverse { value: n.len() }, n.to_string()));
    let r = Reverse { value: 4 };
    let c = r;
    let xs = [3, 1, 2];
    let best = xs.iter().max_by_key(|x| Reverse { value: *x });
    println(f"{v} {names} {r} {c.value} {Reverse { value: 1 } < Reverse { value: 0 }} {r == Reverse { value: 4 }} {best.unwrap()}");
}
"#,
        r#"1b 1a 2z 3c 
7x 5a 5b 
[3, 2, 1] [a, b, c] 4 4 true true 1
"#,
    );
}

#[test]
fn a_reversed_part_orders_the_tuple_key_it_is_in() {
    runs(
        "a_reversed_part_orders_the_tuple_key_it_is_in",
        r#"import std.cmp.Reverse;

#[derive(Clone)]
struct P { score: i64, name: String }

fn main() {
    let mut ps = [P { score: 2, name: "b".to_string() }, P { score: 3, name: "a".to_string() }, P { score: 2, name: "a".to_string() }].to_vec();
    ps.sort_by_key(|p| (Reverse { value: p.score }, p.name.clone()));
    for p in ps.iter() { print(f"{p.score}{p.name} "); }
    println("");
    let mut m: SortedMap[(Reverse[i64], i64), String] = SortedMap.new();
    m.insert((Reverse { value: 1 }, 0), "one".to_string());
    m.insert((Reverse { value: 3 }, 0), "three".to_string());
    m.insert((Reverse { value: 2 }, 0), "two".to_string());
    for (k, v) in m.iter() { print(f"{k.0.value}{v} "); }
    println(f"{m.get((Reverse { value: 2 }, 0))}");
    let mut t = [(Reverse { value: 1 }, 2), (Reverse { value: 3 }, 1), (Reverse { value: 1 }, 1)].to_vec();
    t.sort();
    for (r, x) in t.iter() { print(f"{r.value}/{x} "); }
    println(f"{t.is_sorted()} {t.binary_search((Reverse { value: 1 }, 1))} {t.binary_search((Reverse { value: 1 }, 2))}");
}
"#,
        r#"3a 2a 2b 
3three 2two 1one Some(two)
3/1 1/1 1/2 true Some(1) Some(2)
"#,
    );
}

#[test]
fn a_hand_written_ord_orders_the_tuple_array_vec_option_and_struct_holding_it() {
    runs(
        "a_hand_written_ord_orders_the_tuple_array_vec_option_and_struct_holding_it",
        r#"import std.cmp.Reverse;

#[derive(PartialEq, Eq, Clone)]
struct Down { v: i64 }
impl PartialOrd for Down {
    fn partial_cmp(ref self, other: ref Down) -> Option[Ordering] { other.v.partial_cmp(self.v) }
}
impl Ord for Down {
    fn cmp(ref self, other: ref Down) -> Ordering { other.v.cmp(self.v) }
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
struct Row { r: Reverse[i64], n: i64 }

fn main() {
    let mut b = [2, 3, 1].to_vec();
    b.sort_by_key(|x| (Down { v: x }, 0));
    let mut s: SortedSet[(Down, i64)] = SortedSet.new();
    s.insert((Down { v: 1 }, 0));
    s.insert((Down { v: 3 }, 0));
    s.insert((Down { v: 2 }, 0));
    print(f"{b} ");
    for (d, _) in s.iter() { print(f"{d.v} "); }
    println(f"{s.contains((Down { v: 2 }, 0))}");
    let mut o = [Some(Reverse { value: 1 }), None, Some(Reverse { value: 3 })].to_vec();
    o.sort();
    for x in o.iter() { match x { Some(r) => print(f"{r.value} "), None => print("none ") } }
    let mut a = [[Reverse { value: 1 }, Reverse { value: 0 }], [Reverse { value: 2 }, Reverse { value: 0 }]].to_vec();
    a.sort();
    let mut rows = [Row { r: Reverse { value: 1 }, n: 0 }, Row { r: Reverse { value: 2 }, n: 0 }].to_vec();
    rows.sort();
    let mut vs = [[Reverse { value: 1 }].to_vec(), [Reverse { value: 2 }].to_vec()].to_vec();
    vs.sort();
    println(f"{a[0][0].value} {rows[0].r.value} {vs[0][0].value}");
}
"#,
        r#"[3, 2, 1] 3 2 1 true
none 3 1 2 2 2
"#,
    );
}

#[test]
fn a_generic_cmp_at_a_string_is_lowered() {
    runs(
        "a_generic_cmp_at_a_string_is_lowered",
        r#"struct W[T] { v: T }
impl[T: Ord] W[T] {
    fn rev(ref self, o: ref W[T]) -> Ordering { o.v.cmp(self.v) }
}
fn less[T: Ord](a: T, b: T) -> bool { a.cmp(b) == Ordering.Less }
fn main() {
    let a = W { v: "a".to_string() };
    let b = W { v: "b".to_string() };
    println(f"{a.rev(b) == Ordering.Greater} {W { v: 1 }.rev(W { v: 2 }) == Ordering.Greater} {less(1, 2)} {less("b", "a")}");
}
"#,
        r#"true true true false
"#,
    );
}

#[test]
fn rust_reverse_on_a_part_of_a_heap_item_mends_to_std_cmp_reverse() {
    // Kata 2102's Rust template: a `Reverse` on one part of each heap's item.
    let src = "use std::cmp::Reverse;\nuse std::collections::BinaryHeap;\n\nstruct SORTracker {\n    served: BinaryHeap<(Reverse<i32>, String)>,\n    waiting: BinaryHeap<(i32, Reverse<String>)>,\n}\n\nimpl SORTracker {\n    fn new() -> Self {\n        SORTracker { served: BinaryHeap::new(), waiting: BinaryHeap::new() }\n    }\n\n    fn add(&mut self, name: String, score: i32) {\n        self.served.push((Reverse(score), name));\n        let (Reverse(s), n) = self.served.pop().unwrap();\n        self.waiting.push((s, Reverse(n)));\n    }\n\n    fn get(&mut self) -> String {\n        let (s, Reverse(n)) = self.waiting.pop().unwrap();\n        self.served.push((Reverse(s), n.clone()));\n        n\n    }\n}\n\nfn main() {\n    let mut t = SORTracker::new();\n    t.add(\"bradford\".to_string(), 2);\n    t.add(\"branford\".to_string(), 3);\n    println!(\"{}\", t.get());\n    t.add(\"alps\".to_string(), 2);\n    println!(\"{}\", t.get());\n    let r = Reverse(4);\n    println!(\"{}\", r.0);\n}";
    let (first, fixed, clean, out) = mend("kata-2102", src);
    assert!(
        first.contains("Kāra's `Reverse` is `std.cmp.Reverse`")
            && first.contains("its pattern is `Reverse { value: … }`")
            && first.contains("field 0 is `.value`"),
        "{first}"
    );
    assert!(!first.contains("no such wrapper"), "{first}");
    assert!(clean, "still refused after fix:\n{fixed}");
    assert!(
        fixed.starts_with("import std.cmp.Reverse;\n\nstruct")
            && fixed.contains("PriorityQueue[(Reverse[i32], String)]")
            && fixed.contains("self.served.push((Reverse { value: score }, name));")
            && fixed.contains("let (Reverse { value: s }, n) = self.served.pop().unwrap();")
            && fixed.contains("let (s, Reverse { value: n }) = self.waiting.pop().unwrap();")
            && fixed.contains("PriorityQueue.max_first()")
            && fixed.contains("let r = Reverse { value: 4 };")
            && fixed.contains("{r.value}"),
        "{fixed}"
    );
    assert_eq!(out, "branford\nalps\n4\n", "{fixed}");
}

#[test]
fn an_unimported_reverse_gets_the_import() {
    // The kata's Rust arm as an earlier `karac fix` left it: the `use` line
    // deleted, each `Reverse` refused with no fix.
    let src = "// A heap of names, best score first, ties by name.\n\nfn main() {\n    let mut q: PriorityQueue[(i64, Reverse[String])] = PriorityQueue.max_first();\n    q.push((2, Reverse(\"b\".to_string())));\n    q.push((2, Reverse(\"a\".to_string())));\n    q.push((1, Reverse(\"c\".to_string())));\n    while let Some((s, Reverse(n))) = q.pop() {\n        print(f\"{s}{n} \");\n    }\n    println(\"\");\n}";
    let (first, fixed, clean, out) = mend("unimported", src);
    assert!(
        first.contains("`Reverse` is `std.cmp.Reverse`, which a program imports"),
        "{first}"
    );
    assert!(clean, "still refused after fix:\n{fixed}");
    assert!(
        fixed.starts_with(
            "// A heap of names, best score first, ties by name.\n\nimport std.cmp.Reverse;\n\nfn main()"
        ) && fixed.matches("import std.cmp.Reverse;").count() == 1
            && fixed.contains("q.push((2, Reverse { value: \"b\"")
            && fixed.contains("while let Some((s, Reverse { value: n })) = q.pop()"),
        "{fixed}"
    );
    assert_eq!(out, "2a 2b 1c \n", "{fixed}");
}

#[test]
fn a_whole_heap_item_reverse_is_still_dropped_and_its_import_deleted() {
    let src = "use std::cmp::{Ordering, Reverse};\nuse std::collections::BinaryHeap;\nfn main() {\n    let mut heap = BinaryHeap::new();\n    for d in [5, 2, 9] {\n        heap.push(Reverse(d));\n    }\n    while let Some(Reverse(d)) = heap.pop() {\n        print!(\"{} \", d);\n    }\n}";
    let (first, fixed, clean, out) = mend("whole-item", src);
    assert!(first.contains("no need of `Reverse` here"), "{first}");
    assert!(clean, "still refused after fix:\n{fixed}");
    assert!(
        !fixed.contains("Reverse") && fixed.contains("PriorityQueue.new()"),
        "{fixed}"
    );
    assert_eq!(out, "2 5 9 ", "{fixed}");
}

#[test]
fn a_program_struct_spelled_as_a_tuple_pattern_takes_its_struct_pattern() {
    let src = "struct P { a: i64, b: i64 }\nfn main() {\n    let P(x, y) = P { a: 1, b: 2 };\n    match P { a: 3, b: 4 } {\n        P(u, _) => println(f\"{x} {y} {u}\"),\n    }\n}";
    let (first, fixed, clean, out) = mend("program-struct", src);
    assert!(first.contains("so its pattern is `P { a: … }`"), "{first}");
    assert!(!first.contains("refutable pattern"), "{first}");
    assert!(clean, "still refused after fix:\n{fixed}");
    assert!(
        fixed.contains("let P { a: x, b: y } = P { a: 1, b: 2 };")
            && fixed.contains("P { a: u, b: _ } =>"),
        "{fixed}"
    );
    assert_eq!(out, "1 2 3\n", "{fixed}");
}

#[test]
fn where_one_reverse_stays_every_heap_keeps_its_reverse_items_and_pops_largest_first() {
    // A whole-item `Reverse` heap beside a sort key with `Reverse` on one
    // part: the part's `Reverse` stays, so the heap's do too, and the heap
    // is `max_first()`. Dropping the heap's would also need `new()`, and
    // mixing the two reversed the heap.
    let src = "use std::cmp::Reverse;\nuse std::collections::BinaryHeap;\nfn main() {\n    let mut h = BinaryHeap::new();\n    h.push(Reverse(3));\n    h.push(Reverse(1));\n    h.push(Reverse(2));\n    let mut order = Vec::new();\n    while let Some(Reverse(x)) = h.pop() {\n        order.push(x);\n    }\n    h.push(Reverse(7));\n    let rest: Vec<i64> = h.into_iter().map(|Reverse(x)| x).collect();\n    let mut v = [(1, 2), (1, 3), (0, 1)];\n    v.sort_by_key(|p| (p.0, Reverse(p.1)));\n    println(f\"{order} {v} {rest}\");\n}";
    let (_, fixed, clean, out) = mend("mixed", src);
    assert!(clean, "still refused after fix:\n{fixed}");
    assert!(
        fixed.starts_with("import std.cmp.Reverse;\n")
            && fixed.contains("PriorityQueue.max_first()")
            && fixed.contains("h.push(Reverse { value: 3 });")
            && fixed.contains("while let Some(Reverse { value: x }) = h.pop()")
            && fixed.contains(".map(|Reverse { value: x }| x)")
            && fixed.contains("(p.0, Reverse { value: p.1 })"),
        "{fixed}"
    );
    assert_eq!(out, "[1, 2, 3] [(0, 1), (1, 3), (1, 2)] [7]\n", "{fixed}");
}
