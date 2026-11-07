//! A `weak T` reached through a binding of type `ref weak T` (a loop over
//! `v.iter()` of a `Vec[weak T]`, or a `weak` payload bound by a `match` on
//! an enum) is storage like a `weak` field or local: reading it, or calling
//! `.upgrade()` on it, yields `Option[T]` (design.md § Cycles and `weak`).
//! The checker upgraded only a `weak T` binding, so the `ref weak T` one
//! typed as an error with no message and every run and build stopped at "a
//! type that failed to check is not lowered yet" (B-2026-11-07-7).

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn a_weak_vec_element_bound_by_a_loop_upgrades() {
    let src = "shared struct Node { id: i64 }

fn main() {
    let a = Node { id: 1 };
    let mut v: Vec[weak Node] = Vec.new();
    v.push(a.clone());
    {
        let b = Node { id: 2 };
        v.push(b);
    }
    for w in v.iter() {
        match w.upgrade() {
            Some(n) => println(f\"live {n.id}\"),
            None => println(\"gone\"),
        }
    }
    for w in v.iter() {
        match w {
            Some(n) => println(f\"read {n.id}\"),
            None => println(\"gone\"),
        }
    }
}
";
    assert_eq!(run(src), "live 1\ngone\nread 1\ngone\n");
}

#[test]
fn a_weak_enum_payload_bound_by_a_match_upgrades() {
    let src = "shared struct Sub { name: String }

enum Link { Strong(Sub), Weak(weak Sub) }

fn show(l: Link) -> String {
    match l {
        Link.Strong(s) => s.name.clone(),
        Link.Weak(w) => match w.upgrade() {
            Some(s) => f\"weak {s.name}\",
            None => \"gone\".to_string(),
        },
    }
}

fn main() {
    let a = Sub { name: \"a\".to_string() };
    let l1 = Link.Strong(a.clone());
    let l2 = Link.Weak(a.clone());
    let l3 = Link.Weak(Sub { name: \"b\".to_string() });
    println(show(l1));
    println(show(l2));
    println(show(l3));
}
";
    assert_eq!(run(src), "a\nweak a\ngone\n");
}
