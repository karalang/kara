//! `v.try_extend(src)` runs on the MIR backends as the `extend` it is the
//! fallible companion of, in an `Ok`. `karac check` used to hide the
//! builder's refusal of it (B-2026-11-01-2).

use std::process::{Command, Output};

fn karac(args: &[&str], src: &str) -> Output {
    let dir = std::env::temp_dir().join(format!("karac-try-extend-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .current_dir(&dir)
        .env("KARAC_AUTO_PAR", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

/// The expected lines are the pushes in order.
#[test]
fn try_extend_appends_and_returns_ok() {
    let src = "fn grow(d: mut ref Vec[i64]) -> Result[(), AllocError] {\n    \
               let y: Vec[i64] = vec![7, 8];\n    d.try_extend(y)?;\n    Ok(())\n}\n\n\
               fn main() {\n    let mut d: Vec[i64] = vec![1];\n    d.try_push(5).unwrap();\n    \
               grow(mut d).unwrap();\n    println(f\"{d}\");\n    \
               println(d.try_extend(vec![9]).is_ok());\n    println(d.len());\n}\n";
    let out = karac(&["check"], src);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let mut cmds = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        cmds.push("run");
    }
    for cmd in cmds {
        let out = karac(&[cmd], src);
        assert!(
            out.status.success(),
            "{cmd}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "[1, 5, 7, 8]\ntrue\n5\n",
            "{cmd}"
        );
    }
}
