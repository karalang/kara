#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = karac::cli::parse_args(&args);
    karac::cli::execute(cmd);
}

// `karac` the binary is native-only — the wasm32 build is for the
// browser playground (tracker line 703), which consumes `karac` as a
// library through the `playground/` workspace member, not as a CLI.
// The empty `main` here satisfies `[[bin]]` for wasm32 without pulling
// in the CLI surface (`std::env::args` is unavailable, and `cli` is
// cfg-gated off at lib.rs).
#[cfg(target_arch = "wasm32")]
fn main() {}
