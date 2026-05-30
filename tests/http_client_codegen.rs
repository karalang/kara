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
}
