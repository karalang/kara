//! B-2026-11-16-274: auto-par kept `parallax_lite`'s three provider writes
//! (`record_a(); record_b(); record_c();`, `writes(MetricsA/B/C)`) in order,
//! calling the mutable borrows of the providers of `MetricsA` and `MetricsB`
//! a conflict, while a written `par` over the same calls checks and runs.
//! The written `par` was right: each resource's provider is a value its own
//! `with_provider` moved in, so two hidden provider parameters never hold one
//! value (core semantics §14.4, §14.9), and a provider shared between two
//! resources would have to be a `sync` value to cross a task at all. A
//! statement group that writes one resource twice still keeps its order.
//! `record` nests its loops so the group passes the cost rule (two
//! statements of weight 2); `parallax_lite`'s single loop is weight 1, so
//! that example now stops at the cost rule instead of the conflict.

use std::process::Command;

fn src(body: &str) -> String {
    format!(
        r#"pub trait Recorder {{
    fn record(mut ref self, value: i64);
}}
pub effect resource MetricsA: Recorder;
pub effect resource MetricsB: Recorder;
pub struct Mem {{
    total: i64,
}}
impl Recorder for Mem {{
    fn record(mut ref self, value: i64) {{
        let mut sum: i64 = 0;
        let mut i: i64 = 0;
        while i < value {{
            let mut j: i64 = 0;
            while j < i {{
                sum = sum + j;
                j = j + 1;
            }}
            i = i + 1;
        }}
        self.total = self.total + sum;
    }}
}}
fn record_a() with writes(MetricsA) {{
    MetricsA.record(1000);
}}
fn record_b() with writes(MetricsB) {{
    MetricsB.record(1000);
}}
pub fn process() with writes(MetricsA) writes(MetricsB) {{
{body}
}}
fn main() {{
    providers {{ MetricsA => Mem {{ total: 0 }}, MetricsB => Mem {{ total: 0 }} }} in {{ process(); }}
    println("done");
}}
"#
    )
}

/// `process`'s statement group in `karac query concurrency`'s `auto_par`
/// list: whether it runs as tasks, and why not.
fn verdict(tag: &str, body: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-auto-par-provider-writes-{}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src(body)).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_AUTO_PAR")
        .env_remove("KARAC_AUTO_PAR_SIZE")
        .env_remove("KARAC_AUTO_PAR_BODY_MAX")
        .args(["query", "concurrency", file.to_str().unwrap(), "process"])
        .output()
        .unwrap();
    let json = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{json}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let groups: Vec<&serde_json::Value> = v["auto_par"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            e["function"]
                .as_str()
                .is_some_and(|f| f.starts_with("process{"))
                && e["kind"] == "statements"
        })
        .collect();
    assert_eq!(groups.len(), 1, "{json}");
    let _ = std::fs::remove_dir_all(&dir);
    (
        groups[0]["tasks"].as_bool().unwrap(),
        groups[0]["reason"].as_str().unwrap_or("").to_string(),
    )
}

#[test]
fn auto_par_runs_writes_of_two_providers_as_tasks() {
    let (tasks, why) = verdict("two", "    record_a();\n    record_b();");
    assert!(tasks, "two resources' providers kept their order: {why}");
}

#[test]
fn auto_par_keeps_two_writes_of_one_provider_in_order() {
    let (tasks, why) = verdict("one", "    record_a();\n    record_a();");
    assert!(!tasks, "two writes of `MetricsA` ran as tasks");
    assert!(why.contains("MetricsA"), "{why}");
}
