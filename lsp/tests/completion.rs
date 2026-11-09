//! Completion and rename (v1 checklist): what the editor offers at a cursor,
//! and the edits a rename makes. Each source marks the cursor with `|`.

use std::thread;
use std::time::Duration;

use kara_lsp::analysis;
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::Position;
use serde_json::json;

/// `src` without its `|`, and the position the `|` marked.
fn at(src: &str) -> (String, Position) {
    let i = src.find('|').expect("a cursor");
    let text = format!("{}{}", &src[..i], &src[i + 1..]);
    let line = src[..i].matches('\n').count() as u32;
    let col = (i - src[..i].rfind('\n').map_or(0, |n| n + 1)) as u32;
    (text, Position::new(line, col))
}

fn labels(src: &str) -> Vec<String> {
    let (text, pos) = at(src);
    analysis::completion(&text, pos)
        .into_iter()
        .map(|c| c.label)
        .collect()
}

fn has_all(got: &[String], want: &[&str]) {
    for w in want {
        assert!(got.iter().any(|g| g == w), "missing `{w}` in {got:?}");
    }
}

fn has_none(got: &[String], unwanted: &[&str]) {
    for u in unwanted {
        assert!(!got.iter().any(|g| g == u), "unexpected `{u}` in {got:?}");
    }
}

#[test]
fn completion_after_dot_lists_a_strings_methods() {
    let got = labels("fn main() {\n    let s = \"a\";\n    s.|\n}\n");
    has_all(
        &got,
        &["len", "chars", "split", "starts_with", "to_uppercase"],
    );
}

#[test]
fn completion_after_dot_keeps_only_what_starts_with_the_typed_name() {
    let got = labels("fn main() {\n    let s = \"a\";\n    let n = s.le|;\n}\n");
    assert_eq!(got, vec!["len".to_string()]);
}

#[test]
fn completion_on_an_option_offers_its_methods_and_not_results() {
    let got = labels("fn main() {\n    let o: Option[i64] = Some(1);\n    o.|\n}\n");
    has_all(
        &got,
        &["is_some", "is_none", "map", "and_then", "unwrap_or", "take"],
    );
    has_none(&got, &["is_ok", "is_err", "map_err", "unwrap_err"]);
}

#[test]
fn completion_on_a_result_offers_its_methods() {
    let got = labels("fn main() {\n    let r: Result[i64, String] = Ok(1);\n    r.|\n}\n");
    has_all(&got, &["is_ok", "is_err", "map_err", "unwrap_or", "ok"]);
    has_none(&got, &["is_some", "take"]);
}

#[test]
fn every_method_offered_on_a_builtin_type_checks() {
    for (ty, value) in [
        ("Option[i64]", "Some(1)"),
        ("Result[i64, String]", "Ok(1)"),
        ("String", "\"a\""),
        ("Vec[i64]", "Vec.new()"),
        ("Map[String, i64]", "Map.new()"),
        ("Set[i64]", "Set.new()"),
        ("i64", "1"),
        ("u8", "1"),
        ("f64", "1.5"),
    ] {
        let src = format!("fn main() {{\n    let mut o: {ty} = {value};\n    o.|\n}}\n");
        let offered = labels(&src);
        assert!(offered.len() > 10, "{offered:?}");
        for name in offered {
            // Arity and argument errors are expected; an unknown name is not.
            let call = src.replace('|', &format!("{name}()"));
            let unknown: Vec<_> = karac::check_source(&call)
                .into_iter()
                .filter(|d| {
                    d.message.contains("no method") && !d.message.contains("taking these arguments")
                })
                .map(|d| d.message)
                .collect();
            assert!(unknown.is_empty(), "{ty}.{name}: {unknown:?}");
        }
    }
}

#[test]
fn completion_on_a_struct_lists_fields_and_self_methods_but_not_deprecated_or_static() {
    let got = labels(
        "struct P { x: i64, y: i64 }\n\
         impl P {\n\
             fn norm(self) -> i64 { self.x }\n\
             fn new() -> P { P { x: 1, y: 2 } }\n\
             #[deprecated]\n\
             fn old_norm(self) -> i64 { self.x }\n\
         }\n\
         fn main() {\n    let p = P { x: 1, y: 2 };\n    p.|\n}\n",
    );
    assert_eq!(got, vec!["norm", "x", "y"]);
}

#[test]
fn completion_on_collections_lists_their_methods() {
    has_all(
        &labels("fn main() {\n    let v: Vec[i64] = Vec.new();\n    v.|\n}\n"),
        &["push", "pop", "len", "iter", "sort"],
    );
    has_all(
        &labels("fn main() {\n    let m: Map[String, i64] = Map.new();\n    m.|\n}\n"),
        &["insert", "get", "contains_key", "keys", "values"],
    );
}

#[test]
fn completion_on_an_iterator_lists_the_adaptors_it_has_not_named_yet() {
    let got = labels("fn main() {\n    let v = [1, 2, 3];\n    let s = v.iter().|\n}\n");
    has_all(
        &got,
        &["map", "filter", "fold", "collect", "enumerate", "zip"],
    );
    // A program's own iterator gets the provided methods too.
    let got = labels(
        "struct C { n: i64 }\n\
         impl Iterator for C {\n    type Item = i64;\n    fn next(mut ref self) -> Option[i64] { None }\n}\n\
         fn main() {\n    let c = C { n: 0 };\n    c.fi|\n}\n",
    );
    has_all(&got, &["filter", "filter_map", "find"]);
}

#[test]
fn completion_on_a_number_lists_its_methods() {
    has_all(
        &labels("fn main() {\n    let n: i64 = 3;\n    let k = n.ch|\n}\n"),
        &["checked_add", "checked_mul"],
    );
    has_all(
        &labels("fn main() {\n    let f = 1.5;\n    f.|\n}\n"),
        &["sqrt", "floor"],
    );
}

#[test]
fn completion_lists_locals_and_params_in_scope_and_the_modules_items() {
    let got = labels(
        "fn helper(a: i64) -> i64 { a }\n\
         struct Point { x: i64 }\n\
         fn main() {\n    let alpha = 1;\n    if alpha > 0 {\n        let hidden = 2;\n    }\n    \
         for i in 0..3 {\n        let beta = i;\n        |\n    }\n    let gamma = 2;\n}\n",
    );
    has_all(&got, &["alpha", "beta", "i", "helper", "main", "Point"]);
    // Declared after the cursor, in another block, or another function's.
    has_none(&got, &["gamma", "hidden", "a"]);

    let got = labels("fn f(count: i64) -> i64 {\n    let total = count;\n    co|\n}\n");
    assert_eq!(got, vec!["count".to_string()]);
    // A name typed where an operand goes.
    let got = labels(
        "struct P { x: i64 }\nfn main() {\n    let p = P { x: 1 };\n    let q = p.x + p|\n}\n",
    );
    assert_eq!(got, vec!["p".to_string()]);
}

#[test]
fn completion_after_std_lists_modules_then_their_items() {
    has_all(
        &labels("fn main() {\n    let x = std.|\n}\n"),
        &["io", "json", "time", "net"],
    );
    has_all(
        &labels("fn main() {\n    let x = std.io.|\n}\n"),
        &["File", "BufReader"],
    );
}

fn rename(src: &str, new_name: &str) -> Option<String> {
    let (text, pos) = at(src);
    let edits = analysis::rename(&text, pos, new_name)?;
    let index = analysis::LineIndex::new(&text);
    let mut spans: Vec<(usize, usize)> = edits
        .iter()
        .map(|e| (index.offset(e.range.start), index.offset(e.range.end)))
        .collect();
    spans.sort();
    let mut out = text.clone();
    for (s, e) in spans.into_iter().rev() {
        out.replace_range(s..e, new_name);
    }
    Some(out)
}

#[test]
fn rename_changes_a_local_and_every_use_and_nothing_else() {
    let got = rename(
        "fn main() {\n    let count = 1;\n    let x = co|unt + 1;\n    let count2 = count * x;\n}\n",
        "total",
    );
    assert_eq!(
        got.as_deref(),
        Some("fn main() {\n    let total = 1;\n    let x = total + 1;\n    let count2 = total * x;\n}\n")
    );
}

#[test]
fn rename_changes_a_function_at_its_declaration_and_calls() {
    let got = rename(
        "fn helper(a: i64) -> i64 { a }\nfn main() {\n    let x = help|er(1) + helper(2);\n}\n",
        "assist",
    );
    assert_eq!(
        got.as_deref(),
        Some(
            "fn assist(a: i64) -> i64 { a }\nfn main() {\n    let x = assist(1) + assist(2);\n}\n"
        )
    );
}

#[test]
fn rename_refuses_a_non_name_or_a_bad_new_name() {
    assert_eq!(rename("fn main() {\n    let x = 1 |+ 2;\n}\n", "y"), None);
    assert_eq!(
        rename("fn main() {\n    let x = 1;\n    let y = |x;\n}\n", "1x"),
        None
    );
}

fn req(client: &Connection, id: i32, method: &str, params: serde_json::Value) {
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(id),
            method: method.to_string(),
            params,
        }))
        .unwrap();
}

fn response(client: &Connection, id: i32) -> serde_json::Value {
    loop {
        match client
            .receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("timed out waiting for a server message")
        {
            Message::Response(Response {
                id: rid,
                result: Some(r),
                ..
            }) if rid == RequestId::from(id) => return r,
            Message::Response(r) if r.id == RequestId::from(id) => panic!("error: {r:?}"),
            _ => continue,
        }
    }
}

#[test]
fn server_offers_completion_on_dot_and_rename() {
    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(move || kara_lsp::serve(server));
    req(
        &client,
        1,
        "initialize",
        json!({"capabilities":{}, "processId":null}),
    );
    let caps = response(&client, 1);
    assert_eq!(
        caps["capabilities"]["completionProvider"]["triggerCharacters"],
        json!(["."])
    );
    assert_eq!(caps["capabilities"]["renameProvider"], json!(true));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: json!({}),
        }))
        .unwrap();

    let uri = "file:///c.kara";
    let text = "fn main() {\n    let s = \"a\";\n    s.\n}\n";
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: json!({"textDocument":{"uri":uri,"languageId":"kara","version":1,"text":text}}),
        }))
        .unwrap();
    req(
        &client,
        2,
        "textDocument/completion",
        json!({"textDocument":{"uri":uri},"position":{"line":2,"character":6}}),
    );
    let items = response(&client, 2);
    let len = items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == json!("len"))
        .unwrap_or_else(|| panic!("no `len` in {items}"));
    assert_eq!(len["kind"], json!(2)); // Method

    // Rename needs a buffer that parses.
    let uri = "file:///r.kara";
    let text = "fn main() {\n    let s = \"a\";\n    let n = s.len();\n}\n";
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: json!({"textDocument":{"uri":uri,"languageId":"kara","version":1,"text":text}}),
        }))
        .unwrap();
    req(
        &client,
        3,
        "textDocument/rename",
        json!({"textDocument":{"uri":uri},"position":{"line":1,"character":8},"newName":"t"}),
    );
    let edit = response(&client, 3);
    assert_eq!(
        edit["changes"][uri].as_array().map(Vec::len),
        Some(2),
        "{edit}"
    );

    req(&client, 4, "shutdown", json!(null));
    response_null(&client, 4);
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: json!(null),
        }))
        .unwrap();
    drop(client);
    server_thread.join().unwrap().unwrap();
}

fn response_null(client: &Connection, id: i32) {
    loop {
        if let Message::Response(r) = client
            .receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("timed out")
        {
            if r.id == RequestId::from(id) {
                return;
            }
        }
    }
}
