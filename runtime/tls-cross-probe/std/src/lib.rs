//! Compile-only probe — see Cargo.toml for the per-target recipe.
//! Touching `default_provider()` forces the whole ring crypto surface
//! (incl. the C objects ring's build.rs compiles) through type-check
//! for the target; `cargo check` is the gate, no linking required.

pub fn provider_compiles() -> &'static str {
    let _ = rustls::crypto::ring::default_provider();
    "rustls + ring"
}
