//! `Ordering.reverse`, `then` and `then_with` (docs/library/core-types.md §
//! `Ordering`): a descending sort, a sort by two keys, and a second key
//! that `then_with` computes only when the first ties. Each program runs
//! the same on the interpreter and compiled.

mod common;

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-ordering-{}-{name}", std::process::id()));
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
    let dir = std::env::temp_dir().join(format!("karac-ordering-b-{}-{n}", std::process::id()));
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

const PLAYERS: &str = "struct Player { name: String, score: i64 }
fn players() -> Vec[Player] {
    [
        Player { name: \"cleo\", score: 7 },
        Player { name: \"ana\", score: 9 },
        Player { name: \"bo\", score: 7 },
        Player { name: \"dev\", score: 9 },
    ]
}
";

#[test]
fn ordering_reverse_then_sorts_by_score_descending_then_name() {
    let src = format!(
        "{PLAYERS}fn main() {{
    let mut ps = players();
    ps.sort_by(|a, b| a.score.cmp(b.score).reverse().then(a.name.cmp(b.name)));
    for p in ps {{ println(f\"{{p.name}} {{p.score}}\"); }}
    let mut ns: Vec[i64] = [3, 1, 2];
    ns.sort_by(|a, b| a.cmp(b).reverse());
    println(f\"{{ns[0]}} {{ns[1]}} {{ns[2]}}\");
}}
"
    );
    runs("then", &src, "ana 9\ndev 9\nbo 7\ncleo 7\n3 2 1\n");
}

#[test]
fn ordering_then_with_runs_its_second_key_only_on_a_tie() {
    let src = format!(
        "{PLAYERS}fn main() {{
    let mut ps = players();
    ps.sort_by(|a, b| a.name.len().cmp(b.name.len()).then_with(|| b.name.cmp(a.name)));
    for p in ps {{ println(p.name); }}
    let a = Ordering.Less.then_with(|| {{
        println(\"second key\");
        Ordering.Greater
    }});
    let b = Ordering.Equal.then_with(|| {{
        println(\"second key\");
        Ordering.Greater
    }});
    println(f\"{{a.is_lt()}} {{b.is_gt()}} {{Ordering.Equal.reverse().is_eq()}}\");
}}
"
    );
    runs(
        "then_with",
        &src,
        "bo\ndev\nana\ncleo\nsecond key\ntrue true true\n",
    );
}
