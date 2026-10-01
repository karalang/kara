//! B-2026-09-20-56 -- a `shared enum`'s heap-BOXED tuple payload is freed by
//! the box, and every arm spelling that hands it on stays balanced.

use super::*;

/// B-2026-09-20-56 — an `(Array[String, 2], i64)` payload rides in a malloc'd box behind the
/// shared enum's payload word. Before the fix nothing freed that box: the
/// box and every element string inside it leaked on every spelling. The
/// arms that hand the payload on (`let u = x`, `return x`) must not turn
/// that into a double free, and a second handle must still read the payload
/// after one arm moved it.
#[test]
fn asan_shared_enum_boxed_tuple_payload_is_freed_array() {
    assert_clean_asan_run(
        r#"
shared enum Sh { S((Array[String, 2], i64)), N }
fn mkt(t: String) -> (Array[String, 2], i64) { let a: Array[String, 2] = [f"a-{t}-bbbbbbbbbbbbbbbbbbbbbbbb", f"a-{t}-cc"]; return (a, 7); }
fn ostr(o: ref Option[String]) -> String { match o { Some(v) => v.clone(), None => "none" } }
fn take(s: Sh) -> (Array[String, 2], i64) { match s { Sh.S(x) => { return x; } Sh.N => { return mkt("z"); } } }
fn main() {
    { let s = Sh.S(mkt("r")); match s { Sh.S(x) => { println(f"read {x.0[0]}"); } Sh.N => { println("n"); } } }
    { let s = Sh.S(mkt("l")); match s { Sh.S(x) => { let u = x; println("let"); } Sh.N => { println("n"); } } }
    { let t = mkt("m"); let s = Sh.S(t); match s { Sh.S(x) => { println(f"named {x.0[0]}"); } Sh.N => { println("n"); } } }
    { let s = Sh.S(mkt("u")); println("unmatched"); }
    { let s = Sh.S(mkt("v")); let s2 = s; { match s { Sh.S(x) => { let u = x; println("moved"); } Sh.N => { println("n"); } } } match s2 { Sh.S(x) => { println(f"after {x.0[0]}"); } Sh.N => { println("n"); } } }
    { let s = Sh.S(mkt("t")); let s2 = s; { let y = take(s); println("taken"); } match s2 { Sh.S(x) => { println(f"kept {x.0[0]}"); } Sh.N => { println("n"); } } }
    println("done");
}
"#,
        &[
            "read a-r-bbbbbbbbbbbbbbbbbbbbbbbb",
            "let",
            "named a-m-bbbbbbbbbbbbbbbbbbbbbbbb",
            "unmatched",
            "moved",
            "after a-v-bbbbbbbbbbbbbbbbbbbbbbbb",
            "taken",
            "kept a-t-bbbbbbbbbbbbbbbbbbbbbbbb",
            "done",
        ],
        "b_2026_09_20_56_array",
    );
}

/// B-2026-09-20-56 — an `(Option[String], i64)` payload rides in a malloc'd box behind the
/// shared enum's payload word. Before the fix nothing freed that box: the
/// box and every element string inside it leaked on every spelling. The
/// arms that hand the payload on (`let u = x`, `return x`) must not turn
/// that into a double free, and a second handle must still read the payload
/// after one arm moved it.
#[test]
fn asan_shared_enum_boxed_tuple_payload_is_freed_option() {
    assert_clean_asan_run(
        r#"
shared enum Sh { S((Option[String], i64)), N }
fn mkt(t: String) -> (Option[String], i64) { return (Option.Some(f"o-{t}-dddddddddddddddddddddd"), 7); }
fn ostr(o: ref Option[String]) -> String { match o { Some(v) => v.clone(), None => "none" } }
fn take(s: Sh) -> (Option[String], i64) { match s { Sh.S(x) => { return x; } Sh.N => { return mkt("z"); } } }
fn main() {
    { let s = Sh.S(mkt("r")); match s { Sh.S(x) => { println(f"read {ostr(x.0)}"); } Sh.N => { println("n"); } } }
    { let s = Sh.S(mkt("l")); match s { Sh.S(x) => { let u = x; println("let"); } Sh.N => { println("n"); } } }
    { let t = mkt("m"); let s = Sh.S(t); match s { Sh.S(x) => { println(f"named {ostr(x.0)}"); } Sh.N => { println("n"); } } }
    { let s = Sh.S(mkt("u")); println("unmatched"); }
    { let s = Sh.S(mkt("v")); let s2 = s; { match s { Sh.S(x) => { let u = x; println("moved"); } Sh.N => { println("n"); } } } match s2 { Sh.S(x) => { println(f"after {ostr(x.0)}"); } Sh.N => { println("n"); } } }
    { let s = Sh.S(mkt("t")); let s2 = s; { let y = take(s); println("taken"); } match s2 { Sh.S(x) => { println(f"kept {ostr(x.0)}"); } Sh.N => { println("n"); } } }
    println("done");
}
"#,
        &[
            "read o-r-dddddddddddddddddddddd",
            "let",
            "named o-m-dddddddddddddddddddddd",
            "unmatched",
            "moved",
            "after o-v-dddddddddddddddddddddd",
            "taken",
            "kept o-t-dddddddddddddddddddddd",
            "done",
        ],
        "b_2026_09_20_56_option",
    );
}

/// B-2026-09-20-56 — an `(Array[String, 2], Option[String])` payload rides in a malloc'd box behind the
/// shared enum's payload word. Before the fix nothing freed that box: the
/// box and every element string inside it leaked on every spelling. The
/// arms that hand the payload on (`let u = x`, `return x`) must not turn
/// that into a double free, and a second handle must still read the payload
/// after one arm moved it.
#[test]
fn asan_shared_enum_boxed_tuple_payload_is_freed_array_option() {
    assert_clean_asan_run(
        r#"
shared enum Sh { S((Array[String, 2], Option[String])), N }
fn mkt(t: String) -> (Array[String, 2], Option[String]) { let a: Array[String, 2] = [f"a-{t}-bbbbbbbbbbbbbbbbbbbbbbbb", f"a-{t}-cc"]; return (a, Option.Some(f"q-{t}-gggggggggggggggggggggg")); }
fn ostr(o: ref Option[String]) -> String { match o { Some(v) => v.clone(), None => "none" } }
fn take(s: Sh) -> (Array[String, 2], Option[String]) { match s { Sh.S(x) => { return x; } Sh.N => { return mkt("z"); } } }
fn main() {
    { let s = Sh.S(mkt("r")); match s { Sh.S(x) => { println(f"read {x.0[1]}/{ostr(x.1)}"); } Sh.N => { println("n"); } } }
    { let s = Sh.S(mkt("l")); match s { Sh.S(x) => { let u = x; println("let"); } Sh.N => { println("n"); } } }
    { let t = mkt("m"); let s = Sh.S(t); match s { Sh.S(x) => { println(f"named {x.0[1]}/{ostr(x.1)}"); } Sh.N => { println("n"); } } }
    { let s = Sh.S(mkt("u")); println("unmatched"); }
    { let s = Sh.S(mkt("v")); let s2 = s; { match s { Sh.S(x) => { let u = x; println("moved"); } Sh.N => { println("n"); } } } match s2 { Sh.S(x) => { println(f"after {x.0[1]}/{ostr(x.1)}"); } Sh.N => { println("n"); } } }
    { let s = Sh.S(mkt("t")); let s2 = s; { let y = take(s); println("taken"); } match s2 { Sh.S(x) => { println(f"kept {x.0[1]}/{ostr(x.1)}"); } Sh.N => { println("n"); } } }
    println("done");
}
"#,
        &[
            "read a-r-cc/q-r-gggggggggggggggggggggg",
            "let",
            "named a-m-cc/q-m-gggggggggggggggggggggg",
            "unmatched",
            "moved",
            "after a-v-cc/q-v-gggggggggggggggggggggg",
            "taken",
            "kept a-t-cc/q-t-gggggggggggggggggggggg",
            "done",
        ],
        "b_2026_09_20_56_array_option",
    );
}

/// B-2026-09-20-56 — a `(Vec[String], Option[String])` payload rides in a malloc'd box behind the
/// shared enum's payload word. Before the fix nothing freed that box: the
/// box and every element string inside it leaked on every spelling. The
/// arms that hand the payload on (`let u = x`, `return x`) must not turn
/// that into a double free, and a second handle must still read the payload
/// after one arm moved it.
#[test]
fn asan_shared_enum_boxed_tuple_payload_is_freed_vec_option() {
    assert_clean_asan_run(
        r#"
shared enum Sh { S((Vec[String], Option[String])), N }
fn mkt(t: String) -> (Vec[String], Option[String]) { let mut v: Vec[String] = Vec.new(); v.push(f"v-{t}-eeeeeeeeeeeeeeeeeeeeeeeeee"); return (v, Option.Some(f"q-{t}-gggggggggggggggggggggg")); }
fn ostr(o: ref Option[String]) -> String { match o { Some(v) => v.clone(), None => "none" } }
fn take(s: Sh) -> (Vec[String], Option[String]) { match s { Sh.S(x) => { return x; } Sh.N => { return mkt("z"); } } }
fn main() {
    { let s = Sh.S(mkt("r")); match s { Sh.S(x) => { println(f"read {x.0[0]}/{ostr(x.1)}"); } Sh.N => { println("n"); } } }
    { let s = Sh.S(mkt("l")); match s { Sh.S(x) => { let u = x; println("let"); } Sh.N => { println("n"); } } }
    { let t = mkt("m"); let s = Sh.S(t); match s { Sh.S(x) => { println(f"named {x.0[0]}/{ostr(x.1)}"); } Sh.N => { println("n"); } } }
    { let s = Sh.S(mkt("u")); println("unmatched"); }
    { let s = Sh.S(mkt("v")); let s2 = s; { match s { Sh.S(x) => { let u = x; println("moved"); } Sh.N => { println("n"); } } } match s2 { Sh.S(x) => { println(f"after {x.0[0]}/{ostr(x.1)}"); } Sh.N => { println("n"); } } }
    { let s = Sh.S(mkt("t")); let s2 = s; { let y = take(s); println("taken"); } match s2 { Sh.S(x) => { println(f"kept {x.0[0]}/{ostr(x.1)}"); } Sh.N => { println("n"); } } }
    println("done");
}
"#,
        &[
            "read v-r-eeeeeeeeeeeeeeeeeeeeeeeeee/q-r-gggggggggggggggggggggg",
            "let",
            "named v-m-eeeeeeeeeeeeeeeeeeeeeeeeee/q-m-gggggggggggggggggggggg",
            "unmatched",
            "moved",
            "after v-v-eeeeeeeeeeeeeeeeeeeeeeeeee/q-v-gggggggggggggggggggggg",
            "taken",
            "kept v-t-eeeeeeeeeeeeeeeeeeeeeeeeee/q-t-gggggggggggggggggggggg",
            "done",
        ],
        "b_2026_09_20_56_vec_option",
    );
}

/// B-2026-09-20-56 — a tuple holding an `Array` and an `Option`, moved by
/// value. Its source disarm walked the LLVM type only, which zeroes the
/// array's element caps and cannot see an `Option` tag, so the Option's
/// string was freed by the source and by the destination alike.
#[test]
fn asan_tuple_with_array_and_option_moved_by_value_disarms_both() {
    assert_clean_asan_run(
        r#"
enum E { S((Array[String, 2], Option[String])), N }
fn mkt(t: String) -> (Array[String, 2], Option[String]) { let a: Array[String, 2] = [f"a-{t}-bbbbbbbbbbbbbbbbbbbbbbbb", f"a-{t}-cc"]; return (a, Option.Some(f"q-{t}-gggggggggggggggggggggg")); }
fn ostr(o: ref Option[String]) -> String { match o { Some(v) => v.clone(), None => "none" } }
fn eat(t: (Array[String, 2], Option[String])) -> String { return ostr(t.1); }
fn main() {
    { let t = mkt("c"); let e = E.S(t); match e { E.S(x) => { println(f"ctor {x.0[1]}/{ostr(x.1)}"); } E.N => { println("n"); } } }
    { let t = mkt("f"); let k = eat(t); println(f"call {k}"); }
    println("done");
}
"#,
        &[
            "ctor a-c-cc/q-c-gggggggggggggggggggggg",
            "call q-f-gggggggggggggggggggggg",
            "done",
        ],
        "b_2026_09_20_56_plain",
    );
}
