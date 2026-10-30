//! Effects through `dyn Trait` in `karac check` (design.md § `dyn Trait`,
//! Effects): a call through a non-escaping `dyn` parameter is charged to each
//! caller with the value it passes, and a coercion into a `dyn` type whose
//! `with` clause the concrete type's methods exceed is refused at the
//! coercion.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-dyn-fx-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

const SHAPES: &str = "
pub trait Shape {
    fn area(self) -> i64;
}
pub struct Loud { s: i64 }
impl Shape for Loud {
    fn area(self) -> i64 {
        println(\"loud\");
        self.s
    }
}
pub struct Quiet { s: i64 }
impl Shape for Quiet {
    fn area(self) -> i64 { self.s }
}
pub fn measure(s: ref dyn Shape) -> i64 {
    s.area()
}
fn twice(s: ref dyn Shape) -> i64 {
    measure(s) + measure(s)
}
fn via_closure(s: ref dyn Shape) -> i64 {
    let f = || s.area();
    f()
}
";

fn shapes(rest: &str) -> String {
    format!("{SHAPES}{rest}\nfn main() {{ println(f\"{{t()}}\"); }}\n")
}

#[test]
fn dyn_param_call_is_charged_to_the_caller_passing_a_printing_type() {
    let (ok, text) = check(
        "per-caller",
        &shapes(
            "pub fn loud_area() -> i64 { measure(Loud { s: 3 }) }
             pub fn quiet_area() -> i64 { measure(Quiet { s: 3 }) }
             fn t() -> i64 { loud_area() + quiet_area() }",
        ),
    );
    assert!(!ok, "`loud_area` prints undeclared: {text}");
    assert!(text.contains("'loud_area'"), "{text}");
    assert!(
        !text.contains("'quiet_area'"),
        "a `Quiet` passed to `measure` prints nothing: {text}"
    );
    assert!(
        !text.contains("'measure'"),
        "`measure` is charged nothing for its call through `s`: {text}"
    );
}

#[test]
fn dyn_param_forwarded_to_another_is_charged_at_the_outer_call() {
    let (ok, text) = check(
        "fwd-quiet",
        &shapes("pub fn t() -> i64 { twice(Quiet { s: 1 }) }"),
    );
    assert!(ok, "check must pass: {text}");
    let (ok, text) = check(
        "fwd-loud",
        &shapes("pub fn t() -> i64 { twice(Loud { s: 1 }) }"),
    );
    assert!(!ok && text.contains("writes(Stdout)"), "{text}");
}

#[test]
fn dyn_param_captured_by_a_local_closure_is_charged_at_the_call() {
    let (ok, text) = check(
        "closure",
        &shapes("pub fn t() -> i64 { via_closure(Quiet { s: 1 }) }"),
    );
    assert!(ok, "check must pass: {text}");
    let (ok, text) = check(
        "closure-loud",
        &shapes("pub fn t() -> i64 { via_closure(Loud { s: 1 }) }"),
    );
    assert!(!ok && text.contains("writes(Stdout)"), "{text}");
}

#[test]
fn dyn_value_not_coerced_at_the_call_is_charged_every_impl() {
    // A local `dyn` value with no clause has every effect; the program's
    // impls are what it can run.
    let (ok, text) = check(
        "local",
        &shapes("pub fn t() -> i64 { let q: dyn Shape = Quiet { s: 1 }; measure(q) }"),
    );
    assert!(!ok && text.contains("writes(Stdout)"), "{text}");
    // A function holding a `dyn` parameter, called through a function value,
    // is called with a value no call site names.
    let (ok, text) = check(
        "fn-value",
        &shapes("pub fn t() -> i64 { let f = measure; f(Quiet { s: 1 }) }"),
    );
    assert!(!ok && text.contains("writes(Stdout)"), "{text}");
}

#[test]
fn dyn_param_of_a_method_is_charged_at_the_method_call() {
    let rest = "pub struct Meter { k: i64 }
                impl Meter {
                    pub fn read(self, s: ref dyn Shape) -> i64 { s.area() + self.k }
                }";
    let (ok, text) = check(
        "method-quiet",
        &shapes(&format!(
            "{rest}\npub fn t() -> i64 {{ let m = Meter {{ k: 1 }}; m.read(Quiet {{ s: 1 }}) }}"
        )),
    );
    assert!(ok, "check must pass: {text}");
    let (ok, text) = check(
        "method-loud",
        &shapes(&format!(
            "{rest}\npub fn t() -> i64 {{ let m = Meter {{ k: 1 }}; m.read(Loud {{ s: 1 }}) }}"
        )),
    );
    assert!(!ok && text.contains("writes(Stdout)"), "{text}");
}

const CLAUSED: &str = "
trait Shape {
    fn area(self) -> i64;
}
struct Loud { s: i64 }
impl Shape for Loud {
    fn area(self) -> i64 {
        println(\"loud\");
        self.s
    }
}
struct Quiet { s: i64 }
impl Shape for Quiet {
    fn area(self) -> i64 { self.s }
}
struct Holder { s: dyn Shape with reads(Clock) }
fn keep(s: escaping dyn Shape with reads(Clock)) -> Holder { Holder { s: s } }
";

fn claused(main_body: &str) -> String {
    format!("{CLAUSED}fn main() {{ {main_body} }}\n")
}

fn assert_exceeds(name: &str, main_body: &str, subject: &str) {
    let (ok, text) = check(name, &claused(main_body));
    assert!(!ok, "{name}: the coercion must be refused: {text}");
    assert!(
        text.contains(&format!(
            "{subject} coerces `Loud` to `dyn Shape with reads(Clock)`, but `Loud.area` has \
             effect writes(Stdout)"
        )),
        "{name}: the error names the method and the effect: {text}"
    );
}

#[test]
fn dyn_coercion_exceeding_the_clause_is_refused_at_each_position() {
    assert_exceeds(
        "field",
        "let h = Holder { s: Loud { s: 2 } }; println(f\"{h.s.area()}\");",
        "field `s`",
    );
    assert_exceeds(
        "arg",
        "let h = keep(Loud { s: 2 }); println(f\"{h.s.area()}\");",
        "argument 1",
    );
    assert_exceeds(
        "let",
        "let o: dyn Shape with reads(Clock) = Loud { s: 2 }; println(f\"{o.area()}\");",
        "binding `o`",
    );
    assert_exceeds(
        "element",
        "let v: Vec[dyn Shape with reads(Clock)] = [Quiet { s: 1 }, Loud { s: 2 }]; \
         println(f\"{v.len()}\");",
        "binding `v` element 1",
    );
}

#[test]
fn dyn_coercion_within_the_clause_passes() {
    let (ok, text) = check(
        "within",
        &claused(
            "let h = Holder { s: Quiet { s: 2 } };
             let k = keep(Quiet { s: 3 });
             println(f\"{h.s.area()} {k.s.area()}\");",
        ),
    );
    assert!(ok, "check must pass: {text}");
}

const STORED: &str = "
pub trait Shape {
    fn area(self) -> i64;
}
pub struct Loud { s: i64 }
impl Shape for Loud {
    fn area(self) -> i64 {
        println(\"loud\");
        self.s
    }
}
pub struct Quiet { s: i64 }
impl Shape for Quiet {
    fn area(self) -> i64 { self.s }
}
";

#[test]
fn call_through_a_stored_dyn_value_has_its_types_clause() {
    // The clause, not the program's printing impl: `held` reads the clock.
    let (ok, text) = check(
        "stored-clause",
        &format!(
            "{STORED}
             pub struct Holder {{ s: dyn Shape with reads(Clock) }}
             pub fn held(h: ref Holder) -> i64 {{ h.s.area() }}
             pub fn elems(v: ref Vec[dyn Shape with reads(Clock)]) -> i64 {{
                 let mut t = 0;
                 for s in v {{ t = t + s.area(); }}
                 t
             }}
             fn main() {{ let h = Holder {{ s: Quiet {{ s: 2 }} }}; println(f\"{{held(h)}}\"); }}"
        ),
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains("'held' performs effects [reads(Clock)]")
            && text.contains("'elems' performs effects [reads(Clock)]"),
        "a call through the value has the clause's effects: {text}"
    );
    assert!(!text.contains("writes(Stdout)"), "not every impl's: {text}");
}

#[test]
fn stored_dyn_clause_is_narrowed_to_the_trait_methods_ceiling() {
    let (ok, text) = check(
        "stored-ceiling",
        "pub trait Shape {
             fn area(self) -> i64 with reads(Clock);
         }
         pub struct Quiet { s: i64 }
         impl Shape for Quiet {
             fn area(self) -> i64 { self.s }
         }
         pub struct Holder { s: dyn Shape with reads(Clock) writes(Stdout) }
         pub fn held(h: ref Holder) -> i64 { h.s.area() }
         fn main() { let h = Holder { s: Quiet { s: 2 } }; println(f\"{held(h)}\"); }",
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains("'held' performs effects [reads(Clock)]"),
        "the ceiling leaves `reads(Clock)`: {text}"
    );
    assert!(!text.contains("writes(Stdout)"), "{text}");
}

const ROUTER: &str = "
pub trait Handler {
    fn handle(self, req: i64) -> i64;
}
struct Double {}
impl Handler for Double {
    fn handle(self, req: i64) -> i64 { req * 2 }
}
struct Logged { n: i64 }
impl Handler for Logged {
    fn handle(self, req: i64) -> i64 {
        println(f\"req {req}\");
        req + self.n
    }
}
struct Router[with E] {
    routes: Vec[(String, dyn Handler with E)],
}
impl[with E] Router[with E] {
    fn add(mut ref self, path: Str, h: escaping dyn Handler with E) {
        self.routes.push((path.to_string(), h));
    }
    fn dispatch(self, path: Str, req: i64) -> i64 with E {
        for (p, h) in self.routes.iter() {
            if p == path { return h.handle(req); }
        }
        -1
    }
}
";

#[test]
fn dyn_effect_variable_is_solved_from_the_values_coerced_into_it() {
    let (ok, text) = check(
        "router",
        &format!(
            "{ROUTER}
             pub fn quiet() -> i64 {{
                 let mut r = Router {{ routes: Vec.new() }};
                 r.add(\"/d\", Double {{}});
                 r.dispatch(\"/d\", 4)
             }}
             pub fn noisy() -> i64 {{
                 let mut r = Router {{ routes: Vec.new() }};
                 r.add(\"/l\", Logged {{ n: 1 }});
                 r.dispatch(\"/l\", 4)
             }}
             fn main() {{ println(f\"{{quiet()}} {{noisy()}}\"); }}"
        ),
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains("'noisy'"),
        "the router holding `Logged` prints: {text}"
    );
    assert!(
        !text.contains("'quiet'"),
        "the one holding `Double` does not: {text}"
    );
}

/// Within one function the router's set is the union of what is added, so a
/// pure handler added before a printing one checks (review-thread ruling
/// 2026-10-30); another function that sees the router through a parameter
/// typed with the set may add only what fits it.
#[test]
fn dyn_value_outside_its_receivers_effect_set_is_refused() {
    let (ok, text) = check(
        "router-mixed-one-fn",
        &format!(
            "{ROUTER}
             fn main() {{
                 let mut r = Router {{ routes: Vec.new() }};
                 r.add(\"/d\", Double {{}});
                 r.add(\"/l\", Logged {{ n: 1 }});
                 println(f\"{{r.dispatch(\"/l\", 4)}}\");
             }}"
        ),
    );
    assert!(ok, "{text}");
    let (ok, text) = check(
        "router-mixed",
        &format!(
            "{ROUTER}
             fn add_logged(r: mut ref Router[with reads(Stdout)]) {{
                 r.add(\"/l\", Logged {{ n: 1 }});
             }}
             fn main() {{
                 let mut r = Router {{ routes: Vec.new() }};
                 r.add(\"/d\", Double {{}});
                 add_logged(mut r);
                 println(f\"{{r.dispatch(\"/l\", 4)}}\");
             }}"
        ),
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains(
            "value coerced to `dyn Handler` has `writes(Stdout)`, outside the effect set of `E`"
        ),
        "{text}"
    );
}
