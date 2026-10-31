//! A hand-written `impl Debug` is what `dbg` and a failed `assert_eq` show,
//! at every depth: on its own, as a field, as an element, as a payload
//! (design.md § Display and Debug, B-2026-10-31-12). Its body renders by
//! `Display`, so a `String` field it interpolates is not quoted. Each
//! program writes the same standard error on the interpreter and compiled.

mod common;

use std::process::Command;

/// `src`'s standard error and whether it exited cleanly, on the MIR
/// interpreter (`karac __mir-run`).
fn interp_stderr(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-dbg-user-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The same, compiled at `opt`; `None` without the llvm feature or a
/// runtime archive.
#[cfg(feature = "llvm")]
fn compiled_stderr(src: &str, opt: u8) -> Option<(bool, String)> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("karac-dbg-user-b-{}-{n}", std::process::id()));
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
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

#[cfg(not(feature = "llvm"))]
fn compiled_stderr(_: &str, _: u8) -> Option<(bool, String)> {
    None
}

/// The lines of `err` that start with `prefix`, in order.
fn lines_from(err: &str, prefix: &str) -> Vec<String> {
    err.lines()
        .filter(|l| l.trim_start().starts_with(prefix))
        .map(|l| l.trim_start().to_string())
        .collect()
}

const PT: &str = "struct Pt {
    x: i64,
    tag: String,
}

impl Debug for Pt {
    fn fmt_debug(self) -> String {
        f\"Pt<{self.x} {self.tag}>\"
    }
}

impl PartialEq for Pt {
    fn eq(self, other: Pt) -> bool {
        self.x == other.x
    }
}
";

#[test]
fn dbg_shows_a_hand_written_debug_at_every_depth() {
    let src = format!(
        "{PT}
#[derive(Debug)]
struct Holder {{
    p: Pt,
    n: i64,
}}

#[derive(Debug)]
enum Slot {{
    Full(Pt),
    Empty,
}}

fn main() {{
    let p = Pt {{ x: 4, tag: \"a\".to_string() }};
    dbg(p);
    let h = Holder {{ p: Pt {{ x: 5, tag: \"b\".to_string() }}, n: 1 }};
    dbg(h);
    let v = [Pt {{ x: 6, tag: \"c\".to_string() }}];
    dbg(v);
    let s = Slot.Full(Pt {{ x: 7, tag: \"d\".to_string() }});
    dbg(s);
    let os: Vec[Option[Pt]] = [Some(Pt {{ x: 8, tag: \"e\".to_string() }}), None];
    dbg(os);
}}
"
    );
    let want = [
        "p = Pt<4 a>",
        "h = Holder { p: Pt<5 b>, n: 1 }",
        "v = [Pt<6 c>]",
        "s = Full(Pt<7 d>)",
        "os = [Some(Pt<8 e>), None]",
    ];
    // A `dbg` line is `[file:line] text = value`; the file is named
    // differently on each, so only what follows it is compared.
    let shown = |err: &str| -> Vec<String> {
        lines_from(err, "[")
            .iter()
            .map(|l| l.split_once("] ").map_or(l.clone(), |(_, r)| r.to_string()))
            .collect()
    };
    let (ok, err) = interp_stderr("depth", &src);
    assert!(ok, "{err}");
    assert_eq!(shown(&err), want, "interpreter: {err}");
    for opt in [0, 2] {
        if let Some((ok, err)) = compiled_stderr(&src, opt) {
            assert!(ok, "at -O{opt}: {err}");
            assert_eq!(shown(&err), want, "at -O{opt}: {err}");
        }
    }
}

#[test]
fn assert_eq_failure_shows_a_hand_written_debug() {
    let src = format!(
        "{PT}
fn main() {{
    assert_eq(Pt {{ x: 1, tag: \"l\".to_string() }}, Pt {{ x: 2, tag: \"r\".to_string() }});
}}
"
    );
    let want = ["left:  Pt<1 l>", "right: Pt<2 r>"];
    let sides = |err: &str| {
        let mut v = lines_from(err, "left:");
        v.extend(lines_from(err, "right:"));
        v
    };
    let (ok, err) = interp_stderr("assert", &src);
    assert!(!ok, "the assertion fails");
    assert_eq!(sides(&err), want, "interpreter: {err}");
    for opt in [0, 2] {
        if let Some((ok, err)) = compiled_stderr(&src, opt) {
            assert!(!ok, "at -O{opt}: the assertion fails");
            assert_eq!(sides(&err), want, "at -O{opt}: {err}");
        }
    }
}
