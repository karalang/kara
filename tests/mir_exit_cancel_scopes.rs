//! An early exit runs each scope's drops, `defer`s and `errdefer`s,
//! innermost first. In a program that cancels tasks, a call made on the way
//! out has a cancel exit of its own (core-semantics §11.9), and that exit
//! runs only what the early exit had not reached: not again the scopes it
//! already left, which borrow checking read as a drop of a borrowed
//! `String` (B-2026-11-08-25), and not without the entries of the scope it
//! was leaving, which leaked them.

#[cfg(feature = "llvm")]
mod common;

/// The probe's JSON PATCH loop: a match on `(k.as_str(), val)` whose other
/// arm returns early, in a function a `Server.serve` handler calls. The
/// early return's exit reverses the `for` loop's vector, a call with a
/// cancel exit in a program whose isolating tasks leave at a panic.
const SERVED_PATCH: &str = r#"
enum J { B(bool), S(String) }
fn patch(fields: own Vec[(String, J)]) -> Result[i64, String] {
    for (k, val) in fields.into_iter() {
        match (k.as_str(), val) { ("done", J.B(b)) => {}, (_, _) => return Err("bad".to_string()), }
    }
    Ok(1)
}
fn main() { let _ = Server.serve("127.0.0.1:0", |req| { let r = patch([(req.body(), J.S("s".to_string()))]).is_err(); Response { status: 200, body: f"{r}" } }); }
"#;

#[test]
fn mir_early_return_in_a_served_function_passes_borrow_checking() {
    let lowered = karac::mir::lower::build_source(SERVED_PATCH).unwrap_or_else(|e| panic!("{e}"));
    karac::mir::lower::compiled_check(&lowered).unwrap_or_else(|e| panic!("{e}"));
}

/// A `defer` that panics on the way out of an early return, in an
/// isolating task: the inner scope's `Noisy` dropped once, the outer one
/// still dropped by the panic's exit.
const PANIC_IN_EXIT: &str = r#"
struct Noisy { name: String }

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

fn boom(n: i64) -> i64 {
    if n > 0 {
        panic("in the exit");
    }
    0
}

fn f(n: i64) -> i64 {
    let _outer = Noisy { name: "outer" };
    defer boom(n);
    {
        let _inner = Noisy { name: "inner" };
        if n >= 0 {
            return 1;
        }
    }
    2
}

fn main() {
    let g = TaskGroup.isolating();
    let a = g.spawn(|| f(1));
    match a.join() {
        Ok(v) => println(f"ok {v}"),
        Err(TaskFailed.Panicked { message, .. }) => println(f"panicked: {message}"),
        Err(TaskFailed.Cancelled) => println("cancelled"),
    }
}
"#;

const PANIC_IN_EXIT_OUT: &str = "drop inner\ndrop outer\npanicked: in the exit\n";

#[test]
fn mir_a_panic_in_an_early_exit_drops_what_the_exit_had_not_reached() {
    let r = karac::mir::lower::run_source(PANIC_IN_EXIT).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r.output, PANIC_IN_EXIT_OUT, "stderr: {}", r.stderr);
    assert_eq!(r.exit_code(), Some(0), "{:?}", r.outcome);
}

#[cfg(feature = "llvm")]
#[test]
fn mir_llvm_a_panic_in_an_early_exit_drops_what_the_exit_had_not_reached() {
    use std::process::Command;
    let lowered = karac::mir::lower::build_source(PANIC_IN_EXIT).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-exit-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 0).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    if linked.is_some() {
        let out = Command::new(&exe).output().expect("run");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), PANIC_IN_EXIT_OUT);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
