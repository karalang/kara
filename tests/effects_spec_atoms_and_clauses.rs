//! design.md §12 in the front-end effect checker: integer arithmetic is a
//! `panics` atom (B-2026-11-04-29), the library's allocations reach
//! `#[no_effect(allocates(Heap))]` (B-2026-11-04-28), and a written `with`
//! clause is verified on a private function, a method and a private trait's
//! default body (B-2026-11-04-27). Pinned end to end by corpus/core
//! err_no_effect_panics_arithmetic, no_effect_panics_wrapping_arithmetic,
//! err_no_effect_heap_allocations, no_effect_heap_free_loop and
//! err_effect_clause_checked_everywhere.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-spec-atoms-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn under(attr: &str, ret: &str, body: &str) -> String {
    format!(
        "#[no_effect({attr})]\nfn f(a: i64, b: i64) -> {ret} {{ {body} }}\n\
         fn main() {{ println(f(1, 2)); }}\n"
    )
}

#[test]
fn integer_operators_are_panics_atoms() {
    for (i, body) in [
        "a % b",
        "a / b",
        "a + b",
        "a - b",
        "a * b",
        "-a",
        "let mut x = a; x += b; x",
        "let mut x = a; x /= b; x",
        "a << 2",
    ]
    .iter()
    .enumerate()
    {
        let (ok, text) = check(&format!("op{i}"), &under("panics", "i64", body));
        assert!(
            !ok,
            "`{body}` must be refused under no_effect(panics): {text}"
        );
        assert!(
            text.contains("E_NO_EFFECT_VIOLATED") && text.contains("`panics`"),
            "`{body}`: {text}"
        );
    }
}

#[test]
fn non_trapping_integer_forms_are_not_panics_atoms() {
    for (i, (ret, body)) in [
        ("i64", "a.wrapping_add(b)"),
        ("i64", "a.saturating_mul(b)"),
        ("i64", "a & b"),
        ("i64", "let mut x = a; x |= b; x"),
        ("bool", "a < b"),
        ("f64", "let x = 1.5 * 2.0; x / 3.0"),
    ]
    .iter()
    .enumerate()
    {
        let (ok, text) = check(&format!("pure{i}"), &under("panics", ret, body));
        assert!(ok, "`{body}` must pass under no_effect(panics): {text}");
    }
}

#[test]
fn library_allocations_reach_no_effect_allocates_heap() {
    for (i, body) in [
        "let v = [1, 2].to_vec(); v.len()",
        "let s = \"a\".to_string(); s.len()",
        "let s = 5.to_string(); s.len()",
        "let s = f\"{a}\"; s.len()",
        "let v: Vec[i64] = [1, 2]; v.len()",
        "let m = [\"k\": 1]; m.len()",
        "let v = Vec.filled(3, 0); v.len()",
        "let s = \"ab\".to_string() + \"c\"; s.len()",
        "let v: Vec[i64] = (0..3).collect(); v.len()",
    ]
    .iter()
    .enumerate()
    {
        let (ok, text) = check(&format!("alloc{i}"), &under("allocates(Heap)", "i64", body));
        assert!(
            !ok,
            "`{body}` must be refused under no_effect(allocates(Heap)): {text}"
        );
        assert!(text.contains("allocates(Heap)"), "`{body}`: {text}");
    }
}

#[test]
fn non_allocating_bodies_pass_no_effect_allocates_heap() {
    for (i, body) in [
        "let w = [0; 4]; w.len() + a",
        "let s = f\"plain\"; s.len()",
        "let s = \"lit\"; s.len() + b",
    ]
    .iter()
    .enumerate()
    {
        let (ok, text) = check(&format!("free{i}"), &under("allocates(Heap)", "i64", body));
        assert!(
            ok,
            "`{body}` must pass under no_effect(allocates(Heap)): {text}"
        );
    }
}

#[test]
fn a_private_function_clause_is_verified() {
    let (ok, text) = check(
        "private",
        "fn f() with reads(Clock) { println(\"x\"); }\nfn main() { f(); }\n",
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains("function 'f' performs writes(Stdout)"),
        "{text}"
    );
}

#[test]
fn a_method_clause_is_verified_public_or_private() {
    for (i, vis) in ["", "pub "].iter().enumerate() {
        let src = format!(
            "effect resource Db;\npub fn put() with writes(Db) {{ }}\npub struct S {{}}\n\
             impl S {{ {vis}fn m(self) with reads(Db) {{ put(); }} }}\n\
             fn main() {{ let s = S {{}}; s.m(); }}\n"
        );
        let (ok, text) = check(&format!("method{i}"), &src);
        assert!(!ok, "{vis}method: {text}");
        assert!(text.contains("method 'S.m' performs writes(Db)"), "{text}");
    }
}

#[test]
fn a_private_trait_default_body_is_checked_against_its_clause() {
    let (ok, text) = check(
        "default",
        "effect resource Db;\nfn put() with writes(Db) { }\n\
         trait T { fn m(self) -> i64 with reads(Db) { put(); 1 } }\n\
         fn main() { println(1); }\n",
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains("default body of 'T.m' performs writes(Db)"),
        "{text}"
    );
}

#[test]
fn a_clause_wider_than_its_body_is_allowed() {
    // "A declaration may name more than the body does, so a library can
    // reserve an effect" (design.md §12).
    let (ok, text) = check(
        "wider",
        "effect resource Db;\nfn put() with writes(Db) { }\n\
         fn f() with reads(Db) writes(Db) { put(); }\n\
         struct S {}\nimpl S { fn m(self) with writes(Db) { } }\n\
         fn main() with reads(Db) writes(Db) { f(); let s = S {}; s.m(); }\n",
    );
    assert!(ok, "{text}");
}

#[test]
fn a_resolved_library_send_is_not_charged_as_a_channel_send() {
    // `RequestBuilder.send` resolves to the HTTP client, not to
    // `Sender.send`, so a clause naming only the network admits it; a real
    // channel send under the same clause is still refused.
    let (ok, text) = check(
        "builder_send",
        "fn main() with sends(Network) receives(Network) writes(Stdout) {\n\
         let c = Client.new();\n\
         match c.request(\"GET\", \"http://127.0.0.1:1/\").header(\"X-A\", \"b\").send() {\n\
         Ok(resp) => { println(resp.body()); }\n\
         Err(e) => { println(e.message()); }\n\
         }\n}\n",
    );
    assert!(ok, "{text}");
    let (ok, text) = check(
        "channel_send",
        "fn main() with writes(Stdout) {\n\
         let (tx, rx) = channel(4);\n\
         tx.send(1);\n\
         println(1);\n}\n",
    );
    assert!(!ok, "{text}");
    assert!(text.contains("sends(Channel)"), "{text}");
}
