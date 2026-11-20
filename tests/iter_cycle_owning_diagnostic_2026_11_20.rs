//! B-2026-11-20-89: `cycle` and `clone` on a library iterator that is not
//! `Clone` always blamed a closure ("this one holds a closure (`map`,
//! `filter`, ..)"), including on an owning `v.into_iter()`, an
//! `Option.into_iter()` and a `Map.iter()`, which hold none. The closure
//! reason is now given only when the iterator is, or wraps, a closure
//! adaptor; otherwise the diagnostic names the owning iterator and the list in
//! library/iterators.md § `cycle`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Runs `karac check main.kara` on `body` wrapped in `fn main`; returns stderr.
fn check(body: &str) -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-cycdiag-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.kara"),
        format!("fn main() {{\n    {body}\n}}\n"),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "expected a refusal for {body}");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn cycle_on_owning_iterator_names_no_closure() {
    for body in [
        "let v: Vec[i64] = Vec.new(); let n = v.into_iter().cycle().take(2).count();",
        "let o: Option[i64] = Some(1); let n = o.into_iter().cycle().take(2).count();",
        "let m: Map[i64, i64] = Map.new(); let n = m.iter().cycle().take(2).count();",
    ] {
        let err = check(body);
        assert!(err.contains("no method 'cycle'"), "{body}: {err}");
        assert!(!err.contains("holds a closure"), "{body}: {err}");
        assert!(
            err.contains("an owning iterator (`into_iter()`) is not"),
            "{body}: {err}"
        );
    }
}

#[test]
fn cycle_on_closure_adaptor_still_names_the_closure() {
    for body in [
        "let v: Vec[i64] = Vec.new(); let n = v.iter().map(|x| *x).cycle().take(2).count();",
        "let v: Vec[i64] = Vec.new(); let n = v.iter().map(|x| *x).take(3).cycle().take(2).count();",
    ] {
        let err = check(body);
        assert!(err.contains("holds a closure"), "{body}: {err}");
    }
}

#[test]
fn clone_on_owning_iterator_names_no_closure() {
    let err = check("let v: Vec[i64] = Vec.new(); let it = v.into_iter().clone();");
    assert!(err.contains("no method 'clone'"), "{err}");
    assert!(!err.contains("holds a closure"), "{err}");
    let err = check("let v: Vec[i64] = Vec.new(); let it = v.iter().filter(|x| true).clone();");
    assert!(err.contains("holds a closure"), "{err}");
}

/// B-2026-11-20-90: the library's `impl[I: Clone] Clone for IterTake[I]` was
/// taken without its bound, so `v.iter().map(f).take(3).cycle()` passed
/// `karac check` and then failed inside the compiler on the closure-holding
/// `clone`. A wrapper over a closure adaptor is now refused at the call.
#[test]
fn cycle_on_wrapper_over_closure_adaptor_is_refused() {
    for wrap in [
        "take(3)",
        "skip(1)",
        "enumerate()",
        "step_by(2)",
        "rev()",
        "zip(v.iter())",
    ] {
        let body = format!(
            "let v: Vec[i64] = [1, 2].to_vec(); let n = v.iter().map(|x| *x).{wrap}.cycle().take(5).count(); println(f\"{{n}}\");"
        );
        let err = check(&body);
        assert!(err.contains("no method 'cycle'"), "{wrap}: {err}");
        assert!(err.contains("holds a closure"), "{wrap}: {err}");
    }
}
