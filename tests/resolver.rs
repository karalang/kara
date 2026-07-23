// tests/resolver.rs

use karac::ast::TraitBound;
use karac::resolver::*;
use karac::{desugar_program, parse, resolve};

/// Resolve `source` with `with_test_file(true)` so the signature-from-
/// call-site stub diagnostic fires (phase-5-diagnostics line 633).
fn resolve_as_test_file(source: &str) -> ResolveResult {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Resolver::new(&parsed.program)
        .with_test_file(true)
        .resolve()
}

// ── Test Helpers ────────────────────────────────────────────────

fn resolve_ok(source: &str) -> ResolveResult {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let result = resolve(&parsed.program);
    assert!(
        result.errors.is_empty(),
        "Resolve errors: {}",
        result
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    result
}

fn resolve_errors(source: &str) -> Vec<ResolveError> {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let result = resolve(&parsed.program);
    assert!(
        !result.errors.is_empty(),
        "Expected resolve errors but got none"
    );
    result.errors
}

// ── Scope & Symbol Table ────────────────────────────────────────

#[test]
fn test_empty_program() {
    resolve_ok("");
}

#[test]
fn test_builtin_types_available() {
    // Using primitive types in function signatures should resolve
    resolve_ok("fn foo(x: i64) -> bool { true }");
}

#[test]
fn test_function_registered() {
    let result = resolve_ok("fn hello() { }");
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), "hello");
    assert!(sym.is_some());
    assert!(matches!(sym.unwrap().kind, SymbolKind::Function { .. }));
}

#[test]
fn test_struct_registered() {
    let result = resolve_ok("struct Point { x: i64, y: i64 }");
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), "Point");
    assert!(sym.is_some());
    if let SymbolKind::Struct { ref field_names } = sym.unwrap().kind {
        assert_eq!(field_names, &["x", "y"]);
    } else {
        panic!("expected Struct symbol");
    }
}

#[test]
fn test_enum_and_variants() {
    let result = resolve_ok("enum Color { Red, Green, Blue }");
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), "Color");
    assert!(sym.is_some());
    // Variants should also be registered in global scope
    assert!(result
        .symbol_table
        .lookup_in_scope(ScopeId(0), "Red")
        .is_some());
    assert!(result
        .symbol_table
        .lookup_in_scope(ScopeId(0), "Green")
        .is_some());
    assert!(result
        .symbol_table
        .lookup_in_scope(ScopeId(0), "Blue")
        .is_some());
}

#[test]
fn test_trait_registered() {
    let result = resolve_ok("trait Printable { fn print(self); }");
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), "Printable");
    assert!(sym.is_some());
    if let SymbolKind::Trait { ref method_names } = sym.unwrap().kind {
        assert_eq!(method_names, &["print"]);
    } else {
        panic!("expected Trait symbol");
    }
}

#[test]
fn test_const_registered() {
    let result = resolve_ok("const MAX: i64 = 100;");
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), "MAX");
    assert!(sym.is_some());
    assert!(matches!(sym.unwrap().kind, SymbolKind::Constant));
}

#[test]
fn test_effect_resource_registered() {
    let result = resolve_ok("effect resource UserDB;");
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), "UserDB");
    assert!(sym.is_some());
    assert!(matches!(sym.unwrap().kind, SymbolKind::EffectResource));
}

#[test]
fn test_effect_group_registered() {
    let result = resolve_ok(
        "effect resource UserDB;\n\
         effect group read_all = reads(UserDB);",
    );
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), "read_all");
    assert!(sym.is_some());
    assert!(matches!(sym.unwrap().kind, SymbolKind::EffectGroup));
}

#[test]
fn test_type_alias_registered() {
    let result = resolve_ok("type UserId = u64;");
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), "UserId");
    assert!(sym.is_some());
    assert!(matches!(sym.unwrap().kind, SymbolKind::TypeAlias));
}

#[test]
fn test_use_decl_imports_name() {
    let result = resolve_ok("use std.collections.HashMap;");
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), "HashMap");
    assert!(sym.is_some());
    if let SymbolKind::Import { ref path } = sym.unwrap().kind {
        assert_eq!(path, &["std", "collections", "HashMap"]);
    } else {
        panic!("expected Import symbol");
    }
}

#[test]
fn test_pub_use_decl_visibility() {
    let result = resolve_ok("pub use db.connection.Connection;");
    let sym = result
        .symbol_table
        .lookup_in_scope(ScopeId(0), "Connection");
    assert!(sym.is_some());
    let sym = sym.unwrap();
    assert!(sym.is_pub);
    if let SymbolKind::Import { ref path } = sym.kind {
        assert_eq!(path, &["db", "connection", "Connection"]);
    } else {
        panic!("expected Import symbol");
    }
}

#[test]
fn test_bogus_std_import_rejected_single_file() {
    // B-2026-07-18-25: a std-rooted `import` naming no baked stdlib module
    // (`import std.math`) was SILENTLY accepted in single-file mode — it bound
    // a dead `math` alias that then ICE'd both backends on use ("variable
    // 'math' not found ... should be caught by resolver"). `std` is the
    // compiler's own namespace, fully known at compile time, so it is validated
    // even without a module tree. (`use` is the trusted cross-module-reference
    // mechanism and is deliberately NOT validated — see the tests below.)
    for src in [
        "import std.math;\nfn main() { }",
        "import std.completelybogus;\nfn main() { }",
        "import std.foo.{Bar};\nfn main() { }",
    ] {
        let errors = resolve_errors(src);
        assert!(
            errors
                .iter()
                .any(|e| e.kind == ResolveErrorKind::UnknownModule
                    && e.message.contains("unknown module")),
            "expected UnknownModule for `{src}`, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }
}

#[test]
fn test_valid_std_and_user_imports_still_accepted() {
    // The fix must NOT flag real gated stdlib modules, the prelude, a trusted
    // user cross-file `import` (single-file mode can't see its module tree), or
    // a `use` of any path (the trusted cross-module-reference mechanism).
    resolve_ok("import std.autograd.{Tape};\nfn main() { }");
    resolve_ok("import std.web.{Server};\nfn main() { }");
    resolve_ok("import std.prelude.{Vec};\nfn main() { }");
    resolve_ok("import mymod.{Foo};\nfn main() { }");
    resolve_ok("use std.collections.HashMap;\nfn main() { }");
}

#[test]
fn test_duplicate_top_level_error() {
    let errors = resolve_errors("fn foo() { }\nfn foo() { }");
    assert!(errors[0].kind == ResolveErrorKind::DuplicateDefinition);
    assert!(errors[0].message.contains("foo"));
}

#[test]
fn test_forward_reference() {
    // bar calls foo which is defined after bar
    resolve_ok(
        "fn bar() { foo(); }\n\
         fn foo() { }",
    );
}

// ── Variable Resolution ─────────────────────────────────────────

#[test]
fn test_let_binding_resolves() {
    resolve_ok("fn main() { let x = 5; x; }");
}

#[test]
fn test_undefined_variable_error() {
    let errors = resolve_errors("fn main() { y; }");
    assert!(errors[0].kind == ResolveErrorKind::UndefinedName);
    assert!(errors[0].message.contains("y"));
}

#[test]
fn test_parameter_in_body() {
    resolve_ok("fn add(a: i64, b: i64) -> i64 { a + b }");
}

#[test]
fn test_undefined_name_in_ensures_is_a_resolve_error() {
    // B-2026-07-23-17: the resolver previously skipped `requires`/`ensures`
    // contract expressions entirely, so an undefined name in one (including
    // the common typo of `ensures result …` WITHOUT the `(result)` binding)
    // passed `karac check` and ICE'd at runtime. It must now resolve like any
    // other expression and report the undefined name.
    let errors = resolve_errors("fn f(a: i64) -> i64 ensures result == a { a }");
    assert!(errors
        .iter()
        .any(|e| e.kind == ResolveErrorKind::UndefinedName && e.message.contains("result")));
}

#[test]
fn test_undefined_name_in_requires_is_a_resolve_error() {
    let errors = resolve_errors("fn f(a: i64) -> i64 requires bogus > 0 { a }");
    assert!(errors
        .iter()
        .any(|e| e.kind == ResolveErrorKind::UndefinedName && e.message.contains("bogus")));
}

#[test]
fn test_ensures_result_binding_and_old_resolve() {
    // The `(result)` binding names the return value; `old(expr)` names the
    // entry snapshot. Both must resolve in the postcondition scope.
    resolve_ok("fn f(a: i64) -> i64 ensures(result) result == old(a) { a }");
}

#[test]
fn test_requires_sees_parameters() {
    resolve_ok("fn f(a: i64, b: i64) -> i64 requires a > 0 and b != 0 { a / b }");
}

#[test]
fn test_shadowing_in_block() {
    resolve_ok(
        "fn main() {\n\
             let x = 1;\n\
             let y = {\n\
                 let x = 2;\n\
                 x\n\
             };\n\
             x;\n\
         }",
    );
}

#[test]
fn test_same_scope_let_shadowing_allowed() {
    // design.md § Variables > Shadowing: a later `let` of an existing name in
    // the *same* scope creates a fresh binding rather than erroring. The
    // shadowing initializer may read the previous binding.
    resolve_ok("fn main() { let x = 5; let x = x + 1; x; }");
}

#[test]
fn test_same_scope_let_mut_shadowing_allowed() {
    // `let mut` shadowing is permitted regardless of the prior binding's
    // mutability, and shadowing a `let mut` with a plain `let` is fine too.
    resolve_ok("fn main() { let mut x = 5; let x = x; let mut x = 7; x; }");
}

#[test]
fn test_let_shadows_parameter() {
    // A `let` in the body may shadow a function parameter of the same name.
    resolve_ok("fn f(x: i64) -> i64 { let x = x + 1; x }");
}

#[test]
fn test_let_shadowing_changes_type() {
    // The canonical shadowing example from design.md: rebind with a new type.
    resolve_ok(
        "fn parse_int(s: bool) -> i64 { 0 }\n\
         fn main() { let x = true; let x = parse_int(x); x; }",
    );
}

#[test]
fn test_duplicate_parameter_still_rejected() {
    // Shadowing applies to `let`, not to parameter declarations: two params
    // of the same name remain a duplicate-definition error.
    let errors = resolve_errors("fn f(x: i64, x: i64) -> i64 { x }");
    assert!(
        errors
            .iter()
            .any(|e| e.kind == ResolveErrorKind::DuplicateDefinition),
        "expected duplicate-parameter diagnostic, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_duplicate_binder_in_same_pattern_rejected() {
    // Shadowing is a *top-level* re-`let`; binding the same name twice inside
    // a single destructuring pattern is still an error.
    let errors = resolve_errors("fn main() { let (a, a) = (1, 2); a; }");
    assert!(
        errors
            .iter()
            .any(|e| e.kind == ResolveErrorKind::DuplicateDefinition),
        "expected duplicate-binder-in-pattern diagnostic, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_let_uninit_shadowing_allowed() {
    // The `let x: T;` (uninitialized) form participates in shadowing too.
    resolve_ok("fn main() { let x = 1; let x: i64; x = 2; x; }");
}

#[test]
fn test_variable_not_visible_after_block() {
    let errors = resolve_errors(
        "fn main() {\n\
             {\n\
                 let inner = 1;\n\
             }\n\
             inner;\n\
         }",
    );
    assert!(errors[0].message.contains("inner"));
}

#[test]
fn test_for_loop_binding() {
    // 'list' needs to be defined for the for loop to resolve
    resolve_ok(
        "fn process(list: i64) {\n\
             for item in list {\n\
                 item;\n\
             }\n\
         }",
    );
}

#[test]
fn test_closure_params() {
    resolve_ok(
        "fn main() {\n\
             let f = |x: i64, y: i64| x + y;\n\
         }",
    );
}

#[test]
fn test_closure_captures_outer() {
    resolve_ok(
        "fn main() {\n\
             let a = 10;\n\
             let f = |x: i64| x + a;\n\
         }",
    );
}

// ── Expression Resolution ───────────────────────────────────────

#[test]
fn test_function_call() {
    resolve_ok(
        "fn greet() { }\n\
         fn main() { greet(); }",
    );
}

#[test]
fn test_undefined_function_call() {
    let errors = resolve_errors("fn main() { unknown(); }");
    assert!(errors[0].kind == ResolveErrorKind::UndefinedName);
    assert!(errors[0].message.contains("unknown"));
}

#[test]
fn test_struct_literal() {
    resolve_ok(
        "struct Point { x: i64, y: i64 }\n\
         fn main() { let p = Point { x: 1, y: 2 }; }",
    );
}

#[test]
fn test_if_else_resolution() {
    resolve_ok(
        "fn max(a: i64, b: i64) -> i64 {\n\
             if a > b { a } else { b }\n\
         }",
    );
}

#[test]
fn test_match_arm_bindings() {
    resolve_ok(
        "enum Option { Some(i64), None }\n\
         fn main() {\n\
             let x = Some(5);\n\
             match x {\n\
                 Some(v) => v,\n\
                 None => 0,\n\
             };\n\
         }",
    );
}

#[test]
fn test_match_arm_scope_isolation() {
    // 'v' defined in one match arm should not be visible in another
    let errors = resolve_errors(
        "enum Option { Some(i64), None }\n\
         fn main() {\n\
             let x = Some(5);\n\
             match x {\n\
                 Some(v) => v,\n\
                 None => v,\n\
             };\n\
         }",
    );
    assert!(errors.iter().any(|e| e.message.contains("v")));
}

#[test]
fn test_nested_expressions() {
    resolve_ok(
        "fn main() {\n\
             let a = 1;\n\
             let b = 2;\n\
             let c = (a + b) * a - b;\n\
         }",
    );
}

#[test]
fn test_while_loop() {
    resolve_ok(
        "fn main() {\n\
             let mut count = 0;\n\
             while count < 10 {\n\
                 count = count + 1;\n\
             }\n\
         }",
    );
}

#[test]
fn test_loop_and_break() {
    resolve_ok(
        "fn main() {\n\
             let mut x = 0;\n\
             loop {\n\
                 x = x + 1;\n\
                 break;\n\
             }\n\
         }",
    );
}

#[test]
fn test_return_expression() {
    resolve_ok(
        "fn early(x: i64) -> i64 {\n\
             if x > 0 {\n\
                 return x;\n\
             }\n\
             0\n\
         }",
    );
}

// ── Type Resolution ─────────────────────────────────────────────

#[test]
fn test_type_annotation() {
    resolve_ok("fn main() { let x: i64 = 42; }");
}

#[test]
fn test_undefined_type_error() {
    let errors = resolve_errors("fn main() { let x: Nonexistent = 42; }");
    assert!(errors[0].kind == ResolveErrorKind::UndefinedType);
    assert!(errors[0].message.contains("Nonexistent"));
}

#[test]
fn test_generic_type_param() {
    resolve_ok("fn identity[T](x: T) -> T { x }");
}

#[test]
fn test_struct_type_in_function() {
    resolve_ok(
        "struct User { name: String, age: i64 }\n\
         fn greet(user: User) { }",
    );
}

#[test]
fn test_function_return_type() {
    resolve_ok(
        "struct Point { x: i64, y: i64 }\n\
         fn origin() -> Point { Point { x: 0, y: 0 } }",
    );
}

// ── Pattern Resolution ──────────────────────────────────────────

#[test]
fn test_struct_pattern() {
    resolve_ok(
        "struct Point { x: i64, y: i64 }\n\
         fn main() {\n\
             let p = Point { x: 1, y: 2 };\n\
             let Point { x, y } = p;\n\
             x + y;\n\
         }",
    );
}

#[test]
fn test_tuple_pattern() {
    resolve_ok(
        "fn main() {\n\
             let t = (1, 2);\n\
             let (a, b) = t;\n\
             a + b;\n\
         }",
    );
}

#[test]
fn test_enum_variant_pattern() {
    resolve_ok(
        "enum Result { Ok(i64), Err(String) }\n\
         fn handle(r: Result) -> i64 {\n\
             match r {\n\
                 Ok(val) => val,\n\
                 Err(msg) => 0,\n\
             }\n\
         }",
    );
}

// ── Effect Resolution ───────────────────────────────────────────

#[test]
fn test_effect_resource_in_annotation() {
    resolve_ok(
        "effect resource UserDB;\n\
         fn save() writes(UserDB) { }",
    );
}

#[test]
fn test_undefined_effect_resource() {
    let errors = resolve_errors("fn save() writes(Unknown) { }");
    assert!(errors.iter().any(|e| e.message.contains("Unknown")));
}

#[test]
fn test_effect_group_in_with() {
    resolve_ok(
        "effect resource UserDB;\n\
         effect group read_all = reads(UserDB);\n\
         fn load() with read_all { }",
    );
}

#[test]
fn test_multiple_effects() {
    resolve_ok(
        "effect resource UserDB;\n\
         effect resource OrderDB;\n\
         fn process() with reads(UserDB) writes(OrderDB) { }",
    );
}

// ── Impl Blocks ─────────────────────────────────────────────────

#[test]
fn test_impl_self_available() {
    resolve_ok(
        "struct Counter { value: i64 }\n\
         impl Counter {\n\
             fn get(self) -> i64 { self.value }\n\
         }",
    );
}

#[test]
fn test_impl_self_type() {
    resolve_ok(
        "struct Counter { value: i64 }\n\
         impl Counter {\n\
             fn new() -> Self { Counter { value: 0 } }\n\
         }",
    );
}

#[test]
fn test_trait_impl_resolves() {
    resolve_ok(
        "trait Printable { fn print(self); }\n\
         struct Foo { x: i64 }\n\
         impl Printable for Foo {\n\
             fn print(self) { }\n\
         }",
    );
}

// ── Visibility ──────────────────────────────────────────────────

#[test]
fn test_pub_tracking() {
    let result = resolve_ok(
        "pub fn public_fn() { }\n\
         fn private_fn() { }",
    );
    let pub_sym = result
        .symbol_table
        .lookup_in_scope(ScopeId(0), "public_fn")
        .unwrap();
    assert!(pub_sym.is_pub);
    let priv_sym = result
        .symbol_table
        .lookup_in_scope(ScopeId(0), "private_fn")
        .unwrap();
    assert!(!priv_sym.is_pub);
}

// ── Typo Suggestions ────────────────────────────────────────────

#[test]
fn test_typo_suggestion() {
    let errors = resolve_errors(
        "fn main() {\n\
             let count = 0;\n\
             ccount;\n\
         }",
    );
    assert!(errors[0].suggestion.is_some());
    assert_eq!(errors[0].suggestion.as_ref().unwrap(), "count");
}

#[test]
fn test_no_suggestion_for_very_different() {
    let errors = resolve_errors("fn main() { xyz123; }");
    assert!(errors[0].suggestion.is_none());
}

// ── Complex Programs ────────────────────────────────────────────

#[test]
fn test_complex_program() {
    resolve_ok(
        "effect resource FileSystem;\n\
         effect resource UserDB;\n\
         \n\
         struct User {\n\
             name: String,\n\
             age: i64,\n\
         }\n\
         \n\
         enum Status {\n\
             Active,\n\
             Inactive,\n\
         }\n\
         \n\
         trait Validate {\n\
             fn is_valid(self) -> bool;\n\
         }\n\
         \n\
         impl User {\n\
             fn new(name: String, age: i64) -> User {\n\
                 User { name: name, age: age }\n\
             }\n\
         }\n\
         \n\
         impl Validate for User {\n\
             fn is_valid(self) -> bool { self.age > 0 }\n\
         }\n\
         \n\
         fn process(user: User) -> bool reads(UserDB) {\n\
             let status = Active;\n\
             match status {\n\
                 Active => user.age > 18,\n\
                 Inactive => false,\n\
             }\n\
         }\n\
         \n\
         fn main() {\n\
             let u = User { name: \"Alice\", age: 30 };\n\
             let valid = process(u);\n\
         }",
    );
}

#[test]
fn test_multiple_errors_reported() {
    let parsed = parse("fn main() { a; b; c; }");
    assert!(parsed.errors.is_empty());
    let result = resolve(&parsed.program);
    // Should report all three undefined names, not just the first
    assert!(result.errors.len() >= 3);
}

#[test]
fn test_extern_function() {
    resolve_ok(
        "effect resource FileSystem;\n\
         unsafe extern \"C\" { fn write(fd: i32, buf: i64, count: i64) -> i64 writes(FileSystem); }",
    );
}

#[test]
fn test_extern_block_opaque_type_collected() {
    // Opaque foreign type names register as type-namespace symbols so
    // sibling extern fns referencing them by reference type resolve
    // cleanly. The resolver-level coverage here only asserts no
    // undefined-name error escapes; typechecker tests cover env-side
    // registration.
    let result = resolve_ok(
        "unsafe extern \"C\" {\n\
             pub type File;\n\
         }",
    );
    let _ = result;
}

#[test]
fn test_extern_block_opaque_type_duplicate_in_same_block_rejected() {
    // Two `type Name;` declarations with the same name inside one
    // block collide via the resolver's standard duplicate-name path.
    let errors = resolve_errors(
        "unsafe extern \"C\" {\n\
             pub type File;\n\
             pub type File;\n\
         }",
    );
    assert!(
        !errors.is_empty(),
        "expected duplicate-definition diagnostic"
    );
}

// ── unsafe extern { } slice 3: block items resolve at module scope ──
//
// The block is a syntactic + trust-boundary marker, not a separate
// namespace. Each child name binds at module scope exactly as a
// top-level item would; two-pass resolution sees the children
// through the flat module symbol table.

#[test]
fn test_unsafe_extern_block_function_visible_at_module_scope() {
    // Sibling top-level fn calls the extern fn by bareword — no
    // block-qualified path needed, because the block introduces no
    // namespace.
    resolve_ok(
        "unsafe extern \"C\" { fn libc_strlen(s: i64) -> i64; }\n\
         fn count(s: i64) -> i64 { libc_strlen(s) }",
    );
}

#[test]
fn test_unsafe_extern_block_function_collides_with_top_level_function() {
    // Module-scope `fn write` followed by `unsafe extern { fn write }`
    // collides because both names define into the same module scope.
    let errors = resolve_errors(
        "fn write(x: i32) {}\n\
         unsafe extern \"C\" { fn write(fd: i32, buf: i64, count: i64) -> i64; }",
    );
    assert!(
        errors.iter().any(|e| e.message.contains("already defined")),
        "expected duplicate-definition error across module-scope fn and extern fn, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_unsafe_extern_block_function_collides_across_separate_blocks() {
    // Block boundaries do not shield names — two `unsafe extern { }`
    // blocks both declaring `fn read` collide at module scope.
    let errors = resolve_errors(
        "unsafe extern \"C\" { fn read(fd: i32) -> i64; }\n\
         unsafe extern \"C\" { fn read(handle: i64) -> i64; }",
    );
    assert!(
        errors.iter().any(|e| e.message.contains("already defined")),
        "expected duplicate-definition error across two extern blocks, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_unsafe_extern_block_resolves_type_reference_against_module_scope() {
    // The block doesn't introduce a fresh type namespace — type
    // expressions inside extern fn signatures resolve against the
    // outer module scope (here: a struct defined at module scope
    // before the block).
    resolve_ok(
        "struct Buf { len: i64, ptr: i64 }\n\
         unsafe extern \"C\" { fn fill(b: ref Buf, value: u8); }",
    );
}

// ── unsafe_op_in_unsafe_fn slice 2: resolver passthrough confirmation ──
//
// `unsafe fn` is a precondition marker on the declaration; the resolver
// erases the distinction (no `is_unsafe` lives on `SymbolKind::Function`).
// Name resolution, scope handling, and duplicate-definition detection
// behave identically for `unsafe fn` and plain `fn`. Slice 3 will add the
// operation-lint pass that walks fn bodies; slice 2 just pins the
// no-behaviour-change story with focused tests.

#[test]
fn test_unsafe_fn_resolves_at_module_scope_like_plain_fn() {
    // `unsafe fn` registers a Function symbol at module scope; the
    // resolver does not distinguish it from a plain `fn`.
    let result = resolve_ok("unsafe fn raw_op() { }");
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), "raw_op");
    assert!(
        sym.is_some(),
        "expected `raw_op` to register at module scope"
    );
    assert!(matches!(sym.unwrap().kind, SymbolKind::Function { .. }));
}

#[test]
fn test_unsafe_fn_body_resolves_against_module_scope() {
    // Body name resolution inside `unsafe fn` is identical to plain `fn`:
    // module-scope consts and sibling functions resolve normally.
    resolve_ok(
        "const LIMIT: i64 = 64;\n\
         fn helper(x: i64) -> i64 { x }\n\
         unsafe fn raw_compute(n: i64) -> i64 { helper(n) + LIMIT }",
    );
}

#[test]
fn test_unsafe_fn_collides_with_plain_fn_of_same_name() {
    // The `unsafe` marker does not shield the name — declaring an
    // `unsafe fn` and a plain `fn` with the same name at module scope
    // is duplicate-definition like any other collision.
    let errors = resolve_errors(
        "fn act() {}\n\
         unsafe fn act() {}",
    );
    assert!(
        errors.iter().any(|e| e.message.contains("already defined")),
        "expected duplicate-definition error across plain fn and unsafe fn, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_const_in_expression() {
    resolve_ok(
        "const MAX: i64 = 100;\n\
         fn check(x: i64) -> bool { x < MAX }",
    );
}

#[test]
fn test_generic_function() {
    resolve_ok(
        "trait Display { fn show(self); }\n\
         fn print_item[T: Display](item: T) { item; }",
    );
}

// ── Destructuring in function/closure parameters ─────────────────

#[test]
fn test_tuple_destructuring_param_resolves() {
    resolve_ok("fn add((a, b): (i64, i64)) -> i64 { a + b }");
}

#[test]
fn test_struct_destructuring_param_resolves() {
    resolve_ok(
        "struct Point { x: i64, y: i64 }\n\
         fn get_x(Point { x, y }: Point) -> i64 { x }",
    );
}

#[test]
fn test_wildcard_destructuring_param_resolves() {
    resolve_ok("fn y_only((_, y): (i64, i64)) -> i64 { y }");
}

#[test]
fn test_mixed_params_with_destructuring_resolves() {
    resolve_ok("fn foo(name: i64, (a, b): (i64, i64)) -> i64 { name + a + b }");
}

#[test]
fn test_closure_destructuring_param_resolves() {
    resolve_ok("fn main() { let f = |(a, b)| a; }");
}

#[test]
fn test_nested_tuple_destructuring_param_resolves() {
    resolve_ok("fn nested(((a, b), c): ((i64, i64), i64)) -> i64 { a + b + c }");
}

// ── Generic Params on Structs/Enums/Traits ─────────────────────

#[test]
fn test_generic_struct_field_resolves() {
    resolve_ok("struct Box[T] { value: T }");
}

#[test]
fn test_generic_enum_variant_resolves() {
    resolve_ok("enum Option[T] { Some(T), None }");
}

#[test]
fn test_generic_trait_method_resolves() {
    resolve_ok(
        "trait Container[T] {\n\
             fn get(self) -> T;\n\
         }",
    );
}

#[test]
fn test_generic_impl_block_resolves() {
    resolve_ok(
        "struct Wrapper[T] { inner: T }\n\
         impl[T] Wrapper[T] {\n\
             fn unwrap(self) -> T { self.inner }\n\
         }",
    );
}

// ── Self/Self Outside Impl ─────────────────────────────────────

#[test]
fn test_self_value_outside_impl_error() {
    let errors = resolve_errors("fn main() { self; }");
    assert!(errors
        .iter()
        .any(|e| e.message.contains("self") && e.message.contains("outside")));
}

#[test]
fn test_self_type_outside_impl_error() {
    // Self as a return type outside of an impl block
    let errors = resolve_errors("fn make() -> Self { }");
    assert!(errors.iter().any(|e| e.message.contains("Self")));
}

// ── Module Declaration ─────────────────────────────────────────

#[test]
fn test_process_builtin_module_registered() {
    // `mod name;` is rejected at parse time per CR-24 slice 9, so the
    // resolver no longer registers user-declared modules. The only
    // `SymbolKind::Module` left in scope-0 is the `process` builtin.
    let result = resolve_ok("");
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), "process");
    assert!(sym.is_some());
    assert!(matches!(sym.unwrap().kind, SymbolKind::Module));
}

// ── Effect Verb Declaration ────────────────────────────────────

#[test]
fn test_effect_verb_registered() {
    let result = resolve_ok("effect verb logs;");
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), "logs");
    assert!(sym.is_some());
    assert!(matches!(sym.unwrap().kind, SymbolKind::EffectVerb));
}

// ── Qualified Path ─────────────────────────────────────────────

#[test]
fn test_qualified_enum_variant_path() {
    resolve_ok(
        "enum Color { Red, Green, Blue }\n\
         fn main() { let c = Color.Red; }",
    );
}

// ── Reserved identifiers ────────────────────────────────────────

#[test]
fn test_reserved_identifier_fn_type() {
    // `Fn` triggers both a parser naming error (Type-class fn name) and a resolver
    // ReservedIdentifier error. Parse with errors allowed, then check the resolver.
    let parsed = parse("fn Fn() {}");
    let result = resolve(&parsed.program);
    assert!(result
        .errors
        .iter()
        .any(|e| e.kind == ResolveErrorKind::ReservedIdentifier && e.message.contains("Fn")));
}

#[test]
fn test_reserved_identifier_split_by_variant() {
    let errors = resolve_errors("fn main() { let split_by_variant = 1; }");
    assert!(errors
        .iter()
        .any(|e| e.kind == ResolveErrorKind::ReservedIdentifier
            && e.message.contains("split_by_variant")));
}

#[test]
fn test_non_reserved_identifiers_ok() {
    // Similar names should be fine
    resolve_ok("fn main() { let fn_ptr = 1; let splitter = 2; }");
}

// ── Labeled Loop Validation ────────────────────────────────────

#[test]
fn test_break_undefined_label() {
    // Use `continue` to test undefined labels — no value ambiguity.
    // `break` with an unknown identifier is parsed as `break <value-expr>`,
    // not as a labeled break, since the parser only recognizes known loop labels.
    let errors = resolve_errors("fn main() { loop { continue unknown_label; } }");
    assert!(
        errors
            .iter()
            .any(|e| e.kind == ResolveErrorKind::UndefinedLabel
                && e.message.contains("unknown_label"))
    );
}

#[test]
fn test_break_wrong_label() {
    // `outer:` is a known label, but `wrong` is not — resolver catches it via continue
    let errors = resolve_errors("fn main() { outer: loop { loop { continue wrong; } } }");
    assert!(errors
        .iter()
        .any(|e| e.kind == ResolveErrorKind::UndefinedLabel && e.message.contains("wrong")));
}

#[test]
fn test_continue_undefined_label() {
    let errors = resolve_errors("fn main() { loop { continue unknown_label; } }");
    assert!(
        errors
            .iter()
            .any(|e| e.kind == ResolveErrorKind::UndefinedLabel
                && e.message.contains("unknown_label"))
    );
}

#[test]
fn test_break_valid_label_ok() {
    resolve_ok("fn main() { outer: loop { loop { break outer (); } } }");
}

#[test]
fn test_continue_valid_label_ok() {
    resolve_ok("fn main() { outer: while true { while true { continue outer; } } }");
}

#[test]
fn test_break_label_with_value_undefined() {
    // With a known outer label, break with a wrong label is caught by the resolver
    let errors = resolve_errors("fn main() { outer: loop { loop { continue nope; } } }");
    assert!(errors
        .iter()
        .any(|e| e.kind == ResolveErrorKind::UndefinedLabel && e.message.contains("nope")));
}

#[test]
fn test_labeled_for_loop_ok() {
    resolve_ok("fn main() { outer: for x in [1, 2] { for y in [3, 4] { break outer (); } } }");
}

// ── Labeled Block Validation ───────────────────────────────────

#[test]
fn test_labeled_block_nested_same_name_shadows() {
    // Inner labeled block shadows outer same-name label; inner
    // `break label` resolves to inner. Outer `break label` after
    // exiting inner refers to outer. Resolver-level check: both
    // breaks resolve without an unknown-label diagnostic.
    resolve_ok("fn main() { lbl: { lbl: { break lbl; } break lbl; } }");
}

#[test]
fn test_labeled_block_unknown_label_diagnostic() {
    // `continue never_declared;` outside any labeled construct
    // produces UndefinedLabel — same diagnostic family used for
    // labeled loops. (Use `continue` rather than `break <expr>`
    // to avoid the parser swallowing the identifier as a value.)
    let errors = resolve_errors("fn main() { loop { continue never_declared; } }");
    assert!(errors.iter().any(|e| {
        e.kind == ResolveErrorKind::UndefinedLabel && e.message.contains("never_declared")
    }));
}

#[test]
fn test_continue_label_to_block_rejected() {
    // `lbl: { continue lbl; }` is rejected with the new
    // E_CONTINUE_LABEL_BLOCK diagnostic — `continue` is only valid
    // for loop labels, not for labeled blocks.
    let errors = resolve_errors("fn main() { lbl: { continue lbl; } }");
    let hit = errors.iter().find(|e| {
        e.kind == ResolveErrorKind::ContinueOnBlockLabel
            && e.message.contains("E_CONTINUE_LABEL_BLOCK")
            && e.message.contains("labeled block")
            && e.message.contains("`lbl`")
    });
    assert!(
        hit.is_some(),
        "expected ContinueOnBlockLabel diagnostic; got: {:?}",
        errors
    );
    // The diagnostic carries a fix-it suggestion (renames or
    // restructure as loop).
    let hit = hit.unwrap();
    assert!(
        hit.suggestion.is_some(),
        "expected suggestion on ContinueOnBlockLabel, got None"
    );
}

#[test]
fn test_break_inside_closure_cannot_target_enclosing_label() {
    // LB4 — closure-boundary rule. A `break label` inside a closure
    // body cannot target an enclosing labeled block; the resolver
    // surfaces this as `undefined loop label`. Same shape with a
    // labeled `for` loop also rejects (audit-finding fix: the
    // labeled-loop closure-boundary gap was missed before this slice).
    let errors_block = resolve_errors("fn main() { lbl: { let f = || { break lbl; }; f(); } }");
    assert!(
        errors_block
            .iter()
            .any(|e| e.kind == ResolveErrorKind::UndefinedLabel && e.message.contains("lbl")),
        "labeled-block closure-boundary not enforced: {:?}",
        errors_block
    );

    // Labeled-loop variant — fixes the audit-finding gap (LB4 fixes
    // the loop-side closure-boundary rule as a side-effect).
    let errors_loop =
        resolve_errors("fn main() { lbl: for x in [1, 2] { let f = || { continue lbl; }; f(); } }");
    assert!(
        errors_loop
            .iter()
            .any(|e| e.kind == ResolveErrorKind::UndefinedLabel && e.message.contains("lbl")),
        "labeled-loop closure-boundary not enforced: {:?}",
        errors_loop
    );
}

#[test]
fn test_labeled_block_label_scope_ends_at_closing_brace() {
    // The label scope ends at the closing `}` of the labeled
    // block. A subsequent `continue lbl;` at the same scope as the
    // labeled block's parent rejects with UndefinedLabel.
    let errors = resolve_errors("fn main() { loop { lbl: { } continue lbl; } }");
    assert!(
        errors
            .iter()
            .any(|e| e.kind == ResolveErrorKind::UndefinedLabel && e.message.contains("lbl")),
        "label scope should end at closing brace; errors: {:?}",
        errors
    );
}

// ── Built-in Module Resolution ────────────────────────────────

#[test]
fn test_process_module_resolves() {
    resolve_ok("fn main() { process.exit(0); }");
}

#[test]
fn test_f32_f64_total_order_types_resolve() {
    resolve_ok("fn main() { let x: F32 = F32 { value: 1.0 }; let y: F64 = F64 { value: 2.0 }; }");
}

#[test]
fn test_ordering_variants_resolve() {
    // Comparison-Ordering variants (Less / Equal / Greater)
    resolve_ok("fn main() { let lt = Ordering.Less; let eq = Ordering.Equal; }");
}

#[test]
fn test_memory_ordering_variants_resolve() {
    resolve_ok("fn main() { let r = MemoryOrdering.Relaxed; let a = MemoryOrdering.Acquire; }");
}

#[test]
fn test_atomic_resolves() {
    resolve_ok("fn main() { let a = Atomic.new(0); }");
}

// ── Layout validation ────────────────────────────────────────────

#[test]
fn test_layout_valid_all_fields_assigned() {
    resolve_ok(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
"#,
    );
}

#[test]
fn test_layout_unknown_field_error() {
    let errors = resolve_errors(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, nonexistent }
}
"#,
    );
    assert!(
        errors.iter().any(|e| e.message.contains("nonexistent")),
        "expected error about unknown field, got: {:?}",
        errors
    );
}

#[test]
fn test_layout_duplicate_field_error() {
    let errors = resolve_errors(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group a { x }
    group b { x }
}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("multiple sections")),
        "expected duplicate field error, got: {:?}",
        errors
    );
}

#[test]
fn test_layout_unassigned_fields_warning() {
    let errors = resolve_errors(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x }
}
"#,
    );
    assert!(
        errors.iter().any(|e| e.message.contains("not assigned")),
        "expected unassigned fields warning, got: {:?}",
        errors
    );
}

// ── Layout: cold section and align(N) ────────────────────────

#[test]
fn test_layout_cold_section_valid() {
    resolve_ok(
        r#"
struct Entity { x: f64, y: f64, hp: i64, name: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    cold { hp, name }
}
"#,
    );
}

#[test]
fn test_layout_cold_duplicate_cold_section_error() {
    let errors = resolve_errors(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x }
    cold { y }
    cold { hp }
}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("more than one cold")),
        "expected duplicate cold section error, got: {:?}",
        errors
    );
}

#[test]
fn test_layout_cold_field_also_in_group_error() {
    let errors = resolve_errors(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    cold { y }
}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("multiple sections")),
        "expected field-in-both-group-and-cold error, got: {:?}",
        errors
    );
}

#[test]
fn test_layout_cold_unknown_field_error() {
    let errors = resolve_errors(
        r#"
struct Entity { x: f64, y: f64 }
layout entities: Vec[Entity] {
    group physics { x }
    cold { no_such_field }
}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("does not exist") && e.message.contains("cold section")),
        "expected unknown-field error in cold section, got: {:?}",
        errors
    );
}

#[test]
fn test_layout_align_valid_power_of_two() {
    resolve_ok(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group a { x, y } align(64)
    group b { hp } align(128)
}
"#,
    );
}

#[test]
fn test_layout_align_not_power_of_two_error() {
    let errors = resolve_errors(
        r#"
struct Entity { x: f64, y: f64 }
layout entities: Vec[Entity] {
    group a { x, y } align(63)
}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("not a power of two")),
        "expected align-not-power-of-two error, got: {:?}",
        errors
    );
}

#[test]
fn test_layout_align_zero_error() {
    let errors = resolve_errors(
        r#"
struct Entity { x: f64 }
layout entities: Vec[Entity] {
    group a { x } align(0)
}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("not a power of two")),
        "expected align(0) error, got: {:?}",
        errors
    );
}

// ── Heap-owning fields in SoA layouts ─────────────────────────
// `String` and `Vec[POD]` element fields ARE supported: codegen
// synthesizes `__karac_soa_drop_<layout>` to free each live
// element's String/Vec buffers at scope exit, on overwrite, and on
// the carried-grid reassignment (`src/codegen/runtime.rs`
// `emit_soa_drop_fn`). The remaining heap types stay rejected at
// layout validation because the per-element drop doesn't cover them:
// `Map` / `Set` / `VecDeque` / `Sorted*` (opaque handles),
// `Vec[heap T]` (the drop frees only a Vec field's OUTER buffer,
// not its elements), and `shared struct` / `shared enum` (RC).

#[test]
fn test_layout_accepts_string_field_in_group() {
    // String is now supported in SoA layouts — codegen drops each
    // element's String buffer. All fields are assigned, so no error
    // (heap-rejection or unassigned) should fire.
    resolve_ok(
        r#"
struct Entity { x: f64, name: String }
layout entities: Vec[Entity] {
    group physics { x }
    group meta { name }
}
"#,
    );
}

#[test]
fn test_layout_accepts_vec_pod_field_in_group() {
    // `Vec[bool]` — a Vec over a POD element — is supported: the
    // per-element drop frees the field's outer buffer (no element
    // recursion needed, since `bool` carries no heap).
    resolve_ok(
        r#"
struct Grid { width: i64, cells: Vec[bool] }
layout grids: Vec[Grid] {
    group dims { width }
    group bulk { cells }
}
"#,
    );
}

#[test]
fn test_layout_rejects_vec_of_heap_field_in_group() {
    // `Vec[String]` — a Vec whose ELEMENTS carry heap — stays
    // rejected: the SoA per-element drop frees only the Vec field's
    // outer buffer (one-level), so its element Strings would leak.
    let errors = resolve_errors(
        r#"
struct Doc { id: i64, lines: Vec[String] }
layout docs: Vec[Doc] {
    group ids { id }
    group text { lines }
}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("Vec of heap-owning String")
                && e.message.contains("group 'text'")),
        "expected Vec-of-heap rejection, got: {:?}",
        errors
    );
}

#[test]
fn test_layout_rejects_map_field_in_group() {
    let errors = resolve_errors(
        r#"
struct Index { id: i64, table: Map[String, i64] }
layout indexes: Vec[Index] {
    group ids { id }
    group lookup { table }
}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("heap-owning type (Map[…, …])")),
        "expected heap-owning Map rejection, got: {:?}",
        errors
    );
}

#[test]
fn test_layout_accepts_string_field_in_cold_section() {
    // A String in the cold section is supported — the cold group's
    // buffer is walked by the same per-element drop as the hot groups.
    resolve_ok(
        r#"
struct Entity { x: f64, label: String }
layout entities: Vec[Entity] {
    group physics { x }
    cold { label }
}
"#,
    );
}

#[test]
fn test_layout_accepts_tuple_containing_string() {
    // `(i64, String)` — an inline tuple whose only heap leaf is a
    // String — is supported: `emit_aggregate_heap_field_frees`
    // recurses the tuple and frees the nested String per element.
    resolve_ok(
        r#"
struct Entity { x: f64, tag: (i64, String) }
layout entities: Vec[Entity] {
    group physics { x }
    group meta { tag }
}
"#,
    );
}

#[test]
fn test_layout_rejects_shared_struct_field() {
    let errors = resolve_errors(
        r#"
shared struct Inner { x: i64 }
struct Outer { id: i64, inner: Inner }
layout outers: Vec[Outer] {
    group ids { id }
    group refs { inner }
}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("heap-owning type (shared struct Inner)")),
        "expected shared-struct rejection, got: {:?}",
        errors
    );
}

#[test]
fn test_layout_accepts_primitive_only_fields() {
    // All-primitive SoA must resolve cleanly — no heap-owning
    // rejection should fire and no other layout errors should be
    // raised either.
    resolve_ok(
        r#"
struct Entity { x: f64, y: f64, hp: i64, label: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
    cold { label }
}
"#,
    );
}

// ── Operator-trait impl restriction (Step 7) ──────────────────

#[test]
fn test_user_impl_add_for_struct_rejected() {
    let errors = resolve_errors(
        "struct Vector { x: f64, y: f64 }\n\
         impl Add for Vector {\n\
             fn add(self, rhs: Vector) -> Vector { self }\n\
         }",
    );
    assert!(
        errors.iter().any(|e| matches!(
            e.kind,
            karac::resolver::ResolveErrorKind::OperatorTraitImplRestricted
        )),
        "expected operator-trait restriction error, got: {:?}",
        errors
    );
}

#[test]
fn test_user_impl_add_for_vec_has_concat_hint() {
    let errors = resolve_errors(
        "impl Add for Vec {\n\
             fn add(self, rhs: Vec) -> Vec { self }\n\
         }",
    );
    let err = errors.iter().find(|e| {
        matches!(
            e.kind,
            karac::resolver::ResolveErrorKind::OperatorTraitImplRestricted
        )
    });
    assert!(
        err.is_some(),
        "expected operator-trait restriction error, got: {:?}",
        errors
    );
    let err = err.unwrap();
    assert!(err.message.contains("Vec"), "message: {}", err.message);
    let suggestion = err.suggestion.as_deref().unwrap_or("");
    assert!(
        suggestion.contains("concat") || suggestion.contains("extend"),
        "expected concat/extend hint in suggestion, got: {}",
        suggestion
    );
}

#[test]
fn test_user_impl_from_still_allowed() {
    // From is a conversion trait, not an operator trait — user impls remain
    // legal (required for `?` cross-error propagation).
    resolve_ok(
        "struct ParseError { msg: String }\n\
         struct AppError { msg: String }\n\
         impl From for AppError {\n\
             fn from(e: ParseError) -> AppError { AppError { msg: e.msg } }\n\
         }",
    );
}

#[test]
fn test_user_impl_eq_ord_for_struct_allowed() {
    // Eq/Ord are carved out of the operator-trait restriction — user types
    // routinely need custom equality/ordering, and the operator lowering
    // pass routes `==`/`<` through these impls. CR-202 slice 5b added
    // `Eq: PartialEq` as a supertrait edge, so `impl Eq for Point` now
    // also requires `impl PartialEq for Point` (typechecker-level
    // `MissingSupertrait`; resolver-level passes are unaffected, but the
    // impl is added here so the same source resolves cleanly through
    // typecheck too).
    resolve_ok(
        "struct Point { x: i64, y: i64 }\n\
         impl PartialEq for Point {\n\
             fn eq(ref self, other: ref Point) -> bool { self.x == other.x and self.y == other.y }\n\
         }\n\
         impl Eq for Point {\n\
             fn eq(self, other: Point) -> bool { self.x == other.x and self.y == other.y }\n\
         }\n\
         impl PartialOrd for Point {\n\
             fn partial_cmp(ref self, other: ref Point) -> Option[Ordering] { Some(self.x.cmp(other.x)) }\n\
         }\n\
         impl Ord for Point {\n\
             fn lt(self, other: Point) -> bool { self.x < other.x }\n\
             fn le(self, other: Point) -> bool { self.x <= other.x }\n\
             fn gt(self, other: Point) -> bool { self.x > other.x }\n\
             fn ge(self, other: Point) -> bool { self.x >= other.x }\n\
         }",
    );
}

#[test]
fn test_user_impl_display_for_struct_and_enum_allowed() {
    // `Display` is carved out of the operator-trait restriction — user types
    // routinely need custom string rendering (error enums, domain values).
    // Kāra's `Display` is a single `fn to_string(ref self) -> String`, so a
    // user impl is an ordinary method that `f"{x}"` / `x.to_string()` /
    // `println(x)` dispatch through. GAP-W4 (examples/weave).
    resolve_ok(
        "struct Point { x: i64, y: i64 }\n\
         impl Display for Point {\n\
             fn to_string(ref self) -> String { f\"({self.x}, {self.y})\" }\n\
         }\n\
         enum Status { Ok, Failed(i64) }\n\
         impl Display for Status {\n\
             fn to_string(ref self) -> String {\n\
                 match self { Ok => \"ok\", Failed(code) => f\"failed#{code}\" }\n\
             }\n\
         }",
    );
}

#[test]
fn test_user_impl_arithmetic_still_rejected_after_relational_carveout() {
    // The relational-trait carve-out is surgical: Add/Sub/etc. on user types
    // stay rejected until the heterogeneous `Rhs`/`Output` design lands.
    let errors = resolve_errors(
        "struct Vector { x: f64 }\n\
         impl Mul for Vector {\n\
             fn mul(self, rhs: Vector) -> Vector { self }\n\
         }",
    );
    assert!(
        errors.iter().any(|e| matches!(
            e.kind,
            karac::resolver::ResolveErrorKind::OperatorTraitImplRestricted
        )),
        "expected Mul to still be rejected, got: {:?}",
        errors
    );
}

#[test]
fn test_user_impl_into_rejected() {
    // `impl Into` is derived from `From` via a blanket impl — user impls are
    // rejected with a suggestion to implement `From` instead.
    let errors = resolve_errors(
        "struct Inches { n: i64 }\n\
         struct Cm { n: i64 }\n\
         impl Into for Inches {\n\
             fn into(self) -> Cm { Cm { n: 0 } }\n\
         }",
    );
    assert!(
        errors.iter().any(|e| matches!(
            e.kind,
            karac::resolver::ResolveErrorKind::IntoTraitImplNotAllowed
        )),
        "expected Into impl to be rejected, got: {:?}",
        errors
    );
}

#[test]
fn test_user_impl_tryinto_rejected() {
    let errors = resolve_errors(
        "struct Narrow { n: i8 }\n\
         impl TryInto for i64 {\n\
             fn try_into(self) -> Result[Narrow, String] { Err(\"\") }\n\
         }",
    );
    assert!(
        errors.iter().any(|e| matches!(
            e.kind,
            karac::resolver::ResolveErrorKind::IntoTraitImplNotAllowed
        )),
        "expected TryInto impl to be rejected, got: {:?}",
        errors
    );
}

#[test]
fn test_impl_level_effect_var_rejected() {
    // `impl[..., with E] Trait for Type` is rejected: effect polymorphism is
    // expressed via `with _` on the trait method, not bound at impl level.
    // (Round 6 step 4 — design.md § Conversion Traits.)
    let errors = resolve_errors(
        "trait Sink { fn drain(ref self); }\n\
         struct LogSink { name: String }\n\
         impl[with E] Sink for LogSink {\n\
             fn drain(ref self) {}\n\
         }",
    );
    assert!(
        errors.iter().any(|e| matches!(
            e.kind,
            karac::resolver::ResolveErrorKind::ImplLevelEffectVarNotAllowed
        )),
        "expected ImplLevelEffectVarNotAllowed, got: {:?}",
        errors
    );
    let err = errors
        .iter()
        .find(|e| {
            matches!(
                e.kind,
                karac::resolver::ResolveErrorKind::ImplLevelEffectVarNotAllowed
            )
        })
        .unwrap();
    assert!(
        err.message.contains("`E`"),
        "diagnostic should name the offending effect variable, got: {}",
        err.message
    );
    assert!(
        err.suggestion
            .as_ref()
            .is_some_and(|s| s.contains("with _")),
        "suggestion should point at `with _` on the trait method, got: {:?}",
        err.suggestion
    );
}

#[test]
fn test_impl_with_type_params_only_accepted() {
    // Regression: `impl[T] Trait[T] for Wrapper { ... }` — type-only generics
    // on impl are unaffected by step 4. Only `with E` (effect-variable) impl
    // generics are rejected.
    resolve_ok(
        "trait Container[T] { fn get(ref self) -> T; }\n\
         struct Wrapper[T] { value: T }\n\
         impl[T] Container[T] for Wrapper[T] {\n\
             fn get(ref self) -> T { self.value }\n\
         }",
    );
}

// ── Phase 6 line 26 slice 8ac: `E: Effect` resolver acceptance ────
//
// The bound-form spelling `[T, E: Effect]` (design.md line 736) must
// resolve identically to the positional `[T, with E]` form. These
// tests pin the no-regression resolver acceptance — the parser does
// the classification, so the resolver sees an EffectParam either way.

#[test]
fn test_slice_8ac_effect_bound_form_resolves_ok() {
    resolve_ok(
        "pub fn map[T, U, E: Effect](items: Vec[T], f: Fn(T) -> U with E) -> Vec[U] with E { todo() }\n\
         fn todo() -> Vec[i64] { Vec.new() }",
    );
}

#[test]
fn test_slice_8ac_effect_bound_form_on_impl_still_rejected() {
    // The impl-level effect-variable rejection (slice 4) applies to both
    // spellings — the bound form does not bypass the rule. The diagnostic
    // should still name `E` and point at the `with _` workaround.
    let errors = resolve_errors(
        "trait Sink { fn drain(ref self); }\n\
         struct LogSink { name: String }\n\
         impl[E: Effect] Sink for LogSink {\n\
             fn drain(ref self) {}\n\
         }",
    );
    assert!(
        errors.iter().any(|e| matches!(
            e.kind,
            karac::resolver::ResolveErrorKind::ImplLevelEffectVarNotAllowed
        )),
        "expected ImplLevelEffectVarNotAllowed for bound-form impl, got: {:?}",
        errors
    );
    let err = errors
        .iter()
        .find(|e| {
            matches!(
                e.kind,
                karac::resolver::ResolveErrorKind::ImplLevelEffectVarNotAllowed
            )
        })
        .unwrap();
    assert!(
        err.message.contains("`E`"),
        "diagnostic should name `E` even for the bound-form spelling, got: {}",
        err.message
    );
}

// ── `providers { }` block resource-name resolution ───────────────

#[test]
fn test_providers_block_rejects_undefined_resource() {
    let errors = resolve_errors(
        "fn mk() -> i64 { 0 }\n\
         fn main() { providers { UnknownResource => mk() } in { } }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == ResolveErrorKind::UndefinedName
                && e.message.contains("UnknownResource")),
        "expected UndefinedName for UnknownResource, got {:?}",
        errors
    );
}

#[test]
fn test_providers_block_rejects_non_effect_resource_name() {
    // `Point` is a struct — valid symbol, wrong kind.
    let errors = resolve_errors(
        "struct Point { x: i64 }\n\
         fn mk() -> i64 { 0 }\n\
         fn main() { providers { Point => mk() } in { } }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("not an effect resource")),
        "expected 'not an effect resource' error, got {:?}",
        errors
    );
}

#[test]
fn test_providers_block_accepts_known_effect_resource() {
    let _ = resolve_ok(
        "effect resource UserDB;\n\
         struct Db { tag: i64 }\n\
         fn main() { providers { UserDB => Db { tag: 0 } } in { } }",
    );
}

// ── Reserved effect resource names (E0228) ──────────────────────

#[test]
fn test_compile_time_env_reserved() {
    let errors = resolve_errors("effect resource CompileTimeEnv;");
    assert!(
        errors
            .iter()
            .any(|e| e.kind == ResolveErrorKind::ReservedEffectResource
                && e.message.contains("CompileTimeEnv")),
        "expected ReservedEffectResource for CompileTimeEnv, got {:?}",
        errors
    );
}

#[test]
fn test_compile_time_heap_reserved() {
    let errors = resolve_errors("effect resource CompileTimeHeap;");
    assert!(
        errors
            .iter()
            .any(|e| e.kind == ResolveErrorKind::ReservedEffectResource
                && e.message.contains("CompileTimeHeap")),
        "expected ReservedEffectResource for CompileTimeHeap, got {:?}",
        errors
    );
}

#[test]
fn test_reserved_effect_resource_not_registered_in_scope() {
    // The reserved name must not be inserted into the symbol table,
    // so a subsequent reference to it should be an undefined-resource error.
    let parsed = karac::parse(
        "effect resource CompileTimeEnv;\n\
         effect group g = reads(CompileTimeEnv);",
    );
    assert!(parsed.errors.is_empty());
    let result = karac::resolve(&parsed.program);
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == ResolveErrorKind::ReservedEffectResource),
        "expected ReservedEffectResource error, got {:?}",
        result.errors
    );
    // Must NOT be present in the symbol table.
    assert!(
        result
            .symbol_table
            .lookup_in_scope(karac::resolver::ScopeId(0), "CompileTimeEnv")
            .is_none(),
        "reserved resource should not appear in the symbol table"
    );
}

#[test]
fn test_non_reserved_effect_resource_still_works() {
    // Names that are merely similar must not be rejected.
    let result = resolve_ok("effect resource CompileTimeStorage;");
    let sym = result
        .symbol_table
        .lookup_in_scope(ScopeId(0), "CompileTimeStorage");
    assert!(sym.is_some());
    assert!(matches!(sym.unwrap().kind, SymbolKind::EffectResource));
}

// ── Trait-bound tracking on generic params ──────────────────────

/// Bound trait names recorded for the (single) generic param named `param_name`.
/// Each `TraitBound`'s last path segment is what the typechecker checks against.
fn bounds_for_param(table: &SymbolTable, param_name: &str) -> Vec<String> {
    let entry = table
        .generic_param_bounds
        .iter()
        .find(|(id, _)| table.get_symbol(**id).name == param_name);
    let bounds: &[TraitBound] = entry.map(|(_, bs)| bs.as_slice()).unwrap_or(&[]);
    bounds
        .iter()
        .map(|b| b.path.last().cloned().unwrap_or_default())
        .collect()
}

#[test]
fn test_inline_bound_recorded_on_function_param() {
    let result = resolve_ok("fn sort[T: Ord](items: T) -> T { items }");
    let bounds = bounds_for_param(&result.symbol_table, "T");
    assert_eq!(bounds, vec!["Ord".to_string()]);
}

#[test]
fn test_multiple_inline_bounds_recorded() {
    let result = resolve_ok("fn f[T: Eq + Ord](x: T) -> T { x }");
    let mut bounds = bounds_for_param(&result.symbol_table, "T");
    bounds.sort();
    assert_eq!(bounds, vec!["Eq".to_string(), "Ord".to_string()]);
}

#[test]
fn test_where_clause_bound_recorded_on_function_param() {
    let result = resolve_ok("fn sort[T](items: T) -> T where T: Ord { items }");
    let bounds = bounds_for_param(&result.symbol_table, "T");
    assert_eq!(bounds, vec!["Ord".to_string()]);
}

#[test]
fn test_inline_and_where_clause_bounds_merged() {
    let result = resolve_ok("fn f[T: Eq](x: T) -> T where T: Ord { x }");
    let mut bounds = bounds_for_param(&result.symbol_table, "T");
    bounds.sort();
    assert_eq!(bounds, vec!["Eq".to_string(), "Ord".to_string()]);
}

#[test]
fn test_inline_bound_recorded_on_struct_param() {
    let result = resolve_ok("struct Sorted[T: Ord] { items: T }");
    let bounds = bounds_for_param(&result.symbol_table, "T");
    assert_eq!(bounds, vec!["Ord".to_string()]);
}

#[test]
fn test_where_clause_bound_recorded_on_enum_param() {
    let result = resolve_ok("enum Pair[T] where T: Eq { Single(T), Double(T, T) }");
    let bounds = bounds_for_param(&result.symbol_table, "T");
    assert_eq!(bounds, vec!["Eq".to_string()]);
}

#[test]
fn test_supertrait_recorded_as_self_bound() {
    // `trait Foo: Bar` makes `Bar` a logical bound on Self inside the trait.
    let result = resolve_ok("trait Foo: Eq { fn check(self) -> bool; }");
    let bounds = bounds_for_param(&result.symbol_table, "Self");
    assert_eq!(bounds, vec!["Eq".to_string()]);
}

#[test]
fn test_no_bounds_when_param_has_none() {
    let result = resolve_ok("fn id[T](x: T) -> T { x }");
    let bounds = bounds_for_param(&result.symbol_table, "T");
    assert!(bounds.is_empty());
}

#[test]
fn test_trait_bound_path_resolution_recorded() {
    // Inline bound's trait path should be resolved against the symbol table.
    // `Ord` is a prelude trait; its SymbolId should appear in the resolution
    // map keyed by the bound's span.
    let result = resolve_ok("fn sort[T: Ord](items: T) -> T { items }");
    // Find any resolution that points to a TypeParam Ord (registered as a
    // prelude Primitive with name "Ord").
    let any_ord_resolution = result.resolutions.values().any(|&id| {
        let sym = result.symbol_table.get_symbol(id);
        sym.name == "Ord"
    });
    assert!(
        any_ord_resolution,
        "expected a resolution pointing at the prelude `Ord` trait"
    );
}

#[test]
fn test_resolver_const_param_kind() {
    // `fn f[const N: i64]` defines `N` as a `SymbolKind::ConstParam`, not
    // `SymbolKind::TypeParam` (const generics slice 1, sub-step c). The
    // declaration-site permitted-type check in the typechecker branches on
    // this distinction.
    let result = resolve_ok("fn f[const N: i64](x: i64) -> i64 { x }");
    let n_kind = result
        .symbol_table
        .all_symbols()
        .iter()
        .find(|s| s.name == "N")
        .map(|s| &s.kind)
        .expect("expected a symbol named 'N'");
    assert!(
        matches!(n_kind, SymbolKind::ConstParam),
        "expected SymbolKind::ConstParam for 'N', got {:?}",
        n_kind
    );
}

#[test]
fn test_resolver_const_type_name_collision() {
    // `fn f[T, const T: i64]` — type-param and const-param share the name
    // `T`. The resolver's existing duplicate-name detection fires; no
    // kind-aware special-casing required.
    let errs = resolve_errors("fn f[T, const T: i64](x: i64) -> i64 { x }");
    assert!(
        !errs.is_empty(),
        "expected a duplicate-name diagnostic for type/const param collision"
    );
}

#[test]
fn test_impl_block_generic_bound_recorded() {
    let result = resolve_ok(
        "struct Wrapper[T] { value: T }\n\
         impl[T: Eq] Wrapper[T] { fn check(self) -> bool { true } }",
    );
    // The impl-level T's bounds should be recorded.
    let bounds = bounds_for_param(&result.symbol_table, "T");
    assert!(
        bounds.contains(&"Eq".to_string()),
        "expected Eq among recorded bounds, got {:?}",
        bounds
    );
}

// ── #[compiler_builtin] gate (CR-202 slice 1, E0237) ─────────────
// The attribute is reserved for stdlib source baked into the compiler
// binary. User code is rejected at the resolver layer; the synthetic
// stdlib package opts in via `Resolver::with_stdlib_source(true)` (slice 3
// will wire that path through the bake step).

fn assert_compiler_builtin_rejected(source: &str) {
    let errs = resolve_errors(source);
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::CompilerBuiltinReserved),
        "expected CompilerBuiltinReserved error, got: {:?}",
        errs.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn test_compiler_builtin_rejected_on_function_in_user_code() {
    assert_compiler_builtin_rejected("#[compiler_builtin]\nfn dbg[T](v: T) -> T { v }");
}

#[test]
fn test_compiler_builtin_rejected_on_struct_in_user_code() {
    assert_compiler_builtin_rejected("#[compiler_builtin]\nstruct Vec[T] { }");
}

#[test]
fn test_compiler_builtin_rejected_on_enum_in_user_code() {
    assert_compiler_builtin_rejected("#[compiler_builtin]\nenum Option[T] { Some(T), None }");
}

#[test]
fn test_compiler_builtin_rejected_on_trait_in_user_code() {
    assert_compiler_builtin_rejected(
        "#[compiler_builtin]\ntrait Display { fn fmt(ref self) -> String; }",
    );
}

#[test]
fn test_compiler_builtin_permitted_with_stdlib_source_flag() {
    // `with_stdlib_source(true)` is the opt-in path the bake step (slice 3)
    // will use. The attribute should pass cleanly through the resolver and
    // produce no diagnostic of any kind.
    let parsed = parse(
        "#[compiler_builtin]\nfn dbg[T](v: T) -> T { v }\n\
         #[compiler_builtin]\nstruct Vec[T] { }\n\
         #[compiler_builtin]\nenum Option[T] { Some(T), None }\n\
         #[compiler_builtin]\ntrait Display { fn fmt(ref self) -> String; }",
    );
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let result = Resolver::new(&parsed.program)
        .with_stdlib_source(true)
        .resolve();
    let unrelated: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind != ResolveErrorKind::CompilerBuiltinReserved)
        .collect();
    assert!(
        result
            .errors
            .iter()
            .all(|e| e.kind != ResolveErrorKind::CompilerBuiltinReserved),
        "stdlib-source flag should suppress CompilerBuiltinReserved",
    );
    assert!(
        unrelated.is_empty(),
        "expected no other resolver errors, got: {:?}",
        unrelated.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_compiler_builtin_rejection_does_not_block_definition() {
    // The error fires alongside symbol definition — downstream references
    // to the rejected name should still resolve, so users see the gating
    // diagnostic in isolation rather than a cascade of "undefined name".
    let parsed = parse(
        "#[compiler_builtin]\nfn dbg[T](v: T) -> T { v }\n\
         fn use_it() { let _ = dbg(1); }",
    );
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let errs = resolve(&parsed.program).errors;
    let undef: Vec<_> = errs
        .iter()
        .filter(|e| e.kind == ResolveErrorKind::UndefinedName)
        .collect();
    assert!(
        undef.is_empty(),
        "rejected #[compiler_builtin] item should still be defined; got UndefinedName: {:?}",
        undef.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::CompilerBuiltinReserved),
        "expected CompilerBuiltinReserved among errors"
    );
}

// ── Per-item stdlib-origin tag (CR-202 slice 3b) ────────────────
// The bake step in slice 3c flips `Item::*.stdlib_origin = true` on
// items spliced from `runtime/stdlib/*.kara`. The resolver runs in
// user-mode for the spliced program tree (`is_stdlib_source == false`),
// so the gate must consult per-item origin to let baked items use
// `#[compiler_builtin]` even when the session-wide flag is unset.

fn flip_stdlib_origin_on_first_item(prog: &mut karac::ast::Program) {
    use karac::ast::Item;
    match prog
        .items
        .first_mut()
        .expect("program has at least one item")
    {
        Item::Function(f) => f.stdlib_origin = true,
        Item::StructDef(s) => s.stdlib_origin = true,
        Item::EnumDef(e) => e.stdlib_origin = true,
        Item::TraitDef(t) => t.stdlib_origin = true,
        other => panic!("first item is not a kind we tag: {:?}", other),
    }
}

fn assert_per_item_origin_bypasses_gate(source: &str) {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let mut program = parsed.program;
    flip_stdlib_origin_on_first_item(&mut program);
    // is_stdlib_source defaults to false — per-item tag is the only thing
    // that can suppress the gate here.
    let result = Resolver::new(&program).resolve();
    let blocked: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind == ResolveErrorKind::CompilerBuiltinReserved)
        .collect();
    assert!(
        blocked.is_empty(),
        "per-item stdlib_origin should suppress CompilerBuiltinReserved, got: {:?}",
        blocked.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_compiler_builtin_per_item_origin_bypasses_gate_on_function() {
    assert_per_item_origin_bypasses_gate("#[compiler_builtin]\nfn dbg[T](v: T) -> T { v }");
}

#[test]
fn test_compiler_builtin_per_item_origin_bypasses_gate_on_struct() {
    assert_per_item_origin_bypasses_gate("#[compiler_builtin]\nstruct Vec[T] { }");
}

#[test]
fn test_compiler_builtin_per_item_origin_bypasses_gate_on_enum() {
    assert_per_item_origin_bypasses_gate("#[compiler_builtin]\nenum Option[T] { Some(T), None }");
}

#[test]
fn test_compiler_builtin_per_item_origin_bypasses_gate_on_trait() {
    assert_per_item_origin_bypasses_gate(
        "#[compiler_builtin]\ntrait Display { fn fmt(ref self) -> String; }",
    );
}

#[test]
fn test_compiler_builtin_per_item_tag_is_per_item_not_global() {
    // Two items with `#[compiler_builtin]`. The first is tagged with
    // stdlib_origin = true (bypasses the gate). The second is left at
    // stdlib_origin = false (still rejected). Confirms the tag is
    // per-item, not implicitly applied to siblings.
    let parsed = parse(
        "#[compiler_builtin]\nfn baked[T](v: T) -> T { v }\n\
         #[compiler_builtin]\nfn user_attempt[T](v: T) -> T { v }",
    );
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let mut program = parsed.program;
    flip_stdlib_origin_on_first_item(&mut program);
    let result = Resolver::new(&program).resolve();
    let rejections: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind == ResolveErrorKind::CompilerBuiltinReserved)
        .collect();
    assert_eq!(
        rejections.len(),
        1,
        "expected exactly one rejection (the untagged sibling), got: {:?}",
        rejections.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── #[compiler_builtin] gate on impl blocks ──────────────────────
// Slice 1 closed the gate on top-level items (`fn`/`struct`/`enum`/`trait`).
// Impl blocks were the documented loophole (phase-4-interpreter.md slice 6.3
// findings #2): `collect_impl` did not call `check_compiler_builtin_attr`,
// so user code could sneak `#[compiler_builtin]` past the resolver via
// `impl Foo { #[compiler_builtin] fn bar() { } }`. Closing it: the gate
// fires on both the impl block's own attributes and each method's, with
// the per-method `stdlib_origin` exemption preserved for baked source.

#[test]
fn test_compiler_builtin_rejected_on_impl_method_in_user_code() {
    assert_compiler_builtin_rejected(
        "struct Foo { }\n\
         impl Foo {\n\
             #[compiler_builtin]\n\
             fn bar(self) -> i64 { 0 }\n\
         }",
    );
}

#[test]
fn test_compiler_builtin_rejected_on_impl_block_itself_in_user_code() {
    assert_compiler_builtin_rejected(
        "struct Foo { }\n\
         #[compiler_builtin]\n\
         impl Foo {\n\
             fn bar(self) -> i64 { 0 }\n\
         }",
    );
}

#[test]
fn test_compiler_builtin_permitted_on_impl_method_with_stdlib_source_flag() {
    // The session-wide bypass (`with_stdlib_source(true)`) should suppress
    // the gate on both impl-block-level and method-level `#[compiler_builtin]`.
    let parsed = parse(
        "struct Foo { }\n\
         #[compiler_builtin]\n\
         impl Foo {\n\
             #[compiler_builtin]\n\
             fn bar(self) -> i64 { 0 }\n\
         }",
    );
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let result = Resolver::new(&parsed.program)
        .with_stdlib_source(true)
        .resolve();
    assert!(
        result
            .errors
            .iter()
            .all(|e| e.kind != ResolveErrorKind::CompilerBuiltinReserved),
        "stdlib-source flag should suppress CompilerBuiltinReserved on impl items, got: {:?}",
        result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_compiler_builtin_per_method_origin_bypasses_gate_on_impl_method() {
    // The per-item `stdlib_origin` exemption — the bake step for slice 6.3
    // sets this on every method inside baked impl blocks. Confirms the
    // resolver picks it up: even without the session-wide flag, a method
    // tagged `stdlib_origin = true` is allowed to carry `#[compiler_builtin]`,
    // while an untagged sibling in the same block is still rejected.
    use karac::ast::{ImplItem, Item};
    let parsed = parse(
        "struct Foo { }\n\
         impl Foo {\n\
             #[compiler_builtin]\n\
             fn baked(self) -> i64 { 0 }\n\
             #[compiler_builtin]\n\
             fn user_attempt(self) -> i64 { 0 }\n\
         }",
    );
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let mut program = parsed.program;
    let imp = program
        .items
        .iter_mut()
        .find_map(|i| {
            if let Item::ImplBlock(b) = i {
                Some(b)
            } else {
                None
            }
        })
        .expect("program has an impl block");
    let baked_method = imp
        .items
        .iter_mut()
        .find_map(|i| match i {
            ImplItem::Method(m) if m.name == "baked" => Some(m),
            _ => None,
        })
        .expect("impl block has `baked` method");
    baked_method.stdlib_origin = true;

    let result = Resolver::new(&program).resolve();
    let rejections: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind == ResolveErrorKind::CompilerBuiltinReserved)
        .collect();
    assert_eq!(
        rejections.len(),
        1,
        "expected exactly one rejection (the untagged sibling method), got: {:?}",
        rejections.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── #[non_exhaustive] placement validation (E0239) ────────────────
// The attribute is valid only on `pub struct` and `pub enum`. The
// resolver rejects every other target — private types (no
// cross-package boundary, so the attribute is meaningless), traits /
// fns / impl blocks (no field-or-variant evolution surface), and
// individual struct fields (field-level non-exhaustive is post-v1).
// Per design.md § `#[non_exhaustive]` for Evolvable Public Types.

fn assert_non_exhaustive_rejected_kind(source: &str, expected_target_kind: &str) {
    let errs = resolve_errors(source);
    let matched: Vec<_> = errs
        .iter()
        .filter(|e| {
            e.kind == ResolveErrorKind::NonExhaustiveInvalidTarget
                && e.message.contains(expected_target_kind)
        })
        .collect();
    assert!(
        !matched.is_empty(),
        "expected NonExhaustiveInvalidTarget mentioning {:?}, got: {:?}",
        expected_target_kind,
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn non_exhaustive_slice1_accepted_on_pub_struct() {
    // Positive pin — no diagnostic on the legal placement.
    let parsed = parse("#[non_exhaustive]\npub struct Config { timeout: i64, }");
    assert!(parsed.errors.is_empty());
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter()
            .all(|e| e.kind != ResolveErrorKind::NonExhaustiveInvalidTarget),
        "pub struct should accept #[non_exhaustive] without diagnostic; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn non_exhaustive_slice1_accepted_on_pub_enum() {
    let parsed = parse("#[non_exhaustive]\npub enum Error { NotFound, Conflict, }");
    assert!(parsed.errors.is_empty());
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter()
            .all(|e| e.kind != ResolveErrorKind::NonExhaustiveInvalidTarget),
        "pub enum should accept #[non_exhaustive] without diagnostic; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn non_exhaustive_slice1_rejected_on_private_struct() {
    assert_non_exhaustive_rejected_kind(
        "#[non_exhaustive]\nprivate struct Internal { x: i64, }",
        "private struct",
    );
}

#[test]
fn non_exhaustive_slice1_rejected_on_private_enum() {
    assert_non_exhaustive_rejected_kind(
        "#[non_exhaustive]\nprivate enum Internal { A, B, }",
        "private enum",
    );
}

#[test]
fn non_exhaustive_slice1_rejected_on_non_pub_struct() {
    // Bare struct (no `pub`, no `private`) is module-private by default.
    // The attribute is still meaningless without `pub` and so rejected.
    assert_non_exhaustive_rejected_kind(
        "#[non_exhaustive]\nstruct Module { x: i64, }",
        "private struct",
    );
}

#[test]
fn non_exhaustive_slice1_rejected_on_function() {
    assert_non_exhaustive_rejected_kind("#[non_exhaustive]\nfn frob() { }", "function");
}

#[test]
fn non_exhaustive_slice1_rejected_on_trait() {
    assert_non_exhaustive_rejected_kind(
        "#[non_exhaustive]\ntrait Display { fn fmt(ref self) -> String; }",
        "trait",
    );
}

#[test]
fn non_exhaustive_slice1_rejected_on_marker_trait() {
    assert_non_exhaustive_rejected_kind("#[non_exhaustive]\nmarker trait Send;", "marker trait");
}

#[test]
fn non_exhaustive_slice1_rejected_on_trait_alias() {
    assert_non_exhaustive_rejected_kind("#[non_exhaustive]\ntrait Eq2 = Eq;", "trait alias");
}

#[test]
fn non_exhaustive_slice1_rejected_on_impl_block() {
    assert_non_exhaustive_rejected_kind(
        "pub struct Foo { x: i64, }\n#[non_exhaustive]\nimpl Foo { fn x(ref self) -> i64 { 0 } }",
        "impl block",
    );
}

#[test]
fn non_exhaustive_slice1_rejected_on_struct_field() {
    // Field-level non-exhaustive is post-v1; reject so the silent
    // ignore doesn't masquerade as acceptance.
    assert_non_exhaustive_rejected_kind(
        "pub struct Foo { #[non_exhaustive] x: i64, }",
        "struct field",
    );
}

#[test]
fn non_exhaustive_slice1_rejection_uses_dedicated_error_kind() {
    // Pin that the diagnostic uses the new ResolveErrorKind variant
    // (not the catch-all "compiler builtin reserved" or similar) so
    // CLI / IDE consumers can map it to the typed `E0239` code.
    let errs = resolve_errors("#[non_exhaustive]\nfn frob() { }");
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::NonExhaustiveInvalidTarget),
        "expected NonExhaustiveInvalidTarget kind; got: {:?}",
        errs.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::NonExhaustiveInvalidTarget
                && e.message.contains("E_NON_EXHAUSTIVE_INVALID_TARGET")
        }),
        "expected error message to include the symbolic code; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── #[track_caller] placement validation (E0240) ──────────────────
// The attribute is valid only on `fn` declarations. The resolver
// rejects every other top-level / impl-block target — struct, enum,
// trait, marker trait, trait alias, impl block, struct field. Per
// design.md § Error Handling > "Stdlib panic-emitters report the
// caller's source location". Trait method declarations are *also*
// legal targets per the spec (last-writer-wins propagation to impls),
// but `TraitMethod` has no `attributes` field yet — that's a separate
// enabling change tracked alongside the slice 4–5 codegen work.

fn assert_track_caller_rejected_kind(source: &str, expected_target_kind: &str) {
    let errs = resolve_errors(source);
    let matched: Vec<_> = errs
        .iter()
        .filter(|e| {
            e.kind == ResolveErrorKind::TrackCallerInvalidTarget
                && e.message.contains(expected_target_kind)
        })
        .collect();
    assert!(
        !matched.is_empty(),
        "expected TrackCallerInvalidTarget mentioning {:?}, got: {:?}",
        expected_target_kind,
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn track_caller_slice1_accepted_on_function() {
    // Positive pin — no diagnostic on the legal placement.
    let parsed = parse("#[track_caller]\nfn unwrap_inner() { }");
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter()
            .all(|e| e.kind != ResolveErrorKind::TrackCallerInvalidTarget),
        "fn should accept #[track_caller] without diagnostic; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn track_caller_slice1_accepted_on_impl_method() {
    let parsed = parse(
        "pub struct Foo { x: i64, }\n\
         impl Foo { #[track_caller] fn unwrap_x(ref self) -> i64 { self.x } }",
    );
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter()
            .all(|e| e.kind != ResolveErrorKind::TrackCallerInvalidTarget),
        "impl method should accept #[track_caller]; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn track_caller_slice1_rejected_on_struct() {
    assert_track_caller_rejected_kind("#[track_caller]\nstruct Foo { x: i64, }", "struct");
}

#[test]
fn track_caller_slice1_rejected_on_enum() {
    assert_track_caller_rejected_kind("#[track_caller]\nenum Color { Red, Green, }", "enum");
}

#[test]
fn track_caller_slice1_rejected_on_trait() {
    assert_track_caller_rejected_kind(
        "#[track_caller]\ntrait Show { fn show(ref self) -> String; }",
        "trait",
    );
}

#[test]
fn track_caller_slice1_rejected_on_marker_trait() {
    assert_track_caller_rejected_kind("#[track_caller]\nmarker trait Send;", "marker trait");
}

#[test]
fn track_caller_slice1_rejected_on_trait_alias() {
    assert_track_caller_rejected_kind("#[track_caller]\ntrait Eq2 = Eq;", "trait alias");
}

#[test]
fn track_caller_slice1_rejected_on_impl_block() {
    assert_track_caller_rejected_kind(
        "pub struct Foo { x: i64, }\n#[track_caller]\nimpl Foo { fn x(ref self) -> i64 { 0 } }",
        "impl block",
    );
}

#[test]
fn track_caller_slice1_rejected_on_struct_field() {
    assert_track_caller_rejected_kind("pub struct Foo { #[track_caller] x: i64, }", "struct field");
}

#[test]
fn track_caller_slice1_rejection_uses_dedicated_error_kind() {
    let errs = resolve_errors("#[track_caller]\nstruct Foo { x: i64, }");
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::TrackCallerInvalidTarget),
        "expected TrackCallerInvalidTarget kind; got: {:?}",
        errs.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::TrackCallerInvalidTarget
                && e.message.contains("E_TRACK_CALLER_INVALID_TARGET")
        }),
        "expected error message to include the symbolic code; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── #[gpu] placement validation (E0800), FE-1 ────────────────────
// `#[gpu]` is the GPU-subset constraint marker and is valid only on
// `fn` declarations (free fn, inherent / trait-impl method, trait
// method declaration). The resolver rejects every other top-level /
// impl-block target — struct, enum, union, trait, marker trait, trait
// alias, impl block, struct field, module const, type alias. Per
// design.md § GPU Subset Constraints. Mirrors the `#[track_caller]`
// placement suite above.

fn assert_gpu_rejected_kind(source: &str, expected_target_kind: &str) {
    let errs = resolve_errors(source);
    let matched: Vec<_> = errs
        .iter()
        .filter(|e| {
            e.kind == ResolveErrorKind::GpuInvalidTarget && e.message.contains(expected_target_kind)
        })
        .collect();
    assert!(
        !matched.is_empty(),
        "expected GpuInvalidTarget mentioning {:?}, got: {:?}",
        expected_target_kind,
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn gpu_fe1_accepted_on_function() {
    // Positive pin — no placement diagnostic on the legal site.
    let parsed = parse("#[gpu]\nfn dot() -> i64 { 0 }");
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter()
            .all(|e| e.kind != ResolveErrorKind::GpuInvalidTarget),
        "fn should accept #[gpu] without placement diagnostic; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn gpu_fe1_accepted_on_impl_method() {
    let parsed = parse(
        "pub struct Foo { x: i64, }\n\
         impl Foo { #[gpu] fn double(ref self) -> i64 { self.x } }",
    );
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter()
            .all(|e| e.kind != ResolveErrorKind::GpuInvalidTarget),
        "impl method should accept #[gpu]; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn gpu_fe1_rejected_on_struct() {
    assert_gpu_rejected_kind("#[gpu]\nstruct Foo { x: i64, }", "struct");
}

#[test]
fn gpu_fe1_rejected_on_enum() {
    assert_gpu_rejected_kind("#[gpu]\nenum Color { Red, Green, }", "enum");
}

#[test]
fn gpu_fe1_rejected_on_trait() {
    assert_gpu_rejected_kind(
        "#[gpu]\ntrait Show { fn show(ref self) -> String; }",
        "trait",
    );
}

#[test]
fn gpu_fe1_rejected_on_marker_trait() {
    assert_gpu_rejected_kind("#[gpu]\nmarker trait Send;", "marker trait");
}

#[test]
fn gpu_fe1_rejected_on_trait_alias() {
    assert_gpu_rejected_kind("#[gpu]\ntrait Eq2 = Eq;", "trait alias");
}

#[test]
fn gpu_fe1_rejected_on_impl_block() {
    assert_gpu_rejected_kind(
        "pub struct Foo { x: i64, }\n#[gpu]\nimpl Foo { fn x(ref self) -> i64 { 0 } }",
        "impl block",
    );
}

#[test]
fn gpu_fe1_rejected_on_struct_field() {
    assert_gpu_rejected_kind("pub struct Foo { #[gpu] x: i64, }", "struct field");
}

#[test]
fn gpu_fe1_rejected_on_type_alias() {
    assert_gpu_rejected_kind("#[gpu]\ntype Id = i64;", "type alias");
}

#[test]
fn gpu_fe1_rejection_uses_dedicated_error_kind_and_code() {
    let errs = resolve_errors("#[gpu]\nstruct Foo { x: i64, }");
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::GpuInvalidTarget
                && e.message.contains("E_GPU_INVALID_TARGET")
        }),
        "expected GpuInvalidTarget with symbolic code; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── #[profile(...)] slices 1+2 — resolver validation + placement ──

#[test]
fn profile_slice12_accepted_on_function_with_known_profile() {
    let parsed = parse("#[profile(embedded)]\nfn f() { }");
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        !errs.iter().any(|e| matches!(
            e.kind,
            ResolveErrorKind::UnknownProfile | ResolveErrorKind::ProfileInvalidTarget
        )),
        "did not expect profile diagnostics; got: {errs:?}",
    );
}

#[test]
fn profile_slice12_accepted_on_function_with_all_three_known() {
    let parsed = parse("#[profile(default, embedded, kernel)]\nfn f() { }");
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        !errs.iter().any(|e| matches!(
            e.kind,
            ResolveErrorKind::UnknownProfile | ResolveErrorKind::ProfileInvalidTarget
        )),
        "did not expect profile diagnostics; got: {errs:?}",
    );
}

#[test]
fn profile_slice12_rejects_unknown_profile_name() {
    let errs = resolve_errors("#[profile(yolo)]\nfn f() { }");
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::UnknownProfile
                && e.message.contains("E_UNKNOWN_PROFILE")
                && e.message.contains("`yolo`")
        }),
        "Expected E_UNKNOWN_PROFILE naming `yolo`; got: {errs:?}",
    );
}

#[test]
fn profile_slice12_unknown_diagnostic_lists_valid_profiles() {
    let errs = resolve_errors("#[profile(yolo)]\nfn f() { }");
    let msg = errs
        .iter()
        .find(|e| e.kind == ResolveErrorKind::UnknownProfile)
        .map(|e| e.message.clone())
        .expect("expected UnknownProfile");
    assert!(msg.contains("default"));
    assert!(msg.contains("embedded"));
    assert!(msg.contains("kernel"));
}

#[test]
fn profile_slice12_rejects_one_unknown_among_known() {
    // Mixed list: `embedded` accepted, `yolo` rejected. Pin per-name
    // checking — one bad name doesn't suppress the good ones.
    let errs = resolve_errors("#[profile(embedded, yolo, kernel)]\nfn f() { }");
    let unknowns = errs
        .iter()
        .filter(|e| e.kind == ResolveErrorKind::UnknownProfile)
        .count();
    assert_eq!(
        unknowns, 1,
        "Expected exactly one UnknownProfile; got: {errs:?}"
    );
}

#[test]
fn profile_slice12_rejected_on_struct() {
    let errs = resolve_errors("#[profile(embedded)]\nstruct S { x: i64 }");
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::ProfileInvalidTarget
                && e.message.contains("struct")),
        "Expected ProfileInvalidTarget on struct; got: {errs:?}",
    );
}

#[test]
fn profile_slice12_rejected_on_enum() {
    let errs = resolve_errors("#[profile(embedded)]\nenum E { A, B }");
    assert!(
        errs.iter().any(|e| e.kind == ResolveErrorKind::ProfileInvalidTarget
            && e.message.contains("enum")),
        "Expected ProfileInvalidTarget on enum; got: {errs:?}",
    );
}

#[test]
fn profile_slice12_rejected_on_trait() {
    let errs = resolve_errors("#[profile(embedded)]\ntrait T { fn m(self) -> i64; }");
    assert!(
        errs.iter().any(
            |e| e.kind == ResolveErrorKind::ProfileInvalidTarget && e.message.contains("trait")
        ),
        "Expected ProfileInvalidTarget on trait; got: {errs:?}",
    );
}

#[test]
fn profile_slice12_rejected_on_impl_block() {
    let errs = resolve_errors(
        "struct S { x: i64 }\n\
         #[profile(embedded)]\n\
         impl S { fn m(self) -> i64 { 0 } }",
    );
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::ProfileInvalidTarget
                && e.message.contains("impl block")),
        "Expected ProfileInvalidTarget on impl block; got: {errs:?}",
    );
}

#[test]
fn profile_slice12_rejected_on_module_const() {
    let errs = resolve_errors("#[profile(embedded)]\nconst MAX_X: i64 = 1;");
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::ProfileInvalidTarget
                && e.message.contains("module const")),
        "Expected ProfileInvalidTarget on module const; got: {errs:?}",
    );
}

#[test]
fn profile_slice12_rejected_on_type_alias() {
    let errs = resolve_errors("#[profile(embedded)]\ntype Alias = i64;");
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::ProfileInvalidTarget
                && e.message.contains("type alias")),
        "Expected ProfileInvalidTarget on type alias; got: {errs:?}",
    );
}

#[test]
fn profile_slice12_rejected_on_struct_field() {
    let errs = resolve_errors(
        "struct S {\n\
         #[profile(embedded)]\n\
         x: i64,\n\
         }",
    );
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::ProfileInvalidTarget
                && e.message.contains("struct field")),
        "Expected ProfileInvalidTarget on struct field; got: {errs:?}",
    );
}

#[test]
fn profile_slice12_rejection_uses_dedicated_error_kinds() {
    // Pin that both new error kinds round-trip through the resolver
    // (regression guard against accidental kind overload).
    let off_target = resolve_errors("#[profile(embedded)]\nstruct S { x: i64 }");
    assert!(off_target
        .iter()
        .any(|e| e.kind == ResolveErrorKind::ProfileInvalidTarget));

    let unknown = resolve_errors("#[profile(yolo)]\nfn f() { }");
    assert!(unknown
        .iter()
        .any(|e| e.kind == ResolveErrorKind::UnknownProfile));
}

// ── #[deprecated] placement validation (E0241 / E0242) ────────────
// Per design.md § `#[deprecated]` for Item Deprecation > "Where it
// cannot appear", impl blocks (`E_DEPRECATED_ON_IMPL`) and struct
// fields (`E_DEPRECATED_ON_FIELD`) are explicit rejection sites.
// Every other attribute-bearing item kind accepts the attribute —
// the parser captures the payload, the resolver doesn't fire.

#[test]
fn deprecated_slice3_accepted_on_function() {
    let parsed = parse("#[deprecated]\nfn old() { }");
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter().all(|e| !matches!(
            e.kind,
            ResolveErrorKind::DeprecatedOnImpl | ResolveErrorKind::DeprecatedOnField
        )),
        "fn should accept #[deprecated] without diagnostic; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn deprecated_slice3_rejected_on_impl_block() {
    let errs = resolve_errors(
        "pub struct Foo { x: i64, }\n#[deprecated]\nimpl Foo { fn x(ref self) -> i64 { 0 } }",
    );
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::DeprecatedOnImpl),
        "Expected DeprecatedOnImpl kind; got: {:?}",
        errs.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::DeprecatedOnImpl
                && e.message.contains("E_DEPRECATED_ON_IMPL")
        }),
        "Expected E_DEPRECATED_ON_IMPL symbolic code in message; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn deprecated_slice3_rejected_on_struct_field() {
    let errs = resolve_errors("pub struct Foo { #[deprecated] x: i64, }");
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::DeprecatedOnField),
        "Expected DeprecatedOnField kind; got: {:?}",
        errs.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::DeprecatedOnField
                && e.message.contains("E_DEPRECATED_ON_FIELD")
        }),
        "Expected E_DEPRECATED_ON_FIELD symbolic code; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn deprecated_slice3_accepted_on_struct_enum_trait() {
    // All three are legal placements. No rejection of any kind from
    // the deprecated-validation paths.
    let parsed = parse(
        "#[deprecated]\npub struct OldShape { x: i64, }\n\
         #[deprecated]\npub enum OldErr { Bad, }\n\
         #[deprecated]\npub trait OldFmt { fn fmt(ref self); }",
    );
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter().all(|e| !matches!(
            e.kind,
            ResolveErrorKind::DeprecatedOnImpl | ResolveErrorKind::DeprecatedOnField
        )),
        "Legal placements should not produce DeprecatedOnImpl/Field; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── Slice 3b — Deprecation payload recorded on the symbol table ──
//
// The parser captures `#[deprecated]` on each attribute-bearing item
// kind into `<Item>.deprecation: Option<Deprecation>` (slice 1+2+3a
// plus the AST enabling change for variants / trait methods /
// const-decls / type-aliases). Slice 3b records that payload against
// the freshly-defined symbol's id in
// `SymbolTable::deprecations: HashMap<SymbolId, Deprecation>`, so
// slice 4's use-site lint emission can consult it without
// re-walking the AST. The lookup helper is
// `SymbolTable::deprecation_for(symbol_id)`.

fn lookup_deprecation<'a>(
    result: &'a karac::resolver::ResolveResult,
    name: &str,
) -> Option<&'a karac::ast::Deprecation> {
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), name)?;
    result.symbol_table.deprecation_for(sym.id)
}

#[test]
fn deprecated_slice3b_records_payload_on_function() {
    let result = resolve_ok("#[deprecated]\nfn old() { }");
    let dep = lookup_deprecation(&result, "old").expect("deprecation should be recorded on `old`");
    assert!(dep.since.is_none(), "bare form leaves `since` None");
    assert!(dep.note.is_none(), "bare form leaves `note` None");
}

#[test]
fn deprecated_slice3b_preserves_since_and_note() {
    let result = resolve_ok(
        "#[deprecated(since: \"1.2.0\", note: \"use `read_to_string` instead\")]\n\
         pub fn old_reader() { }",
    );
    let dep = lookup_deprecation(&result, "old_reader").expect("recorded");
    assert_eq!(dep.since.as_deref(), Some("1.2.0"));
    assert_eq!(dep.note.as_deref(), Some("use `read_to_string` instead"));
}

#[test]
fn deprecated_slice3b_shorthand_populates_note() {
    let result = resolve_ok("#[deprecated = \"replaced by `v2`\"]\npub fn old_api() { }");
    let dep = lookup_deprecation(&result, "old_api").expect("recorded");
    assert!(dep.since.is_none());
    assert_eq!(dep.note.as_deref(), Some("replaced by `v2`"));
}

#[test]
fn deprecated_slice3b_undecorated_symbol_has_no_entry() {
    let result = resolve_ok("fn fresh() { }");
    let sym = result
        .symbol_table
        .lookup_in_scope(ScopeId(0), "fresh")
        .unwrap();
    assert!(
        result.symbol_table.deprecation_for(sym.id).is_none(),
        "undecorated fn must not appear in the deprecations sidecar"
    );
}

#[test]
fn deprecated_slice3b_per_item_kind_coverage() {
    // One entry per attribute-bearing item kind that the spec lists
    // as a legal `#[deprecated]` target. Each gets a distinct note
    // string so a misrouting (one item's payload landing on another
    // item's id) would surface as a mismatched assertion.
    let result = resolve_ok(
        "#[deprecated = \"fn-note\"]\nfn old_fn() { }\n\
         #[deprecated = \"struct-note\"]\npub struct OldShape { x: i64, }\n\
         #[deprecated = \"enum-note\"]\npub enum OldErr { Bad, }\n\
         #[deprecated = \"trait-note\"]\npub trait OldFmt { fn fmt(ref self); }\n\
         #[deprecated = \"const-note\"]\npub const OLD_LIMIT: i64 = 0;\n\
         #[deprecated = \"alias-note\"]\npub type OldAlias = i64;\n\
         #[deprecated = \"marker-note\"]\npub marker trait OldMarker;\n",
    );
    assert_eq!(
        lookup_deprecation(&result, "old_fn")
            .unwrap()
            .note
            .as_deref(),
        Some("fn-note")
    );
    assert_eq!(
        lookup_deprecation(&result, "OldShape")
            .unwrap()
            .note
            .as_deref(),
        Some("struct-note")
    );
    assert_eq!(
        lookup_deprecation(&result, "OldErr")
            .unwrap()
            .note
            .as_deref(),
        Some("enum-note")
    );
    assert_eq!(
        lookup_deprecation(&result, "OldFmt")
            .unwrap()
            .note
            .as_deref(),
        Some("trait-note")
    );
    assert_eq!(
        lookup_deprecation(&result, "OLD_LIMIT")
            .unwrap()
            .note
            .as_deref(),
        Some("const-note")
    );
    assert_eq!(
        lookup_deprecation(&result, "OldAlias")
            .unwrap()
            .note
            .as_deref(),
        Some("alias-note")
    );
    assert_eq!(
        lookup_deprecation(&result, "OldMarker")
            .unwrap()
            .note
            .as_deref(),
        Some("marker-note")
    );
}

#[test]
fn deprecated_slice3b_records_on_enum_variant() {
    // Variants get their own SymbolId (registered in global scope
    // beside their parent enum). Slice 3b records the
    // variant-level payload there, not on the parent enum's id —
    // misrouting would surface as `Bad` having no entry or the
    // parent enum holding the note.
    let result = resolve_ok(
        "pub enum Op {\n\
         #[deprecated = \"variant-note\"]\nBad,\nGood,\n\
         }",
    );
    let dep = lookup_deprecation(&result, "Bad").expect("variant payload recorded");
    assert_eq!(dep.note.as_deref(), Some("variant-note"));
    // The parent enum carries no top-level `#[deprecated]` — its own
    // payload entry must be absent.
    assert!(
        lookup_deprecation(&result, "Op").is_none(),
        "parent enum without top-level deprecation must not inherit \
         the variant's payload"
    );
    // The sibling variant `Good` (no attribute) must also be absent.
    assert!(
        lookup_deprecation(&result, "Good").is_none(),
        "undecorated sibling variant must not appear in the sidecar"
    );
}

#[test]
fn deprecated_slice3b_records_on_impl_method() {
    // Impl-block methods are pushed directly onto the symbol table
    // (bypassing `table.define` because they live in
    // `type_methods`, not the global scope). Slice 3b records
    // their deprecation against the per-method SymbolId. Look-up
    // walks `type_methods["S"]` and grabs the first matching name.
    let result = resolve_ok(
        "pub struct S { x: i64, }\n\
         impl S {\n\
             #[deprecated = \"method-note\"]\nfn old_m(ref self) -> i64 { 0 }\n\
             fn fresh_m(ref self) -> i64 { 0 }\n\
         }",
    );
    let methods = result
        .symbol_table
        .type_methods
        .get("S")
        .expect("methods registered for S");
    let old_id = methods
        .iter()
        .copied()
        .find(|id| result.symbol_table.get_symbol(*id).name == "old_m")
        .expect("old_m method symbol");
    let fresh_id = methods
        .iter()
        .copied()
        .find(|id| result.symbol_table.get_symbol(*id).name == "fresh_m")
        .expect("fresh_m method symbol");
    let dep = result
        .symbol_table
        .deprecation_for(old_id)
        .expect("impl-method payload recorded");
    assert_eq!(dep.note.as_deref(), Some("method-note"));
    assert!(
        result.symbol_table.deprecation_for(fresh_id).is_none(),
        "sibling undecorated method must not appear in the sidecar"
    );
}

#[test]
fn deprecated_slice3b_lookup_returns_distinct_payloads_for_two_items() {
    // Two deprecated symbols must hold distinct payloads — pins
    // that the sidecar is keyed by id, not by name or position.
    let result = resolve_ok(
        "#[deprecated = \"first\"]\npub fn one() { }\n\
         #[deprecated = \"second\"]\npub fn two() { }\n",
    );
    let d1 = lookup_deprecation(&result, "one").unwrap();
    let d2 = lookup_deprecation(&result, "two").unwrap();
    assert_eq!(d1.note.as_deref(), Some("first"));
    assert_eq!(d2.note.as_deref(), Some("second"));
    // Pointer identity would be a stronger pin but the helper
    // returns `&Deprecation` from two distinct HashMap entries
    // already — the string compare is the right granularity.
}

// ── TraitMethod + Variant attribute placement validation ─────────
//
// With attribute support landed on `TraitMethod` and `Variant`, the
// resolver extends the per-site rejection helpers to cover the new
// surface:
//   - `#[track_caller]` on enum variants → rejected (variants
//     aren't fns)
//   - `#[non_exhaustive]` on enum variants → rejected (type-level
//     only per spec)
//   - `#[non_exhaustive]` on trait methods → rejected (type-level
//     only)
//   - `#[track_caller]` on trait methods → ACCEPTED (propagates to
//     impls per spec)
//   - `#[deprecated]` on enum variants → ACCEPTED (spec allows
//     variant-level deprecation)
//   - `#[deprecated]` on trait methods → ACCEPTED (spec allows
//     trait-method-level deprecation)

#[test]
fn enabling_change_track_caller_on_enum_variant_rejected() {
    let errs = resolve_errors("pub enum E { #[track_caller] V, }");
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::TrackCallerInvalidTarget
                && e.message.contains("enum variant")
        }),
        "Expected TrackCallerInvalidTarget on enum variant; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn enabling_change_non_exhaustive_on_enum_variant_rejected() {
    let errs = resolve_errors("pub enum E { #[non_exhaustive] V, }");
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::NonExhaustiveInvalidTarget
                && e.message.contains("enum variant")
        }),
        "Expected NonExhaustiveInvalidTarget on enum variant; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn enabling_change_non_exhaustive_on_trait_method_rejected() {
    let errs = resolve_errors("pub trait Show { #[non_exhaustive] fn show(ref self) -> String; }");
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::NonExhaustiveInvalidTarget
                && e.message.contains("trait method")
        }),
        "Expected NonExhaustiveInvalidTarget on trait method; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn enabling_change_track_caller_on_trait_method_accepted() {
    let parsed = parse("pub trait Show { #[track_caller] fn show(ref self) -> String; }");
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter()
            .all(|e| e.kind != ResolveErrorKind::TrackCallerInvalidTarget),
        "#[track_caller] on trait method should be accepted; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn enabling_change_deprecated_on_enum_variant_accepted() {
    let parsed = parse("pub enum E { #[deprecated] Old, New, }");
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter().all(|e| !matches!(
            e.kind,
            ResolveErrorKind::DeprecatedOnImpl | ResolveErrorKind::DeprecatedOnField
        )),
        "#[deprecated] on enum variant should be accepted; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn enabling_change_deprecated_on_trait_method_accepted() {
    let parsed = parse("pub trait Show { #[deprecated] fn show(ref self) -> String; }");
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter().all(|e| !matches!(
            e.kind,
            ResolveErrorKind::DeprecatedOnImpl | ResolveErrorKind::DeprecatedOnField
        )),
        "#[deprecated] on trait method should be accepted; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── TypeAliasDef + ConstDecl attribute placement validation ──────
//
// With attributes now landing on ConstDecl and TypeAliasDef:
//   - `#[deprecated]` → ACCEPTED (spec lists both as legal targets)
//   - `#[track_caller]` → rejected (not fns)
//   - `#[non_exhaustive]` → rejected (not pub struct / pub enum)

#[test]
fn const_attrs_track_caller_on_module_const_rejected() {
    let errs = resolve_errors("#[track_caller]\npub const VAL: i64 = 42;");
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::TrackCallerInvalidTarget
                && e.message.contains("module const")
        }),
        "Expected TrackCallerInvalidTarget on module const; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn const_attrs_non_exhaustive_on_module_const_rejected() {
    let errs = resolve_errors("#[non_exhaustive]\npub const VAL: i64 = 42;");
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::NonExhaustiveInvalidTarget
                && e.message.contains("module const")
        }),
        "Expected NonExhaustiveInvalidTarget on module const; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn const_attrs_track_caller_on_type_alias_rejected() {
    let errs = resolve_errors("#[track_caller]\npub type Handle = i64;");
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::TrackCallerInvalidTarget && e.message.contains("type alias")
        }),
        "Expected TrackCallerInvalidTarget on type alias; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn const_attrs_non_exhaustive_on_type_alias_rejected() {
    let errs = resolve_errors("#[non_exhaustive]\npub type Handle = i64;");
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::NonExhaustiveInvalidTarget
                && e.message.contains("type alias")
        }),
        "Expected NonExhaustiveInvalidTarget on type alias; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn const_attrs_deprecated_on_module_const_accepted() {
    let parsed = parse("#[deprecated]\npub const OLD: i64 = 42;");
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter().all(|e| !matches!(
            e.kind,
            ResolveErrorKind::DeprecatedOnImpl | ResolveErrorKind::DeprecatedOnField
        )),
        "#[deprecated] on module const should be accepted; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn const_attrs_deprecated_on_type_alias_accepted() {
    let parsed = parse("#[deprecated]\npub type OldHandle = i64;");
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter().all(|e| !matches!(
            e.kind,
            ResolveErrorKind::DeprecatedOnImpl | ResolveErrorKind::DeprecatedOnField
        )),
        "#[deprecated] on type alias should be accepted; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── Slice / array patterns (phase 5.2 sub-item 1) ─────────────────────────

#[test]
fn test_slice_pattern_binds_prefix_suffix_and_rest_names() {
    // The resolver should define `a`, `b`, `mid`, `c` from the slice pattern
    // so that the arm body can reference them without "undefined name" errors.
    // (The typechecker stub still rejects the program semantically — covered
    // in tests/typechecker.rs — but resolver scope binding is independent.)
    let parsed = parse(
        "fn f(xs: Vec[i64]) -> i64 { \
         match xs { \
         [a, b, ..mid, c] => a + b + c, \
         _ => 0, \
         } \
         }",
    );
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    let result = resolve(&parsed.program);
    assert!(
        result.errors.is_empty(),
        "expected no resolve errors, got: {:?}",
        result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_slice_pattern_unbound_rest_does_not_introduce_name() {
    // `..` (ignored rest) does not introduce a binding — referencing `rest`
    // in the body should produce an unresolved-name error.
    let errs = resolve_errors(
        "fn f(xs: Vec[i64]) -> i64 { \
         match xs { \
         [.., last] => rest + last, \
         _ => 0, \
         } \
         }",
    );
    assert!(
        errs.iter().any(|e| e.message.contains("rest")),
        "expected unresolved 'rest' diagnostic, got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── GAT slice 3: resolver wiring for assoc-type generic params ──
// `type Mapped[U]` (trait side) and `type Mapped[U] = Vec[U]` (impl
// side) open a fresh generic scope. Inside that scope, the GAT's
// parameters are bound so the bound expression, where-clause, and
// (impl side) RHS type resolve against them. Outside the assoc-type
// node, the params are not visible — sibling methods that try to
// reference them produce an unresolved-name diagnostic.

#[test]
fn gat_slice3_trait_assoc_type_generic_param_registered_as_typeparam() {
    let result = resolve_ok("trait Functor { type Mapped[U]; }");
    let u_kind = result
        .symbol_table
        .all_symbols()
        .iter()
        .find(|s| s.name == "U")
        .map(|s| &s.kind)
        .expect("expected a symbol named 'U' from the GAT parameter list");
    assert!(
        matches!(u_kind, SymbolKind::TypeParam),
        "expected SymbolKind::TypeParam for GAT param 'U', got {:?}",
        u_kind
    );
}

#[test]
fn gat_slice3_trait_assoc_type_non_generic_form_registers_no_param() {
    // Regression pin: `type Item;` is the legacy non-generic shape.
    // It must NOT register any extra symbol — the resolver's scope
    // push runs only when `generic_params` is `Some(...)`.
    let result = resolve_ok("trait It { type Item; }");
    let extra = result
        .symbol_table
        .all_symbols()
        .iter()
        .find(|s| s.name == "Item" && matches!(s.kind, SymbolKind::TypeParam));
    assert!(
        extra.is_none(),
        "non-generic assoc type should not register Item as a TypeParam symbol"
    );
}

#[test]
fn gat_slice3_trait_assoc_type_param_not_visible_in_sibling_method() {
    // The GAT param `U` lives only inside the assoc-type declaration's
    // own scope. A sibling method that references `U` in its return
    // type must see an undefined-type diagnostic.
    let errs = resolve_errors(
        "trait Functor { \
         type Mapped[U]; \
         fn other(self) -> U; \
         }",
    );
    assert!(
        errs.iter().any(|e| e.message.contains("U")),
        "expected unresolved 'U' diagnostic in sibling method, got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn gat_slice3_impl_assoc_type_binding_rhs_resolves_param() {
    // Headline: the impl-side `type Mapped[U] = Vec[U]` binding must
    // see `U` when resolving the RHS `Vec[U]`. With slice 3 wiring,
    // this passes with zero resolve errors.
    resolve_ok(
        "trait Functor { type Mapped[U]; } \
         struct Wrapper[T] { value: T } \
         impl[T] Functor for Wrapper[T] { type Mapped[U] = Wrapper[U]; }",
    );
}

#[test]
fn gat_slice3_impl_assoc_type_binding_rhs_errors_on_undefined_param() {
    // Negative pin: a name that's neither the GAT param nor an
    // enclosing trait/impl param produces an unresolved-name
    // diagnostic. Confirms the new scope doesn't accidentally swallow
    // all names.
    let errs = resolve_errors(
        "trait Functor { type Mapped[U]; } \
         struct Wrapper[T] { value: T } \
         impl[T] Functor for Wrapper[T] { type Mapped[U] = Wrapper[Q]; }",
    );
    assert!(
        errs.iter().any(|e| e.message.contains("Q")),
        "expected unresolved 'Q' diagnostic in GAT binding RHS, got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn gat_slice3_impl_assoc_type_binding_param_not_visible_in_sibling_method() {
    // The impl-side GAT param `U` is scoped to the assoc-type binding
    // only. A sibling method that references `U` in its return type
    // must see an undefined-type diagnostic — the scope was popped
    // before sibling-item resolution.
    let errs = resolve_errors(
        "trait Functor { type Mapped[U]; fn other(self) -> i64; } \
         struct Wrapper[T] { value: T } \
         impl[T] Functor for Wrapper[T] { \
         type Mapped[U] = Wrapper[U]; \
         fn other(self) -> U { self.value } \
         }",
    );
    assert!(
        errs.iter().any(|e| e.message.contains("U")),
        "expected unresolved 'U' diagnostic in sibling impl method, got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn gat_slice3_trait_assoc_type_with_where_clause_resolves() {
    // The where-clause on a GAT declaration must see the GAT's own
    // generic params. `where U: Clone` references `U` — slice 3
    // ensures `U` is in scope when the where-clause runs through
    // `resolve_where_clause`.
    resolve_ok(
        "trait Clone { fn clone(self) -> Self; } \
         trait Functor { type Mapped[U] where U: Clone; }",
    );
}

#[test]
fn gat_slice3_impl_assoc_type_binding_with_where_clause_resolves() {
    // Impl-side mirror: `where U: Clone` on the binding sees the
    // binding's own `U`.
    resolve_ok(
        "trait Clone { fn clone(self) -> Self; } \
         trait Functor { type Mapped[U]; } \
         struct Wrapper[T] { value: T } \
         impl[T] Functor for Wrapper[T] { \
         type Mapped[U] = Wrapper[U] where U: Clone; \
         }",
    );
}

// ── `impl Trait` slice 2: resolver desugar (argument-position) ──

/// Helper: run [`karac::desugar_program`] over the parsed program and then
/// resolve it. Asserts both parse and resolve are clean. Returns the
/// `(post-desugar program, resolve result)` pair so the per-test
/// assertions can introspect the rewritten AST.
fn desugar_and_resolve_ok(source: &str) -> (karac::ast::Program, ResolveResult) {
    let mut parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    desugar_program(&mut parsed.program);
    let result = resolve(&parsed.program);
    assert!(
        result.errors.is_empty(),
        "Resolve errors: {}",
        result
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    (parsed.program, result)
}

#[test]
fn impl_trait_slice2_argument_position_desugars_to_synthetic_generic_param() {
    // `fn f(x: impl Display)` desugars to
    // `fn f[T_impl_arg_0: Display](x: T_impl_arg_0)`. The argument-
    // position `impl Trait` is replaced with a `TypeKind::Path`
    // reference to the freshly synthesized anonymous generic param,
    // and the param is appended to `f.generic_params` carrying the
    // trait as its bound. The post-desugar resolver registers the
    // synthetic param like any other generic and routes the param's
    // type lookup through it without complaint.
    let (program, _resolved) = desugar_and_resolve_ok(
        "trait Display { fn show(ref self) -> String; } fn f(x: impl Display) {}",
    );
    let karac::ast::Item::Function(f) = &program.items[1] else {
        panic!("Expected Function at items[1]");
    };
    let gp = f.generic_params.as_ref().expect("expected generic_params");
    assert_eq!(gp.params.len(), 1);
    let synth = &gp.params[0];
    assert_eq!(synth.name, "T_impl_arg_0");
    assert_eq!(synth.bounds.len(), 1);
    assert_eq!(synth.bounds[0].path, vec!["Display".to_string()]);
    let param_ty = &f.params[0].ty;
    let karac::ast::TypeKind::Path(path) = &param_ty.kind else {
        panic!(
            "Expected param ty to be Path after desugar; got {:?}",
            param_ty.kind
        );
    };
    assert_eq!(path.segments, vec!["T_impl_arg_0".to_string()]);
}

#[test]
fn impl_trait_slice2_pair_args_produce_two_distinct_synthetic_params() {
    // `fn pair(x: impl Display, y: impl Display)` desugars to
    // `fn pair[T_impl_arg_0: Display, T_impl_arg_1: Display](x: T_impl_arg_0, y: T_impl_arg_1)`.
    // The two `impl Display` parameters get distinct synthetic names
    // — verifying the per-occurrence desugar rule. Without this the
    // typechecker would unify the two args at every call site, which
    // is the wrong semantics for argument-position `impl Trait`.
    let (program, _resolved) = desugar_and_resolve_ok(
        "trait Display { fn show(ref self) -> String; } \
         fn pair(x: impl Display, y: impl Display) {}",
    );
    let karac::ast::Item::Function(f) = &program.items[1] else {
        panic!("Expected Function at items[1]");
    };
    let gp = f.generic_params.as_ref().expect("expected generic_params");
    assert_eq!(gp.params.len(), 2);
    assert_eq!(gp.params[0].name, "T_impl_arg_0");
    assert_eq!(gp.params[1].name, "T_impl_arg_1");
    let karac::ast::TypeKind::Path(p0) = &f.params[0].ty.kind else {
        panic!("Expected first param ty to be Path");
    };
    let karac::ast::TypeKind::Path(p1) = &f.params[1].ty.kind else {
        panic!("Expected second param ty to be Path");
    };
    assert_eq!(p0.segments, vec!["T_impl_arg_0".to_string()]);
    assert_eq!(p1.segments, vec!["T_impl_arg_1".to_string()]);
}

#[test]
fn impl_trait_slice2_explicit_generic_form_continues_to_work_alongside_impl_trait() {
    // Pre-existing explicit `[T: Display]` form keeps working — the
    // synthetic params from `impl Trait` arguments append to whatever
    // the user already declared rather than replacing it. Here
    // `fn mixed[T: Display](x: T, y: impl Display)` desugars to a
    // two-param generics list: the user's `T` followed by the
    // synthetic `T_impl_arg_0`.
    let (program, _resolved) = desugar_and_resolve_ok(
        "trait Display { fn show(ref self) -> String; } \
         fn mixed[T: Display](x: T, y: impl Display) {}",
    );
    let karac::ast::Item::Function(f) = &program.items[1] else {
        panic!("Expected Function at items[1]");
    };
    let gp = f.generic_params.as_ref().expect("expected generic_params");
    assert_eq!(gp.params.len(), 2);
    assert_eq!(gp.params[0].name, "T");
    assert_eq!(gp.params[1].name, "T_impl_arg_0");
}

#[test]
fn impl_trait_slice2_return_position_kept_as_impl_trait() {
    // Return-position `impl Trait` is slice 3's job — slice 2 only
    // touches argument-position. The desugar must leave
    // `fn iter() -> impl Iterator` with its `TypeKind::ImplTrait`
    // return type intact so the slice-3 typechecker pipeline sees
    // the unchanged sugar.
    let (program, _resolved) = desugar_and_resolve_ok(
        "trait Iterator { fn next(mut ref self); } \
         fn iter() -> impl Iterator { todo() }",
    );
    let karac::ast::Item::Function(f) = &program.items[1] else {
        panic!("Expected Function at items[1]");
    };
    assert!(
        f.generic_params.is_none(),
        "return-position `impl Trait` must not add synthetic generic params"
    );
    let ret = f.return_type.as_ref().expect("expected return type");
    assert!(
        matches!(ret.kind, karac::ast::TypeKind::ImplTrait { .. }),
        "return type must remain TypeKind::ImplTrait after desugar; got {:?}",
        ret.kind
    );
}

#[test]
fn impl_trait_slice2_impl_method_argument_position_desugars() {
    // Inherent-impl methods are `Function`s carried inside
    // `ImplItem::Method`; the desugar runs over them as well so
    // `impl Display { fn echo(ref self, x: impl Display) {} }`
    // gets the same per-method synthetic generic param treatment as
    // free functions.
    let (program, _resolved) = desugar_and_resolve_ok(
        "trait Display { fn show(ref self) -> String; } \
         struct Thing { v: i64 } \
         impl Thing { fn echo(ref self, x: impl Display) {} }",
    );
    let karac::ast::Item::ImplBlock(imp) = &program.items[2] else {
        panic!("Expected ImplBlock at items[2]");
    };
    let karac::ast::ImplItem::Method(method) = &imp.items[0] else {
        panic!("Expected ImplItem::Method at imp.items[0]");
    };
    let gp = method
        .generic_params
        .as_ref()
        .expect("expected method generic_params");
    assert_eq!(gp.params.len(), 1);
    assert_eq!(gp.params[0].name, "T_impl_arg_0");
    assert_eq!(gp.params[0].bounds[0].path, vec!["Display".to_string()]);
}

#[test]
fn impl_trait_slice2_synthetic_param_bounds_recorded_in_symbol_table() {
    // The post-desugar resolver treats the synthetic param like any
    // other declared generic — defining it in the function scope and
    // recording its trait bounds via `record_generic_bounds`. This
    // pins that pipeline (slice 3's typechecker dispatches trait
    // methods on synthetic params via these bound records).
    let (_program, resolved) = desugar_and_resolve_ok(
        "trait Display { fn show(ref self) -> String; } fn f(x: impl Display) {}",
    );
    let sym = resolved
        .symbol_table
        .all_symbols()
        .iter()
        .find(|s| s.name == "T_impl_arg_0")
        .expect("synthetic param symbol must be registered");
    assert!(matches!(sym.kind, SymbolKind::TypeParam));
    let bounds: &[TraitBound] = resolved.symbol_table.get_generic_bounds(sym.id);
    assert_eq!(bounds.len(), 1);
    assert_eq!(bounds[0].path, vec!["Display".to_string()]);
}

// ── `#[diagnostic::*]` slice 2 — namespace dispatch + E_UNKNOWN_ATTRIBUTE ──
// Validates the central attribute checker shipped in
// `src/attribute_validator.rs`. Bare-name attributes that are not in the
// recognised set produce `error[E_UNKNOWN_ATTRIBUTE]` (`E0243`); members
// of a compiler-reserved namespace (`#[diagnostic::*]`) and tool
// namespaces are silently accepted at this layer (per-namespace
// validation lives in slices 3, 4 / item 37).

fn assert_unknown_attribute(source: &str, name: &str) {
    let errs = resolve_errors(source);
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::UnknownAttribute
                && e.message.contains("E_UNKNOWN_ATTRIBUTE")
                && e.message.contains(name)
        }),
        "expected E_UNKNOWN_ATTRIBUTE for `{name}`; got: {:?}",
        errs.iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

fn assert_no_unknown_attribute(source: &str) {
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        errs.iter()
            .all(|e| e.kind != ResolveErrorKind::UnknownAttribute),
        "unexpected E_UNKNOWN_ATTRIBUTE; got: {:?}",
        errs.iter()
            .filter(|e| e.kind == ResolveErrorKind::UnknownAttribute)
            .map(|e| &e.message)
            .collect::<Vec<_>>()
    );
}

#[test]
fn attr_slice2_unknown_bare_attribute_on_function_rejected() {
    assert_unknown_attribute("#[no_such_thing]\nfn f() { }", "no_such_thing");
}

#[test]
fn attr_slice2_unknown_bare_attribute_with_args_rejected() {
    // Argument shape is irrelevant — the rejection is on the path's
    // first segment alone. Pins that a parenthesised unknown attribute
    // also fires E_UNKNOWN_ATTRIBUTE.
    assert_unknown_attribute(
        "#[polish_my_error_pretty(level: 9)]\nfn f() { }",
        "polish_my_error_pretty",
    );
}

#[test]
fn attr_slice2_unknown_bare_attribute_on_struct_rejected() {
    assert_unknown_attribute(
        "#[invent_a_layout]\npub struct Foo { x: i64, }",
        "invent_a_layout",
    );
}

#[test]
fn attr_slice2_unknown_bare_attribute_on_struct_field_rejected() {
    assert_unknown_attribute(
        "pub struct Foo { #[invent_a_layout] x: i64, }",
        "invent_a_layout",
    );
}

#[test]
fn attr_slice2_unknown_bare_attribute_on_enum_variant_rejected() {
    assert_unknown_attribute(
        "pub enum Color { #[invent_a_layout] Red, Green, }",
        "invent_a_layout",
    );
}

#[test]
fn attr_slice2_unknown_bare_attribute_on_trait_method_rejected() {
    assert_unknown_attribute(
        "trait Show { #[polish_my_error_pretty] fn show(ref self) -> String; }",
        "polish_my_error_pretty",
    );
}

#[test]
fn attr_slice2_unknown_bare_attribute_on_impl_method_rejected() {
    assert_unknown_attribute(
        "pub struct Foo { x: i64, }\nimpl Foo { #[polish_my_error_pretty] fn x(ref self) -> i64 { self.x } }",
        "polish_my_error_pretty",
    );
}

#[test]
fn attr_slice2_recognised_bare_attributes_accepted() {
    // Spot-check across families — lint level, item annotation, FFI,
    // testing — that the recognised list does not flag any of them.
    // Each on its own item to keep failure messages crisp.
    assert_no_unknown_attribute("#[allow(deprecated)]\nfn a() { }");
    assert_no_unknown_attribute("#[deprecated]\nfn b() { }");
    assert_no_unknown_attribute("#[derive(Eq)]\npub struct C { x: i64, }");
    assert_no_unknown_attribute("#[must_use]\nfn d() -> i64 { 0 }");
    assert_no_unknown_attribute("#[non_exhaustive]\npub struct E { x: i64, }");
    assert_no_unknown_attribute("#[track_caller]\nfn f() { }");
    assert_no_unknown_attribute(
        "#[kara_name = \"GLXFBConfig\"]\nunsafe extern \"C\" { fn g() -> i64; }",
    );
}

#[test]
fn attr_slice2_profile_attribute_recognised() {
    // `#[profile(...)]` is a fully-wired attribute (parser scan →
    // `Function.profile_compat` → effect-checker enforcement in
    // `effectchecker/profile_compat.rs`) but was missing from
    // `RECOGNIZED_BARE_ATTRIBUTES`, so every use also emitted a bogus
    // E_UNKNOWN_ATTRIBUTE alongside the real profile diagnostics.
    // B-2026-07-02-32.
    assert_no_unknown_attribute("#[profile(embedded)]\nfn f() { }");
    assert_no_unknown_attribute("#[profile(embedded, kernel)]\nfn g() { }");
}

#[test]
fn attr_slice2_diagnostic_namespaced_unknown_member_accepted_silently() {
    // The headline rule of the compiler-reserved namespace — even a
    // member the compiler has never heard of is silently accepted
    // (per design.md § Diagnostic Namespace Attributes "advisory
    // contract").
    assert_no_unknown_attribute(
        "#[diagnostic::polish_my_error_pretty]\ntrait Show { fn show(ref self) -> String; }",
    );
}

#[test]
fn attr_slice2_diagnostic_namespaced_known_member_accepted_silently() {
    // The two v1 members of the namespace — pins that slice 2 does
    // not error on the ones slices 3 / 4 will validate; the
    // per-member shape checks live with those slices.
    assert_no_unknown_attribute(
        "#[diagnostic::on_unimplemented(message: \"x\")]\ntrait Show { fn show(ref self) -> String; }",
    );
    assert_no_unknown_attribute(
        "pub struct Foo { x: i64, }\n#[diagnostic::do_not_recommend]\nimpl Foo { fn x(ref self) -> i64 { self.x } }",
    );
}

#[test]
fn attr_slice2_tool_namespaced_attribute_accepted_silently() {
    // Tool-namespaced attributes (item 37 — `#[TOOL::NAME(...)]`) are
    // already silently accepted at the slice-2 layer because the
    // walker only validates bare-name paths; item 37 will formalise
    // the rule + add `karac query attributes`. Pinning here so the
    // existing surface does not regress when item 37 lands.
    assert_no_unknown_attribute("#[karafmt::skip]\nfn manually_aligned() { }");
    assert_no_unknown_attribute(
        "#[acmecorp_security::audit_required(level: \"strict\")]\npub fn login() { }",
    );
}

#[test]
fn attr_slice2_unknown_attribute_has_dedicated_error_kind_and_e0243() {
    // Pin the discriminant + the symbolic prefix so CLI / IDE
    // consumers can branch reliably. The E0243 mapping is asserted
    // separately in tests/cli.rs (the cli-side mapping table); here
    // we only pin the kind and the message prefix.
    let errs = resolve_errors("#[no_such_thing]\nfn f() { }");
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::UnknownAttribute),
        "expected UnknownAttribute kind; got: {:?}",
        errs.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::UnknownAttribute
                && e.message.contains("E_UNKNOWN_ATTRIBUTE")
        }),
        "expected message to include symbolic code; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn attr_slice2_unknown_attribute_message_suggests_namespaced_form() {
    // The error message offers the namespaced alternatives
    // (`#[diagnostic::NAME]` or `#[your_tool::NAME]`) so the author
    // can pivot to a silently-accepted form when the bare name was a
    // tool hint. Pin the suggestion so future message tweaks keep the
    // recovery hint.
    let errs = resolve_errors("#[no_such_thing]\nfn f() { }");
    let msg = errs
        .iter()
        .find(|e| e.kind == ResolveErrorKind::UnknownAttribute)
        .map(|e| e.message.clone())
        .expect("expected an UnknownAttribute diagnostic");
    assert!(
        msg.contains("#[diagnostic::no_such_thing]"),
        "expected diagnostic suggestion; got: {msg}"
    );
    assert!(
        msg.contains("#[your_tool::no_such_thing]"),
        "expected tool-namespace suggestion; got: {msg}"
    );
}

// ── Phase-8 stdlib-floor § Compiler queries channel sub-item 1 ──

#[test]
fn def_paths_top_level_function_single_segment() {
    let result = resolve_ok("fn sort_inplace() {}\nfn main() {}\n");
    use karac::def_path::DefPath;
    assert_eq!(
        result.def_paths.get("sort_inplace"),
        Some(&DefPath::item("sort_inplace")),
    );
    assert_eq!(result.def_paths.get("main"), Some(&DefPath::item("main")),);
}

#[test]
fn def_paths_impl_method_uses_target_then_method_segments() {
    let result = resolve_ok(
        "struct Point { x: i64 }\n\
         impl Point { fn new() -> Point { Point { x: 0 } } }\n\
         fn main() {}\n",
    );
    use karac::def_path::DefPath;
    // Impl methods key on the qualified `Type.method` name in the
    // resolver index, with `DefPath` segments [target, method] so the
    // human-readable form is `Point::new` (matches the path syntax
    // users see in error messages and `karac query` output).
    let dp = result
        .def_paths
        .get("Point.new")
        .expect("expected DefPath for Point.new");
    assert_eq!(
        dp,
        &DefPath::new(vec!["Point".to_string(), "new".to_string()]),
    );
    assert_eq!(dp.render(), "Point::new");
}

#[test]
fn def_paths_stable_under_unrelated_insertion() {
    // The motivating property: SpanKey-based identity invalidates
    // every downstream key when a line is inserted at the top.
    // DefPath-based identity must NOT.
    let v1 = resolve_ok("fn alpha() {}\nfn beta() {}\nfn main() {}\n");
    let v2 = resolve_ok(
        "// unrelated leading comment\n\
         const SEED_VALUE: i64 = 1;\n\
         fn alpha() {}\n\
         fn beta() {}\n\
         fn main() {}\n",
    );
    assert_eq!(v1.def_paths.get("alpha"), v2.def_paths.get("alpha"));
    assert_eq!(v1.def_paths.get("beta"), v2.def_paths.get("beta"));
    assert_eq!(v1.def_paths.get("main"), v2.def_paths.get("main"));
}

#[test]
fn def_paths_stable_under_unrelated_item_rename() {
    let v1 = resolve_ok("fn alpha() {}\nfn beta() {}\n");
    let v2 = resolve_ok("fn renamed_alpha() {}\nfn beta() {}\n");
    // `beta` survives the rename of `alpha` to `renamed_alpha`.
    assert_eq!(v1.def_paths.get("beta"), v2.def_paths.get("beta"));
    // `alpha` is gone in v2; `renamed_alpha` is now keyed.
    assert!(!v2.def_paths.contains_key("alpha"));
    assert!(v2.def_paths.contains_key("renamed_alpha"));
}

#[test]
fn query_resolving_attribute_propagates_through_pipeline_without_diagnostic() {
    // Phase-8 stdlib-floor § Compiler queries channel sub-item 4 —
    // regression that a future query-resolving attribute (the v1-
    // reserved `#[specialize(...)]` is the canonical placeholder)
    // carrying nested args parses, attaches to the item, and survives
    // resolve + typecheck + ownership without any diagnostic. The
    // surface is the lever that lets P1.x catalogue entries ship
    // resolution attributes without parser changes.
    use karac::{ownershipcheck, typecheck};

    let src = "#[specialize(T = i64, mode: \"x\")]\nfn make() {}\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors should be absent for a v1-reserved query-resolving attribute; got: {:?}",
        parsed.errors,
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "Resolve errors should be absent — `specialize` is in the v1 reserved registry; got: {:?}",
        resolved
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>(),
    );
    let typed = typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "Type errors should be absent; got: {:?}",
        typed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>(),
    );
    let ownership = ownershipcheck(&parsed.program, &typed);
    assert!(
        ownership.errors.is_empty(),
        "Ownership errors should be absent; got: {:?}",
        ownership
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>(),
    );

    // The attribute must be attached intact, with args preserved.
    use karac::ast::{ExprKind, Item};
    let make_fn = parsed
        .program
        .items
        .iter()
        .find_map(|i| match i {
            Item::Function(f) if f.name == "make" => Some(f),
            _ => None,
        })
        .expect("expected fn `make`");
    let attr = make_fn
        .attributes
        .iter()
        .find(|a| a.path == ["specialize".to_string()])
        .expect("expected #[specialize(...)] on `make`");
    assert_eq!(attr.args.len(), 2, "expected two attr args: {attr:?}");
    let t_arg = &attr.args[0];
    assert_eq!(t_arg.name.as_deref(), Some("T"));
    let t_val = t_arg.value.as_ref().expect("expected value for T").clone();
    // `T = i64` lowers to an Identifier expression — the parser
    // doesn't know about types here; only that an identifier value
    // followed the `=`.
    match t_val.kind {
        ExprKind::Identifier(ref n) => assert_eq!(n, "i64"),
        other => panic!("expected Identifier(\"i64\") for T arg; got {other:?}"),
    }
    let mode_arg = &attr.args[1];
    assert_eq!(mode_arg.name.as_deref(), Some("mode"));
    let mode_val = mode_arg
        .value
        .as_ref()
        .expect("expected value for mode")
        .clone();
    match mode_val.kind {
        ExprKind::StringLit(ref s) => assert_eq!(s, "x"),
        other => panic!("expected StringLit(\"x\") for mode arg; got {other:?}"),
    }
}

#[test]
fn inline_and_cold_on_same_fn_is_legal() {
    // design.md § Codegen Hint Attributes makes `#[cold]` + `#[inline]`
    // an explicitly legal combination ("definitely cold, but small
    // enough to suggest inlining"). The codegen-hint semantics replace
    // the old reserved `cold`↔`inline` query-conflict entry, so neither
    // the query-conflict diagnostic nor any codegen-hint placement error
    // may fire here.
    let result = resolve_ok("#[inline]\n#[cold]\nfn hot() {}\n");
    assert!(
        !result.errors.iter().any(|e| matches!(
            e.kind,
            ResolveErrorKind::QueryResolutionConflict
                | ResolveErrorKind::CodegenHintInvalidTarget
                | ResolveErrorKind::CodegenHintOnExternDecl
        )),
        "#[inline] + #[cold] must resolve cleanly; got: {:?}",
        result.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn query_resolution_conflict_no_rc_and_prefer_rc_on_same_fn() {
    // Phase-8 stdlib-floor § Compiler queries channel sub-item 5.
    // `#[no_rc]` (existing) + `#[prefer_rc]` (v1-reserved) resolve
    // the RC-fallback query (P1.1) in conflicting ways.
    let errors = resolve_errors("#[no_rc]\n#[prefer_rc]\nfn never_rcs() {}\n");
    let conflict = errors
        .iter()
        .find(|e| matches!(e.kind, ResolveErrorKind::QueryResolutionConflict))
        .expect("expected E_QUERY_RESOLUTION_CONFLICT for #[no_rc] + #[prefer_rc]");
    assert!(
        conflict.message.contains("`#[no_rc]`") && conflict.message.contains("`#[prefer_rc]`"),
        "diagnostic should name both attributes; got: {}",
        conflict.message,
    );
}

#[test]
fn query_resolution_conflict_does_not_fire_on_single_attribute() {
    // Sanity — having just one half of a conflict pair must NOT
    // emit the diagnostic. `#[inline]` alone is legitimate (a
    // future P1.3 query-resolution attribute); `#[cold]` alone
    // likewise. The conflict only exists when both appear together.
    let result = resolve_ok("#[inline]\nfn hot() {}\n");
    assert!(
        !result
            .errors
            .iter()
            .any(|e| matches!(e.kind, ResolveErrorKind::QueryResolutionConflict)),
        "lone #[inline] must not trigger the conflict diagnostic",
    );
    let result = resolve_ok("#[cold]\nfn rare() {}\n");
    assert!(
        !result
            .errors
            .iter()
            .any(|e| matches!(e.kind, ResolveErrorKind::QueryResolutionConflict)),
        "lone #[cold] must not trigger the conflict diagnostic",
    );
}

// ── FFI unions (line 549) ────────────────────────────────────────
//
// Resolver-time registration: a `union NAME { ... }` declaration
// surfaces in the type namespace under `SymbolKind::Union { ... }`
// so downstream type-expression resolution finds the name (e.g.
// `extern "C" { fn f(u: *mut FloatBits); }` resolves `FloatBits`).

#[test]
fn union_registers_in_type_namespace() {
    let result = resolve_ok("#[repr(C)]\nunion FloatBits {\n    f: f32,\n    bits: u32,\n}");
    let sym = result
        .symbol_table
        .lookup_in_scope(ScopeId(0), "FloatBits")
        .expect("FloatBits should be registered in the type namespace");
    match &sym.kind {
        SymbolKind::Union { field_names } => {
            assert_eq!(field_names, &["f".to_string(), "bits".to_string()]);
        }
        other => panic!("expected SymbolKind::Union, got {:?}", other),
    }
}

#[test]
fn union_field_types_resolve_in_module_scope() {
    // i32 / f32 must resolve; if union registration broke field-type
    // resolution we would see an unresolved-type error.
    resolve_ok(
        "#[repr(C)]\nunion FloatBits {\n    f: f32,\n    bits: u32,\n}\n\
         fn read_bits(u: *const FloatBits) -> i32 { 0 }",
    );
}

// ── FFI unions slice 3b — E_UNION_NON_EXHAUSTIVE_FORBIDDEN ──────
//
// `#[non_exhaustive]` on a union gets a focused, union-specific
// diagnostic code distinct from the generic
// `E_NON_EXHAUSTIVE_INVALID_TARGET` because the reason it's
// meaningless is union-specific: unions are an FFI boundary shape
// whose field list is determined by the C side, not a versioned
// Kāra-owned aggregate that can be extended in a backwards-compatible
// way like `pub struct` / `pub enum` can.

#[test]
fn union_non_exhaustive_uses_focused_code() {
    let errs = resolve_errors(
        "#[non_exhaustive]\n\
         #[repr(C)]\n\
         union FloatBits { f: f32, bits: u32 }",
    );
    let diag = errs
        .iter()
        .find(|e| e.kind == ResolveErrorKind::UnionNonExhaustiveForbidden)
        .unwrap_or_else(|| {
            panic!(
                "expected UnionNonExhaustiveForbidden, got kinds: {:?}",
                errs.iter().map(|e| &e.kind).collect::<Vec<_>>()
            )
        });
    assert!(
        diag.message.contains("E_UNION_NON_EXHAUSTIVE_FORBIDDEN"),
        "diagnostic should carry the focused code in the message body, got: {}",
        diag.message,
    );
    assert!(
        diag.message.contains("FloatBits"),
        "diagnostic should name the union, got: {}",
        diag.message,
    );
    assert!(
        diag.message.contains("FFI"),
        "diagnostic body should explain the FFI-shape reasoning, got: {}",
        diag.message,
    );
    // Slice 3b's focused kind replaces the generic placement kind —
    // emitting both would route through two distinct CLI codes and
    // confuse IDE consumers. Pin the absence so a future refactor
    // doesn't accidentally double-fire.
    assert!(
        !errs
            .iter()
            .any(|e| e.kind == ResolveErrorKind::NonExhaustiveInvalidTarget
                && e.message.contains("union")),
        "generic NonExhaustiveInvalidTarget should not also fire for the same union, got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn union_non_exhaustive_fires_regardless_of_pub() {
    // `#[non_exhaustive]` on a `pub union` is just as meaningless —
    // the attribute presupposes a versioned-aggregate surface that
    // FFI shapes do not have, irrespective of visibility. The focused
    // code fires for `pub union` as it does for the bare form.
    let errs = resolve_errors(
        "#[non_exhaustive]\n\
         #[repr(C)]\n\
         pub union FloatBits { f: f32, bits: u32 }",
    );
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::UnionNonExhaustiveForbidden),
        "expected UnionNonExhaustiveForbidden on `pub union`, got: {:?}",
        errs.iter().map(|e| &e.kind).collect::<Vec<_>>(),
    );
}

#[test]
fn union_field_non_exhaustive_still_uses_generic_kind() {
    // The focused code is type-level only — field-level
    // `#[non_exhaustive]` inside a union body continues to route
    // through the generic helper (the field surface is the same
    // post-v1 deferred case across struct / union / enum, so the
    // diagnostic stays uniform there).
    let errs = resolve_errors(
        "#[repr(C)]\n\
         union FloatBits { #[non_exhaustive] f: f32, bits: u32 }",
    );
    assert!(
        errs.iter().any(|e| {
            e.kind == ResolveErrorKind::NonExhaustiveInvalidTarget
                && e.message.contains("union field")
        }),
        "expected generic NonExhaustiveInvalidTarget for the field, got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
    assert!(
        !errs
            .iter()
            .any(|e| e.kind == ResolveErrorKind::UnionNonExhaustiveForbidden),
        "focused kind should not fire on a field-level attribute, got: {:?}",
        errs.iter().map(|e| &e.kind).collect::<Vec<_>>(),
    );
}

// ── FFI unions slice 4 — layout-block-on-union rejection ────────
//
// A union's bytes are a single alternation slot; the SoA / grouping /
// cache-locality reasoning that motivates layout blocks doesn't apply,
// and the C-side ABI we're locked to leaves nothing for the layout
// surface to influence. The resolver emits per-item diagnostics so a
// user pasting a multi-group block sees every offending item, not
// just the first.

#[test]
fn layout_block_on_union_is_rejected() {
    let errs = resolve_errors(
        r#"
#[repr(C)]
union FloatBits { f: f32, bits: u32 }
layout u: Vec[FloatBits] {
    group bits { bits }
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.message.contains("union 'FloatBits'")
                && e.message.contains("FFI alternation")),
        "expected union-layout rejection, got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn layout_block_on_union_fires_per_item() {
    // Multi-group layout on a union: the diagnostic must fire once per
    // group so the user lands on every site to remove, not just the
    // first.
    let errs = resolve_errors(
        r#"
#[repr(C)]
union BitsLR { l: u32, r: u32 }
layout u: Vec[BitsLR] {
    group a { l }
    group b { r }
}
"#,
    );
    let count = errs
        .iter()
        .filter(|e| e.message.contains("union 'BitsLR'"))
        .count();
    assert_eq!(
        count,
        2,
        "expected two per-item rejections, got {} ({:?})",
        count,
        errs.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn layout_block_on_struct_unaffected_by_union_arm() {
    // Regression: the union arm sits ahead of the struct fallthrough;
    // adding it must not stop the well-formed struct-layout case from
    // resolving cleanly.
    resolve_ok(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
"#,
    );
}

// ── Signature-from-call-site stub diagnostic (phase-5-diagnostics line 633) ─

#[test]
fn unresolved_call_in_production_file_has_no_stub_hint() {
    // Activation gate: production-file (`is_test_file=false`)
    // unresolved-call sites keep the plain `UndefinedName` diagnostic —
    // no stub hint attached. Mirrors design.md § Signature-from-Call-Site
    // Stub: stubs are a TDD-loop affordance, not a production-code one.
    let errs = resolve_errors(
        r#"
fn main() {
    add(1, 2);
}
"#,
    );
    let undefined = errs
        .iter()
        .find(|e| matches!(e.kind, ResolveErrorKind::UndefinedName) && e.message.contains("'add'"))
        .expect("expected UndefinedName for 'add'");
    assert!(
        undefined.stub_hint.is_none(),
        "production-file unresolved call must not carry stub_hint, got: {:?}",
        undefined.stub_hint
    );
}

#[test]
fn unresolved_call_in_test_file_carries_stub_hint() {
    // Test-file (`is_test_file=true`) unresolved-call sites enrich the
    // `UndefinedName` diagnostic with a `StubHint` carrying one entry
    // per argument. Slice 2: unsuffixed integer literals infer to `i64`
    // (Kāra's default integer type), so `add(2, 3)` produces two i64
    // arguments. Return type stays `None` — resolver-time inference
    // does not yet read enclosing comparison context.
    let result = resolve_as_test_file(
        r#"
fn test_add_two_and_three() {
    let _ = add(2, 3);
}
"#,
    );
    let undefined = result
        .errors
        .iter()
        .find(|e| matches!(e.kind, ResolveErrorKind::UndefinedName) && e.message.contains("'add'"))
        .expect("expected UndefinedName for 'add'");
    let stub = undefined
        .stub_hint
        .as_ref()
        .expect("test-file unresolved call must carry stub_hint");
    assert_eq!(stub.callee_name, "add");
    assert_eq!(stub.args.len(), 2);
    assert_eq!(stub.args[0].inferred_type.as_deref(), Some("i64"));
    assert_eq!(stub.args[1].inferred_type.as_deref(), Some("i64"));
    assert!(stub.return_type.is_none());
}

#[test]
fn stub_hint_render_source_emits_compiling_skeleton() {
    // The rendered stub is what the CLI emits as the `hints[].diff.new`
    // field — must be syntactically a valid Kāra fn item with a
    // `todo()` body per the design.md compiling-skeleton convention.
    let hint = StubHint {
        callee_name: "add".to_string(),
        args: vec![
            StubArg {
                inferred_type: None,
            },
            StubArg {
                inferred_type: None,
            },
        ],
        return_type: None,
    };
    assert_eq!(
        hint.render_source(),
        "fn add(arg0: _, arg1: _) -> _ {\n    todo()\n}\n"
    );
}

#[test]
fn stub_hint_render_source_zero_args() {
    // Zero-arg call (`init()`) renders an empty parameter list. Pins
    // the format string against accidental ", " injection.
    let hint = StubHint {
        callee_name: "init".to_string(),
        args: vec![],
        return_type: None,
    };
    assert_eq!(hint.render_source(), "fn init() -> _ {\n    todo()\n}\n");
}

#[test]
fn stub_hint_infers_suffixed_int_literals() {
    // Suffixed integer literals carry their type verbatim — i8, u32,
    // u128 round-trip through the suffix name into the stub signature.
    let result = resolve_as_test_file(
        r#"
fn test_with_suffixed_ints() {
    let _ = stash(1i8, 200u32, 999u128);
}
"#,
    );
    let stub = result
        .errors
        .iter()
        .find_map(|e| e.stub_hint.as_ref())
        .expect("expected stub_hint");
    assert_eq!(
        stub.args
            .iter()
            .map(|a| a.inferred_type.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("i8"), Some("u32"), Some("u128")],
    );
}

#[test]
fn stub_hint_infers_bool_char_string_float_literals() {
    // Bool / char / string / float literals each route to their
    // canonical wire-form type. Unsuffixed float defaults to f64
    // (mirrors `infer_operand_target_ty` / type_display).
    let result = resolve_as_test_file(
        r#"
fn test_mixed_literals() {
    let _ = grab(true, 'x', "hello", 3.14);
}
"#,
    );
    let stub = result
        .errors
        .iter()
        .find_map(|e| e.stub_hint.as_ref())
        .expect("expected stub_hint");
    assert_eq!(
        stub.args
            .iter()
            .map(|a| a.inferred_type.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("bool"), Some("char"), Some("String"), Some("f64")],
    );
}

#[test]
fn stub_hint_non_literal_args_fall_back_to_placeholder() {
    // Identifier arguments depend on type-checker state; the resolver
    // leaves their stub-slot as `_`. Pins the slice-2 fallback path
    // against accidentally over-inferring.
    let result = resolve_as_test_file(
        r#"
fn test_with_identifier_arg() {
    let x = 5;
    let _ = consume(x);
}
"#,
    );
    let stub = result
        .errors
        .iter()
        .find_map(|e| e.stub_hint.as_ref())
        .expect("expected stub_hint");
    assert_eq!(stub.args.len(), 1);
    assert!(stub.args[0].inferred_type.is_none());
}

#[test]
fn stub_hint_render_source_with_typed_args() {
    // Slice 2 inference flows through to the rendered stub — concrete
    // types appear in the parameter list where inferred, `_` elsewhere.
    let hint = StubHint {
        callee_name: "mix".to_string(),
        args: vec![
            StubArg {
                inferred_type: Some("i64".to_string()),
            },
            StubArg {
                inferred_type: None,
            },
            StubArg {
                inferred_type: Some("bool".to_string()),
            },
        ],
        return_type: None,
    };
    assert_eq!(
        hint.render_source(),
        "fn mix(arg0: i64, arg1: _, arg2: bool) -> _ {\n    todo()\n}\n"
    );
}

#[test]
fn test_module_binding_resolves_at_module_scope() {
    // Slice 3 of design.md § Module-Level Bindings: a valid
    // Const-class module binding registers in the symbol table
    // and produces no resolver-layer diagnostics. The
    // typechecker layer remains unwired until slice 5; its
    // diagnostic surface is exercised through `tests/typechecker.rs`,
    // not here.
    let result = resolve_ok("let MIN_FLOOR: i64 = 1;");
    let sym = result.symbol_table.lookup_in_scope(ScopeId(0), "MIN_FLOOR");
    assert!(
        sym.is_some(),
        "module binding `MIN_FLOOR` should register in the global scope",
    );
    assert!(matches!(sym.unwrap().kind, SymbolKind::Constant));
}

#[test]
fn test_module_binding_mut_resolves_at_module_scope() {
    // `let mut` shape uses the same Const-class namespace as
    // `let`; the mutability bit lives on the AST item, not the
    // symbol kind. Slice 5 (typechecker) reads it back from the
    // AST when enforcing assignment-LHS mutability.
    let result = resolve_ok("let mut COUNTER: i64 = 0;");
    assert!(result
        .symbol_table
        .lookup_in_scope(ScopeId(0), "COUNTER")
        .is_some());
}

#[test]
fn test_module_binding_lowercase_name_rejected() {
    // Slice 3's Const-class naming check. Since script mode (phase-8 Q7),
    // a BARE lowercase top-level `let` is a script statement (it feeds the
    // synthesized `fn main()`), so the module-binding naming check is
    // exercised through a spelling that still routes to `ModuleBinding`:
    // `pub let` (visibility unambiguously means a module binding was
    // intended, so the script-let carve-out does not apply).
    let errors = resolve_errors("pub let max_retries: i32 = 3;");
    assert!(
        errors.iter().any(|e| {
            e.message.contains("E_MODULE_BINDING_NAMING")
                && e.message.contains("max_retries")
                && e.message.contains("MAX_RETRIES")
        }),
        "expected naming diagnostic with rename suggestion, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_module_binding_naming_carries_machine_replacement() {
    // B-2026-07-06-3: the Const-class naming diagnostic computes the exact
    // SCREAMING_SNAKE candidate — wire it as a machine-applicable
    // `.replacement` spanning the name identifier only, so `karac fix` can
    // apply it (previously emitted as prose in `suggestion` only).
    // `pub let` keeps the binding on the ModuleBinding path under script
    // mode (see test_module_binding_lowercase_name_rejected).
    let src = "pub let maxRetries: i32 = 3;";
    let errors = resolve_errors(src);
    let err = errors
        .iter()
        .find(|e| e.message.contains("E_MODULE_BINDING_NAMING"))
        .expect("expected naming diagnostic");
    let edit = err
        .replacement
        .as_ref()
        .expect("naming diagnostic must carry a machine-applicable replacement");
    assert_eq!(edit.replacement, "MAX_RETRIES");
    // The edit must span exactly the `maxRetries` identifier, not the whole
    // `let … = …;` statement — applying it in place yields the rename.
    let applied = {
        let mut s = src.to_string();
        s.replace_range(edit.offset..edit.offset + edit.length, &edit.replacement);
        s
    };
    assert_eq!(applied, "pub let MAX_RETRIES: i32 = 3;");
}

#[test]
fn test_undefined_label_suggests_nearby_label() {
    // B-2026-07-06-3: a misspelled `continue` label fuzzy-matches the loop
    // labels in scope and reports a `did you mean` suggestion.
    // B-2026-07-07-3: that suggestion is now machine-applicable — the label
    // identifier's span (`label_span`) anchors a `.replacement`.
    let src = "fn main() { outer: loop { loop { continue otuer; } } }";
    let errors = resolve_errors(src);
    let err = errors
        .iter()
        .find(|e| e.kind == ResolveErrorKind::UndefinedLabel)
        .expect("expected UndefinedLabel diagnostic");
    assert_eq!(err.suggestion.as_deref(), Some("outer"));
    assert!(
        err.message.contains("did you mean `outer`?"),
        "expected did-you-mean prose, got: {}",
        err.message
    );
    // The machine edit must span exactly the `otuer` label token.
    let edit = err
        .replacement
        .as_ref()
        .expect("label suggestion must carry a machine-applicable replacement");
    assert_eq!(edit.replacement, "outer");
    assert_eq!(&src[edit.offset..edit.offset + edit.length], "otuer");
}

#[test]
fn test_undefined_label_no_suggestion_when_nothing_close() {
    // No nearby label → no suggestion, no spurious `did you mean`.
    let errors = resolve_errors("fn main() { loop { continue zzzzzzzz; } }");
    let err = errors
        .iter()
        .find(|e| e.kind == ResolveErrorKind::UndefinedLabel)
        .expect("expected UndefinedLabel diagnostic");
    assert!(err.suggestion.is_none());
    assert!(!err.message.contains("did you mean"));
}

#[test]
fn test_duplicate_module_binding_rejected() {
    // Same-name module binding declared twice at module scope is
    // rejected with `E_DUPLICATE_MODULE_BINDING` per the tracker
    // and design.md § Module-Level Bindings.
    let errors = resolve_errors("let MAX: i64 = 1;\nlet MAX: i64 = 2;\n");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("E_DUPLICATE_MODULE_BINDING")),
        "expected duplicate-binding diagnostic, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_module_binding_use_site_resolves() {
    // A use site inside a function body must resolve cleanly at
    // the resolver layer — the symbol is registered in the
    // Const-class namespace. The typechecker still flags the
    // use because slice 5 hasn't wired binding-type inference;
    // that's a separate test surface.
    let result = resolve_ok("let MAX: i64 = 100;\nfn pick() -> i64 { MAX }\n");
    assert!(result
        .symbol_table
        .lookup_in_scope(ScopeId(0), "MAX")
        .is_some());
}

#[test]
fn test_file_resolves_visible_callee_without_stub_hint() {
    // Sanity / regression: a *resolved* call in a test file produces
    // no diagnostic at all — the stub-hint enrichment must fire only
    // on the unresolved path, not on every call.
    let result = resolve_as_test_file(
        r#"
fn add(a: i32, b: i32) -> i32 { a + b }
fn test_add_visible() {
    let _ = add(1, 2);
}
"#,
    );
    assert!(
        result.errors.is_empty(),
        "expected clean resolve, got: {:?}",
        result.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

// ── `#[default]` placement + arg-shape (phase-8 stdlib-floor) ────────
//
// `#[default]` is legal only on a unit enum variant under
// `#[derive(Default)]`. `validate_default_attribute` (resolver pass 1.7)
// emits the position / without-derive / malformed-args diagnostics; the
// "exactly one field-less marker" rule is a typechecker concern tested
// in tests/typechecker.rs.

#[test]
fn test_default_attr_on_struct_rejected() {
    let errors = resolve_errors(
        r#"
#[default]
struct S { x: i64 }
"#,
    );
    assert!(
        errors.iter().any(
            |e| e.kind == ResolveErrorKind::DefaultAttributeInvalidPosition
                && e.message.contains("E_DEFAULT_ATTRIBUTE_INVALID_POSITION")
        ),
        "expected E_DEFAULT_ATTRIBUTE_INVALID_POSITION, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_default_attr_on_struct_field_rejected() {
    let errors = resolve_errors(
        r#"
struct S { #[default] x: i64 }
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == ResolveErrorKind::DefaultAttributeInvalidPosition),
        "expected invalid-position on a struct field, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_default_attr_on_fn_rejected() {
    let errors = resolve_errors(
        r#"
#[default]
fn f() {}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == ResolveErrorKind::DefaultAttributeInvalidPosition),
        "expected invalid-position on a fn, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_default_attr_without_derive_rejected() {
    let errors = resolve_errors(
        r#"
enum E { #[default] A, B }
"#,
    );
    assert!(
        errors.iter().any(
            |e| e.kind == ResolveErrorKind::DefaultAttributeWithoutDerive
                && e.message.contains("E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE")
                && e.message.contains("`A`")
        ),
        "expected E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE naming variant A, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_default_attr_malformed_args_rejected() {
    let errors = resolve_errors(
        r#"
#[derive(Default)]
enum E { #[default(some)] A, B }
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == ResolveErrorKind::MalformedAttributeArgs
                && e.message.contains("E_MALFORMED_ATTRIBUTE_ARGS")),
        "expected E_MALFORMED_ATTRIBUTE_ARGS, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_default_attr_on_marked_variant_accepted() {
    // A well-formed `#[default]` on a unit variant under
    // `#[derive(Default)]` produces no resolve-phase diagnostic.
    let result = resolve_ok(
        r#"
#[derive(Default)]
enum E { #[default] A, B }
"#,
    );
    assert!(
        !result.errors.iter().any(|e| matches!(
            e.kind,
            ResolveErrorKind::DefaultAttributeInvalidPosition
                | ResolveErrorKind::DefaultAttributeWithoutDerive
                | ResolveErrorKind::MalformedAttributeArgs
                | ResolveErrorKind::UnknownAttribute
        )),
        "expected no #[default] diagnostics, got: {:?}",
        result.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

// ── Codegen hint attributes (#[inline] / #[cold]) — placement ──────
//
// design.md § Codegen Hint Attributes > "Where they may appear". The
// resolver gates placement: hints are legal on functions, impl methods,
// and trait method declarations; rejected on non-fn items
// (E_CODEGEN_HINT_INVALID_POSITION), whole impl blocks, and foreign-fn
// declarations (E_CODEGEN_HINT_ON_EXTERN_DECL). Closures are rejected
// earlier at parse (covered in tests/parser.rs).

fn has_codegen_hint_position_error(errs: &[ResolveError]) -> bool {
    errs.iter().any(|e| {
        matches!(
            e.kind,
            ResolveErrorKind::CodegenHintInvalidTarget | ResolveErrorKind::CodegenHintOnExternDecl
        )
    })
}

#[test]
fn codegen_hint_accepted_on_function() {
    let result = resolve_ok("#[inline]\n#[cold]\nfn f() {}\n");
    assert!(!has_codegen_hint_position_error(&result.errors));
}

#[test]
fn codegen_hint_accepted_on_impl_method() {
    let parsed = parse(
        "struct Foo { x: i64 }\n\
         impl Foo { #[inline(always)] fn get(ref self) -> i64 { self.x } }",
    );
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(
        !has_codegen_hint_position_error(&errs),
        "impl method should accept codegen hints; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn codegen_hint_accepted_on_trait_method_decl() {
    let parsed = parse("trait T { #[cold] fn slow(ref self); }");
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let errs = resolve(&parsed.program).errors;
    assert!(!has_codegen_hint_position_error(&errs));
}

#[test]
fn inline_rejected_on_struct() {
    let errs = resolve_errors("#[inline]\nstruct S { x: i64 }\n");
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::CodegenHintInvalidTarget
                && e.message.contains("struct")),
        "expected E_CODEGEN_HINT_INVALID_POSITION on struct; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn cold_rejected_on_enum() {
    let errs = resolve_errors("#[cold]\nenum E { A, B }\n");
    assert!(errs
        .iter()
        .any(|e| e.kind == ResolveErrorKind::CodegenHintInvalidTarget));
}

#[test]
fn inline_rejected_on_whole_impl_block() {
    let errs = resolve_errors(
        "struct Foo { x: i64 }\n\
         #[inline]\n\
         impl Foo { fn get(ref self) -> i64 { self.x } }",
    );
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::CodegenHintInvalidTarget
                && e.message.contains("impl block")),
        "expected E_CODEGEN_HINT_INVALID_POSITION on impl block; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn inline_rejected_on_extern_decl() {
    let errs = resolve_errors("unsafe extern \"C\" {\n  #[inline] fn ext_fn(x: i64) -> i64;\n}");
    assert!(
        errs.iter()
            .any(|e| e.kind == ResolveErrorKind::CodegenHintOnExternDecl),
        "expected E_CODEGEN_HINT_ON_EXTERN_DECL; got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── Codegen hint trait → impl propagation (desugar) ────────────────

fn impl_method_hint(
    prog: &karac::ast::Program,
    method: &str,
) -> (Option<karac::ast::InlineHint>, bool) {
    use karac::ast::{ImplItem, Item};
    for item in &prog.items {
        if let Item::ImplBlock(imp) = item {
            for ii in &imp.items {
                if let ImplItem::Method(m) = ii {
                    if m.name == method {
                        return (m.inline_hint, m.is_cold);
                    }
                }
            }
        }
    }
    panic!("impl method `{method}` not found");
}

#[test]
fn inline_hint_propagates_from_trait_to_impl() {
    let (prog, _) = desugar_and_resolve_ok(
        "trait Greet { #[inline] #[cold] fn hi(ref self); }\n\
         struct P { x: i64 }\n\
         impl Greet for P { fn hi(ref self) {} }",
    );
    let (hint, cold) = impl_method_hint(&prog, "hi");
    assert_eq!(hint, Some(karac::ast::InlineHint::Default));
    assert!(cold, "impl method should inherit trait's #[cold]");
}

#[test]
fn impl_override_wins_over_trait_inline_hint() {
    // The impl declares its own inline axis; it overrides the trait's
    // (last-writer-wins). The cold axis, which the impl leaves unset,
    // still inherits.
    let (prog, _) = desugar_and_resolve_ok(
        "trait Greet { #[inline] #[cold] fn hi(ref self); }\n\
         struct P { x: i64 }\n\
         impl Greet for P { #[inline(never)] fn hi(ref self) {} }",
    );
    let (hint, cold) = impl_method_hint(&prog, "hi");
    assert_eq!(hint, Some(karac::ast::InlineHint::Never));
    assert!(
        cold,
        "cold axis still inherited when impl only overrides inline axis"
    );
}

// ── B-2026-07-02-5: `par { }` sibling-branch binding reads ──
//
// Each top-level statement in an explicit `par { }` block is a concurrent
// branch with its own scope. Pre-fix the resolver treated `par` like a
// sequential block, so a branch reading a sibling branch's binding sailed
// through resolution — the interpreter then panicked (`unreachable:
// variable 'x' not found ... should be caught by resolver`) and codegen
// errored ungracefully.

#[test]
fn test_par_sibling_branch_read_rejected() {
    let errors = resolve_errors(
        "fn build() -> Vec[i64] { return [1, 2, 3]; }\n\
         fn main() {\n\
             par {\n\
                 let x = build();\n\
                 let y = build();\n\
                 println(x.len() + y.len());\n\
             }\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("sibling `par` branch")),
        "expected the cross-branch diagnostic; got {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_par_sibling_branch_assign_rejected() {
    // An assignment TARGETING a sibling branch's binding is the same
    // cross-branch access (`x = 2` is its own top-level statement, i.e.
    // its own branch).
    let errors = resolve_errors(
        "fn main() {\n\
             par {\n\
                 let mut x = 1;\n\
                 x = 2;\n\
                 println(\"done\");\n\
             }\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("sibling `par` branch")),
        "expected the cross-branch diagnostic; got {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_par_nested_par_still_sees_outer_siblings_as_illegal() {
    // A nested `par` inside branch 2 reading branch 1's binding is still a
    // cross-branch read of the OUTER par — the sibling set must survive
    // nesting.
    let errors = resolve_errors(
        "fn main() {\n\
             let t = par {\n\
                 let a = 1;\n\
                 let b = par {\n\
                     let c = a + 1;\n\
                     c\n\
                 };\n\
                 a + b\n\
             };\n\
             println(t);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("sibling `par` branch")),
        "expected the cross-branch diagnostic; got {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_par_tail_expression_sees_all_branch_bindings() {
    // The join point: the block's tail expression combines branch results
    // (design.md § Explicit Concurrency) — every branch binding is in
    // scope there, including through tuple destructuring and reads of
    // OUTER (pre-`par`) bindings inside branches.
    resolve_ok(
        "fn f() -> i64 { return 10; }\n\
         fn g() -> i64 { return 20; }\n\
         fn main() {\n\
             let base = 100;\n\
             let (a, b) = par {\n\
                 let p = f() + base;\n\
                 let q = g() + base;\n\
                 (p, q)\n\
             };\n\
             let t = par {\n\
                 let x = 1;\n\
                 let y = { let x = 10; x + 1 };\n\
                 x + y\n\
             };\n\
             println(a + b + t);\n\
         }",
    );
}

#[test]
fn test_par_branch_bindings_escape_to_enclosing_scope() {
    // B-2026-07-11-3: the join barrier hoists each branch's top-level `let`
    // into the ENCLOSING scope, so branch results are usable AFTER the
    // `par { }` block — not only in a tail expression. This is the exact
    // shape `examples/db_pipeline`'s `execute_pair` and design.md's
    // structured-concurrency model rely on. Pre-fix this errored with
    // `undefined name 'a'` after the block.
    resolve_ok(
        "fn fa() -> i64 { return 10; }\n\
         fn fb() -> i64 { return 20; }\n\
         fn main() {\n\
             par {\n\
                 let a = fa();\n\
                 let b = fb();\n\
             }\n\
             println(a + b);\n\
         }",
    );
}
