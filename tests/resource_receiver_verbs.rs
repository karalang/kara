//! A call on a resource takes its verb from the method's receiver (core
//! semantics §14.2): a bare `self` reads `R`, a `mut ref self` writes it.
//! `karac query` reads the source without the check's D5 rewrite of bare
//! parameters, and still reports a bare `self` as a read (B-2026-10-30-15),
//! so two reads of one resource may run together.

use std::process::Command;

const SRC: &str = "trait Source {
    fn load(self) -> i64;
    fn bump(mut ref self) -> i64;
}
effect resource UserDB: Source;
struct Fixed { n: i64 }
impl Source for Fixed {
    fn load(self) -> i64 { self.n }
    fn bump(mut ref self) -> i64 { self.n += 1; self.n }
}
fn a() -> i64 { UserDB.load() }
fn b() -> i64 { UserDB.load() }
fn c() -> i64 { UserDB.bump() }
fn report() {
    let x = a();
    let y = b();
    println(f\"{x + y}\");
}
fn main() {
    providers { UserDB => Fixed { n: 1 } } in { report(); println(c()); }
}
";

fn query(tag: &str, args: &[&str]) -> String {
    let dir =
        std::env::temp_dir().join(format!("karac-receiver-verbs-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("r.kara");
    std::fs::write(&path, SRC).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .arg("query")
        .args(args)
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "query {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `karac query effects`: `a` and `b` read `UserDB`, `c` writes it.
#[test]
fn receiver_verbs_bare_self_is_a_read_in_query_effects() {
    let out = query("effects", &["effects", "r.kara"]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let effects = |name: &str| -> Vec<String> {
        v["functions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["function"] == name)
            .unwrap_or_else(|| panic!("no `{name}` in {out}"))["inferred_effects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                format!(
                    "{}({})",
                    e["verb"].as_str().unwrap(),
                    e["resource"].as_str().unwrap()
                )
            })
            .collect()
    };
    assert_eq!(effects("a"), ["reads(UserDB)"]);
    assert_eq!(effects("b"), ["reads(UserDB)"]);
    assert_eq!(effects("c"), ["writes(UserDB)"]);
}

/// `karac query concurrency`: the two reads in `report` form a group.
#[test]
fn receiver_verbs_two_reads_run_together_in_query_concurrency() {
    let out = query("concurrency", &["concurrency", "r.kara.report"]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let groups = v["parallel_groups"].as_array().expect("groups");
    assert!(
        groups
            .iter()
            .any(|g| g["statements"] == serde_json::json!([0, 1])),
        "statements 0 and 1 should be one group: {out}"
    );
    assert!(!out.contains("conflicts with writes(UserDB)"), "{out}");
}
