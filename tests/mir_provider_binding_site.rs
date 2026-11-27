//! A provider's methods run with the bindings in scope where it was bound
//! (core semantics §14.3, §14.4; B-2026-11-25-50): a nearer binding of the
//! same resource does not change them, and a provider never reaches
//! itself, so a fake that prints, or that forwards to the resource it
//! fakes, reaches the binding outside it. Each program runs on the MIR
//! interpreter (`karac __mir-run`) and, built ahead of time, as a native
//! binary; both must print the same, byte for byte.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-provider-binding-site-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What a run printed: its stdout, then its stderr.
type Printed = (String, String);

fn run(cmd: &mut Command, what: &str) -> Printed {
    let out = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{what}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

/// Asserts that `src` prints `want` on the interpreter and, when the
/// runtime archive links, as a built binary.
fn check(name: &str, src: &str, want: (&str, &str)) {
    let dir = Scratch::new(name);
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let interp = run(
        Command::new(karac)
            .current_dir(dir.path())
            .args(["__mir-run", file.to_str().unwrap()]),
        "interpreter",
    );
    assert_eq!(
        (interp.0.as_str(), interp.1.as_str()),
        want,
        "{name} on the interpreter"
    );
    let exe = dir.path().join("main");
    let built = Command::new(karac)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    });
    if linked.is_none() {
        return;
    }
    let binary = run(Command::new(&exe).current_dir(dir.path()), "binary");
    assert_eq!(binary, interp, "{name}: the binary and the interpreter");
}

/// `Stderr` bound to `X`, `Stdout` to `C` inside it, and `Stderr` again to
/// `Y` inside that. `C` was bound where `Stderr` is `X`, so its `eprint`
/// reaches `X` although `Y` is nearer where it runs; `Y` was bound where
/// `Stdout` is `C`, so its `print` reaches `C`. A plain function and a
/// closure value between them pass the providers on.
const XCY: &str = r#"import std.io.{StderrProvider, StdoutProvider};
struct X {}
impl StderrProvider for X {
    fn write(mut ref self, s: Str) { print(f"X[{s}]"); }
    fn flush(mut ref self) {}
}

struct C {}
impl StdoutProvider for C {
    fn write(mut ref self, s: Str) { eprint(f"C[{s}]"); }
    fn flush(mut ref self) {}
}

struct Y {}
impl StderrProvider for Y {
    fn write(mut ref self, s: Str) { print(f"Y[{s}]"); }
    fn flush(mut ref self) {}
}

fn shout(s: Str) {
    print(s);
}

fn twice(f: Fn() -> ()) {
    f();
    f();
}

fn main() {
    with_provider[Stderr](X {}, || {
        with_provider[Stdout](C {}, || {
            with_provider[Stderr](Y {}, || {
                shout("hi");
                eprint("e");
                twice(|| print("t"));
            });
        });
    });
    println("");
}
"#;

#[test]
fn provider_methods_run_with_their_binding_site_x_c_y() {
    check("xcy", XCY, ("X[C[hi]]X[C[Y[e]]]X[C[t]]X[C[t]]\n", ""));
}

/// A tee: a fake `Stdout` that counts and forwards to the `Stdout` it was
/// bound under, the real stream at top level and an outer fake inside
/// one.
const TEE: &str = r#"import std.io.{StdoutProvider};
struct Outer {}
impl StdoutProvider for Outer {
    fn write(mut ref self, s: Str) { eprint(f"<{s}>"); }
    fn flush(mut ref self) {}
}

struct Tee { n: i64 }
impl StdoutProvider for Tee {
    fn write(mut ref self, s: Str) {
        self.n = self.n + 1;
        print(f"{self.n}:{s}");
    }
    fn flush(mut ref self) {}
}

fn main() {
    with_provider[Stdout](Tee { n: 0 }, || {
        print("a");
        print("b");
    });
    with_provider[Stdout](Outer {}, || {
        with_provider[Stdout](Tee { n: 10 }, || print("c"));
        print("d");
    });
    println("");
}
"#;

#[test]
fn provider_tee_forwards_to_the_binding_outside_it() {
    check("tee", TEE, ("1:a2:b\n", "<11:c><d>"));
}

/// A `Stdout` fake that prints to `Stderr` and a `Stderr` fake that prints
/// to `Stdout`, one after the other and never nested: each reaches the real
/// other stream (probe wk48j e1, the row's case (3)).
const DISJOINT: &str = r#"import std.io.{StderrProvider, StdoutProvider};
struct Cap { n: i64 }
impl StdoutProvider for Cap { fn write(mut ref self, s: Str) { eprintln("c"); } fn flush(mut ref self) {} }
struct ErrCap {}
impl StderrProvider for ErrCap { fn write(mut ref self, s: Str) { print("E"); } fn flush(mut ref self) {} }
fn helper() { print("a"); }
fn main() { with_provider[Stdout](Cap { n: 0 }, || { helper(); }); with_provider[Stderr](ErrCap {}, || { eprintln("warn"); }); }
"#;

#[test]
fn provider_fakes_of_each_stream_reach_the_real_other() {
    check("disjoint", DISJOINT, ("E", "c\n"));
}

/// The two fakes nested: the inner `Stdout` fake's `eprintln` reaches the
/// outer `Stderr` fake, whose `print` reaches the `Stdout` in scope where
/// it was bound, the real one (probe wk48j e5, the row's case (2)).
const MUTUAL: &str = r#"import std.io.{StderrProvider, StdoutProvider};
struct Cap { n: i64 }
impl StdoutProvider for Cap { fn write(mut ref self, s: Str) { eprintln("c"); } fn flush(mut ref self) {} }
struct ErrCap {}
impl StderrProvider for ErrCap { fn write(mut ref self, s: Str) { print("E"); } fn flush(mut ref self) {} }
fn main() { with_provider[Stderr](ErrCap {}, || { with_provider[Stdout](Cap { n: 0 }, || { print("a"); }); }); }
"#;

#[test]
fn provider_mutual_nest_reaches_the_outer_fake_then_the_real_stream() {
    check("mutual", MUTUAL, ("E", ""));
}

/// The mutual nest through two other resources, each fake using the
/// other's (probe wk48j e7).
const MUTUAL_CLOCK: &str = r#"import std.random.{RandomProvider}; import std.time.{ClockProvider};
import std.time.{Clock};
import std.random;
struct R1 { k: u64 }
impl RandomProvider for R1 { fn next_u64(mut ref self) -> u64 { Clock.now() as u64 } }
struct FakeClock {}
impl ClockProvider for FakeClock { fn now(ref self) -> i64 { random.next_u64() as i64 } }
fn main() { println(with_provider[RandomSource](R1 { k: 0 }, || random.next_u64() + random.next_u64()) > 0); println(with_provider[Clock](FakeClock {}, || Clock.now()) != 0); }
"#;

#[test]
fn provider_mutual_nest_through_clock_and_random() {
    check("mutual_clock", MUTUAL_CLOCK, ("true\ntrue\n", ""));
}

/// A `Stderr` fake whose `write` prints to `Stderr` itself reaches the
/// real stream, never itself, beside a disjoint `Stdout` fake (probe wk48j
/// e6, the row's case (1)).
const OWN_RESOURCE: &str = r#"import std.io.{StderrProvider, StdoutProvider};
struct Cap { n: i64 }
impl StdoutProvider for Cap { fn write(mut ref self, s: Str) { eprintln("c"); } fn flush(mut ref self) {} }
struct ErrCap {}
impl StderrProvider for ErrCap { fn write(mut ref self, s: Str) { eprintln("E"); } fn flush(mut ref self) {} }
fn main() { with_provider[Stdout](Cap { n: 0 }, || { print("a"); }); with_provider[Stderr](ErrCap {}, || { eprintln("warn"); }); }
"#;

#[test]
fn provider_using_its_own_resource_reaches_the_outer_binding() {
    check("own_resource", OWN_RESOURCE, ("", "c\nE\n"));
}

/// A tee nested `n` deep, written out: each `Tee` bound where the one
/// before it is in scope, and printing to it.
fn tee_nest(n: usize) -> String {
    let mut body = "print(\"x\")".to_string();
    for i in 0..n {
        body = format!("with_provider[Stdout](Tee {{ n: {i} }}, || {body})");
    }
    format!(
        "import std.io.{{StdoutProvider}}; struct Tee {{ n: i64 }}\n\
         impl StdoutProvider for Tee {{ fn write(mut ref self, s: Str) {{ print(f\"[{{s}}]\"); }} \
         fn flush(mut ref self) {{}} }}\n\
         fn main() {{ {body}; println(\"\"); }}\n"
    )
}

/// `karac check` of `src`: whether it passed, and what it printed.
fn check_only(name: &str, src: &str) -> (bool, String) {
    let dir = Scratch::new(name);
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Sixteen tees nested, each forwarding to the one outside it, run; the
/// seventeenth is refused by the check with `E_PROVIDER_NEST` (core
/// semantics §14.4).
#[test]
fn provider_nest_of_sixteen_runs_and_seventeen_is_refused() {
    let sixteen = tee_nest(16);
    let want = format!("{}x{}\n", "[".repeat(16), "]".repeat(16));
    check("nest16", &sixteen, (&want, ""));
    let (ok, text) = check_only("nest17", &tee_nest(17));
    assert!(
        !ok && text.contains("error[E_PROVIDER_NEST]")
            && text.contains("nests more than 16 provider bindings"),
        "{text}"
    );
}

/// A recursion that binds a tee around each recursive call is refused,
/// however deep it runs.
#[test]
fn provider_rebound_on_every_round_of_a_recursion_is_refused() {
    let src = "import std.io.{StdoutProvider}; struct Tee { n: i64 }\n\
        impl StdoutProvider for Tee { fn write(mut ref self, s: Str) { print(f\"[{s}]\"); } \
        fn flush(mut ref self) {} }\n\
        fn nest(k: i64) { if k == 0 { print(\"x\"); } else { \
        with_provider[Stdout](Tee { n: k }, || nest(k - 1)); } }\n\
        fn main() { nest(3); println(\"\"); }\n";
    let (ok, text) = check_only("nest_rec", src);
    assert!(!ok && text.contains("error[E_PROVIDER_NEST]"), "{text}");
}
