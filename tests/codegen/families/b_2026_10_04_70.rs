//! B-2026-10-04-70 -- a field store through a `Map` value (`m[1].n = 9`),
//! a `Map` held in a field, a `Vec` or a tuple, and a container in a tuple
//! that is itself an indexed element or a map value, failed the build while
//! `--interp` applied it. So did reading a field of a `Map` field's value
//! (`o.m[1].n`) and indexing a `Map` held in a tuple (`t.0[1]`).

use super::*;

/// Each store lands in the container, and reads it back: a named `Map`, a
/// `mut ref` map param (`String` key, `+=`), a map field from `main` and
/// from a `mut ref self` method, `Vec[Map]`, `Map[Map]`, `Map[Vec]`, a
/// `Vec` in an indexed tuple, a tuple in a map value, a map in a tuple, a
/// `Vec` in a tuple in a map value, an `Array` map value and a `SortedMap`.
#[test]
fn e2e_field_store_through_map_value() {
    let src = r#"struct Q { n: i64 }
struct P { n: i64, s: String, q: Q }
struct O { m: Map[i64, P], k: i64 }
impl O {
    fn set(mut ref self, k: i64) { self.m[k].n = 40; self.m[k].s = f"self{k}"; }
}
fn bump(m: mut ref Map[String, P]) { m["a"].s = f"z{9}"; m["a"].n += 3; }
fn mk(n: i64, t: String) -> P { P { n: n, s: t, q: Q { n: n * 10 } } }
fn main() {
    let mut m: Map[i64, P] = Map.new();
    m.insert(1, mk(5, f"a{1}"));
    m.insert(2, mk(6, f"b{2}"));
    m[1].n = 9;
    m[2].s = f"bb{2}";
    m[1].q.n += 1;
    println(f"a:{m[1].n} {m[1].s} {m[2].s} {m[1].q.n} {m[2].n}");
    let mut sm: Map[String, P] = Map.new();
    sm.insert(f"a", mk(5, f"q{1}"));
    bump(mut sm);
    println(f"b:{sm["a"].n} {sm["a"].s}");
    let mut o = O { m: Map.new(), k: 7 };
    o.m.insert(3, mk(1, f"o{3}"));
    o.m[3].n = 33;
    o.m[3].s = f"oo{3}";
    println(f"c:{o.m[3].n} {o.m[3].s} {o.k}");
    o.set(3);
    println(f"d:{o.m[3].n} {o.m[3].s}");
    let mut vm: Vec[Map[i64, P]] = Vec.new();
    let mut m2: Map[i64, P] = Map.new();
    m2.insert(4, mk(2, f"v{4}"));
    vm.push(m2);
    vm[0][4].n = 44;
    vm[0][4].s = f"vv{4}";
    println(f"e:{vm[0][4].n} {vm[0][4].s}");
    let mut mm: Map[i64, Map[i64, P]] = Map.new();
    let mut m3: Map[i64, P] = Map.new();
    m3.insert(2, mk(3, f"w{2}"));
    mm.insert(1, m3);
    mm[1][2].n = 12;
    mm[1][2].s = f"ww{2}";
    println(f"f:{mm[1][2].n} {mm[1][2].s}");
    let mut mv: Map[i64, Vec[P]] = Map.new();
    let mut ps: Vec[P] = Vec.new();
    ps.push(mk(4, f"x{0}"));
    mv.insert(1, ps);
    mv[1][0].n = 10;
    mv[1][0].s = f"xx{0}";
    println(f"g:{mv[1][0].n} {mv[1][0].s}");
    let mut vt: Vec[(Vec[P], i64)] = [([mk(5, f"t{1}")], 1)];
    vt[0].0[0].n = 9;
    vt[0].0[0].s = f"tt{1}";
    println(f"h:{vt[0].0[0].n} {vt[0].0[0].s} {vt[0].1}");
    let mut mt: Map[i64, (P, String)] = Map.new();
    mt.insert(1, (mk(5, f"i{1}"), f"j{1}"));
    mt[1].0.n = 15;
    mt[1].0.s = f"ii{1}";
    println(f"i:{mt[1].0.n} {mt[1].0.s} {mt[1].1}");
    let mut tm: (Map[i64, P], i64) = (Map.new(), 1);
    tm.0.insert(1, mk(6, f"k{1}"));
    tm.0[1].n = 16;
    tm.0[1].s = f"kk{1}";
    println(f"j:{tm.0[1].n} {tm.0[1].s} {tm.1}");
    let mut mvt: Map[i64, (Vec[P], i64)] = Map.new();
    mvt.insert(1, ([mk(7, f"l{1}")], 2));
    mvt[1].0[0].n = 17;
    mvt[1].0[0].s = f"ll{1}";
    println(f"k:{mvt[1].0[0].n} {mvt[1].0[0].s} {mvt[1].1}");
    let mut ma: Map[i64, Array[P, 1]] = Map.new();
    ma.insert(1, [mk(8, f"m{1}")]);
    ma[1][0].n = 18;
    ma[1][0].s = f"mm{1}";
    println(f"l:{ma[1][0].n} {ma[1][0].s}");
    let mut so: SortedMap[i64, P] = SortedMap.new();
    so.insert(2, mk(9, f"n{2}"));
    so[2].n = 19;
    so[2].s = f"nn{2}";
    println(f"m:{so[2].n} {so[2].s}");
}
"#;
    let want = "a:9 a1 bb2 51 6\nb:8 z9\nc:33 oo3 7\nd:40 self3\ne:44 vv4\nf:12 ww2\ng:10 xx0\nh:9 tt1 1\ni:15 ii1 j1\nj:16 kk1 1\nk:17 ll1 2\nl:18 mm1\nm:19 nn2\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// A store through a missing key panics, as the read does, instead of
/// writing anywhere.
#[test]
fn e2e_field_store_through_missing_map_key_panics() {
    let prog = "struct P { n: i64 }
fn main() { let mut m: Map[i64, P] = Map.new(); m.insert(1, P { n: 5 }); let k = 5 - 3; m[k].n = 9; println(\"unreached\") }
";
    let Some(run) = run_program_capturing(prog) else {
        return;
    };
    assert!(
        !run.status.success(),
        "expected a panic, got {:?}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("unreached"),
        "stdout: {:?}",
        run.stdout
    );
    assert!(
        run.stderr.contains("Map index: key not present"),
        "stderr: {:?}",
        run.stderr
    );
}
