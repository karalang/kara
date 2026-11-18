//! A removal by a scalar key frees the index cell the in-line probe found
//! (`karac_mir_map_remove_cell`) rather than handing the runtime the entry
//! to probe for again (B-2026-11-17-134: kata 220's sliding window ran
//! 11.07G instructions with the second probe, 10.23G without). A key the
//! runtime searches for (a `String`) keeps `karac_mir_map_remove_entry`.
//! The values a window of removals and inserts leaves are checked on the
//! compiled backend against the interpreter's and a Python mirror's.
#![cfg(feature = "llvm")]

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

const WINDOW: &str = r#"fn main() {
    let mut m: Map[i64, i64] = Map.new();
    let mut s: Set[i64] = Set.new();
    let mut sum = 0;
    for i in 0..2000 {
        m.insert(i % 97, i);
        s.insert(i % 89);
        if i >= 40 {
            match m.remove((i - 40) % 97) {
                Some(v) => sum = sum + v,
                None => {}
            }
            if s.remove((i - 40) % 89) {
                sum = sum + 1;
            }
        }
    }
    let mut left = 0;
    for k in 0..97 {
        match m.get(k) {
            Some(v) => left = left + k * 1000 + v,
            None => {}
        }
    }
    println(f"{sum} {left} {m.len()} {s.len()}");
}
"#;

#[test]
fn mir_map_remove_cell_frees_the_probed_cell_for_a_scalar_key() {
    let out = ir(WINDOW);
    assert!(out.contains("@karac_mir_map_remove_cell("), "{out}");
    assert!(
        !out.contains("call void @karac_mir_map_remove_entry("),
        "{out}"
    );
}

#[test]
fn mir_map_remove_cell_leaves_a_searched_key_to_remove_entry() {
    let out = ir(r#"fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", 1);
    let _ = m.remove("a");
    println(m.len());
}
"#);
    assert!(out.contains("@karac_mir_map_remove_entry("), "{out}");
    assert!(!out.contains("@karac_mir_map_remove_cell("), "{out}");
}

#[test]
fn mir_map_remove_cell_window_matches_the_interpreter() {
    let dir = std::env::temp_dir().join(format!("karac-remove-cell-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("w.kara");
    std::fs::write(&src, WINDOW).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let interp = std::process::Command::new(karac)
        .arg("__mir-run")
        .arg(&src)
        .output()
        .unwrap();
    let exe = dir.join("w");
    let build = std::process::Command::new(karac)
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    if !build.status.success() {
        let err = String::from_utf8_lossy(&build.stderr);
        assert!(
            err.contains("Linker failed")
                && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none(),
            "does not build: {err}"
        );
        eprintln!("skipping the run: no runtime archive");
        return;
    }
    let run = std::process::Command::new(&exe).output().unwrap();
    assert!(interp.status.success());
    // A Python mirror of the window prints the same.
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        "1921780 1659180 40 40\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&interp.stdout)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
