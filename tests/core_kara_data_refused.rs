//! `Column`, `DataFrame`, `LazyExpr` and `Tensor` are the `kara-data`
//! package's (docs/library/data.md), and the legacy library's baked
//! surface for them is lowered by no v2 backend. `karac check` passed a
//! program that builds one and `karac run` then refused it ("native
//! `Column[f64].from_vec` is not lowered yet"); a constructor call is now
//! refused at check, as `Stats` is (B-2026-10-28-26).

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-karadata-{}-{name}", std::process::id()));
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

#[test]
fn a_column_built_from_a_vec_is_refused_at_check() {
    let src = "fn main() {
    let a = Column.from_vec([1.0, 2.0, 3.0].to_vec());
    println(a.sum());
}
";
    let (ok, err) = check("column", src);
    assert!(!ok, "check must refuse:\n{err}");
    let want = "main.kara:2:13: `Column.from_vec` is not in the standard library: \
                `Column` comes with the `kara-data` package after v1 (docs/library/data.md)";
    assert!(err.contains(want), "want {want:?} in\n{err}");
}

#[test]
fn a_dataframe_a_lazy_expression_and_a_tensor_are_refused_at_check() {
    let src = "fn main() {
    let df = DataFrame.new();
    let e = LazyExpr.col(\"x\");
    let t: Tensor[f64, [2, 3]] = Tensor.zeros([2, 3].to_vec());
    println(df.width());
}
";
    let (ok, err) = check("frame", src);
    assert!(!ok, "check must refuse:\n{err}");
    for want in [
        "main.kara:2:14: `DataFrame.new` is not in the standard library",
        "main.kara:3:13: `LazyExpr.col` is not in the standard library",
        "main.kara:4:34: `Tensor.zeros` is not in the standard library: `Tensor` comes \
         with the `kara-data` package after v1 (docs/deferred.md § Numerical types)",
    ] {
        assert!(err.contains(want), "want {want:?} in\n{err}");
    }
}

#[test]
fn a_program_s_own_column_type_still_checks_and_runs() {
    let src = "struct Column { n: i64 }
impl Column { fn new() -> Column { return Column { n: 3 }; } }
fn main() { let c = Column.new(); println(c.n); }
";
    let (ok, err) = check("own-column", src);
    assert!(ok, "{err}");
    let out = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(out.output, "3\n");
}
