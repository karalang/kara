//! B-2026-10-03-23 -- a tuple literal stored into a container whose element
//! type has narrower integer fields is laid out at the container's declared
//! widths, not at the literal's default `i64` widths.

use super::*;

/// B-2026-10-03-23 — every container position a tuple literal reaches with a
/// narrower declared element type. Before, compiled: `vec![(1, 2), (3, 4)]`
/// as `Vec[(i32, i32)]` printed `[(1, 0), (2, 0)]`, `push`, `insert`, an index
/// assignment and a `VecDeque` push read the neighbour's half back, a `Map`
/// value read garbage, a `SortedSet` lost a member, and a nested `vec!` or
/// an inner tuple read the wrong fields. The expected text is
/// `--interp`'s output.
#[test]
fn e2e_narrow_tuple_literal_into_container_keeps_declared_layout() {
    let out = run_program(
        r#"fn main() {
    let a: Vec[(i32, i32)] = vec![(1, 2), (3, 4)];
    println(f"vec_lit {a}");
    let mut b: Vec[(i32, i32)] = Vec.new();
    b.push((1, 2));
    b.push((3, 4));
    println(f"push {b}");
    b.insert(0, (5, 6));
    println(f"insert {b}");
    b[1] = (7, 8);
    println(f"index_assign {b}");
    let mut d: VecDeque[(i32, i32)] = VecDeque.new();
    d.push_back((1, 2));
    d.push_front((3, 4));
    println(f"deque {d[0].0} {d[0].1} {d[1].0} {d[1].1}");
    let mut m: Map[i64, (i32, i32)] = Map.new();
    m.insert(1, (1, 2));
    let mv = m.get(1).unwrap();
    println(f"map_val {mv.0} {mv.1}");
    let mut s: SortedSet[(i32, i32)] = SortedSet.new();
    s.insert((1, 2));
    s.insert((3, 4));
    println(f"set {s.len()} {s.contains((1, 2))} {s.contains((3, 4))}");
    let o: Option[(i32, i32)] = Some((1, 2));
    match o {
        Some(p) => println(f"opt {p.0} {p.1}"),
        None => {}
    }
    let arr: Array[(i32, i32), 2] = [(1, 2), (3, 4)];
    println(f"arr {arr[1].0} {arr[1].1}");
    let nested: Vec[Vec[(i32, i32)]] = vec![vec![(1, 2), (3, 4)]];
    println(f"nested {nested}");
    let mut c: Vec[(u8, u8)] = Vec.new();
    c.push((200, 7));
    println(f"u8 {c}");
    let nn: Vec[Vec[(i32, i32)]] = vec![vec![(1, 2), (3, 4)], vec![(5, 6)]];
    println(f"nested2 {nn}");
    let t: Vec[(i32, (u8, i64))] = vec![(1, (200, 3)), (4, (5, 6))];
    println(f"inner_tuple {t}");
    let fl: Vec[(f64, i32)] = vec![(1.5, 2), (3, 4)];
    println(f"float {fl[1].0} {fl[1].1}");
}
"#,
    );
    let Some(out) = out else { return };
    assert_eq!(out, "vec_lit [(1, 2), (3, 4)]\npush [(1, 2), (3, 4)]\ninsert [(5, 6), (1, 2), (3, 4)]\nindex_assign [(5, 6), (7, 8), (3, 4)]\ndeque 3 4 1 2\nmap_val 1 2\nset 2 true true\nopt 1 2\narr 3 4\nnested [[(1, 2), (3, 4)]]\nu8 [(200, 7)]\nnested2 [[(1, 2), (3, 4)], [(5, 6)]]\ninner_tuple [(1, (200, 3)), (4, (5, 6))]\nfloat 3 4\n");
}
