//! B-2026-09-17-26 -- a by-value `Result` param whose payload is a TUPLE
//! owning heap (`Result[(W, i64), i64]`) is entry-copied, so the callee and
//! the caller each free exactly what they own.

use super::*;

/// B-2026-09-17-26 — the interpreter twin of
/// `asan_result_tuple_payload_param_is_entry_copied`, same source and output.
#[test]
fn test_result_tuple_payload_param_is_entry_copied() {
    let out = run(r#"struct W { id: i64, name: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.name}") } }
fn sink(w: W) -> i64 { return w.id; }
fn mk(i: i64) -> W { return W { id: i, name: f"n{i}" }; }
fn wild(o: Result[(W, i64), i64]) -> i64 { match o { Ok(_) => { return 1; } Err(e) => { return e; } } }
fn nomatch(o: Result[(W, i64), i64]) -> i64 { return 5; }
fn bindread(o: Result[(W, i64), i64]) -> i64 {
    match o { Ok(t) => { println(f"  {t.0.id}"); return t.1; } Err(e) => { return e; } }
}
fn bindret(o: Result[(W, i64), i64]) -> i64 { match o { Ok(t) => { return t.1; } Err(e) => { return e; } } }
fn destr(o: Result[(W, i64), i64]) -> i64 {
    match o { Ok((w, n)) => { println(f"  {w.id}"); return n; } Err(e) => { return e; } }
}
fn destrsink(o: Result[(W, i64), i64]) -> i64 { match o { Ok((w, n)) => { return sink(w) + n; } Err(e) => { return e; } } }
fn destrwild(o: Result[(W, i64), i64]) -> i64 { match o { Ok((_, n)) => { return n; } Err(e) => { return e; } } }
fn iflet(o: Result[(W, i64), i64]) -> i64 { if let Ok(t) = o { return t.1; } return 0; }
fn bindsink(o: Result[(W, i64), i64]) -> i64 { match o { Ok(t) => { return sink(t.0) + t.1; } Err(e) => { return e; } } }
fn twice(o: Result[(W, i64), i64]) -> i64 {
    match o { Ok(t) => { println(f"  {t.0.id}"); } Err(e) => { println(f"  e{e}"); } }
    match o { Ok(_) => { return 1; } Err(e) => { return e; } }
}
fn errside(o: Result[i64, (W, i64)]) -> i64 { match o { Ok(v) => { return v; } Err(_) => { return 3; } } }
fn strtup(o: Result[(String, i64), i64]) -> i64 { match o { Ok(t) => { return t.1; } Err(e) => { return e; } } }
fn main() {
    println(f"a{wild(Ok((mk(1), 9)))}");
    let r1: Result[(W, i64), i64] = Ok((mk(2), 8));
    println(f"a{wild(r1)}");
    println(f"a{wild(Err(7))}");
    println(f"b{nomatch(Ok((mk(3), 9)))}");
    let r2: Result[(W, i64), i64] = Ok((mk(4), 9));
    println(f"b{nomatch(r2)}");
    println(f"c{bindread(Ok((mk(5), 9)))}");
    let r3: Result[(W, i64), i64] = Ok((mk(6), 8));
    println(f"c{bindread(r3)}");
    println(f"d{bindret(Ok((mk(7), 9)))}");
    println(f"e{destr(Ok((mk(8), 9)))}");
    println(f"f{destrsink(Ok((mk(10), 9)))}");
    println(f"g{destrwild(Ok((mk(11), 9)))}");
    println(f"g{destrwild(Err(6))}");
    println(f"h{iflet(Ok((mk(12), 9)))}");
    println(f"i{bindsink(Ok((mk(13), 9)))}");
    println(f"j{twice(Ok((mk(14), 9)))}");
    println(f"j{twice(Err(7))}");
    println(f"k{errside(Err((mk(15), 9)))}");
    let r5: Result[i64, (W, i64)] = Err((mk(16), 8));
    println(f"k{errside(r5)}");
    println(f"k{errside(Ok(4))}");
    println(f"l{strtup(Ok((f"s1", 9)))}");
    println("end");
}
"#);
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        vec![
            "dW1/n1", "a1", "a1", "dW2/n2", "a7", "dW3/n3", "b5", "b5", "dW4/n4", "  5", "dW5/n5",
            "c9", "  6", "c8", "dW6/n6", "dW7/n7", "d9", "  8", "dW8/n8", "e9", "dW10/n10", "f19",
            "dW11/n11", "g9", "g6", "dW12/n12", "h9", "dW13/n13", "i22", "  14", "dW14/n14", "j1",
            "  e7", "j7", "dW15/n15", "k3", "k3", "dW16/n16", "k4", "l9", "end",
        ],
        "got:\n{out}"
    );
}
