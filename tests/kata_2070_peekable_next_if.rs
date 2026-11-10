//! Kata 2070's peek arm walks the items sorted by price beside the queries
//! sorted the same way, taking each item while its price is within the
//! query with Rust's `it.next_if(|x| ..)`. The library's `Peekable` had
//! only `peek`, so the call was refused "no method 'next_if' on type
//! 'Peekable'". It now takes the next item when the predicate holds for it
//! and otherwise keeps it next, as Rust's does.

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn kata_2070_next_if_takes_while_the_predicate_holds_and_keeps_the_rest() {
    // Over `v.iter()` the items are borrowed. The calls are written out
    // here; the arm's `while let` loop of `next_if` calls, once refused by
    // MIR borrowck, runs since B-2026-11-10-24
    // (tests/mir_closure_into_iterator_slot.rs).
    let src = "fn main() {
    let v = [1, 2, 3, 7, 8].to_vec();
    let mut it = v.iter().peekable();
    let a = it.next_if(|x| x < 5);
    let b = it.next_if(|x| x < 3);
    let c = it.next_if(|x| x < 3);
    let d = it.next_if(|x| x > 100);
    let next = it.next();
    let rest: Vec[i64] = it.collect();
    println(f\"{a} {b} {c} {d} {next} {rest}\");
}
";
    assert_eq!(run(src), "Some(1) Some(2) None None Some(3) [7, 8]\n");
}

#[test]
fn kata_2070_next_if_in_a_loop_over_owned_items() {
    let src = "fn main() {
    let mut it = [1, 2, 3, 7, 8].to_vec().into_iter().peekable();
    let mut small = 0;
    while let Some(x) = it.next_if(|x| x < 5) {
        small += x;
    }
    let rest: Vec[i64] = it.collect();
    println(f\"{small} {rest}\");
}
";
    assert_eq!(run(src), "6 [7, 8]\n");
}

#[test]
fn kata_2070_next_if_after_peek_and_at_the_end() {
    let src = "fn main() {
    let mut it = (0..3).peekable();
    let p = it.peek().is_some();
    let a = it.next_if(|x| x == 0);
    let b = it.next_if(|x| x == 0);
    let c = it.next_if(|x| x == 1);
    let d = it.next_if(|x| x == 2);
    let e = it.next_if(|_| true);
    println(f\"{p} {a} {b} {c} {d} {e} {it.next()}\");
}
";
    assert_eq!(run(src), "true Some(0) None Some(1) Some(2) None None\n");
}
