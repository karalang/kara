//! A call through a function value typed `with E` has the set solved for
//! the value it is read out of (design.md § Effect variables), so a `par`
//! over two buses' `emit`s is checked against their handlers rather than
//! refused as a call with every effect. The handlers a value holds are
//! every function value that can flow into it: bound here, added by a
//! callee, or made by the function that built it.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

const BUS: &str = "struct Event { n: i64 }

struct EventBus[with E] {
    handlers: Vec[Fn(Event) with E],
}

impl[with E] EventBus[with E] {
    fn register(mut ref self, h: escaping Fn(Event) with E) {
        self.handlers.push(h);
    }

    fn emit(self, e: Event) with E {
        for h in self.handlers { h(e); }
    }
}

fn shout(e: Event) { println(f\"shout {e.n}\"); }
fn quiet(e: Event) { let _ = e.n + 1; }
";

fn fixture(tag: &str, body: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-effect-vars-par-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, format!("{BUS}\n{body}\n")).unwrap();
    (dir, path)
}

fn check(tag: &str, body: &str) -> (bool, String) {
    let (dir, path) = fixture(tag, body);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
    )
}

fn refused(tag: &str, body: &str, want: &str) {
    let (ok, out) = check(tag, body);
    assert!(!ok, "{tag}: check must refuse: {out}");
    assert!(out.contains(want), "{tag}: want `{want}` in: {out}");
}

const STDOUT: &str = "the `par` conflict: `writes(Stdout)` and `writes(Stdout)`";

#[test]
fn par_over_buses_whose_handlers_do_not_conflict_runs() {
    let body = "fn main() {
    let mut bus = EventBus { handlers: vec![shout] };
    let mut tally = EventBus { handlers: Vec.new() };
    tally.register(|e| quiet(e));
    par { bus.emit(Event { n: 1 }), tally.emit(Event { n: 2 }) };
}";
    let (ok, out) = check("apart", body);
    assert!(ok, "check must accept: {out}");
    let (dir, path) = fixture("apart_run", body);
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "shout 1\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn par_over_buses_whose_handlers_both_print_is_refused() {
    refused(
        "both_print",
        "fn main() {
    let mut bus = EventBus { handlers: vec![shout] };
    let mut logs = EventBus { handlers: Vec.new() };
    logs.register(|e| shout(e));
    par { bus.emit(Event { n: 1 }), logs.emit(Event { n: 2 }) };
}",
        STDOUT,
    );
}

#[test]
fn a_handler_a_callee_adds_is_seen() {
    refused(
        "callee_adds",
        "fn fill(b: mut ref EventBus) { b.register(shout); }
fn main() {
    let mut bus = EventBus { handlers: Vec.new() };
    fill(mut bus);
    par { bus.emit(Event { n: 1 }), println(\"other\") };
}",
        STDOUT,
    );
}

#[test]
fn a_handler_the_builder_added_is_seen() {
    refused(
        "built_elsewhere",
        "fn make() -> EventBus {
    let mut b = EventBus { handlers: Vec.new() };
    b.register(shout);
    b
}
fn main() {
    let bus = make();
    par { bus.emit(Event { n: 1 }), println(\"other\") };
}",
        STDOUT,
    );
}

#[test]
fn a_moved_bus_keeps_its_handlers() {
    refused(
        "moved",
        "fn main() {
    let mut bus = EventBus { handlers: Vec.new() };
    bus.register(shout);
    let other = bus;
    par { other.emit(Event { n: 1 }), println(\"x\") };
}",
        STDOUT,
    );
}

#[test]
fn a_bus_in_a_field_keeps_its_handlers() {
    refused(
        "field",
        "struct App { bus: EventBus }
fn main() {
    let mut app = App { bus: EventBus { handlers: Vec.new() } };
    app.bus.register(shout);
    par { app.bus.emit(Event { n: 1 }), println(\"x\") };
}",
        STDOUT,
    );
}

#[test]
fn buses_a_function_is_passed_keep_their_callers_handlers() {
    // Only the caller knows what `a` and `b` hold.
    refused(
        "params",
        "fn both(a: ref EventBus, b: ref EventBus) {
    par { a.emit(Event { n: 1 }), b.emit(Event { n: 2 }) };
}
fn main() {
    let bus = EventBus { handlers: vec![quiet] };
    let logs = EventBus { handlers: vec![quiet] };
    both(bus, logs);
}",
        "`calls(in p1)` and `calls(in p2)`",
    );
}
