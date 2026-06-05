//! Compile-only no_std probe — see Cargo.toml for the per-target recipe.
//! `#![no_std]` is the point: this proves the rustls + ring tree builds
//! without std for the Cortex-M leg of the v1 target matrix. (On native,
//! where workspace builds unify rustls's `std` feature in, the attribute
//! still holds for this crate's own code.)
#![no_std]

pub fn provider_compiles() -> &'static str {
    let _ = rustls::crypto::ring::default_provider();
    "rustls + ring (no_std)"
}
