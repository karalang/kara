//! An element of a tuple literal coerces into its `dyn` slot (design.md §
//! `dyn Trait`), as an enum payload or a struct field does:
//! `routes.push((path, Hello {}))` into a `Vec[(String, dyn Handler)]`, a
//! `let` with a tuple type, `Some((1, Hello {}))`, and a nested tuple. The
//! check once typed the literal whole, as `(String, Hello)`, and refused it.
//! Each program runs the same on the interpreter and compiled.

mod common;

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-dyntuple-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// What the program prints compiled at `opt`, and whether it exited
/// cleanly; `None` without the llvm feature or a runtime archive.
#[cfg(feature = "llvm")]
fn build_and_run(src: &str, opt: u8) -> Option<(bool, String)> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("karac-dyntuple-b-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| Command::new(&exe).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    Some((
        out.status.success(),
        String::from_utf8(out.stdout).expect("utf-8"),
    ))
}

#[cfg(not(feature = "llvm"))]
fn build_and_run(_: &str, _: u8) -> Option<(bool, String)> {
    None
}

/// The check accepts `src`, the interpreter prints `want`, and so does the
/// program compiled.
fn runs(name: &str, src: &str, want: &str) {
    let (ok, err) = check(name, src);
    assert!(ok, "{err}");
    let got = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(got, want);
    for opt in [0, 2] {
        if let Some((ok, out)) = build_and_run(src, opt) {
            assert!(ok, "at -O{opt}: {out}");
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

const NOISY: &str = "trait Handler { fn handle(self) -> i64; }
struct Noisy { id: i64 }
impl Handler for Noisy { fn handle(self) -> i64 { self.id } }
impl Drop for Noisy { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }
";

#[test]
fn a_tuple_pushed_into_a_dyn_slot_coerces_its_element() {
    let src = format!(
        "{NOISY}fn main() {{
    let mut v: Vec[(String, dyn Handler)] = Vec.new();
    v.push((\"a\".to_string(), Noisy {{ id: 1 }}));
    v.push((\"b\".to_string(), Noisy {{ id: 2 }}));
    let mut s = 0;
    for (_, h) in v {{ s += h.handle(); }}
    println(f\"sum {{s}}\");
}}
"
    );
    runs("push", &src, "sum 3\ndrop 1\ndrop 2\n");
}

#[test]
fn a_tuple_let_some_payload_and_nested_tuple_coerce_their_elements() {
    let src = format!(
        "{NOISY}fn main() {{
    let t: (String, dyn Handler) = (\"t\".to_string(), Noisy {{ id: 1 }});
    let o: Option[(i64, dyn Handler)] = Some((10, Noisy {{ id: 2 }}));
    let n: ((i64, dyn Handler), i64) = ((20, Noisy {{ id: 3 }}), 30);
    let v: Vec[(String, dyn Handler)] = [(\"v\".to_string(), Noisy {{ id: 4 }})];
    let mut s = t.1.handle() + n.0.0 + n.0.1.handle() + n.1 + v[0].1.handle();
    if let Some((k, h)) = o {{ s += k + h.handle(); }}
    println(f\"sum {{s}} {{t.0}}\");
}}
"
    );
    let (ok, err) = check("forms", &src);
    assert!(ok, "{err}");
    let got = karac::mir::lower::run_source(&src)
        .expect("MIR interpreter")
        .output;
    // The drops' order is the scope's; only the set and the sum are pinned.
    let mut lines: Vec<&str> = got.lines().collect();
    assert_eq!(lines.first().copied(), Some("drop 2"), "{got}");
    assert!(lines.contains(&"sum 70 t"), "{got}");
    lines.sort();
    assert_eq!(
        lines,
        ["drop 1", "drop 2", "drop 3", "drop 4", "sum 70 t"],
        "{got}"
    );
    for opt in [0, 2] {
        if let Some((ok, out)) = build_and_run(&src, opt) {
            assert!(ok, "at -O{opt}: {out}");
            assert_eq!(out, got, "at -O{opt}");
        }
    }
}

#[test]
fn a_tuple_element_that_does_not_implement_the_trait_is_named() {
    let src = format!(
        "{NOISY}fn main() {{
    let mut v: Vec[(String, dyn Handler)] = Vec.new();
    v.push((\"a\".to_string(), 5));
    println(f\"{{v.len()}}\");
}}
"
    );
    let (ok, err) = check("unfit", &src);
    assert!(!ok, "check accepted an `i64` in a `dyn Handler` slot");
    assert!(
        err.contains("expected 'dyn Handler', found 'i64': `i64` does not implement `Handler`"),
        "{err}"
    );
}
