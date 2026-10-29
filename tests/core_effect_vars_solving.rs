//! Solving effect variables per value (design.md § Effect variables): the
//! first function value bound into a value's variable fixes its set, a
//! later one must fit it, a written type argument fixes it instead, and the
//! function where the set is solved is charged it. The front end (parse,
//! resolve) is pinned in tests/core_effect_vars_front_end.rs.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

const BUS: &str = "effect resource OrderDb;
effect resource Log;

struct Event { n: i64 }

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

fn record(e: Event) with writes(OrderDb) { }
fn note(e: Event) with writes(Log) { }
";

fn fixture(tag: &str, body: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-effect-vars-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, format!("{BUS}\n{body}\nfn main() {{ }}\n")).unwrap();
    (dir, path)
}

/// `karac check`'s verdict and its diagnostics.
fn check(tag: &str, body: &str) -> (bool, String) {
    let (dir, path) = fixture(tag, body);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
    )
}

fn accepted(tag: &str, body: &str) {
    let (ok, out) = check(tag, body);
    assert!(ok, "{tag}: check must accept: {out}");
}

fn refused(tag: &str, body: &str, want: &str) {
    let (ok, out) = check(tag, body);
    assert!(!ok, "{tag}: check must refuse: {out}");
    assert!(out.contains(want), "{tag}: want `{want}` in: {out}");
}

#[test]
fn the_spec_event_bus_solves_e_and_charges_audit() {
    accepted(
        "spec",
        "pub fn audit() with writes(OrderDb) {
    let mut bus = EventBus { handlers: Vec.new() };
    bus.register(|e| record(e));
    bus.emit(Event { n: 7 });
}",
    );
}

#[test]
fn a_public_function_must_declare_the_solved_set() {
    refused(
        "public",
        "pub fn audit() {
    let mut bus = EventBus { handlers: Vec.new() };
    bus.register(record);
    bus.emit(Event { n: 7 });
}",
        "public function 'audit' performs effects [writes(OrderDb)]",
    );
}

#[test]
fn a_value_bound_later_must_fit_the_first() {
    refused(
        "later",
        "fn audit() {
    let mut bus = EventBus { handlers: Vec.new() };
    bus.register(record);
    bus.register(note);
}",
        "function value has `writes(Log)`, outside the effect set of `E` \
         (`{writes(OrderDb)}`), which was fixed at 25:18 by the first value bound to it",
    );
}

#[test]
fn a_later_value_within_the_set_fits() {
    accepted(
        "fits",
        "fn audit() {
    let mut bus = EventBus { handlers: Vec.new() };
    bus.register(record);
    bus.register(|e| record(e));
}",
    );
}

#[test]
fn the_construction_fixes_the_set_with_the_union_of_its_values() {
    accepted(
        "union",
        "fn audit() {
    let mut bus = EventBus { handlers: vec![record, note] };
    bus.register(note);
    bus.register(record);
}",
    );
    refused(
        "construction",
        "fn audit() {
    let mut bus = EventBus { handlers: vec![record] };
    bus.register(|e| note(e));
}",
        "which was fixed at 24:45 by the first value bound to it",
    );
}

#[test]
fn a_written_type_argument_fixes_the_set() {
    refused(
        "written",
        "fn audit() {
    let mut bus: EventBus[with writes(OrderDb)] = EventBus { handlers: Vec.new() };
    bus.register(note);
}",
        "outside the effect set of `E` (`{writes(OrderDb)}`), which was fixed at 24:27 \
         by its type argument",
    );
    refused(
        "written_param",
        "fn add(bus: mut ref EventBus[with writes(OrderDb)]) {
    bus.register(note);
}",
        "by its type argument",
    );
    accepted(
        "written_fits",
        "fn add(bus: mut ref EventBus[with writes(OrderDb)]) {
    bus.register(record);
}",
    );
}

#[test]
fn each_value_has_its_own_instance() {
    accepted(
        "two_values",
        "fn audit() {
    let mut orders = EventBus { handlers: Vec.new() };
    let mut logs = EventBus { handlers: Vec.new() };
    orders.register(record);
    logs.register(note);
}",
    );
    accepted(
        "rebound",
        "fn audit() {
    let mut bus = EventBus { handlers: Vec.new() };
    bus.register(record);
    let mut bus = EventBus { handlers: Vec.new() };
    bus.register(note);
}",
    );
}

#[test]
fn the_types_own_methods_bind_its_unsolved_variable() {
    accepted(
        "own_impl",
        "impl[with E] EventBus[with E] {
    fn register_twice(mut ref self, h: escaping Fn(Event) with E, g: escaping Fn(Event) with E) {
        self.register(h);
        self.register(g);
    }
}",
    );
}

#[test]
fn the_spec_event_bus_runs() {
    let (dir, path) = fixture(
        "run",
        "fn shout(e: Event) { println(f\"order {e.n}\"); }

fn audit() {
    let mut bus = EventBus { handlers: Vec.new() };
    bus.register(|e| shout(e));
    bus.register(shout);
    bus.emit(Event { n: 7 });
}",
    );
    let src = std::fs::read_to_string(&path)
        .unwrap()
        .replace("fn main() { }", "fn main() { audit(); }");
    std::fs::write(&path, src).unwrap();
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "order 7\norder 7\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
