//! A `karac build` under a manifest `cpu-baseline` level installs that
//! level's CPU without validating it against LLVM's registry, which costs a
//! child karac per build (B-2026-10-31-8). That is sound only while every
//! name the baseline table can produce is in the registry; this reads the
//! registry through karac's own `__list-target-cpus` dump and checks each
//! one. With no level, a build takes the per-target table, `x86-64-v2` on
//! x86_64 (B-2026-11-16-201), which the last test pins.

#![cfg(feature = "llvm")]

use std::process::Command;

fn listing(triple: Option<&str>) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.args(["__list-target-cpus", "native"]);
    if let Some(t) = triple {
        cmd.arg(t);
    }
    String::from_utf8_lossy(&cmd.output().expect("karac").stderr).into_owned()
}

fn names_in(listing: &str, section: &str) -> Vec<String> {
    let mut inside = false;
    let mut out = Vec::new();
    for line in listing.lines() {
        if line.starts_with("Available ") {
            inside = line.starts_with(section);
            continue;
        }
        if inside {
            if let Some(name) = line
                .strip_prefix("  ")
                .and_then(|r| r.split_whitespace().next())
            {
                out.push(name.to_string());
            }
        }
    }
    out
}

#[test]
fn every_cpu_baseline_name_is_in_llvms_registry() {
    for (arch, triple) in [
        ("x86_64", "x86_64-unknown-linux-gnu"),
        ("aarch64", "aarch64-unknown-linux-gnu"),
    ] {
        let dump = listing(Some(triple));
        let cpus = names_in(&dump, "Available CPUs");
        let features = names_in(&dump, "Available features");
        assert!(!cpus.is_empty() && !features.is_empty(), "{triple}: {dump}");
        for level in ["v1", "v2", "v3", "v4"] {
            let (cpu, feats) = karac::manifest::cpu_baseline_native_override(level, arch);
            if let Some(cpu) = cpu {
                assert!(
                    cpus.contains(&cpu),
                    "{arch} {level}: CPU {cpu} not in the registry"
                );
            }
            for f in feats.iter().flat_map(|f| f.split(',')) {
                let name = f.trim_start_matches(['+', '-']);
                assert!(
                    features.iter().any(|k| k == name),
                    "{arch} {level}: feature {f} not in the registry"
                );
            }
        }
    }
}

/// design.md § CPU baseline: a default build runs on every CPU the triple
/// promises, so on x86_64 it is `x86-64-v2` and emits no AVX register; the
/// v3 it used to default to emitted 22 `ymm` instructions for this loop
/// (review-thread ruling 2026-11-16, B-2026-11-16-201). `--target-cpu`
/// still raises it, which is the control arm.
#[test]
fn a_default_build_stays_off_avx_on_x86_64() {
    if std::env::consts::ARCH != "x86_64" {
        return;
    }
    let dir = std::env::temp_dir().join(format!("karac-cpu-default-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("vec.kara"),
        "fn scale(v: mut ref Vec[f64], k: f64) { for i in 0..v.len() { v[i] = v[i] * k + 1.0; } }\n\
         fn main() {\n    let mut v: Vec[f64] = Vec.filled(4096, 1.5);\n    scale(mut v, 2.0);\n    \
         println(v[7]);\n}\n",
    )
    .unwrap();
    let ymm = |extra: &[&str], out: &str| -> usize {
        let st = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["build", "vec.kara", "-o", out])
            .args(extra)
            .current_dir(&dir)
            .env_remove("KARAC_TARGET_CPU")
            .env_remove("KARAC_TARGET_FEATURES")
            .output()
            .expect("karac");
        assert!(
            st.status.success(),
            "{}",
            String::from_utf8_lossy(&st.stderr)
        );
        let dis = Command::new("objdump")
            .args(["-d", out])
            .current_dir(&dir)
            .output()
            .expect("objdump");
        String::from_utf8_lossy(&dis.stdout).matches("%ymm").count()
    };
    assert_eq!(ymm(&[], "plain"), 0, "a default build used AVX registers");
    assert!(
        ymm(&["--target-cpu=x86-64-v3"], "v3") > 0,
        "control: x86-64-v3 emitted no ymm, so the default's 0 proves nothing"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
