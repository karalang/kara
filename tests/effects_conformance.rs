//! Claims of design.md § 12 Effects checked against the binary.
//!
//! - `karac explain` and `karac query effects` wrote a resource-less effect
//!   as `blocks()` (B-2026-11-16-213); it is `blocks`, as source writes it,
//!   and so are the impl-ceiling and public-declaration diagnostics.
//! - A derivation step into the library named the user's file with the
//!   library's line (B-2026-11-16-214); the call into the library now ends
//!   the chain, saying what the library function declares.
//! - A derivation named the checker's internal keys, `__builtin_heap_alloc`
//!   and `__builtin_int_overflow` (B-2026-11-16-215) and
//!   `__bound_ceiling.Store.load` (B-2026-11-16-216); it now names the
//!   construct, or the trait method and how it was reached.
//! - `dbg` inside a `par` branch carried no task id, and
//!   `KARAC_DBG_OUTPUT=json` left it in the terminal format
//!   (B-2026-11-16-217), on every lane.
//! - Review-thread rulings 2026-11-16: a `par` branch's task id counts
//!   from 1 in source order on every lane (B-2026-11-16-225); a call through
//!   a bound is charged the trait's clause only (B-2026-11-16-226); an
//!   empty slot is written `with ()` (B-2026-11-16-227); a closed reader on
//!   karac's stdout ends it quietly with SIGPIPE's 141 (B-2026-11-16-224).

use std::path::PathBuf;
use std::process::{Command, Output};

const LLVM: bool = cfg!(feature = "llvm");

fn file(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-effects-conf-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    dir
}

fn karac(dir: &PathBuf, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.current_dir(dir).args(args).env_remove("KARAC_DBG_OUTPUT");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

const NAP: &str =
    "import std.time;\nfn nap() { time.sleep(Duration.ms(1)); }\nfn main() { nap(); }\n";

#[test]
fn effects_explain_writes_resource_less_verbs_bare() {
    // A sleep suspends, and a file read blocks (B-2026-11-27-72): both verbs name
    // no resource.
    let dir = file(
        "bare-blocks",
        "fn load() -> bool { fs.read_to_string(\"x\").is_ok() }\nfn main() { println(load()); }\n",
    );
    let out = text(&karac(&dir, &["explain", "main.kara.load"], &[]).stdout);
    assert!(out.contains("\n  blocks\n"), "{out}");
    assert!(!out.contains("()"), "{out}");
    let dir = file("bare", NAP);
    let out = text(&karac(&dir, &["explain", "main.kara.nap"], &[]).stdout);
    assert!(out.contains("\n  suspends\n"), "{out}");
    assert!(!out.contains("()"), "{out}");
    let q = text(&karac(&dir, &["query", "effects", "main.kara.nap"], &[]).stdout);
    let v: serde_json::Value = serde_json::from_str(q.trim()).unwrap();
    let reasons: Vec<&str> = v["derivation"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["reason"].as_str().unwrap())
        .collect();
    assert!(
        reasons.contains(&"nap calls sleep, which declares suspends"),
        "{q}"
    );
    assert!(!q.contains("suspends()"), "{q}");
}

#[test]
fn effects_derivation_stops_at_the_library_call() {
    let dir = file("lib", NAP);
    let out = text(&karac(&dir, &["explain", "main.kara.nap"], &[]).stdout);
    // Every step is a line of main.kara, which has three.
    for l in out.lines().filter(|l| l.contains("main.kara:")) {
        let n: usize = l
            .split("main.kara:")
            .nth(1)
            .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|d| d.parse().ok())
            .unwrap_or(0);
        assert!((1..=3).contains(&n), "{l}\n{out}");
    }
    assert!(
        out.contains("main.kara:2  nap calls sleep, which declares reads(Clock)"),
        "{out}"
    );
}

#[test]
fn effects_derivation_names_constructs_not_internal_keys() {
    let src = "fn mk(n: i64) -> String {\n    let v = [1, 2, 3];\n    f\"n={n} {v.len()}\"\n}\nfn ovf(a: i64) -> i64 {\n    a + 1\n}\nfn main() {\n    println(mk(3));\n    println(f\"{ovf(1)}\");\n}\n";
    let dir = file("keys", src);
    let mk = text(&karac(&dir, &["explain", "main.kara.mk"], &[]).stdout);
    assert!(
        mk.contains("mk has allocates(Heap) (a collection literal or f-string that allocates)"),
        "{mk}"
    );
    let ovf = text(&karac(&dir, &["explain", "main.kara.ovf"], &[]).stdout);
    assert!(
        ovf.contains("ovf has panics (integer arithmetic that can overflow)"),
        "{ovf}"
    );
    assert!(!mk.contains("__") && !ovf.contains("__"), "{mk}\n{ovf}");
}

const STORE: &str = "trait Store {\n    fn load(self, k: i64) -> i64 with reads(Clock);\n}\nstruct A {}\nimpl Store for A {\n    fn load(self, k: i64) -> i64 with reads(Clock) { k + 1 }\n}\nfn get[T: Store](s: T, k: i64) -> i64 { s.load(k) }\nfn main() {\n    println(get(A {}, 1));\n}\n";

#[test]
fn effects_derivation_through_a_bound_names_the_trait_method() {
    let dir = file("bound", STORE);
    let out = text(&karac(&dir, &["explain", "main.kara.get"], &[]).stdout);
    assert!(
        out.contains("get calls Store.load (through a bound: the trait's clause)"),
        "{out}"
    );
    assert!(
        out.contains("main.kara:2  Store.load declares reads(Clock)"),
        "{out}"
    );
    assert!(!out.contains("__"), "{out}");
}

#[test]
fn effects_ceiling_diagnostic_writes_blocks_bare() {
    let src = "trait Store {\n    fn load(self, k: i64) -> i64 with reads(Clock);\n}\nstruct C {}\nimpl Store for C {\n    fn load(self, k: i64) -> i64 with reads(Clock) blocks { k }\n}\nfn main() { println(C {}.load(1)); }\n";
    let dir = file("ceiling", src);
    let out = karac(&dir, &["check", "main.kara"], &[]);
    let err = text(&out.stderr) + &text(&out.stdout);
    assert!(
        err.contains("impl method 'C.load' performs blocks but"),
        "{err}"
    );
    assert!(!err.contains("blocks()"), "{err}");
}

const DBG: &str = "fn f(x: i64) -> i64 {\n    dbg(x * 2)\n}\nfn main() {\n    let top = dbg(7);\n    let (a, b) = par { f(1), f(2) };\n    println(f\"{top} {a} {b}\");\n}\n";

fn dbg_lanes() -> Vec<&'static str> {
    let mut l = vec!["__mir-run"];
    let archive =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/release/libkarac_runtime.a");
    if LLVM && archive.exists() {
        l.push("__mir-llvm-run");
    }
    l
}

#[test]
fn effects_dbg_in_a_branch_carries_its_task() {
    let dir = file("dbg", DBG);
    for lane in dbg_lanes() {
        let out = karac(&dir, &[lane, "main.kara"], &[]);
        assert_eq!(text(&out.stdout), "7 2 4\n", "{lane}");
        let err = text(&out.stderr);
        assert!(err.contains("[main.kara:5] 7 = 7\n"), "{lane}: {err}");
        let tagged: Vec<&str> = err.lines().filter(|l| l.starts_with("[task:")).collect();
        assert_eq!(tagged.len(), 2, "{lane}: {err}");
        assert!(
            tagged.iter().all(|l| l.contains(" main.kara:2] x * 2 = ")),
            "{lane}: {err}"
        );
    }
}

#[test]
fn effects_dbg_structured_output_is_one_object_per_call() {
    let dir = file("dbgjson", DBG);
    for lane in dbg_lanes() {
        let out = karac(&dir, &[lane, "main.kara"], &[("KARAC_DBG_OUTPUT", "json")]);
        let err = text(&out.stderr);
        let objs: Vec<serde_json::Value> = err
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{lane}: {e}: {l}")))
            .collect();
        assert_eq!(objs.len(), 3, "{lane}: {err}");
        let top = objs.iter().find(|o| o["line"] == 5).unwrap();
        assert_eq!(top["kind"], "dbg");
        assert!(top["task_id"].is_null(), "{lane}: {err}");
        assert_eq!(top["expr"], "7");
        assert_eq!(top["type"], "i64");
        assert_eq!(top["value"], "7");
        let branches: Vec<&serde_json::Value> = objs.iter().filter(|o| o["line"] == 2).collect();
        assert_eq!(branches.len(), 2, "{lane}: {err}");
        assert!(
            branches.iter().all(|o| o["task_id"].is_u64()),
            "{lane}: {err}"
        );
        assert_ne!(
            branches[0]["task_id"], branches[1]["task_id"],
            "{lane}: {err}"
        );
        assert!(
            branches.iter().all(|o| o["file"] == "main.kara"),
            "{lane}: {err}"
        );
    }
}

#[test]
fn effects_dbg_task_ids_count_from_one_on_every_lane() {
    let dir = file("dbgids", DBG);
    for lane in dbg_lanes() {
        let out = karac(&dir, &[lane, "main.kara"], &[("KARAC_DBG_OUTPUT", "json")]);
        let err = text(&out.stderr);
        let mut ids: Vec<u64> = err
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|o| o["task_id"].as_u64())
            .collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2], "{lane}: {err}");
    }
}

#[test]
fn effects_a_bound_call_is_charged_the_trait_clause_only() {
    let dir = file("boundonly", STORE);
    let q = text(&karac(&dir, &["query", "effects", "main.kara.get"], &[]).stdout);
    let v: serde_json::Value = serde_json::from_str(q.trim()).unwrap();
    // `A.load`'s `k + 1` may panic; `Store.load`'s clause says only
    // `reads(Clock)`, and that is what `get` is checked against.
    assert_eq!(
        v["inferred_effects"],
        serde_json::json!([{"verb": "reads", "resource": "Clock"}]),
        "{q}"
    );
}

#[test]
fn effects_an_empty_slot_is_written_with_unit_clause() {
    let src = "struct Bus {\n    hs: Vec[Fn(i64) -> i64 with ()],\n}\nfn main() {\n    let mut b = Bus { hs: Vec.new() };\n    b.hs.push(|x| { println(\"io\"); x + 1 });\n}\n";
    let dir = file("unitslot", src);
    let out = karac(&dir, &["check", "main.kara"], &[]);
    let err = text(&out.stderr) + &text(&out.stdout);
    assert!(
        err.contains("has effect writes(Stdout) not declared in slot `with ()`"),
        "{err}"
    );
    assert!(!err.contains("[pure]"), "{err}");
}

#[cfg(unix)]
#[test]
fn effects_a_closed_reader_ends_karac_quietly() {
    use std::os::unix::process::ExitStatusExt;
    let dir = file("epipe", NAP);
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["check", "--output=json", "main.kara"])
        .stdout(writer)
        .output()
        .unwrap();
    assert!(
        out.status.signal() == Some(13) || out.status.code() == Some(141),
        "{:?}",
        out.status
    );
    assert_eq!(text(&out.stderr), "");
}
