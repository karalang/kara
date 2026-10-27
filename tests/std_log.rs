//! `std.log` (docs/library/log.md; B-2026-10-27-20): the level calls' open
//! `key: value` fields and the errors `karac check` reports on them, the
//! default backend, transparency to effects, and the order of one task's
//! lines under statement auto-par. The formats are pinned by the corpus
//! programs `corpus/core/log_*`.

use std::path::PathBuf;
use std::process::{Command, Output};

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-std-log-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `karac check` refuses `src` with `expect` in its report.
fn refused(tag: &str, src: &str, expect: &str) -> PathBuf {
    let path = write(tag, src);
    let out = karac(&["check"], &path);
    let err = text(&out.stderr);
    assert!(!out.status.success(), "{tag}: check must refuse:\n{err}");
    assert!(
        err.contains(expect),
        "{tag}: expected `{expect}` in:\n{err}"
    );
    path
}

#[test]
fn std_log_reserved_label_refused() {
    for label in ["level", "msg", "trace_id", "span_id", "task_id"] {
        let src = format!(
            "import std.log.{{info}};\n\nfn main() {{\n    info(\"start\", {label}: 1);\n}}\n"
        );
        refused(
            &format!("reserved-{label}"),
            &src,
            &format!("`{label}` cannot label a log field: every record already carries `{label}`"),
        );
    }
}

#[test]
fn std_log_message_may_be_labeled_msg() {
    let path = write(
        "msg-label",
        "import std.log.{info};\n\nfn main() {\n    info(msg: \"start\", port: 80);\n}\n",
    );
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn std_log_duplicate_label_refused() {
    refused(
        "duplicate",
        "import std.log.{warn};\n\nfn main() {\n    warn(\"retry\", attempt: 1, attempt: 2);\n}\n",
        "the log field `attempt` is given twice in one call",
    );
}

#[test]
fn std_log_unlabeled_field_refused() {
    refused(
        "unlabeled",
        "import std.log.{error};\n\nfn main() {\n    let status = 500;\n    error(\"failed\", status);\n}\n",
        "a log field after the message is written `key: value`",
    );
}

#[test]
fn std_log_unserializable_field_fixed_as_text() {
    let src = "import std.log.{info};\n\nstruct Port {\n    n: i64,\n}\n\nimpl Display for Port {\n    fn to_string(ref self) -> String {\n        f\"port {self.n}\"\n    }\n}\n\nfn main() {\n    let p = Port { n: 80 };\n    info(\"listening\", port: p);\n}\n";
    let path = refused(
        "not-serialize",
        src,
        "the log field `port` is a `Port`, which does not implement `Serialize`",
    );
    karac(&["fix"], &path);
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(fixed.contains("port: f\"{p}\""), "{fixed}");
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{fixed}\n{}", text(&out.stderr));
}

#[test]
fn std_log_unserializable_field_without_display_says_derive() {
    refused(
        "not-serialize-derive",
        "import std.log.{info};\n\nstruct Plain {\n    n: i64,\n}\n\nfn main() {\n    let p = Plain { n: 1 };\n    info(\"x\", p: p);\n}\n",
        "derive it with `#[derive(Serialize)]` on `Plain`",
    );
}

#[test]
fn std_log_observability_attribute_reserved() {
    refused(
        "attr",
        "#[observability]\nfn quiet() {\n    println(\"x\");\n}\n\nfn main() {\n    quiet();\n}\n",
        "`#[observability]` is reserved for stdlib source",
    );
}

/// A public function declared with no effects may log, and two `par`
/// branches that log do not conflict.
#[test]
fn std_log_transparent_to_effects() {
    let path = write(
        "transparent",
        "import std.log.{info, warn};\n\npub fn add(a: i64, b: i64) -> i64 {\n    info(\"adding\", a: a, b: b);\n    a + b\n}\n\nfn main() {\n    par { warn(\"left\", side: 1), warn(\"right\", side: 2) };\n    println(f\"{add(1, 2)}\");\n}\n",
    );
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", text(&out.stderr));
}

/// Before any `init`, records at `Info` and above go to standard error as
/// JSON, and `main`'s lines keep their source order with no task id, even
/// where statement auto-par would run independent statements together.
#[test]
fn std_log_default_backend_in_order() {
    let path = write(
        "default",
        "import std.log.{debug, info, warn};\n\nfn main() {\n    let xs = vec![3, 1, 2];\n    let ys = vec![\"b\", \"a\"];\n    debug(\"dropped\", xs: xs);\n    info(\"first\", xs: xs);\n    warn(\"second\", ys: ys);\n    info(\"third\", n: xs.len() + ys.len());\n    println(\"done\");\n}\n",
    );
    let expect = "{\"level\":\"info\",\"msg\":\"first\",\"xs\":[3,1,2],\"task_id\":null}\n\
                  {\"level\":\"warn\",\"msg\":\"second\",\"ys\":[\"b\",\"a\"],\"task_id\":null}\n\
                  {\"level\":\"info\",\"msg\":\"third\",\"n\":5,\"task_id\":null}\n";
    for lane in [&["run"][..], &["run", "--interp"]] {
        let out = karac(lane, &path);
        assert!(out.status.success(), "{lane:?}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "done\n", "{lane:?}");
        assert_eq!(text(&out.stderr), expect, "{lane:?}");
    }
}
