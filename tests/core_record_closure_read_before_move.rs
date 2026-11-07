//! A closure handed to an iterator chain over a collection of plain user
//! records (`ps.iter().any(|p| p.name == name)` over a `Vec[P]`) runs within
//! its statement, so moving a binding it reads afterwards is an ordinary
//! move. The statement-closure rule of B-2026-10-29-43 accepted only a chain
//! rooted at a standard type, and only a `let` or expression statement, so a
//! user struct at the root or the chain in an `if`'s condition was paired
//! with the later move as "moved inside a loop", with no loop in the program
//! (B-2026-11-07-4). So was a chain in one `match` arm with the move in
//! another, which no run reaches both of. The pairs inside a loop, where the
//! closure runs again after the move, are still refused.

use std::process::Command;

fn check(name: &str, body: &str) -> (bool, String, String) {
    let src = format!(
        "enum Kind {{ A, B(String) }}

struct P {{ name: String, kind: Kind, tags: Vec[String] }}

fn rec(name: Str) -> P {{
    P {{ name: name.to_string(), kind: Kind.B(\"x\".to_string()), tags: Vec.new() }}
}}

{body}
"
    );
    let dir = std::env::temp_dir().join(format!("karac-recread-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, &src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        src,
    )
}

#[test]
fn a_binding_moved_after_a_chain_over_user_records_is_accepted() {
    for (name, body, want) in [
        (
            "let_any",
            "fn main() {
    let ps = [rec(\"a\")];
    let name = \"b\".to_string();
    let dup = ps.iter().any(|p| p.name == name);
    let q = P { name, kind: Kind.A, tags: Vec.new() };
    println(f\"{dup} {q.name}\");
}",
            "false b\n",
        ),
        (
            "if_statement",
            "fn add(ps: Slice[P], name: own String) -> Result[P, String] {
    if ps.iter().any(|p| p.name == name) {
        return Err(f\"dup {name}\");
    }
    Ok(P { name, kind: Kind.A, tags: Vec.new() })
}

fn main() {
    let ps = [rec(\"a\")];
    for n in [\"a\", \"b\"] {
        match add(ps, n.to_string()) {
            Ok(p) => println(p.name),
            Err(e) => println(e),
        }
    }
}",
            "dup a\nb\n",
        ),
        (
            "tail_if_branches",
            "fn show(ps: Slice[P], name: own String) {
    if ps.iter().any(|p| p.name == name) {
        let moved = name;
        println(f\"dup {moved}\");
    } else {
        let q = P { name, kind: Kind.A, tags: Vec.new() };
        println(q.name);
    }
}

fn main() {
    let ps = [rec(\"a\")];
    show(ps, \"a\".to_string());
    show(ps, \"c\".to_string());
}",
            "dup a\nc\n",
        ),
        (
            "other_match_arm",
            "struct H { name: String, f: Fn(i64) -> i64 }

fn pick(hs: Slice[H], kind: Str, opt: own String) -> String {
    match kind {
        \"find\" => {
            let h = hs.iter().find(|h| h.name == opt);
            match h {
                Some(h) => f\"found {(h.f)(1)}\",
                None => \"none\".to_string(),
            }
        }
        _ => {
            let p = P { name: opt, kind: Kind.A, tags: Vec.new() };
            p.name
        }
    }
}

fn main() {
    let hs = [H { name: \"a\".to_string(), f: |x| x + 1 }];
    println(pick(hs, \"find\", \"a\".to_string()));
    println(pick(hs, \"make\", \"z\".to_string()));
}",
            "found 2\nz\n",
        ),
    ] {
        let (ok, err, src) = check(name, body);
        assert!(ok, "{name}: {err}");
        assert_eq!(
            karac::mir::lower::run_source(&src)
                .expect("MIR interpreter")
                .output,
            want,
            "{name}"
        );
    }
}

#[test]
fn a_chain_over_records_whose_move_repeats_in_a_loop_is_still_refused() {
    for (name, body) in [
        (
            "other_arm_in_loop",
            "struct H { name: String, f: Fn(i64) -> i64 }

fn main() {
    let hs = [H { name: \"a\".to_string(), f: |x| x + 1 }];
    let opt = \"a\".to_string();
    let mut ps: Vec[P] = Vec.new();
    for kind in [\"make\", \"find\"] {
        match kind {
            \"find\" => {
                let n = hs.iter().filter(|h| h.name == opt).count();
                println(f\"{n}\");
            }
            _ => ps.push(P { name: opt, kind: Kind.A, tags: Vec.new() }),
        }
    }
    println(f\"{ps.len()}\");
}",
        ),
        (
            "if_in_loop",
            "fn main() {
    let ps = [rec(\"a\")];
    let name = \"b\".to_string();
    let mut out: Vec[P] = Vec.new();
    for i in 0..2 {
        if ps.iter().any(|p| p.name == name) {
            println(f\"{i}\");
        } else {
            out.push(P { name, kind: Kind.A, tags: Vec.new() });
        }
    }
    println(f\"{out.len()}\");
}",
        ),
    ] {
        let (ok, err, _) = check(name, body);
        assert!(!ok, "{name}: the move must still be refused");
        assert!(err.contains("moved"), "{name}: {err}");
    }
}
