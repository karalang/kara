//! B-2026-11-25-67: the intrinsics the prelude declares for the standard
//! library (`fence`, `compiler_fence`, `volatile_read`, `volatile_write`,
//! `forget`, `ref_eq`) are refused in a program, E_INTRINSIC_STDLIB_ONLY
//! (design.md § Intrinsics), each naming the user surface; `karac fix`
//! rewrites a free `ref_eq(a, b)` to the method `a.ref_eq(b)`.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-intrinsic-stdlib-only-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn intrinsic_calls_in_a_program_are_refused_naming_the_user_surface() {
    for (name, src, surface) in [
        (
            "fence",
            "import std.sync.{MemoryOrdering}; fn main() {\n    // Safety: none needed.\n    unsafe { fence(MemoryOrdering.SeqCst); }\n}\n",
            "library/concurrency.md § Atomics",
        ),
        (
            "compiler_fence",
            "import std.sync.{MemoryOrdering}; fn main() { compiler_fence(MemoryOrdering.SeqCst); }\n",
            "sequentially consistent",
        ),
        (
            "volatile_write",
            "fn main() {\n    let mut c: i32 = 0;\n    // Safety: c is live.\n    unsafe { volatile_write(ptr.mut(c), 5); }\n}\n",
            "`p.write_volatile(v)`",
        ),
        (
            "forget",
            "struct G { id: i64 }\nfn main() { let g = G { id: 1 }; forget(g); }\n",
            "FFI unions and exported C ABI",
        ),
        (
            "ref_eq",
            "shared struct N { v: i64 }\nfn main() { let a = N { v: 1 }; let b = a; println(ref_eq(a, b)); }\n",
            "`a.ref_eq(b)`",
        ),
    ] {
        let file = program(name, src);
        let (ok, out) = karac(&["check", file.to_str().unwrap()]);
        assert!(!ok, "{name}: {out}");
        assert!(out.contains("E_INTRINSIC_STDLIB_ONLY"), "{name}: {out}");
        assert!(out.contains(surface), "{name}: {out}");
    }
}

#[test]
fn karac_fix_rewrites_a_free_ref_eq_to_the_method() {
    let file = program(
        "fix",
        "shared struct N { v: i64 }\nfn main() {\n    let a = N { v: 1 };\n    let b = a;\n    let c = N { v: 1 };\n    println(f\"{ref_eq(a, b)} {ref_eq(a, c)}\");\n}\n",
    );
    let f = file.to_str().unwrap();
    let (_, out) = karac(&["fix", f]);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("println(f\"{a.ref_eq(b)} {a.ref_eq(c)}\");"),
        "{out}\n{text}"
    );
    let (ok, out) = karac(&["check", f]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, "true false\n");
}

#[test]
fn a_programs_own_function_of_an_intrinsics_name_is_its_own() {
    let file = program(
        "own",
        "fn forget(x: i64) -> i64 { x + 1 }\nfn main() { println(forget(1)); }\n",
    );
    let f = file.to_str().unwrap();
    let (ok, out) = karac(&["check", f]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, "2\n");
}
