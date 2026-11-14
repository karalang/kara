//! Kata 2115's Rust template keeps a `Map[Str, i64]` of counts and, for
//! each `r` of a `for` over a `Vec[Str]`, calls `indeg.get_mut(r)`. The
//! borrow checker took a whole-local `mut ref` argument to store every
//! other operand's borrows, so the reference to `r` passed as the key was
//! kept in the map, and the loop's next `r` was refused: "cannot write to
//! `r` while `indeg` still holds a borrow of it". A reference argument
//! the receiver has no slot for can only leave what its referent holds
//! there, the `Str`'s own origin; that is still tracked.

fn run(src: &str) -> Result<String, String> {
    karac::mir::lower::run_source(src)
        .map(|r| r.output)
        .map_err(|e| format!("{e:?}"))
}

#[test]
fn kata_2115_get_mut_with_a_loop_key_is_accepted() {
    let out = run(r#"
fn main() {
    let names: Vec[String] = ["a", "b"].to_vec();
    let mut count: Map[Str, i64] = Map.new();
    for n in names.iter() {
        count.insert(n.as_str(), 2);
    }
    let keys: Vec[Str] = names.iter().map(|n| n.as_str()).collect();
    for k in keys.iter() {
        let d = count.get_mut(k).unwrap();
        *d -= 1;
    }
    for k in keys.iter() {
        if let Some(d) = count.get_mut(k) {
            *d -= 1;
        }
    }
    println(f"{count["a"]} {count["b"]}");
}
"#)
    .expect("accepted");
    assert_eq!(out, "0 0\n");
}

/// The view itself still goes in: a `Str` stored by a callee through a
/// borrowed parameter, or by `insert`, keeps its owner borrowed, and the
/// MIR borrow checker refuses the write to the owner.
#[test]
fn kata_2115_a_stored_view_still_borrows_its_owner() {
    for src in [
        r#"
fn put(v: mut ref Vec[Str], s: Str) {
    v.push(s);
}

fn main() {
    let mut owner: String = "abc";
    let mut v: Vec[Str] = Vec.new();
    let s = owner.as_str();
    put(mut v, s);
    owner.push('d');
    println(f"{v.len()}");
}
"#,
        r#"
fn main() {
    let mut owner: String = "abc";
    let mut m: Map[Str, i64] = Map.new();
    let k = owner.as_str();
    m.insert(k, 1);
    owner.push('d');
    println(f"{m.len()}");
}
"#,
    ] {
        let err = run(src).expect_err("refused");
        assert!(err.contains("while it is borrowed"), "{err}");
    }
}
