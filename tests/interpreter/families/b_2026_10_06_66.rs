//! B-2026-10-06-66: interpreter SortedMap / SortedSet storage is shared

use super::*;

/// B-2026-10-06-66: `SortedMap` / `SortedSet` storage became an
/// `Arc<RwLock<..>>` (as `Map` / `Set` did in B-2026-08-21-8) so reading the
/// binding for a method call no longer copies the whole tree. Value semantics
/// now rest on `deep_clone_value` at the binding, exactly as for `Map`, so
/// these cells are the regression the change could introduce: a moved or
/// copied sorted container must not alias its source.
#[test]
fn sorted_map_binding_is_a_value_not_an_alias() {
    let out = run("fn main() {\n\
        let mut m: SortedMap[i64, i64] = SortedMap.new();\n\
        m.insert(1, 1);\n\
        let mut n = m;\n\
        n.insert(2, 2);\n\
        println(f\"m={m.len()} n={n.len()}\");\n\
    }");
    assert_eq!(out, "m=1 n=2\n");
}

#[test]
fn sorted_set_binding_is_a_value_not_an_alias() {
    let out = run("fn main() {\n\
        let mut s: SortedSet[i64] = SortedSet.new();\n\
        s.insert(1);\n\
        let mut t = s;\n\
        t.insert(2);\n\
        println(f\"s={s.len()} t={t.len()}\");\n\
    }");
    assert_eq!(out, "s=1 t=2\n");
}

#[test]
fn sorted_containers_in_a_struct_field_are_independent_per_struct() {
    let out = run("struct H { m: SortedMap[i64, i64], s: SortedSet[i64] }\n\
    fn main() {\n\
        let mut a = H { m: SortedMap.new(), s: SortedSet.new() };\n\
        a.m.insert(1, 1);\n\
        a.s.insert(1);\n\
        let mut b = a;\n\
        b.m.insert(2, 2);\n\
        b.s.insert(2);\n\
        b.m.clear();\n\
        println(f\"a={a.m.len()},{a.s.len()} b={b.m.len()},{b.s.len()}\");\n\
    }");
    assert_eq!(out, "a=1,1 b=0,2\n");
}

/// Every mutating method writes through the shared storage, and every copy
/// (`clone`, a struct literal of clones, a `Vec` clone, a `mut ref` param)
/// keeps its own tree. Output is the pre-change interpreter's, unchanged.
#[test]
fn sorted_containers_mutate_in_place_and_copy_by_value() {
    let out = run(r#"struct Holder {
    m: SortedMap[i64, i64],
    s: SortedSet[i64],
}

fn bump(m: mut ref SortedMap[i64, i64]) {
    m.insert(100, 1000);
}

fn takes(m: SortedMap[i64, i64]) -> i64 {
    return m.len();
}

fn main() {
    let mut a: SortedMap[i64, i64] = SortedMap.new();
    a.insert(1, 10);
    let mut b = a;
    b.insert(2, 20);
    let mut c = b.clone();
    c.insert(3, 30);
    println(f"b={b.len()} c={c.len()}");
    let mut s: SortedSet[i64] = SortedSet.new();
    s.insert(5);
    let mut t = s.clone();
    t.insert(6);
    println(f"s={s.len()} t={t.len()}");
    let mut h = Holder { m: c.clone(), s: t.clone() };
    h.m.insert(4, 40);
    h.s.insert(7);
    println(f"c={c.len()} h.m={h.m.len()} t={t.len()} h.s={h.s.len()}");
    let mut h2 = Holder { m: h.m.clone(), s: h.s.clone() };
    h2.m.remove(1);
    h2.s.remove(5);
    println(f"h.m={h.m.len()} h2.m={h2.m.len()} h.s={h.s.len()} h2.s={h2.s.len()}");
    bump(mut c);
    println(f"c after bump={c.len()} takes={takes(c.clone())}");
    let mut v: Vec[SortedSet[i64]] = Vec.new();
    v.push(t.clone());
    v[0].insert(99);
    println(f"t={t.len()} v0={v[0].len()}");
    let mut w = v.clone();
    w[0].insert(98);
    println(f"v0={v[0].len()} w0={w[0].len()}");
    h.m.clear();
    println(f"h.m={h.m.len()} h2.m={h2.m.len()}");
    let mut e: SortedMap[i64, i64] = SortedMap.new();
    e.entry(7).or_insert(0);
    e.entry(7).and_modify(|x| { x += 5; });
    if let Some((k, x)) = e.floor(9) {
        println(f"floor {k}={x}");
    }
}
"#);
    assert_eq!(
        out,
        "b=2 c=3\ns=1 t=2\nc=3 h.m=4 t=2 h.s=3\nh.m=4 h2.m=3 h.s=3 h2.s=2\n\
         c after bump=4 takes=4\nt=2 v0=3\nv0=3 w0=4\nh.m=0 h2.m=3\nfloor 7=5\n"
    );
}
