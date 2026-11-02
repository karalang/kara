//! A statement the recursion budget refuses must still be stepped past
//! (B-2026-11-02-25). At the depth limit, a block's statement that begins
//! with a keyword (`if`, `while`, `let`, ...) fails without consuming a
//! token, and statement recovery stops at exactly those keywords, so the
//! block loop retried it forever, adding a diagnostic per turn: the
//! scheduled `fuzz_parser` run reached 2 GB on about 200 nested `{` plus
//! garbage, minimized here to 128 `{` and `if`.

use std::sync::mpsc;
use std::time::Duration;

/// The parse's diagnostic count, on a 32 MiB thread (the descent's
/// debug-build frames, as in `tests/parser.rs`'s depth tests), or `None`
/// when it has not returned within 30 seconds.
fn errors_within_time(src: String) -> Option<usize> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let n = karac::parse(&src).errors.len();
            let _ = tx.send(n);
        })
        .expect("spawn parse thread");
    rx.recv_timeout(Duration::from_secs(30)).ok()
}

#[test]
fn a_statement_refused_at_the_depth_limit_is_stepped_past() {
    let heads = [
        "if",
        "while",
        "for",
        "loop",
        "match",
        "let",
        "return",
        "break",
        "continue",
        "if x {}",
        "let y = 1;",
    ];
    let mut stuck = Vec::new();
    for depth in [127usize, 128, 129, 200] {
        for head in heads {
            let src = format!("{}{head}", "{".repeat(depth));
            match errors_within_time(src) {
                // One diagnostic per open brace at most, and a few more.
                Some(n) if n <= depth + 8 => {}
                Some(n) => stuck.push(format!("{depth} x `{{` + {head:?}: {n} errors")),
                None => stuck.push(format!("{depth} x `{{` + {head:?}: did not return")),
            }
        }
    }
    assert!(stuck.is_empty(), "{stuck:#?}");
}

/// The input the scheduled fuzz run found (CI run 41439333367, job
/// 125256952974), verbatim, and its minimization.
#[test]
fn fuzz_parser_oom_input_returns() {
    let found: &[u8] = b"JJ{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{pr{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{J{{#}{{#}try{#}}{''''\x00o{#}try{#}{{z##}{{=}pry#{{-{{z#ty{IIIIIII\nifz\nefsI\nif\n|=ii&'III#[\"\\a}{{=}tr[\"c\"cba[\"yIII4#I&&#\x08#\x08!'";
    let src = String::from_utf8(found.to_vec()).expect("utf-8");
    assert!(errors_within_time(src).is_some(), "the fuzz input");
    let min = format!("{}if", "{".repeat(128));
    assert!(errors_within_time(min).is_some(), "the minimized input");
}
