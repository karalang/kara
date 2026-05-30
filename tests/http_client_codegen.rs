//! Phase-8 line 17 slice 2 — std.http `Client.get` / `Client.post`
//! codegen IR-grep tests.
//!
//! Mirrors `tests/tls_codegen.rs`: codegen-side wire-up tests (kara
//! source → IR contains the right FFI calls / packing shape) for the
//! lowerings in `compile_client_http_method`. The wire-format
//! correctness of `karac_runtime_http_client_get` / `_post` is pinned
//! by the runtime crate's unit tests in `runtime/src/lib.rs`'s `tests`
//! module; this file pins the codegen routing.

#![cfg(feature = "llvm")]

mod http_client_codegen_tests {
    use karac::codegen::compile_to_ir;

    fn ir_for(src: &str) -> String {
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        assert!(resolved.errors.is_empty(), "resolve: {:?}", resolved.errors);
        let typed = karac::typecheck(&parsed.program, &resolved);
        assert!(typed.errors.is_empty(), "typecheck: {:?}", typed.errors);
        karac::lower(&mut parsed.program, &typed);
        compile_to_ir(&parsed.program, None, None).expect("codegen failed")
    }

    fn function_body(ir: &str, name: &str) -> Option<String> {
        let needle = format!("@{name}(");
        let mut found_define = false;
        let mut depth = 0i32;
        let mut body = String::new();
        for line in ir.lines() {
            if !found_define {
                if line.starts_with("define ") && line.contains(&needle) {
                    found_define = true;
                    depth = line.matches('{').count() as i32 - line.matches('}').count() as i32;
                    continue;
                }
            } else {
                body.push_str(line);
                body.push('\n');
                depth += line.matches('{').count() as i32;
                depth -= line.matches('}').count() as i32;
                if depth <= 0 {
                    return Some(body);
                }
            }
        }
        None
    }

    /// Both `karac_runtime_http_client_get` and `_post` are declared
    /// unconditionally in `Codegen::new`, so even a program that doesn't
    /// call them surfaces the declarations in the module. Pins the
    /// declaration so the `module.get_function(...).expect(...)` calls
    /// inside `compile_client_http_method` cannot regress to a panic.
    #[test]
    fn test_ir_http_client_runtime_ffis_declared() {
        let ir = ir_for("fn main() {}");
        for name in [
            "karac_runtime_http_client_get",
            "karac_runtime_http_client_post",
        ] {
            assert!(
                ir.contains("declare ") && ir.contains(name),
                "expected declaration of `{name}` in IR"
            );
        }
    }

    /// `Client.new().get(url)` lowers through `compile_client_http_method`
    /// to a single `karac_runtime_http_client_get(url_ptr, url_len, *out
    /// status, *out body_ptr, *out body_len, *out err_ptr, *out err_len)`
    /// call inside `main`. Pins the FFI dispatch + URL-string lowering.
    #[test]
    fn test_ir_client_get_dispatches_to_runtime_ffi() {
        let ir = ir_for(
            r#"
fn main() with sends(Network) receives(Network) {
    let c = Client.new();
    let url = "http://127.0.0.1:65535/";
    let _r = c.get(url);
}
"#,
        );
        let main_body = function_body(&ir, "main").expect("main body");
        assert!(
            main_body.contains("call void @karac_runtime_http_client_get("),
            "main should call _http_client_get; body was:\n{}",
            main_body
        );
        // The Ok / Err packing branches both get named labels — pin
        // them so a future refactor that drops the conditional-branch
        // surfaces here.
        assert!(
            main_body.contains("client.ok") && main_body.contains("client.err"),
            "main should branch into client.ok / client.err arms; body was:\n{}",
            main_body
        );
    }

    /// `Client.new().post(url, body)` lowers to
    /// `karac_runtime_http_client_post(url_ptr, url_len, body_ptr,
    /// body_len, ...)`. Pins the body-string lowering (the additional
    /// two args before the out-params).
    #[test]
    fn test_ir_client_post_passes_body_and_dispatches_to_runtime_ffi() {
        let ir = ir_for(
            r#"
fn main() with sends(Network) receives(Network) {
    let c = Client.new();
    let url = "http://127.0.0.1:65535/items";
    let body = "name=widget";
    let _r = c.post(url, body);
}
"#,
        );
        let main_body = function_body(&ir, "main").expect("main body");
        assert!(
            main_body.contains("call void @karac_runtime_http_client_post("),
            "main should call _http_client_post; body was:\n{}",
            main_body
        );
    }

    /// Phase-8 line 24 — all five chained-builder runtime externs are
    /// declared unconditionally in `Codegen::new`. Same rationale as
    /// the eager-form siblings above: pin the declarations so the
    /// `module.get_function(...).expect(...)` calls inside
    /// `compile_client_request_builder` / `compile_request_builder_*`
    /// can never regress to a panic.
    #[test]
    fn test_ir_http_builder_runtime_ffis_declared() {
        let ir = ir_for("fn main() {}");
        for name in [
            "karac_runtime_http_builder_new",
            "karac_runtime_http_builder_add_header",
            "karac_runtime_http_builder_set_body",
            "karac_runtime_http_builder_set_timeout",
            "karac_runtime_http_builder_send",
        ] {
            assert!(
                ir.contains("declare ") && ir.contains(name),
                "expected declaration of `{name}` in IR"
            );
        }
    }

    /// `c.request("GET", url).header("X", "y").timeout(5000).send()`
    /// lowers to the full handle-based dispatch chain: one
    /// `_builder_new` call to mint the handle, one `_builder_add_header`
    /// per `.header(...)`, one `_builder_set_timeout` for the `.timeout`,
    /// and one `_builder_send` to drive the request and pack the
    /// `Result[Response, HttpError]`. Pins the dispatch + the Ok/Err
    /// branch-label convention from `compile_request_builder_send`.
    #[test]
    fn test_ir_request_builder_chain_dispatches_through_handle_ffi() {
        let ir = ir_for(
            r#"
fn main() with sends(Network) receives(Network) {
    let c = Client.new();
    let url = "http://127.0.0.1:65535/api";
    let _r = c.request("GET", url).header("X-Custom", "abc").timeout(5000).send();
}
"#,
        );
        let main_body = function_body(&ir, "main").expect("main body");
        assert!(
            main_body.contains("call i64 @karac_runtime_http_builder_new("),
            "expected _builder_new call in main; body was:\n{}",
            main_body
        );
        assert!(
            main_body.contains("call void @karac_runtime_http_builder_add_header("),
            "expected _builder_add_header call in main; body was:\n{}",
            main_body
        );
        assert!(
            main_body.contains("call void @karac_runtime_http_builder_set_timeout("),
            "expected _builder_set_timeout call in main; body was:\n{}",
            main_body
        );
        assert!(
            main_body.contains("call void @karac_runtime_http_builder_send("),
            "expected _builder_send call in main; body was:\n{}",
            main_body
        );
        assert!(
            main_body.contains("rb.send.ok") && main_body.contains("rb.send.err"),
            "expected rb.send.ok / rb.send.err arms; body was:\n{}",
            main_body
        );
    }

    /// `c.request("POST", url).body("payload").send()` lowers the
    /// chained `.body(...)` call to `_builder_set_body`. Pins the
    /// body-setter routing distinctly from the headers/timeout paths
    /// (different runtime extern, different lowering arm in
    /// `compile_request_builder_setter`).
    #[test]
    fn test_ir_request_builder_body_dispatches_to_set_body_ffi() {
        let ir = ir_for(
            r#"
fn main() with sends(Network) receives(Network) {
    let c = Client.new();
    let url = "http://127.0.0.1:65535/api";
    let body = "hello-world";
    let _r = c.request("POST", url).body(body).send();
}
"#,
        );
        let main_body = function_body(&ir, "main").expect("main body");
        assert!(
            main_body.contains("call void @karac_runtime_http_builder_set_body("),
            "expected _builder_set_body call in main; body was:\n{}",
            main_body
        );
    }

    /// Phase-8 line 32 — `Response.text()` / `.bytes()` both lower through
    /// `compile_response_accessor`'s `karac_string_clone`-backed deep
    /// clone of the entity buffer (String for `text`, `Vec[u8]` for
    /// `bytes` — layout-identical). This pins that the dispatch arm in
    /// `compile_method_call` recognises both methods (a regression would
    /// surface as `codegen failed` from `ir_for`) and that each emits a
    /// clone rather than aliasing the receiver's field.
    #[test]
    fn test_ir_response_text_and_bytes_clone_entity_buffer() {
        let ir = ir_for(
            r#"
fn main() with sends(Network) receives(Network) {
    let c = Client.new();
    let url = "http://127.0.0.1:65535/";
    match c.get(url) {
        Ok(resp) => {
            let t: String = resp.text();
            let b: Vec[u8] = resp.bytes();
            println(t);
            println(b.len());
        }
        Err(e) => {
            println(e.message());
        }
    }
}
"#,
        );
        // Two distinct accessor sites (text + bytes) → at least two
        // `karac_string_clone` calls inside the Ok arm of `main`.
        let clone_calls = ir.matches("call void @karac_string_clone(").count();
        assert!(
            clone_calls >= 2,
            "expected >= 2 karac_string_clone calls (text + bytes); saw {clone_calls}\n{ir}"
        );
    }

    /// Phase-8 line 39 — `Response.header(name)` lowers through
    /// `compile_response_header`: it GEPs the hidden `headers: i64`
    /// handle off the destructured Response and calls
    /// `karac_runtime_http_response_header`. Pins that the extern is
    /// declared unconditionally and that the dispatch arm in
    /// `compile_method_call` recognises the `header`-with-one-arg shape
    /// on a Response receiver (a regression — e.g. the arm not matching,
    /// or the seeded Response losing its third field — would surface as
    /// `codegen failed` from `ir_for`, or a missing call here).
    #[test]
    fn test_ir_response_header_dispatches_to_runtime_lookup() {
        let ir = ir_for(
            r#"
fn main() with sends(Network) receives(Network) {
    let c = Client.new();
    let url = "http://127.0.0.1:65535/";
    match c.get(url) {
        Ok(resp) => {
            match resp.header("x-custom") {
                Some(v) => println(v),
                None => println("none"),
            }
        }
        Err(e) => {
            println(e.message());
        }
    }
}
"#,
        );
        assert!(
            ir.contains("declare ") && ir.contains("karac_runtime_http_response_header"),
            "expected declaration of karac_runtime_http_response_header in IR"
        );
        assert!(
            ir.contains("call ptr @karac_runtime_http_response_header("),
            "expected a call to karac_runtime_http_response_header; IR was:\n{ir}"
        );
    }

    /// Phase-8 line 39 follow-up — `Response.headers()` lowers through
    /// `compile_response_pairs`: it reads the hidden headers handle and
    /// drives the runtime `_response_headers_count` (loop bound) +
    /// `_response_header_key_at` / `_val_at` iteration accessors. Pins the
    /// dispatch arm + the iteration calls (a regression — the arm not
    /// matching `headers`, or the seeded Response losing its handle field
    /// — surfaces as `codegen failed` from `ir_for`, or a missing call).
    #[test]
    fn test_ir_response_headers_iterates_via_runtime_accessors() {
        let ir = ir_for(
            r#"
fn main() with sends(Network) receives(Network) {
    let c = Client.new();
    let url = "http://127.0.0.1:65535/";
    match c.get(url) {
        Ok(resp) => {
            let hs: Vec[(String, String)] = resp.headers();
            println(hs.len());
        }
        Err(e) => {
            println(e.message());
        }
    }
}
"#,
        );
        assert!(
            ir.contains("call i64 @karac_runtime_http_response_headers_count("),
            "expected a call to karac_runtime_http_response_headers_count; IR was:\n{ir}"
        );
        assert!(
            ir.contains("@karac_runtime_http_response_header_key_at(")
                && ir.contains("@karac_runtime_http_response_header_val_at("),
            "expected key_at + val_at iteration calls; IR was:\n{ir}"
        );
    }

    /// Phase-8 line 39 follow-up — a pattern-bound `Response` (from
    /// `Ok(resp)`) now registers a synthesized scope-exit Drop that frees
    /// BOTH its `body` String buffer (libc `free`) AND its `headers`
    /// side-table handle (`karac_runtime_http_response_headers_free`),
    /// fixing the two latent leaks. Pins (a) the drop fn is synthesized
    /// and calls the headers-free, and (b) `main` invokes the drop fn at
    /// the `Ok(resp)` arm's scope exit (the `StructDrop` cleanup action).
    #[test]
    fn test_ir_response_drop_frees_headers_handle() {
        let ir = ir_for(
            r#"
fn main() with sends(Network) receives(Network) {
    let c = Client.new();
    let url = "http://127.0.0.1:65535/";
    match c.get(url) {
        Ok(resp) => {
            println(resp.status());
        }
        Err(e) => {
            println(e.message());
        }
    }
}
"#,
        );
        let drop_body =
            function_body(&ir, "__karac_drop_struct_Response").expect("Response drop fn defined");
        assert!(
            drop_body.contains("call void @karac_runtime_http_response_headers_free("),
            "Response drop fn must free the headers side-table handle; body was:\n{drop_body}"
        );
        // The body String is also freed (the latent body leak). The
        // synthesized drop guards `cap > 0` then calls libc `free`.
        assert!(
            drop_body.contains("@free("),
            "Response drop fn must free the body String buffer; body was:\n{drop_body}"
        );
        // The Ok(resp) arm invokes the drop fn at scope exit.
        let main_body = function_body(&ir, "main").expect("main body");
        assert!(
            main_body.contains("call void @__karac_drop_struct_Response("),
            "main should invoke the Response drop fn at the Ok-arm scope exit; body was:\n{main_body}"
        );
    }

    /// Phase-8 line 39 follow-up — `HttpError` (from `Err(e)`) now gets a
    /// synthesized scope-exit Drop that frees its runtime-malloc'd
    /// `message` String, fixing the latent leak (the seeded `HttpError`
    /// previously had no Drop, same as `Response.body` before its fix).
    /// Pins (a) the drop fn is synthesized and frees the message buffer,
    /// and (b) `main` invokes it at the `Err(e)` arm's scope exit.
    #[test]
    fn test_ir_http_error_drop_frees_message() {
        let ir = ir_for(
            r#"
fn main() with sends(Network) receives(Network) {
    let c = Client.new();
    let url = "http://127.0.0.1:65535/";
    match c.get(url) {
        Ok(resp) => {
            println(resp.status());
        }
        Err(e) => {
            println(e.message());
        }
    }
}
"#,
        );
        let drop_body =
            function_body(&ir, "__karac_drop_struct_HttpError").expect("HttpError drop fn defined");
        assert!(
            drop_body.contains("@free("),
            "HttpError drop fn must free the message String buffer; body was:\n{drop_body}"
        );
        let main_body = function_body(&ir, "main").expect("main body");
        assert!(
            main_body.contains("call void @__karac_drop_struct_HttpError("),
            "main should invoke the HttpError drop fn at the Err-arm scope exit; body was:\n{main_body}"
        );
    }

    /// Phase-8 line 39 follow-up — a chained `RequestBuilder` configured
    /// but never `.send()`-ed and never bound (`c.request(url).header(...);`
    /// as a discarded statement) is a temporary; Kāra has no general
    /// temporary-drop, so its `HTTP_BUILDERS` entry would leak. The
    /// `StmtKind::Expr` arm frees the abandoned handle via
    /// `karac_runtime_http_builder_free`. A `.send()`-ed (or let-bound)
    /// chain is consumed / drop-tracked, so it must NOT get the extra free.
    #[test]
    fn test_ir_abandoned_request_builder_frees_handle() {
        let ir = ir_for(
            r#"
fn main() with sends(Network) receives(Network) {
    let c = Client.new();
    let url = "http://127.0.0.1:65535/api";
    c.request("GET", url).header("X-Custom", "abc");
}
"#,
        );
        let main_body = function_body(&ir, "main").expect("main body");
        assert!(
            main_body.contains("call void @karac_runtime_http_builder_free("),
            "an abandoned chained builder must free its handle; body was:\n{main_body}"
        );

        // A sent chain is consumed by `_builder_send` (which removes the
        // entry); the `let _r = …send()` binds a `Result`, not a builder,
        // so no `_builder_free` should be emitted.
        let ir_sent = ir_for(
            r#"
fn main() with sends(Network) receives(Network) {
    let c = Client.new();
    let url = "http://127.0.0.1:65535/api";
    let _r = c.request("GET", url).header("X-Custom", "abc").send();
}
"#,
        );
        let sent_body = function_body(&ir_sent, "main").expect("main body");
        assert!(
            !sent_body.contains("call void @karac_runtime_http_builder_free("),
            "a sent builder is consumed by _builder_send; no _builder_free expected; \
             body was:\n{sent_body}"
        );
    }
}
