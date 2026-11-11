//! B-2026-11-10-45 — an integer's bytes in a fixed order and back:
//! `x.to_le_bytes()` / `x.to_be_bytes()` beside `to_ne_bytes()`, and
//! `<int>.from_ne_bytes(b)` / `from_le_bytes` / `from_be_bytes` taking the
//! `Array[u8, N]` of the type's width (docs/library/core-types.md § Byte
//! order). A SOCKS5 port is big-endian on the wire. The same output on the
//! interpreter and a compiled build.

mod common;

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> std::path::PathBuf {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "karac-int-byte-order-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn karac(cmd: &str, src: &str) -> std::process::Output {
    let dir = scratch(cmd);
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([cmd, file.to_str().unwrap()])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[cfg(feature = "llvm")]
fn compiled(src: &str) -> Option<String> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = scratch("b");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 0).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| Command::new(&exe).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    assert!(out.status.success(), "{:?}", out.status);
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

const ORDER: &str = r#"fn port_of(b: Array[u8, 2]) -> u16 {
    u16.from_be_bytes(b)
}

fn main() {
    let p: u16 = 47762;
    let b = p.to_be_bytes();
    let l = p.to_le_bytes();
    println(f"{b[0]} {b[1]} {l[0]} {l[1]}");
    println(f"{port_of(b)} {u16.from_le_bytes(l)} {u16.from_le_bytes(b)}");
    let w: u32 = 0x01020304;
    let wb = w.to_be_bytes();
    println(f"{wb[0]} {wb[3]} {u32.from_be_bytes(wb)} {u32.from_le_bytes(wb)} {u32.from_ne_bytes(w.to_ne_bytes())}");
    let n: i32 = -2;
    let h: Array[u8, 2] = [0xFE, 0xFF];
    println(f"{i32.from_be_bytes(n.to_be_bytes())} {i16.from_le_bytes(h)} {i16.from_be_bytes(h)}");
    let big: i64 = -1234567890123;
    let bb = big.to_be_bytes();
    println(f"{i64.from_be_bytes(bb)} {i64.from_le_bytes(bb)}");
    let u: u8 = 200;
    println(f"{u.to_be_bytes()[0]} {i8.from_ne_bytes(u.to_ne_bytes())}");
}
"#;

const ORDER_OUT: &str = "186 146 146 186\n\
47762 47762 37562\n\
1 4 16909060 67305985 16909060\n\
-2 -2 -257\n\
-1234567890123 3889707714871230463\n\
200 -56\n";

#[test]
fn interp_int_byte_order() {
    let out = karac("__mir-run", ORDER);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), ORDER_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_int_byte_order() {
    if let Some(out) = compiled(ORDER) {
        assert_eq!(out, ORDER_OUT);
    }
}

#[test]
fn from_bytes_of_the_wrong_width_is_refused() {
    let out = karac(
        "check",
        "fn main() {\n    let b = 7u32.to_be_bytes();\n    let p = u16.from_be_bytes(b);\n    println(p);\n}\n",
    );
    let text =
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("u16.from_be_bytes expects an `Array[u8, 2]`, got `Array[u8, 4]`"),
        "{text}"
    );
}
