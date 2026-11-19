//! B-2026-11-19-38: a call through an escaping function value has the
//! effects its type declares (design.md §12 How a call through a function
//! value gets its effects). A struct field, a collection element, an
//! `escaping` parameter and a `for` binding over a collection of them are
//! charged the clause written on their function type, nothing for
//! `with ()`, and every effect when the type writes no clause. A
//! non-escaping parameter is still charged to its callers.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, sub: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-fn-value-calls-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(sub)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn clean(tag: &str, src: &str) {
    let out = karac(tag, "check", src);
    assert!(
        out.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn refused(tag: &str, src: &str, errs: &[&str]) {
    let out = karac(tag, "check", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{tag} was accepted");
    for err in errs {
        assert!(stderr.contains(err), "{tag}: wanted {err}\n{stderr}");
    }
}

fn runs(tag: &str, src: &str, want: &str) {
    let out = karac(tag, "__mir-run", src);
    assert!(
        out.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{tag}");
}

const HEAD: &str = "effect resource OrderDb;
fn rec(x: i64) -> i64 with writes(OrderDb) { x }
";

const MISSING: &str = "public function 'go' performs effects [writes(OrderDb)] but has no \
                       effect declaration";

#[test]
fn fn_value_call_through_a_field_is_charged_its_clause() {
    let job = "pub struct Job { run: Fn(i64) -> i64 with writes(OrderDb) }\n";
    let main = "fn main() { println(go(Job { run: |x| rec(x) })); }\n";
    refused(
        "field",
        &format!("{HEAD}{job}pub fn go(j: Job) -> i64 {{ (j.run)(3) }}\n{main}"),
        &["E0400", MISSING],
    );
    // Through a `ref`, and in a method on the struct itself.
    refused(
        "field-ref",
        &format!("{HEAD}{job}pub fn go(j: ref Job) -> i64 {{ (j.run)(3) }}\n{main}"),
        &["E0400", MISSING],
    );
    // In a method on the struct itself, charged to its public caller.
    refused(
        "field-self",
        &format!(
            "{HEAD}{job}impl Job {{ fn go(self) -> i64 {{ (self.run)(3) }} }}
pub fn fire(j: Job) -> i64 {{ j.go() }}
fn main() {{ println(fire(Job {{ run: |x| rec(x) }})); }}\n"
        ),
        &["E0400", "public function 'fire' performs effects [writes(OrderDb)]"],
    );
    let declared = format!(
        "{HEAD}{job}pub fn go(j: Job) -> i64 with writes(OrderDb) {{ (j.run)(3) }}\n{main}"
    );
    clean("field-declared", &declared);
    runs("field-declared", &declared, "3\n");
}

#[test]
fn fn_value_call_through_an_element_or_a_for_binding_is_charged_its_clause() {
    let ty = "Vec[Fn(i64) -> i64 with writes(OrderDb)]";
    let main = format!("fn main() {{ let v: {ty} = [|x| rec(x)]; println(go(v)); }}\n");
    refused(
        "element",
        &format!("{HEAD}pub fn go(v: {ty}) -> i64 {{ v[0](3) }}\n{main}"),
        &["E0400", MISSING],
    );
    refused(
        "for",
        &format!(
            "{HEAD}pub fn go(v: {ty}) -> i64 {{ let mut t = 0; for f in v {{ t += f(3); }} t }}\n{main}"
        ),
        &["E0400", MISSING],
    );
    // A collection in a field, and the design's own `EventBus`.
    let bus = "pub struct Event { n: i64 }
fn record(e: Event) with writes(OrderDb) { let _ = e.n; }
pub struct EventBus { handlers: Vec[Fn(Event) with writes(OrderDb)] }
";
    refused(
        "bus",
        &format!(
            "{HEAD}{bus}impl EventBus {{
    fn emit(self, n: i64) {{ for h in self.handlers {{ h(Event {{ n: n }}); }} }}
}}
pub fn fire(b: EventBus) {{ b.emit(1); }}
fn main() {{ fire(EventBus {{ handlers: [|e| record(e)] }}); }}
"
        ),
        &["E0400", "public function 'fire' performs effects [writes(OrderDb)]"],
    );
    let inferred = format!(
        "{HEAD}{bus}impl EventBus {{
    fn emit(self, n: i64) {{ for h in self.handlers {{ h(Event {{ n: n }}); }} }}
}}
pub fn fire(b: EventBus) with writes(OrderDb) {{ b.emit(1); }}
fn main() {{ fire(EventBus {{ handlers: [|e| record(e)] }}); println(1); }}
"
    );
    clean("bus-inferred", &inferred);
    runs("bus-inferred", &inferred, "1\n");
}

#[test]
fn fn_value_call_through_an_escaping_parameter_is_charged_its_clause() {
    refused(
        "escaping",
        &format!(
            "{HEAD}pub fn go(f: escaping Fn(i64) -> i64 with writes(OrderDb)) -> i64 {{ f(3) }}
fn main() {{ println(go(|x| rec(x))); }}\n"
        ),
        &["E0400", MISSING],
    );
    // A non-escaping parameter is charged to each caller with what it
    // passes, so the callee declares nothing.
    let src = format!(
        "{HEAD}pub fn go(f: Fn(i64) -> i64) -> i64 {{ f(3) }}
pub fn caller() -> i64 with writes(OrderDb) {{ go(|x| rec(x)) }}
fn main() {{ println(caller()); }}\n"
    );
    clean("non-escaping", &src);
    runs("non-escaping", &src, "3\n");
}

#[test]
fn fn_value_call_through_a_type_with_no_clause_has_every_effect() {
    refused(
        "no-clause",
        "pub struct Job { run: Fn(i64) -> i64 }
pub fn go(j: Job) -> i64 { (j.run)(3) }
fn main() { println(go(Job { run: |x| x + 1 })); }
",
        &[
            "E0400",
            "through a function value whose type writes no `with` clause, which has every \
             effect; write the clause on the type, `Fn(..) with ...`, or `with ()` for none",
        ],
    );
    // `with ()` declares none.
    let src = "pub struct Job { run: Fn(i64) -> i64 with () }
pub fn go(j: Job) -> i64 { (j.run)(3) }
fn main() { println(go(Job { run: |x| x + 1 })); }
";
    clean("empty-clause", src);
    runs("empty-clause", src, "4\n");
}

#[test]
fn fn_value_call_charges_reach_no_effect() {
    refused(
        "no-effect",
        &format!(
            "{HEAD}pub struct Job {{ run: Fn(i64) -> i64 with writes(OrderDb) }}
#[no_effect(writes(OrderDb))]
fn go(j: Job) -> i64 {{ (j.run)(3) }}
fn main() {{ println(go(Job {{ run: |x| rec(x) }})); }}\n"
        ),
        &["writes(OrderDb)"],
    );
}
