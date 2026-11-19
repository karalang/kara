//! B-2026-11-19-108: a call through a `dyn` parameter whose type declares
//! its effects (a `with` clause on the type, or the trait method's own
//! clause, a ceiling every concrete method stays within) has those effects
//! in a `par for` (design.md § `dyn Trait`), so two iterations calling a
//! pure one do not conflict, and two calling one that writes a resource do.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, sub: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-par-dyn-declared-{tag}-{}.kara",
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

fn runs(tag: &str, src: &str, want: &str) {
    let out = karac(tag, "check", src);
    assert!(
        out.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac(tag, "__mir-run", src);
    assert!(
        out.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{tag}");
}

const HEAD: &str = "trait T { fn g(self, x: i64) -> i64; }
trait U { fn g(self, x: i64) -> i64 with allocates(Heap); }
struct S {}
impl T for S { fn g(self, x: i64) -> i64 { x * 2 } }
impl U for S { fn g(self, x: i64) -> i64 { x * 3 } }
";

#[test]
fn par_for_through_a_dyn_param_whose_type_writes_an_empty_clause() {
    let src = format!(
        "{HEAD}fn w(xs: Vec[i64], t: dyn T + CrossTask with ()) -> Vec[i64] {{
    let r = par for x in xs.iter() {{ t.g(x) }};
    r
}}
fn v(xs: Vec[i64], t: ref dyn T + CrossTask with ()) -> Vec[i64] {{
    let r = par for x in xs.iter() {{ t.g(x) }};
    r
}}
fn main() {{ let s = S {{}}; println(f\"{{w([1, 2], S {{}})}} {{v([3], s)}}\"); }}
"
    );
    runs("empty-clause", &src, "[2, 4] [6]\n");
}

#[test]
fn par_for_through_a_dyn_param_whose_method_writes_a_ceiling() {
    let src = format!(
        "{HEAD}fn du(xs: Vec[i64], t: dyn U + CrossTask) -> Vec[i64] {{
    let r = par for x in xs.iter() {{ t.g(x) }};
    r
}}
fn main() {{ println(f\"{{du([1, 2], S {{}})}}\"); }}
"
    );
    runs("ceiling", &src, "[3, 6]\n");
}

#[test]
fn par_for_through_a_dyn_param_that_writes_a_resource_still_conflicts() {
    let src = "effect resource Log;
trait W { fn g(self, x: i64) -> i64 with writes(Log); }
struct S {}
impl W for S { fn g(self, x: i64) -> i64 with writes(Log) { x } }
fn dw(xs: Vec[i64], t: dyn W + CrossTask) -> Vec[i64] {
    let r = par for x in xs.iter() { t.g(x) };
    r
}
fn main() { println(f\"{dw([1, 2], S {})}\"); }
";
    let out = karac("writes", "check", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "writes was accepted");
    assert!(
        stderr.contains("two iterations of the `par for` conflict")
            && stderr.contains("writes(Log)"),
        "{stderr}"
    );
}
