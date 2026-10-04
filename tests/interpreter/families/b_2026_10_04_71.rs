//! B-2026-10-04-71: `--interp` evaluated a nested store's subscripts more than once

use super::*;

const IDX: &str = r#"struct P { n: i64, s: String }
fn idx(k: i64) -> i64 { println(f"idx{k}"); return k }
fn val(k: i64) -> i64 { println(f"val{k}"); return k }
"#;

/// B-2026-10-04-71: the store walk read its receiver more than once
/// (`set_index` once per Tensor / Map / Vec arm, `set_field` once to read and
/// once to write back), and each read re-ran every subscript inside it, so
/// `w[idx(0)][idx(1)] = 5` called `idx` four times and the field form five.
/// Each subscript now runs once, after the value and in source order, which
/// is what compiled code does for the plain stores.
#[test]
fn interp_nested_store_runs_each_subscript_once() {
    let out = run(&format!(
        "{IDX}fn main() {{
    let mut w: Vec[Vec[i64]] = [[1, 2], [3, 4]];
    w[idx(0)][idx(1)] = val(5);
    println(\"--\");
    let mut u: Vec[Vec[P]] = [[P {{ n: 1, s: \"a\" }}, P {{ n: 1, s: \"a\" }}], [P {{ n: 1, s: \"a\" }}, P {{ n: 1, s: \"a\" }}]];
    u[idx(1)][idx(0)].n = val(6);
    println(\"--\");
    let mut v: Vec[P] = [P {{ n: 1, s: \"a\" }}];
    v[idx(0)].n = val(77);
    println(f\"{{w[0][1]}} {{u[1][0].n}} {{v[0].n}}\");
}}
"
    ));
    assert_eq!(
        out,
        "val5\nidx0\nidx1\n--\nval6\nidx1\nidx0\n--\nval77\nidx0\n5 6 77\n"
    );
}

/// B-2026-10-04-71: a compound assignment is `a = a op b` (design.md §
/// Compound assignment), so its subscripts run once for the read and once for
/// the store, after the right-hand side, which is what compiled code does. The
/// store walk used to add more on top (`w[idx(1)][idx(0)] += val(2)` called
/// `idx` six times).
#[test]
fn interp_compound_store_runs_each_subscript_once_per_half() {
    let out = run(&format!(
        "{IDX}fn main() {{
    let mut w: Vec[Vec[i64]] = [[1, 2], [3, 4]];
    w[idx(1)][idx(0)] += val(2);
    println(\"--\");
    let mut u: Vec[Vec[P]] = [[P {{ n: 1, s: \"a\" }}, P {{ n: 1, s: \"a\" }}]];
    u[idx(0)][idx(1)].n += val(3);
    println(\"--\");
    let mut v: Vec[i64] = [1, 2];
    v[idx(1)] += val(4);
    println(f\"{{w[1][0]}} {{u[0][1].n}} {{v[1]}}\");
}}
"
    ));
    assert_eq!(
        out,
        "idx1\nidx0\nval2\nidx1\nidx0\n--\nidx0\nidx1\nval3\nidx0\nidx1\n--\nidx1\nval4\nidx1\n5 4 6\n"
    );
}

/// B-2026-10-04-71: a subscript that is not an integer (a `Map` key) is held
/// in a hidden local for the store walk, so it too runs once; field-rooted
/// chains (`h.xs[i][j]`, `h.t.1[i]`, `hs[i].xs[j][k]`) count the same.
#[test]
fn interp_map_key_and_field_rooted_stores_run_each_subscript_once() {
    let out = run(r#"struct H { xs: Vec[Vec[i64]], t: (i64, Vec[i64]) }
fn idx(k: i64) -> i64 { println(f"idx{k}"); return k }
fn key(k: String) -> String { println(f"key{k}"); return k }
fn main() {
    let mut mv: Vec[Map[String, i64]] = [Map.new()];
    mv[idx(0)][key("a")] = 1;
    mv[idx(0)][key("a")] = 2;
    let mut m: Map[String, i64] = Map.new();
    m[key("b")] = 3;
    let a = mv[0]["a"];
    println(f"{mv[0].len()} {a} {m.len()}");
    let mut h = H { xs: [[1, 2], [3, 4]], t: (0, [5, 6]) };
    h.xs[idx(1)][idx(0)] = 9;
    h.t.1[idx(1)] = 8;
    let mut hs: Vec[H] = [H { xs: [[1]], t: (0, [5]) }];
    hs[idx(0)].xs[idx(0)][idx(0)] = 7;
    println(f"{h.xs[1][0]} {h.t.1[1]} {hs[0].xs[0][0]}");
}
"#);
    assert_eq!(
        out,
        "idx0\nkeya\nidx0\nkeya\nkeyb\n1 2 1\nidx1\nidx0\nidx1\nidx0\nidx0\nidx0\n9 8 7\n"
    );
}

/// B-2026-10-04-71: a subscript that propagates with `?` still leaves the
/// store undone and the error returned, now that subscripts run before the
/// store walk rather than inside it.
#[test]
fn interp_store_subscript_that_propagates_skips_the_store() {
    let out = run(
        r#"fn pick(k: i64) -> Option[i64] { println(f"pick{k}"); if k > 5 { return None } return Some(k) }
fn store(w: mut ref Vec[Vec[i64]], k: i64) -> Option[i64] {
    w[pick(0)?][pick(k)?] = 7;
    w[pick(k - 1)?][0] += 1;
    return Some(w[0][0])
}
fn main() {
    let mut w: Vec[Vec[i64]] = [[1, 2]];
    println(f"{store(mut w, 1)}");
    println(f"{store(mut w, 9)}");
    println(f"{w[0][0]} {w[0][1]}");
}
"#,
    );
    assert_eq!(
        out,
        "pick0\npick1\npick0\npick0\nSome(2)\npick0\npick9\nNone\n2 7\n"
    );
}
