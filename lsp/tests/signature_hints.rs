//! Signature help and inlay hints (v1 checklist, Track 3): a call's
//! signature with the parameter under the cursor and each parameter's mode,
//! and the `let` types and private-function effects the checker inferred.

use std::time::Instant;

use karac::{DocumentChecker, InlayHintKind, ParamMode, SignatureHelp};

const PROGRAM: &str = "\
struct P { x: i64 }

impl P {
    fn scale(self, k: i64) -> i64 {
        self.x * k
    }
}

/// Adds one.
fn helper(a: i64, s: own String, v: mut ref Vec[i64]) -> i64 {
    v.push(a);
    a + s.len()
}

fn noisy() {
    println(\"hi\");
}

pub fn shown() -> i64 with writes(Stdout) {
    println(\"x\");
    1
}

fn main() {
    let p = P { x: 1 };
    let mut v: Vec[i64] = Vec.new();
    let y = helper(1, \"s\".to_string(), mut v);
    let k = p.scale(2);
    let found = v.contains(3);
    let n: i64 = 3;
    noisy();
    println(y + k + n + shown());
    println(found);
}
";

/// Signature help with the cursor just after `marker`.
fn help_after(src: &str, marker: &str) -> Option<SignatureHelp> {
    let at = src.find(marker).expect("marker") + marker.len();
    karac::signature_help_at(src, at)
}

fn param_text(h: &SignatureHelp, i: usize) -> &str {
    &h.label[h.params[i].start..h.params[i].end]
}

#[test]
fn signature_help_shows_each_parameters_mode() {
    let h = help_after(PROGRAM, "= helper(").expect("help");
    assert_eq!(
        h.label,
        "fn helper(a: i64, s: own String, v: mut ref Vec[i64]) -> i64"
    );
    let modes: Vec<ParamMode> = h.params.iter().map(|p| p.mode).collect();
    assert_eq!(
        modes,
        [ParamMode::Borrowed, ParamMode::Owned, ParamMode::MutRef]
    );
    assert_eq!(param_text(&h, 1), "s: own String");
    assert_eq!(h.active, Some(0));
    assert_eq!(h.doc.as_deref().map(str::trim), Some("Adds one."));
}

#[test]
fn signature_help_follows_the_argument_under_the_cursor() {
    assert_eq!(help_after(PROGRAM, "helper(1, ").unwrap().active, Some(1));
    assert_eq!(
        help_after(PROGRAM, "helper(1, \"s\".to_string(), mu")
            .unwrap()
            .active,
        Some(2)
    );
    // Inside a nested call's parentheses, that call's signature.
    let inner = help_after(PROGRAM, "\"s\".to_string(").map(|h| h.label);
    assert!(
        inner.as_ref().is_none_or(|l| l.contains("to_string")),
        "{inner:?}"
    );
}

#[test]
fn signature_help_on_a_method_counts_the_receiver() {
    let h = help_after(PROGRAM, "p.scale(").expect("help");
    assert_eq!(h.label, "fn scale(self, k: i64) -> i64");
    assert_eq!(h.params[0].mode, ParamMode::Borrowed);
    assert_eq!(h.active, Some(1));
    // A library method declared in Kāra.
    let h = help_after(PROGRAM, "v.contains(").expect("library help");
    assert!(h.label.starts_with("fn contains("), "{}", h.label);
    assert_eq!(h.active, Some(1));
}

#[test]
fn signature_help_while_the_call_is_still_being_typed() {
    // No `)` yet, and a trailing comma.
    let src = PROGRAM.replace("let k = p.scale(2);", "let k = helper(1, ");
    let h = help_after(&src, "let k = helper(1, ").expect("unclosed call");
    assert_eq!(h.active, Some(1));
    // `(` with nothing typed after it, closed by the editor.
    let src = PROGRAM.replace("let k = p.scale(2);", "let k = helper();");
    let h = help_after(&src, "let k = helper(").expect("empty call");
    assert_eq!(h.active, Some(0));
}

#[test]
fn no_signature_help_outside_a_call() {
    let src = PROGRAM.replace("let n: i64 = 3;", "let n: i64 = (3 + 4);");
    assert_eq!(help_after(&src, "let n: i64 = (3"), None);
    assert_eq!(help_after(PROGRAM, "fn helper("), None);
    assert_eq!(help_after(PROGRAM, "println(\"h"), None);
}

#[test]
fn inlay_hints_show_inferred_let_types_and_effects() {
    let mut c = DocumentChecker::new();
    c.full(PROGRAM);
    let at = |h: &karac::InlayHint| &PROGRAM[..h.offset];
    let types: Vec<(String, String)> = c
        .inlay_hints()
        .iter()
        .filter(|h| h.kind == InlayHintKind::Type)
        .map(|h| {
            let name = at(h).rsplit(' ').next().unwrap_or("").to_string();
            (name, h.label.clone())
        })
        .collect();
    assert_eq!(
        types,
        [
            ("p".to_string(), ": P".to_string()),
            ("y".to_string(), ": i64".to_string()),
            ("k".to_string(), ": i64".to_string()),
            ("found".to_string(), ": bool".to_string()),
        ]
    );
    let effects: Vec<(&str, &str)> = c
        .inlay_hints()
        .iter()
        .filter(|h| h.kind == InlayHintKind::Effects)
        .map(|h| {
            let line = at(h).rsplit('\n').next().unwrap_or("");
            (line, h.label.as_str())
        })
        .collect();
    // Every private function with an effect shows it (overflow-checked
    // arithmetic panics); `shown` declares its own.
    let lines: Vec<&str> = effects.iter().map(|e| e.0.trim_start()).collect();
    assert_eq!(
        lines,
        [
            "fn scale(self, k: i64) -> i64",
            "fn helper(a: i64, s: own String, v: mut ref Vec[i64]) -> i64",
            "fn noisy()",
            "fn main()",
        ]
    );
    assert_eq!(effects[2].1, "with writes(Stdout)");
}

#[test]
fn inlay_hints_after_an_incremental_edit_match_a_full_check() {
    let mut c = DocumentChecker::new();
    c.full(PROGRAM);
    let edited = PROGRAM.replace(
        "    let k = p.scale(2);",
        "    let extra = 2.5;\n    let k = p.scale(2);",
    );
    let (_, kind) = c.check_reporting(&edited);
    assert_eq!(kind, karac::CheckKind::Incremental);
    let mut fresh = DocumentChecker::new();
    fresh.full(&edited);
    assert_eq!(c.inlay_hints(), fresh.inlay_hints());
    assert!(c
        .inlay_hints()
        .iter()
        .any(|h| h.label == ": f64" && edited[..h.offset].ends_with("extra")));
}

#[test]
fn a_let_after_a_derived_type_keeps_its_type() {
    // A derive's items were spliced in numbered from 0, so the nodes after
    // the type lost the ids the type checker's tables are keyed by: no
    // hover or hint on any `let` name past a `#[derive(Serialize)]`.
    let src = "#[derive(Serialize, Deserialize)]\nstruct R {\n    x: i64,\n}\n\n\
               fn order(v: ref Vec[R]) -> i64 {\n    let n = v.len();\n    n\n}\n\n\
               fn main() {\n    println(order(Vec.new()));\n}\n";
    let at = src.find("let n").unwrap() + 4;
    assert_eq!(
        karac::hover_at(src, at).map(|h| h.type_display).as_deref(),
        Some("i64")
    );
    let mut c = DocumentChecker::new();
    c.full(src);
    assert!(c
        .inlay_hints()
        .iter()
        .any(|h| h.label == ": i64" && src[..h.offset].ends_with("let n")));
}

/// Over every `.kara` file under `KARA_SWEEP_DIR`: how many inlay hints of
/// each kind a file gets per thousand lines, and at each call's `(` (a
/// sample of them per file), whether signature help answers and how fast.
/// Run on demand:
/// `KARA_SWEEP_DIR=$PWD/corpus/apps cargo test --release -p kara-lsp --test signature_hints -- --ignored --nocapture`.
#[test]
#[ignore]
fn signature_help_and_hints_over_a_directory() {
    let dir = std::env::var("KARA_SWEEP_DIR").expect("KARA_SWEEP_DIR");
    let mut files = Vec::new();
    let mut work = vec![std::path::PathBuf::from(&dir)];
    while let Some(d) = work.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                work.push(p);
            } else if p.extension().is_some_and(|e| e == "kara")
                && !p.to_string_lossy().ends_with(".orig.kara")
                && !p.to_string_lossy().ends_with(".fixed.kara")
            {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(!files.is_empty(), "no .kara files under {dir}");
    let (mut lines, mut types, mut effects, mut pure_panics) = (0usize, 0usize, 0usize, 0usize);
    let (mut calls, mut answered, mut secs, mut worst) = (0usize, 0usize, 0.0f64, 0.0f64);
    let mut missed: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for path in &files {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        lines += src.lines().count();
        let mut c = DocumentChecker::new();
        c.full(&src);
        for h in c.inlay_hints() {
            match h.kind {
                InlayHintKind::Type => types += 1,
                InlayHintKind::Effects => {
                    effects += 1;
                    pure_panics += usize::from(h.label == "with panics");
                }
            }
        }
        // `name(` inside a body: four of them, spread through the file.
        let opens: Vec<usize> = src
            .match_indices('(')
            .map(|(i, _)| i)
            .filter(|&i| {
                let b = src.as_bytes();
                if i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_') {
                    return false;
                }
                let line = &src[src[..i].rfind('\n').map_or(0, |l| l + 1)..i];
                line.starts_with("    ")
                    && !line.trim_start().starts_with("fn ")
                    && !line.contains("//")
                    && line.matches('"').count() % 2 == 0
            })
            .collect();
        for k in 0..4.min(opens.len()) {
            let at = opens[(opens.len() * (2 * k + 1)) / 8] + 1;
            let t = Instant::now();
            let got = karac::signature_help_at(&src, at);
            let dt = t.elapsed().as_secs_f64();
            secs += dt;
            worst = worst.max(dt);
            calls += 1;
            answered += usize::from(got.is_some());
            if got.is_none() {
                let name: String = src[..at - 1]
                    .chars()
                    .rev()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                *missed.entry(name).or_insert(0usize) += 1;
            }
        }
    }
    println!(
        "SWEEP files={} lines={lines} type_hints={types} ({:.1}/kloc) effect_hints={effects} \
         ({:.1}/kloc, {pure_panics} only `panics`) calls={calls} answered={answered} \
         mean_ms={:.0} worst_ms={:.0}",
        files.len(),
        types as f64 * 1000.0 / lines as f64,
        effects as f64 * 1000.0 / lines as f64,
        secs * 1000.0 / calls.max(1) as f64,
        worst * 1000.0
    );
    let mut missed: Vec<_> = missed.into_iter().collect();
    missed.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    println!("MISSED {:?}", &missed[..missed.len().min(40)]);
}

/// Both over the protocol: the capabilities, a `textDocument/signatureHelp`
/// with the active parameter and the mode in each parameter's text, and
/// `textDocument/inlayHint` limited to the requested range.
#[test]
fn server_answers_signature_help_and_inlay_hints() {
    use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
    use serde_json::json;
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || kara_lsp::serve(server));
    let send = |m: Message| client.sender.send(m).unwrap();
    let req = |id: i32, method: &str, params: serde_json::Value| {
        send(Message::Request(Request {
            id: RequestId::from(id),
            method: method.to_string(),
            params,
        }))
    };
    let notify = |method: &str, params: serde_json::Value| {
        send(Message::Notification(Notification {
            method: method.to_string(),
            params,
        }))
    };
    // The response to request `id`, skipping notifications.
    let response = |id: i32| loop {
        let m = client
            .receiver
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("timed out waiting for the server");
        if let Message::Response(Response {
            id: rid, result, ..
        }) = m
        {
            if rid == RequestId::from(id) {
                return result.unwrap_or(serde_json::Value::Null);
            }
        }
    };
    req(1, "initialize", json!({"capabilities":{}}));
    let caps = response(1);
    assert_eq!(
        caps["capabilities"]["signatureHelpProvider"]["triggerCharacters"],
        json!(["(", ","])
    );
    assert_eq!(caps["capabilities"]["inlayHintProvider"], json!(true));
    notify("initialized", json!({}));
    let uri = "file:///sig.kara";
    notify(
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"languageId":"kara","version":1,"text":PROGRAM}}),
    );
    // `let y = helper(1, |` on line 26 (0-based).
    let line = PROGRAM
        .lines()
        .position(|l| l.contains("let y = helper("))
        .unwrap();
    let col = PROGRAM
        .lines()
        .nth(line)
        .unwrap()
        .find("helper(1, ")
        .unwrap()
        + "helper(1, ".len();
    req(
        2,
        "textDocument/signatureHelp",
        json!({"textDocument":{"uri":uri},"position":{"line":line,"character":col}}),
    );
    let help = response(2);
    let sig = &help["signatures"][0];
    assert_eq!(
        sig["label"],
        json!("fn helper(a: i64, s: own String, v: mut ref Vec[i64]) -> i64")
    );
    assert_eq!(help["activeParameter"], json!(1));
    // Offsets into the label, and what the mode means.
    assert_eq!(sig["parameters"][1]["label"], json!([18, 31]));
    let doc = sig["parameters"][1]["documentation"]["value"]
        .as_str()
        .unwrap();
    assert!(doc.contains("`own`") && doc.contains("moves"), "{doc}");
    let doc = sig["parameters"][2]["documentation"]["value"]
        .as_str()
        .unwrap();
    assert!(doc.contains("`mut ref`"), "{doc}");
    // The hints in `main` only.
    let main = PROGRAM.lines().position(|l| l == "fn main() {").unwrap();
    req(
        3,
        "textDocument/inlayHint",
        json!({"textDocument":{"uri":uri},"range":{
            "start":{"line":main + 1,"character":0},"end":{"line":main + 40,"character":0}}}),
    );
    let hints = response(3);
    let labels: Vec<&str> = hints
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, [": P", ": i64", ": i64", ": bool"]);
    assert_eq!(hints[0]["kind"], json!(1));
    req(4, "shutdown", serde_json::Value::Null);
    response(4);
    notify("exit", serde_json::Value::Null);
    drop(client);
    assert!(server_thread.join().unwrap().is_ok());
}

#[test]
fn signature_help_for_builtin_methods_variants_and_printers() {
    let src = "\
enum Shape {
    Circle(f64),
    Rect(f64, f64),
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    let mut s = String.new();
    s.push_str(\"a\");
    let r: Result[i64, String] = Ok(v.len());
    let sh = Shape.Rect(1.0, 2.0);
    println(s);
}
";
    // A builtin method, from the library reference's table.
    let h = help_after(src, "v.push(").expect("Vec.push");
    assert_eq!(h.label, "fn push(mut ref self, val: own T)");
    assert_eq!(h.active, Some(1));
    assert_eq!(h.params[1].mode, ParamMode::Owned);
    assert_eq!(h.params[0].mode, ParamMode::MutRef);
    let h = help_after(src, "s.push_str(").expect("String.push_str");
    assert!(
        h.label.starts_with("fn push_str(mut ref self"),
        "{}",
        h.label
    );
    assert_eq!(h.active, Some(1));
    // A builtin type's function.
    let src2 = src.replace("Vec.new()", "Vec.with_capacity(4)");
    let h = help_after(&src2, "Vec.with_capacity(").expect("Vec.with_capacity");
    assert_eq!(h.label, "fn with_capacity(cap: i64) -> Vec[T]");
    assert_eq!(h.active, Some(0));
    // A variant and a prelude variant.
    let h = help_after(src, "Shape.Rect(1.0, ").expect("variant");
    assert_eq!(h.label, "Shape.Rect(f64, f64)");
    assert_eq!(h.active, Some(1));
    let h = help_after(src, "= Ok(").expect("Ok");
    assert_eq!(h.params[0].mode, ParamMode::Owned);
    // A prelude printer borrows what it prints.
    let h = help_after(src, "println(").expect("println");
    assert_eq!(h.params[0].mode, ParamMode::Borrowed);
}
