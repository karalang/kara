//! Gaps the dns_resolver app (kara-katas apps/service/dns_resolver) found
//! porting a Rust-shaped draft: Rust lifetimes had no fix (B-2026-11-14-25), a
//! closure's `*n` over lent items was caught only by the MIR check, with no
//! edit (B-2026-11-14-26), and the interpreter could not send an array handed by
//! value, such as a byte string literal (B-2026-11-14-27).

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-dns-gaps-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

/// `karac fix` until it has nothing left to apply, as the Mend loop runs it.
fn fix_to_fixpoint(path: &std::path::Path) {
    for _ in 0..8 {
        let out = karac(&["fix"], path);
        if text(&out).contains("no fixable") {
            return;
        }
    }
    panic!("karac fix did not settle on {}", path.display());
}

#[test]
fn rust_lifetimes_in_borrows_and_generic_lists_are_fixed_away() {
    let path = scratch(
        "lifetimes.kara",
        "struct Parser<'a> {
    text: &'a str,
    pos: usize,
}
impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str { &self.text[self.pos..] }
}
fn name_of(n: i64) -> &'static str {
    if n == 1 { \"one\" } else { \"many\" }
}
fn first<'a, T: Clone>(xs: &'a [T]) -> T { xs[0].clone() }
fn pick<T: Clone, 'a, 'b: 'a>(xs: &'a [T], _ys: &'b [T]) -> T { xs[0].clone() }
fn main() {
    let v = vec![3, 4];
    println!(\"{}\", name_of(1));
    println!(\"{}\", first(&v));
    println!(\"{}\", pick(&v, &v));
    let p = Parser { text: \"hello\", pos: 2 };
    println!(\"{}\", p.rest());
}
",
    );
    let first = karac(&["check"], &path);
    assert!(
        text(&first).contains("is a Rust lifetime: Kāra has none"),
        "{}",
        text(&first)
    );
    fix_to_fixpoint(&path);
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(!src.contains('\''), "{src}");
    assert!(src.contains("struct Parser {\n    text: Str,"), "{src}");
    assert!(src.contains("impl Parser {"), "{src}");
    assert!(src.contains("fn name_of(n: i64) -> Str {"), "{src}");
    assert!(
        src.contains("fn first[T: Clone](xs: Slice[T]) -> T"),
        "{src}"
    );
    assert!(
        src.contains("fn pick[T: Clone](xs: Slice[T], _ys: Slice[T]) -> T"),
        "{src}"
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "one\n3\n3\nllo\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_closure_tail_deref_of_a_lent_string_is_fixed_to_a_clone() {
    let path = scratch(
        "deref.kara",
        "fn main() {
    let pairs = [(1, \"a\".to_string()), (2, \"b\".to_string())];
    let names = [\"x\".to_string(), \"y\".to_string()];
    let v: Vec[String] = pairs.iter().map(|(_, n)| *n).collect();
    let w: Vec[String] = names.iter().map(|n| *n).collect();
    let t: Vec[(i64, String)] = pairs.iter().map(|p| *p).collect();
    println(f\"{v.join(\",\")} {w.join(\",\")} {t.len()}\");
}
",
    );
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(
        text(&check).contains("`n` lends the 'String', and the closure's result would move it out"),
        "{}",
        text(&check)
    );
    fix_to_fixpoint(&path);
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(src.contains(".map(|(_, n)| n.clone())"), "{src}");
    assert!(src.contains(".map(|n| n.clone())"), "{src}");
    assert!(src.contains(".map(|p| (p.0, p.1.clone()))"), "{src}");
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "a,b x,y 2\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_copy_item_deref_in_a_closure_tail_is_not_refused() {
    let path = scratch(
        "copy-deref.kara",
        "fn main() {
    let xs = [1, 2, 3];
    let v: Vec[i64] = xs.iter().map(|x| *x).collect();
    println(v.len());
}
",
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
}

#[test]
fn the_interpreter_sends_a_byte_string_literal_over_udp() {
    let path = scratch(
        "udp-literal.kara",
        "import std.net.UdpSocket;
fn main() {
    let a = UdpSocket.bind(\"127.0.0.1:0\").unwrap();
    let b = UdpSocket.bind(\"127.0.0.1:0\").unwrap();
    a.send_to(b\"STOP\", b.local_addr().unwrap()).unwrap();
    let mut buf = [0u8; 16];
    let (n, _) = b.recv_from(mut buf).unwrap();
    println(n);
    println(buf[..n] == b\"STOP\");
}
",
    );
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "4\ntrue\n",
        "{}",
        text(&run)
    );
}
