//! Spike probe (CG-5): does `codegen`'s REAL expression lowering produce a
//! kernel body that could be re-targeted to NVPTX, or does it bake in
//! host-only machinery?
//!
//! The assessment's recommended spike asks whether `codegen` can lower a real
//! `#[gpu]` body to NVPTX. Building a second target machine is the expensive
//! way to find out. This is the cheap way: compile a `#[gpu]` kernel through
//! the ORDINARY native path and read the IR that comes out. Everything that
//! would block re-targeting is visible there —
//!
//!   * calls to `karac_*` runtime symbols (no such symbol exists on a device),
//!   * panic landing pads / `unwind` edges (no unwinder on a device),
//!   * host-pointer assumptions in the signature.
//!
//! If the body is self-contained arithmetic and control flow, re-targeting is
//! mechanical (wrapper + address spaces + `nvvm.annotations`). If it is not,
//! the finding is exactly what the spike was commissioned to produce.
//!
//! Run: `cargo run --release --features llvm --example nvptx_body_probe`

fn main() {
    let cases: &[(&str, &str)] = &[
        ("scalar map", "#[gpu]\nfn k(x: f32) -> f32 { x * 2.0 }"),
        (
            "locals",
            "#[gpu]\nfn k(x: f32) -> f32 { let t: f32 = x * 2.0; t + 1.0 }",
        ),
        (
            "while + accumulator",
            "#[gpu]\nfn k(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    let mut n: i32 = 0;\n    while n < 4 { acc = acc + x; n = n + 1; }\n    acc\n}",
        ),
        (
            "for-range",
            "#[gpu]\nfn k(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    for n in 0..4 { acc = acc + x; }\n    acc\n}",
        ),
        (
            "value match",
            "#[gpu]\nfn k(x: i32) -> i32 { match x { 0 => 10, 1 => 20, _ => 30 } }",
        ),
        (
            "statement if",
            "#[gpu]\nfn k(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    if x > 2.0 { acc = x; } else { acc = 0.0 - x; }\n    acc\n}",
        ),
        (
            "integer div (panic path?)",
            "#[gpu]\nfn k(x: i32) -> i32 { x / 2 }",
        ),
        (
            "overflow-checked add",
            "#[gpu]\nfn k(x: i32) -> i32 { x + 1 }",
        ),
    ];

    println!(
        "{:<28} {:>6} {:>8} {:>7} {:>7}  verdict",
        "kernel", "lines", "rt calls", "unwind", "alloca"
    );
    println!("{}", "-".repeat(88));

    for (name, kernel) in cases {
        // Call the kernel DIRECTLY rather than through `gpu.dispatch`. The
        // dispatch path needs the typechecker-recorded WGSL threaded through
        // lowering, and it is not what is under test: the question is what
        // `codegen` emits for the kernel BODY, and a direct call is the way to
        // make it emit exactly that and nothing else.
        let arg = if kernel.contains("-> i32") {
            "3"
        } else {
            "3.0"
        };
        let src =
            format!("{kernel}\nfn main() {{\n    let r = k({arg});\n    println(f\"{{r}}\")\n}}\n");

        match compile(&src) {
            Err(e) => println!(
                "{name:<28} {:>6} {:>8} {:>7} {:>7}  BUILD FAILED: {}",
                "-",
                "-",
                "-",
                "-",
                first_line(&e)
            ),
            Ok(ir) => {
                let body = extract_fn(&ir, "k").unwrap_or_default();
                if body.is_empty() {
                    println!(
                        "{name:<28} {:>6} {:>8} {:>7} {:>7}  no `k` in module (inlined away?)",
                        "-", "-", "-", "-"
                    );
                    continue;
                }
                let lines = body.lines().count();
                let rt = count_runtime_calls(&body);
                let unwind = body.matches("invoke ").count() + body.matches("landingpad").count();
                let alloca = body.matches("alloca ").count();
                let verdict = if rt == 0 && unwind == 0 {
                    "device-clean"
                } else if unwind > 0 {
                    "HAS UNWIND EDGES"
                } else {
                    "HAS RUNTIME CALLS"
                };
                println!("{name:<28} {lines:>6} {rt:>8} {unwind:>7} {alloca:>7}  {verdict}");
                if rt > 0 {
                    for sym in runtime_syms(&body) {
                        println!("{:<28}     -> {sym}", "");
                    }
                }
            }
        }
    }
}

fn compile(src: &str) -> Result<String, String> {
    let parsed = karac::parse(src);
    if !parsed.errors.is_empty() {
        return Err(format!("{:?}", parsed.errors[0]));
    }
    let program = parsed.program;
    let resolved = karac::resolve(&program);
    if !resolved.errors.is_empty() {
        return Err(format!("{:?}", resolved.errors[0]));
    }
    let tc = karac::typecheck(&program, &resolved);
    if !tc.errors.is_empty() {
        return Err(format!("{:?}", tc.errors[0]));
    }
    let own = karac::ownershipcheck(&program, &tc);
    karac::codegen::compile_to_ir(&program, Some(&own), None)
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(90).collect()
}

/// Pull one `define ... @name(` block out of a textual module.
fn extract_fn(ir: &str, name: &str) -> Option<String> {
    let needle = format!("@{name}(");
    let start = ir
        .lines()
        .position(|l| l.starts_with("define") && l.contains(&needle))?;
    let mut out = String::new();
    for line in ir.lines().skip(start) {
        out.push_str(line);
        out.push('\n');
        if line == "}" {
            break;
        }
    }
    Some(out)
}

fn count_runtime_calls(body: &str) -> usize {
    runtime_syms(body).len()
}

/// Distinct `karac_*` / libc symbols called in the body — the things a device
/// module has no way to resolve.
fn runtime_syms(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in body.lines() {
        if !line.contains("call ") && !line.contains("invoke ") {
            continue;
        }
        if let Some(at) = line.find(" @") {
            let rest = &line[at + 2..];
            let sym: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
                .collect();
            // llvm.* intrinsics are fine on a device; everything else is not.
            if !sym.is_empty() && !sym.starts_with("llvm.") && !out.contains(&sym) {
                out.push(sym);
            }
        }
    }
    out
}
