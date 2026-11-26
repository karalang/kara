//! Two holes in function effect sets, both filed by the effects probe.
//!
//! B-2026-11-17-14: inside `with_provider[R](p, f)` the call supplies `R`,
//! so `R` is not among the enclosing function's effects (core semantics
//! §14.4), and `providers { R => p } in { .. }` nests the same way (§14.8).
//! A public function that bound its own provider had to declare `R`, and
//! `karac query effects` listed it.
//!
//! B-2026-11-17-13: a drop's effects are its type's drop glue's, charged to
//! the scope where the drop runs (core semantics §12 default 3). They were
//! in no function's effect set, so a public function whose local prints on
//! drop declared nothing and `#[no_effect(writes(Stdout))]` held. A value
//! the function hands on drops nothing there, so it is not charged.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-eff-discharge-drops-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

/// `function` -> its inferred effects, as `karac query effects` renders them.
fn inferred(tag: &str, src: &str, function: &str) -> Vec<String> {
    let out = karac(tag, &["query", "effects"], src);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{tag}: {e}: {}", text(&out)));
    let f = json["functions"]
        .as_array()
        .expect("functions")
        .iter()
        .find(|f| f["function"] == function)
        .unwrap_or_else(|| panic!("{tag}: no {function} in {json}"));
    f["inferred_effects"]
        .as_array()
        .expect("inferred_effects")
        .iter()
        .map(|e| {
            format!(
                "{}({})",
                e["verb"].as_str().unwrap(),
                e["resource"].as_str().unwrap()
            )
        })
        .collect()
}

const DB: &str = "trait DbT { fn get(self, k: i64) -> i64; }\n\
                  effect resource Db: DbT;\n\
                  struct Mem { n: i64 }\n\
                  impl DbT for Mem { fn get(self, k: i64) -> i64 { self.n + k } }\n";

#[test]
fn provider_bound_inside_a_pub_fn_needs_no_clause() {
    let src = format!(
        "{DB}pub fn w() -> i64 {{ with_provider[Db](Mem {{ n: 1 }}, || Db.get(1)) }}\n\
         fn main() {{ println(f\"{{w()}}\"); }}\n"
    );
    let out = karac("w-check", &["check"], &src);
    assert!(out.status.success(), "{}", text(&out));
    let w = inferred("w-query", &src, "w");
    assert!(!w.iter().any(|e| e.contains("Db")), "{w:?}");
    let main = inferred("w-main", &src, "main");
    assert!(!main.iter().any(|e| e.contains("Db")), "{main:?}");
}

#[test]
fn provider_supplies_only_inside_its_closure() {
    // The same callee reached outside the closure still needs `Db`.
    let src = format!(
        "{DB}fn look(k: i64) -> i64 {{ Db.get(k) }}\n\
         fn both() -> i64 {{ with_provider[Db](Mem {{ n: 1 }}, || look(2)) + look(3) }}\n\
         fn inside() -> i64 {{ with_provider[Db](Mem {{ n: 1 }}, || look(2)) }}\n\
         fn main() {{ println(f\"{{inside()}}\"); }}\n"
    );
    let both = inferred("both", &src, "both");
    assert!(both.contains(&"reads(Db)".to_string()), "{both:?}");
    let inside = inferred("inside", &src, "inside");
    assert!(!inside.contains(&"reads(Db)".to_string()), "{inside:?}");
}

#[test]
fn providers_sugar_supplies_its_resources() {
    let src = "import std.time.{ClockProvider};\nstruct Fixed { t: i64 }\n\
               impl ClockProvider for Fixed { fn now(ref self) -> i64 { self.t } }\n\
               pub fn noon() -> i64 { providers { Clock => Fixed { t: 43200 } } in { Clock.now() / 3600 } }\n\
               fn main() { println(f\"{noon()}\"); }\n";
    let out = karac("sugar", &["check"], src);
    assert!(out.status.success(), "{}", text(&out));
    let noon = inferred("sugar-query", src, "noon");
    assert!(!noon.iter().any(|e| e.contains("Clock")), "{noon:?}");
}

const L: &str = "pub struct L { n: i64 }\n\
                 impl Drop for L { fn drop(mut ref self) { println(f\"drop {self.n}\"); } }\n";

#[test]
fn drop_body_effects_are_charged_where_the_drop_runs() {
    let src = format!(
        "{L}fn s() {{ let _l = L {{ n: 5 }}; }}\n\
         fn t() {{ s(); }}\n\
         fn main() {{ t(); }}\n"
    );
    for f in ["s", "t", "main"] {
        let got = inferred(&format!("drop-{f}"), &src, f);
        assert!(got.contains(&"writes(Stdout)".to_string()), "{f}: {got:?}");
    }
    let src = format!("{L}pub fn p(l: own L) {{ }}\nfn main() {{ p(L {{ n: 1 }}); }}\n");
    let out = karac("drop-pub", &["check"], &src);
    assert!(!out.status.success());
    assert!(
        text(&out).contains("public function 'p' performs effects [writes(Stdout)]"),
        "{}",
        text(&out)
    );
    // An `own self` receiver the method does not hand on drops at its end.
    let src = format!(
        "{L}impl L {{ pub fn into_n(own self) -> i64 {{ self.n }} }}\n\
         fn main() {{ println(f\"{{L {{ n: 2 }}.into_n()}}\"); }}\n"
    );
    let got = inferred("drop-own-self", &src, "L.into_n");
    assert!(got.contains(&"writes(Stdout)".to_string()), "{got:?}");
}

#[test]
fn a_value_handed_on_charges_no_drop() {
    let src = format!(
        "{L}impl L {{ pub fn new(n: i64) -> L {{ L {{ n }} }} pub fn get(self) -> i64 {{ self.n }} }}\n\
         pub fn make(n: i64) -> L {{ let l = L {{ n }}; l }}\n\
         pub fn wrap(n: i64) -> Option[L] {{ let l = L.new(n); Some(l) }}\n\
         pub fn keep(l: own L) -> L {{ l }}\n\
         pub fn into_vec(l: own L) -> Vec[L] {{ let mut v: Vec[L] = Vec.new(); v.push(l); v }}\n\
         pub fn store(v: mut ref Vec[L], n: i64) {{ let l = L.new(n); v.push(l); }}\n\
         pub fn look(l: L) -> i64 {{ l.get() }}\n\
         fn main() {{ let a = make(1); let mut v: Vec[L] = Vec.new(); store(mut v, 2); \
         println(f\"{{look(a)}} {{wrap(4).is_some()}} {{keep(make(5)).get()}} {{into_vec(make(6)).len()}}\"); }}\n"
    );
    let out = karac("handed-on", &["check"], &src);
    assert!(out.status.success(), "{}", text(&out));
}
