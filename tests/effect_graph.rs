// tests/effect_graph.rs

//! Focused tests for `src/effect_graph.rs` — the Cartographer / `karac query
//! effects|concurrency` JSON emission (project-review-2026-08-16 item-8
//! runner-up; split to `docs/spikes/structural-debt.md`).
//!
//! The module's only prior coverage was the CLI-parity pin
//! (`tests/cli.rs::test_cartograph_json_matches_cli_query_output`), which
//! proves the two surfaces AGREE but not that either is RIGHT. These tests
//! pin the envelope contracts directly on `cartograph_json`, the library
//! entry the playground WASM export wraps: envelope validity, node/edge
//! content, the 1:1 node-key join between envelopes, and the documented
//! never-panics error behavior.

use karac::effect_graph::cartograph_json;
use serde_json::Value;

fn parse_envelopes(src: &str) -> (Value, Value) {
    let result = cartograph_json(src, "test.kara");
    assert!(
        result.ok,
        "expected ok:true, diagnostics: {:?}",
        result
            .diagnostics
            .iter()
            .map(|d| (d.phase, d.message.clone()))
            .collect::<Vec<_>>()
    );
    let effects: Value =
        serde_json::from_str(&result.effects_json).expect("effects envelope is valid JSON");
    let concurrency: Value =
        serde_json::from_str(&result.concurrency_json).expect("concurrency envelope is valid JSON");
    (effects, concurrency)
}

/// Collect the node-key strings from an envelope's `functions` array.
fn node_keys(envelope: &Value) -> Vec<String> {
    envelope["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .map(|f| {
            f["function"]
                .as_str()
                .expect("function key is a string")
                .to_string()
        })
        .collect()
}

const EFFECT_CHAIN: &str = "effect resource Db;\n\
     \n\
     fn load() -> i64 reads(Db) {\n    1\n}\n\
     \n\
     fn caller() -> i64 {\n    load()\n}\n\
     \n\
     fn main() {\n    print(\"{caller()}\");\n}\n";

/// Both envelopes are valid JSON and carry one node per source function.
#[test]
fn envelopes_are_valid_json_with_one_node_per_function() {
    let (effects, concurrency) = parse_envelopes(EFFECT_CHAIN);
    let mut keys = node_keys(&effects);
    keys.sort();
    assert_eq!(keys, vec!["caller", "load", "main"]);
    // The documented invariant: node keys join 1:1 across the two envelopes.
    let mut conc_keys = node_keys(&concurrency);
    conc_keys.sort();
    assert_eq!(
        keys, conc_keys,
        "effect and concurrency node keys must join 1:1"
    );
}

/// The effect envelope carries the declared effect on `load` and propagates
/// the inferred `reads(Db)` through the call edge into `caller`.
#[test]
fn effects_propagate_through_call_edges() {
    let (effects, _) = parse_envelopes(EFFECT_CHAIN);
    let functions = effects["functions"].as_array().unwrap();
    let find = |name: &str| {
        functions
            .iter()
            .find(|f| f["function"] == name)
            .unwrap_or_else(|| panic!("node {name} present"))
    };
    let load_inferred = serde_json::to_string(&find("load")["inferred_effects"]).unwrap();
    assert!(
        load_inferred.contains("Db"),
        "load's inferred effects name the Db resource: {load_inferred}"
    );
    let caller_inferred = serde_json::to_string(&find("caller")["inferred_effects"]).unwrap();
    assert!(
        caller_inferred.contains("Db"),
        "caller inherits reads(Db) through the call edge: {caller_inferred}"
    );
    // The call graph records caller → load.
    let calls = effects["calls"].as_array().expect("calls array");
    assert!(
        calls
            .iter()
            .any(|c| c["caller"] == "caller" && c["callee"] == "load"),
        "call edge caller→load present: {calls:?}"
    );
}

/// A parse error is fatal: ok:false, a parse-phase diagnostic, and EMPTY
/// envelopes (the documented contract — no partial JSON).
#[test]
fn parse_error_is_fatal_with_empty_envelopes() {
    let result = cartograph_json("fn main( {", "broken.kara");
    assert!(!result.ok);
    assert!(result.effects_json.is_empty());
    assert!(result.concurrency_json.is_empty());
    assert!(
        result.diagnostics.iter().any(|d| d.phase == "parse"),
        "a parse diagnostic is surfaced: {:?}",
        result
            .diagnostics
            .iter()
            .map(|d| d.phase)
            .collect::<Vec<_>>()
    );
}

/// A typecheck error is NON-fatal: the graph still builds (mirroring the CLI
/// query) and the error is surfaced in diagnostics.
#[test]
fn typecheck_error_is_nonfatal() {
    let result = cartograph_json(
        "fn main() {\n    let x: i64 = \"not a number\";\n    print(\"{x}\");\n}\n",
        "test.kara",
    );
    assert!(result.ok, "typecheck errors must not kill the graph");
    assert!(
        result.diagnostics.iter().any(|d| d.phase == "typecheck"),
        "the typecheck diagnostic is surfaced"
    );
    let effects: Value = serde_json::from_str(&result.effects_json).expect("envelope still built");
    assert!(
        node_keys(&effects).contains(&"main".to_string()),
        "main's node is present despite the type error"
    );
}

/// Impl methods key as `Type.method` — the join key shared with
/// `karac query affected-by`.
#[test]
fn impl_methods_key_as_type_dot_method() {
    let (effects, _) = parse_envelopes(
        "struct Counter {\n    n: i64,\n}\n\
         \n\
         impl Counter {\n    fn bump(mut ref self) {\n        self.n += 1;\n    }\n}\n\
         \n\
         fn main() {\n    let mut c = Counter { n: 0 };\n    c.bump();\n}\n",
    );
    assert!(
        node_keys(&effects).contains(&"Counter.bump".to_string()),
        "impl method keys as Type.method; got: {:?}",
        node_keys(&effects)
    );
}
