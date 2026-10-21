//! `std.crypto`'s digests on the v2 backends (B-2026-10-15-17): SHA-1,
//! SHA-2 and HMAC over `ring` (runtime/stdlib/std_crypto*.kara). The
//! expected output is the published test vectors, not a compiler's run:
//! FIPS 180-4's examples (with the million-`a` message), RFC 4231 test
//! cases 1, 2 and 6, and RFC 6455 section 1.3's accept value. Each program
//! runs on the MIR interpreter and, compiled, at -O0 and -O2, and under the
//! ASan lane.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("karac-mir-crypto-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The program's output on the compiled backend at `opt`; `None` when the
/// runtime archive is absent.
fn build_and_run(src: &str, opt: u8) -> Option<String> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = Scratch::new();
    let obj = dir.path().join("main.o");
    let exe = dir.path().join("main");
    karac::codegen::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    common::link_or_skip(karac::codegen::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).expect("utf-8"))
}

/// `src` prints `want` on the interpreter and, compiled, at -O0 and -O2.
fn agrees(src: &str, want: &str) {
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want, "on the interpreter");
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "compiled at -O{opt}");
        }
    }
}

const VECTORS: &str = r#"import std.crypto.sha2.{sha256, sha384, sha512, Sha256, Sha384, Sha512};
import std.crypto.hmac.{hmac_sha256, hmac_sha384, hmac_sha512, HmacSha256, HmacSha384, HmacSha512};
import std.crypto.legacy.{sha1, Sha1};
import std.crypto.{constant_time_eq};
import std.encoding.{hex_encode, base64_encode};

fn main() {
    let abc = "abc".bytes();
    let empty = "".bytes();
    let long = "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".bytes();
    println(hex_encode(sha1(abc)));
    println(hex_encode(sha1(empty)));
    println(hex_encode(sha1(long)));
    println(hex_encode(sha256(abc)));
    println(hex_encode(sha256(empty)));
    println(hex_encode(sha256(long)));
    println(hex_encode(sha384(abc)));
    println(hex_encode(sha512(abc)));
    println(hex_encode(sha512(empty)));
    // One million `a`s, a thousand at a time.
    let chunk = Vec.filled(1000, 97u8);
    let mut a = Sha256.new();
    let mut b = Sha1.new();
    let mut i = 0;
    while i < 1000 {
        a.update(chunk);
        b.update(chunk);
        i = i + 1;
    }
    println(hex_encode(a.finish()));
    println(hex_encode(b.finish()));
    // RFC 4231 test cases 1, 2 and 6.
    let k1 = [0x0bu8; 20];
    let d1 = "Hi There".bytes();
    println(hex_encode(hmac_sha256(k1, d1)));
    println(hex_encode(hmac_sha384(k1, d1)));
    println(hex_encode(hmac_sha512(k1, d1)));
    let k2 = "Jefe".bytes();
    let d2 = "what do ya want for nothing?".bytes();
    println(hex_encode(hmac_sha256(k2, d2)));
    println(hex_encode(hmac_sha384(k2, d2)));
    println(hex_encode(hmac_sha512(k2, d2)));
    let k6 = [0xaau8; 131];
    let d6 = "Test Using Larger Than Block-Size Key - Hash Key First".bytes();
    println(hex_encode(hmac_sha256(k6, d6)));
    // The same MACs given in parts.
    let mut m = HmacSha256.new(k2);
    m.update("what do ya ".bytes());
    m.update("want for nothing?".bytes());
    println(hex_encode(m.finish()));
    let mut m3 = HmacSha384.new(k2);
    m3.update(d2);
    println(hex_encode(m3.finish()));
    let mut m5 = HmacSha512.new(k2);
    m5.update(d2);
    println(hex_encode(m5.finish()));
    let mut s3 = Sha384.new();
    s3.update("a".bytes());
    s3.update("bc".bytes());
    println(hex_encode(s3.finish()));
    let mut s5 = Sha512.new();
    s5.update(abc);
    println(hex_encode(s5.finish()));
    // RFC 6455 section 1.3: the accept value for its sample key.
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    println(base64_encode(sha1(f"{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11".bytes())));
    // Verifying a MAC, and the unequal-length case.
    let tag = hmac_sha256(k2, d2);
    println(f"{constant_time_eq(tag, hmac_sha256(k2, d2))} {constant_time_eq(tag, hmac_sha256(k1, d2))} {constant_time_eq(abc, long)} {constant_time_eq(empty, empty)}");
    // A running hash dropped unfinished frees its state.
    let mut dropped = Sha256.new();
    dropped.update(abc);
    let mut unused = HmacSha512.new(k1);
    unused.update(d1);
    println("done");
}
"#;

const WANT_VECTORS: &str = "a9993e364706816aba3e25717850c26c9cd0d89d\n\
da39a3ee5e6b4b0d3255bfef95601890afd80709\n\
84983e441c3bd26ebaae4aa1f95129e5e54670f1\n\
ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n\
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n\
248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1\n\
cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7\n\
ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f\n\
cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e\n\
cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0\n\
34aa973cd4c4daa4f61eeb2bdbad27316534016f\n\
b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7\n\
afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59cfaea9ea9076ede7f4af152e8b2fa9cb6\n\
87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cdedaa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854\n\
5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843\n\
af45d2e376484031617f78d2b58a6b1b9c7ef464f5a01b47e42ec3736322445e8e2240ca5e69e2c78b3239ecfab21649\n\
164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea2505549758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737\n\
60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54\n\
5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843\n\
af45d2e376484031617f78d2b58a6b1b9c7ef464f5a01b47e42ec3736322445e8e2240ca5e69e2c78b3239ecfab21649\n\
164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea2505549758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737\n\
cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7\n\
ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f\n\
s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\n\
true false false true\n\
done\n\
";

/// Every digest, one-shot and in parts, every HMAC, the WebSocket accept
/// value and `constant_time_eq`, against the published vectors.
#[test]
fn crypto_vectors_agree_on_both_backends() {
    agrees(VECTORS, WANT_VECTORS);
}

/// The compiled program has no leak, double free or use-after-free under
/// the ASan lane: a finished hash frees its state once, and one dropped
/// unfinished frees it too.
#[test]
fn crypto_vectors_are_clean_under_asan() {
    let dir = Scratch::new();
    let src = dir.path().join("vectors.kara");
    std::fs::write(&src, VECTORS).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-asan")
        .arg(&src)
        .output()
        .expect("karac");
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("not found") || err.contains("fsanitize") {
        eprintln!("skipping: no ASan toolchain or runtime archive: {err}");
        return;
    }
    assert!(!err.contains("Sanitizer"), "{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT_VECTORS, "{err}");
}

/// `karac check` accepts the surface.
#[test]
fn crypto_surface_checks() {
    let dir = Scratch::new();
    let ok = dir.path().join("ok.kara");
    std::fs::write(&ok, VECTORS).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&ok)
        .output()
        .expect("karac");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
