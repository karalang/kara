//! A default `karac build` installs its CPU baseline (`cpu-baseline`, `v3`
//! unless the manifest says otherwise) without validating it against LLVM's
//! registry, which costs a child karac per build (B-2026-10-31-8). That is
//! sound only while every name the baseline table can produce is in the
//! registry; this reads the registry through karac's own
//! `__list-target-cpus` dump and checks each one.

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
