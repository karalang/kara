//! B-2026-11-04-39: a body whose result is a two-scalar tuple or struct
//! returns it in a register pair, and a body whose result is larger takes
//! a pointer to it first and keeps its parameters in registers. Either
//! result used to send the whole call through memory, so a `ref Vec`
//! parameter arrived as a pointer to a slot holding the reference (katas
//! 337 and 333's recursive walks). Each program prints what the MIR
//! interpreter prints, and the IR shows the signatures, and that the
//! runtime's vector growth is declared to keep no pointer (kata 343).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"
struct Node {
    val: i64,
    mut left: i64,
    mut right: i64,
}

struct Span {
    lo: i64,
    ok: bool,
}

struct Info {
    bst: bool,
    size: i64,
    lo: i64,
    hi: i64,
}

fn pair(nodes: ref Vec[Node], node: i64) -> (i64, i64) {
    if node == -1 {
        return (0, 0);
    }
    let (lr, ls) = pair(nodes, nodes[node].left);
    let (rr, rs) = pair(nodes, nodes[node].right);
    return (nodes[node].val + ls + rs, lr.max(ls) + rr.max(rs));
}

fn span(nodes: ref Vec[Node], node: i64) -> Span {
    if node == -1 {
        return Span { lo: 1000, ok: true };
    }
    let l = span(nodes, nodes[node].left);
    let r = span(nodes, nodes[node].right);
    return Span { lo: l.lo.min(r.lo).min(nodes[node].val), ok: l.ok and r.ok and nodes[node].val > 0 };
}

fn info(nodes: ref Vec[Node], node: i64, best: mut ref i64) -> Info {
    let val = nodes[node].val;
    let mut size = 1;
    let mut lo = val;
    let mut hi = val;
    let mut bst = true;
    if nodes[node].left != -1 {
        let l = info(nodes, nodes[node].left, mut best);
        if l.bst and l.hi < val { size += l.size; lo = l.lo; } else { bst = false; }
    }
    if nodes[node].right != -1 {
        let r = info(nodes, nodes[node].right, mut best);
        if r.bst and r.lo > val { size += r.size; hi = r.hi; } else { bst = false; }
    }
    if bst and size > best {
        *best = size;
    }
    return Info { bst: bst, size: size, lo: lo, hi: hi };
}

fn label(nodes: ref Vec[Node], node: i64, depth: i64) -> String {
    if node == -1 {
        return "-";
    }
    let l = label(nodes, nodes[node].left, depth + 1);
    let r = label(nodes, nodes[node].right, depth + 1);
    return "(" + l + " " + nodes[node].val.to_string() + "@" + depth.to_string() + " " + r + ")";
}

fn swap_pair(p: (i64, bool)) -> (bool, i64) {
    return (p.1, p.0 * 2);
}

fn main() {
    let mut nodes: Vec[Node] = Vec.new();
    nodes.push(Node { val: 5, left: 1, right: 2 });
    nodes.push(Node { val: 3, left: 3, right: -1 });
    nodes.push(Node { val: 8, left: -1, right: 4 });
    nodes.push(Node { val: 1, left: -1, right: -1 });
    nodes.push(Node { val: 9, left: -1, right: -1 });
    let (r, s) = pair(nodes, 0);
    println(r.max(s));
    let sp = span(nodes, 0);
    println(sp.lo);
    println(sp.ok);
    let mut best = 0;
    let i = info(nodes, 0, mut best);
    println(i.size);
    println(best);
    println(label(nodes, 0, 0));
    let (b, d) = swap_pair((21, true));
    println(b);
    println(d);
}
"#;

const WANT: &str = "15\n1\ntrue\n5\n5\n(((- 1@2 -) 3@1 -) 5@0 (- 8@1 (- 9@2 -)))\ntrue\n42\n";

#[test]
fn mir_pair_results_and_sret_results_keep_parameters_in_registers() {
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    for want in [
        "define internal { i64, i64 } @kara.pair(ptr",
        "define internal { i64, i8 } @kara.span(ptr",
        "define internal void @kara.info(ptr",
        "define internal void @kara.label(ptr",
        "define internal { i8, i64 } @kara.swap_pair({ i64, i8 }",
        // The runtime keeps no pointer it is passed to grow a vector, so
        // a local vector's slot is out of reach of stores through its
        // buffer (kata 343's `best[i] = whole`).
        "declare void @karac_mir_vec_reserve(ptr captures(none), i64, i64)",
    ] {
        assert!(ir.contains(want), "no `{want}` in:\n{ir}");
    }
    // `info` takes its result slot, then `nodes` and `best` as the
    // references themselves (dereferenceable for a `Vec` and an `i64`,
    // B-2026-11-05-4) and `node` as an integer.
    // `nodes` is `noalias` as well, a `Vec` header frozen by its borrow
    // (B-2026-11-09-8); `best`, a `mut ref i64`, takes no borrow
    // flag and so stays unmarked (B-2026-11-09-2).
    let refs =
        "ptr noalias align 8 dereferenceable(24) %1, i64 %2, ptr align 8 dereferenceable(8) %3)";
    assert!(
        ir.contains(&format!("@kara.info(ptr noalias %0, {refs}"))
            || ir.contains(&format!("@kara.info(ptr %0, {refs}")),
        "info's parameters are not in registers:\n{ir}"
    );
    let dir = std::env::temp_dir().join(format!("karac-mir-pair-sret-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    if linked.is_some() {
        let out = Command::new(&exe).output().expect("run");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8(out.stdout).unwrap(), WANT);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
