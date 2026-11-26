//! One interpreter per REPL session (B-2026-11-19-3): a cell starts from the
//! values the cells before it left, not from a replay of those cells, so a
//! task, a `shared` handle, a closure and the logger go on as they would in
//! the one `main` the session is.

use super::session;

/// Each cell's output, `Ok` for what it printed, `Err` for its errors.
fn outputs(s: &mut karac::repl::Session, cells: &[&str]) -> Vec<Result<String, String>> {
    cells
        .iter()
        .map(|cell| {
            let r = s.evaluate_cell_captured(cell);
            match r.errors.is_empty() {
                true => Ok(r.stdout),
                false => Err([r.notes, r.errors].concat().join("\n")),
            }
        })
        .collect()
}

fn ok(s: &str) -> Result<String, String> {
    Ok(s.to_string())
}

/// A task spawned in one cell runs once, when a later cell joins it; the
/// cells after the join do not run it again.
#[test]
fn repl_carry_a_task_runs_once_across_cells() {
    let mut s = session();
    let out = outputs(
        &mut s,
        &[
            "import std.sync.{TaskGroup};\nfn work(n: i64) -> i64 { println(f\"work {n}\"); n * 10 }",
            "let g = TaskGroup.new();",
            "let h = g.spawn(|| work(4));",
            "let r = h.join();",
            "println(r);",
            "println(r + 1);",
        ],
    );
    assert!(out.iter().all(Result::is_ok), "{out:?}");
    let all: String = out.iter().map(|o| o.clone().unwrap()).collect();
    assert_eq!(all.matches("work 4").count(), 1, "{out:?}");
    assert_eq!(out[4], ok("40\n"));
    assert_eq!(out[5], ok("41\n"));
}

/// A task of an isolating group that panics, spawned in one cell and
/// joined in another, gives its handle the panic, as in one `main`.
#[test]
fn repl_carry_an_isolated_panic_across_cells() {
    let mut s = session();
    let out = outputs(
        &mut s,
        &[
            "import std.sync.{TaskGroup};\nfn risky(n: i64) -> i64 { if n == 2 { panic(\"bad request\"); } n * 10 }",
            "let g = TaskGroup.isolating();",
            "let b = g.spawn(|| risky(2));",
            "let rb = b.join();",
            "println(rb.is_err());",
        ],
    );
    assert_eq!(out[4], ok("true\n"), "{out:?}");
}

/// Two bindings of one `shared` value stay one value across cells.
#[test]
fn repl_carry_keeps_shared_handles_one_value() {
    let mut s = session();
    let out = outputs(
        &mut s,
        &[
            "shared struct N { mut x: i64 }",
            "let n = N { x: 5 };",
            "let m = n;",
            "m.x = 7;",
            "println(n.x);",
        ],
    );
    assert_eq!(out[4], ok("7\n"), "{out:?}");
}

/// A closure made in one cell is called in the cells after it, with what
/// it captured.
#[test]
fn repl_carry_keeps_a_closure() {
    let mut s = session();
    let out = outputs(
        &mut s,
        &[
            "let k = 5;",
            "let f = |x: i64| x + k;",
            "println(f(10));",
            "println(f(20));",
        ],
    );
    assert_eq!(out[2], ok("15\n"), "{out:?}");
    assert_eq!(out[3], ok("25\n"), "{out:?}");
}

/// A logging backend with state keeps it from cell to cell.
#[test]
fn repl_carry_keeps_the_logger() {
    let mut s = session();
    let out = outputs(
        &mut s,
        &[
            "import std.log;",
            "struct Numbered { seen: i64 }",
            "impl log.Backend for Numbered {
                 fn emit(mut ref self, record: log.Record) {
                     self.seen += 1;
                     println(f\"#{self.seen} {record.msg}\");
                 }
             }",
            "log.init(Numbered { seen: 0 }, level: log.Level.Debug);",
            "log.info(\"one\");",
            "log.info(\"two\");",
        ],
    );
    assert_eq!(out[4], ok("#1 one\n"), "{out:?}");
    assert_eq!(out[5], ok("#2 two\n"), "{out:?}");
}

/// A value's `Drop` runs once, at the session's end, not at the end of the
/// cell that made it, even when the drop suspends.
#[test]
fn repl_carry_drops_once_at_the_end() {
    let mut s = session();
    let out = outputs(
        &mut s,
        &[
            "import std.time.{Duration, sleep};",
            "shared struct H { n: i64 }",
            "impl Drop for H { fn drop(mut ref self) { sleep(Duration.ms(1)); println(f\"drop {self.n}\"); } }",
            "let a = H { n: 1 };",
            "let b = a;",
            "println(\"end\");",
        ],
    );
    assert!(out.iter().all(Result::is_ok), "{out:?}");
    assert_eq!(out[5], ok("end\n"), "{out:?}");
    assert_eq!(s.end(), Ok("drop 1\n".to_string()));
}

/// A `fn` item a binding holds as a value (here in a `Vec`) is still
/// callable in a later cell: the later program builds its body too.
#[test]
fn repl_carry_keeps_a_fn_item_value() {
    let mut s = session();
    let out = outputs(
        &mut s,
        &[
            "fn shout(n: i64) -> i64 { println(f\"shout {n}\"); n * 2 }",
            "let fs: Vec[Fn(i64) -> i64] = [shout];",
            "println(fs[0](4));",
            "println(fs[0](5));",
        ],
    );
    assert_eq!(out[2], ok("shout 4\n8\n"), "{out:?}");
    assert_eq!(out[3], ok("shout 5\n10\n"), "{out:?}");
}

/// A value that cannot be carried still reaches the next cell, by replay:
/// a closure that borrows its parameter, held as an `Fn(own ..)`, and a
/// closure that borrows a local of its cell.
#[test]
fn repl_carry_falls_back_to_replay() {
    let mut s = session();
    let out = outputs(
        &mut s,
        &[
            "let f: Fn(own Vec[i64]) -> Vec[i64] = |v: Vec[i64]| v.iter().map(|x| x * 2).collect();",
            "let b = f([4, 5]);",
            "println(f\"{b[0]} {b[1]}\");",
            "let mut seen: Vec[i64] = [];",
            "let mut push = |x: i64| seen.push(x);",
            "push(1);",
            "push(2);",
            "println(seen.len());",
        ],
    );
    assert_eq!(out[2], ok("8 10\n"), "{out:?}");
    assert!(out.iter().all(Result::is_ok), "{out:?}");
}
