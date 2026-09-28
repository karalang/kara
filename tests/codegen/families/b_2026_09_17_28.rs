//! B-2026-09-17-28 -- an enum tuple payload whose narrow elements share an
//! LLVM word is dropped and entry-copied through a rebuilt copy of the tuple,
//! not by walking the payload words as if they were the tuple.

use super::*;

/// B-2026-09-17-28 — the memory fixture's program as a compiled-output pin
/// (`asan_narrow_shared_word_tuple_enum_payload_released_once`).
#[test]
fn e2e_narrow_shared_word_tuple_enum_payload_output() {
    let Some(out) = run_program(
        r#"enum M { P((bool, i32, String)), Q }
enum N { P((String, i32, bool)), Q }
enum K { P((u8, u8, u8, String, i32)), Q }
enum L { P((i32, String), i64), Q }
fn take(m: M) -> i64 { match m { M.P((b, k, s)) => { return s.len() + k as i64; }, M.Q => { return 0; } } }
fn peek(m: M) -> i64 { match m { M.P((b, k, s)) => { return k as i64; }, M.Q => { return 0; } } }
fn ign(m: M) -> i64 { return 5; }
fn keepS(m: M) -> String { match m { M.P((b, k, s)) => { return s; }, M.Q => { return f"q"; } } }
fn main() {
    let mut i = 0;
    while i < 3 {
        let g = M.P((true, 3i32, f"row-aaaaaaaaaaaaaaaaaaaaaa-{i}"));
        let h = N.P((f"nnn-aaaaaaaaaaaaaaaaaaaaaa-{i}", 4i32, false));
        let k = K.P((1u8, 2u8, 3u8, f"kkk-aaaaaaaaaaaaaaaaaaaaaa-{i}", 9i32));
        let l = L.P((7i32, f"lll-aaaaaaaaaaaaaaaaaaaaaa-{i}"), 5);
        let u = M.P((false, 1i32, f"unused-aaaaaaaaaaaaaaaaaaa-{i}"));
        println(f"a{take(M.P((false, 2i32, f"tk-aaaaaaaaaaaaaaaaaaaaaaa-{i}")))}");
        println(f"b{peek(M.P((false, 6i32, f"pk-aaaaaaaaaaaaaaaaaaaaaaa-{i}")))}");
        println(f"c{ign(M.P((false, 6i32, f"ig-aaaaaaaaaaaaaaaaaaaaaaa-{i}")))}");
        println(f"d{keepS(M.P((true, 1i32, f"ks-aaaaaaaaaaaaaaaaaaaaaaa-{i}")))}");
        let g2 = M.P((true, 8i32, f"mv-aaaaaaaaaaaaaaaaaaaaaaa-{i}"));
        println(f"e{take(g2)}");
        match g { M.P((b, kk, s)) => println(f"f{b}{kk}{s}"), M.Q => println("q") }
        match h { N.P((s, kk, b)) => println(f"g{b}{kk}{s}"), N.Q => println("q") }
        match k { K.P((x, y, z, s, w)) => println(f"h{x}{y}{z}{w}{s}"), K.Q => println("q") }
        match l { L.P((x, s), n) => println(f"j{x}{n}{s}"), L.Q => println("q") }
        i = i + 1;
    }
    println("end");
}
"#,
    ) else {
        return;
    };
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        vec![
            "a30",
            "b6",
            "c5",
            "dks-aaaaaaaaaaaaaaaaaaaaaaa-0",
            "e36",
            "ftrue3row-aaaaaaaaaaaaaaaaaaaaaa-0",
            "gfalse4nnn-aaaaaaaaaaaaaaaaaaaaaa-0",
            "h1239kkk-aaaaaaaaaaaaaaaaaaaaaa-0",
            "j75lll-aaaaaaaaaaaaaaaaaaaaaa-0",
            "a30",
            "b6",
            "c5",
            "dks-aaaaaaaaaaaaaaaaaaaaaaa-1",
            "e36",
            "ftrue3row-aaaaaaaaaaaaaaaaaaaaaa-1",
            "gfalse4nnn-aaaaaaaaaaaaaaaaaaaaaa-1",
            "h1239kkk-aaaaaaaaaaaaaaaaaaaaaa-1",
            "j75lll-aaaaaaaaaaaaaaaaaaaaaa-1",
            "a30",
            "b6",
            "c5",
            "dks-aaaaaaaaaaaaaaaaaaaaaaa-2",
            "e36",
            "ftrue3row-aaaaaaaaaaaaaaaaaaaaaa-2",
            "gfalse4nnn-aaaaaaaaaaaaaaaaaaaaaa-2",
            "h1239kkk-aaaaaaaaaaaaaaaaaaaaaa-2",
            "j75lll-aaaaaaaaaaaaaaaaaaaaaa-2",
            "end",
        ],
        "got:\n{out}"
    );
}
