//! `karac repl` on v2 (M3 item 3, B-2026-10-22-45): each cell's program is
//! checked as `karac check` checks a program and run on the MIR interpreter.
//! `tests/repl.rs` and `tests/repl_jit.rs` keep covering the legacy REPL
//! (`--legacy`) until M3 deletes it.

use karac::repl::{ReplOptions, Session};

fn session() -> Session {
    Session::with_options(ReplOptions {
        v2: true,
        ..Default::default()
    })
}

/// Runs each cell, asserting it is accepted, and returns the last one's
/// output.
fn run(cells: &[&str]) -> String {
    let mut s = session();
    let mut out = String::new();
    for cell in cells {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "cell {cell:?}: {:?}", r.errors);
        out = r.stdout;
    }
    out
}

#[test]
fn repl_v2_runs_items_and_statements() {
    assert!(session().v2());
    assert_eq!(
        run(&[
            "struct P { x: i64 }",
            "fn twice(p: P) -> i64 { p.x * 2 }",
            "let p = P { x: 21 };",
            "println(twice(p));",
        ]),
        "42\n"
    );
}

/// A later cell sees what an earlier one did to a binding, not only how it
/// was declared: v2 replays each earlier cell whole.
#[test]
fn repl_v2_carries_a_mutation_across_cells() {
    let cases: &[(&str, &str, &str)] = &[
        ("let mut n: i64 = 0; n = 5;", "println(n);", "5"),
        ("let mut f: f64 = 0.0; f = 2.5;", "println(f);", "2.5"),
        (
            "let mut s: String = \"a\".to_string(); s.push_str(\"b\");",
            "println(s);",
            "ab",
        ),
        (
            "let mut m: Map[i64, i64] = Map.new(); m.insert(1, 2);",
            "println(m.len());",
            "1",
        ),
        (
            "let mut acc: i64 = 0; for i in 0..5 { acc = acc + i; }",
            "println(acc);",
            "10",
        ),
    ];
    for (bind, read, want) in cases {
        assert_eq!(run(&[bind, read]).trim(), *want, "{bind}");
    }
    // A mutation in a cell of its own.
    assert_eq!(
        run(&[
            "let mut v: Vec[i64] = Vec.new();",
            "v.push(7);",
            "v.push(8);",
            "println(v.len());",
        ]),
        "2\n"
    );
}

/// What a cell printed is not printed again when a later cell replays it.
#[test]
fn repl_v2_does_not_repeat_an_earlier_cells_output() {
    let mut s = session();
    let r = s.evaluate_cell_captured("let x = { println(\"once\"); 3 };");
    assert_eq!(r.stdout, "once\n");
    let r = s.evaluate_cell_captured("println(\"two\");");
    assert_eq!(r.stdout, "two\n");
    let r = s.evaluate_cell_captured("println(x);");
    assert_eq!(r.stdout, "3\n");
}

/// A cell may end in a statement without its `;`, as a program's last line
/// cannot: the cells after it still run.
#[test]
fn repl_v2_takes_a_cell_without_its_final_semicolon() {
    assert_eq!(
        run(&["let mut n = 1;", "n = 5", "println(n)", "println(n + 1)"]),
        "6\n"
    );
    // A comment-only cell is still a no-op.
    assert_eq!(run(&["// nothing", "println(1)"]), "1\n");
}

/// Cells are checked by v2's rules: a bare parameter borrows (D5), so an
/// item giving it away is refused when it is entered, names its cell, and
/// leaves the session as it was.
#[test]
fn repl_v2_refuses_an_item_v2_rejects_and_rolls_it_back() {
    let mut s = session();
    s.evaluate_cell_captured("struct H { n: i64 }");
    let r = s.evaluate_cell_captured("fn id(h: H) -> H { return h }");
    let joined = r.errors.join("\n");
    assert!(
        joined.contains("cannot give away parameter `h`") && joined.contains("cell 2:1:"),
        "{joined}"
    );
    assert!(!s.items_source().contains("fn id"), "{}", s.items_source());
    assert_eq!(s.cell_history().len(), 1);
    let r = s.evaluate_cell_captured("fn id(h: own H) -> H { return h }");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let r = s.evaluate_cell_captured("println(id(H { n: 2 }).n);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout, "2\n");
}

/// A diagnostic names the cell it is in, and the cell a binding was moved
/// in.
#[test]
fn repl_v2_use_after_move_names_the_moving_cell() {
    let mut s = session();
    s.evaluate_cell_captured("let s = String.from(\"a\");");
    s.evaluate_cell_captured("let t = s;");
    let r = s.evaluate_cell_captured("println(s);");
    let joined = r.errors.join("\n");
    assert!(
        joined.contains("cell 3:1:9") && joined.contains("moved at cell 2:1:9"),
        "{joined}"
    );
    // The refused cell is not in the history.
    assert_eq!(s.cell_history().len(), 2);
}

/// A panicking cell reports its panic, keeps what it printed first, and
/// does not join the session: the cells after it run.
#[test]
fn repl_v2_a_panicking_cell_is_rolled_back() {
    let mut s = session();
    s.evaluate_cell_captured("let d: i64 = 0;");
    let r = s.evaluate_cell_captured("println(\"before\"); let n: i64 = 1 / d;");
    assert_eq!(r.stdout, "before\n");
    let joined = r.errors.join("\n");
    assert!(joined.contains("division by zero"), "{joined}");
    assert!(joined.contains("cell 2:1:"), "{joined}");
    assert_eq!(s.cell_history().len(), 1);
    let r = s.evaluate_cell_captured("println(d + 1);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout, "1\n");
}

/// `:provide` opens a provider scope that later cells run under.
#[test]
fn repl_v2_runs_a_cell_under_a_provider() {
    let mut s = session();
    s.evaluate_cell_captured("struct FakeClock {}");
    let r = s.evaluate_cell_captured(
        "impl ClockProvider for FakeClock { fn now(ref self) -> i64 { 42 } }",
    );
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    s.dispatch_meta(":provide Clock = FakeClock {}");
    assert_eq!(s.provider_stack().len(), 1);
    let r = s.evaluate_cell_captured("println(Clock.now());");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout, "42\n");
}

/// `%show` and auto-clone answer the legacy interpreter and checker; on v2
/// they say so rather than doing nothing.
#[test]
fn repl_v2_show_and_auto_clone_name_the_legacy_repl() {
    let mut s = session();
    let show = format!("{:?}", s.dispatch_magic("%show 1"));
    assert!(show.contains("karac repl --legacy"), "{show}");
    let set = format!("{:?}", s.dispatch_magic("%set auto-clone on"));
    assert!(set.contains("karac repl --legacy"), "{set}");
    assert!(!s.auto_clone());
}

fn karac_repl(args: &[&str], input: &str, legacy_env: bool) -> std::process::Output {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.arg("repl").args(args).env_remove("KARAC_LEGACY");
    if legacy_env {
        cmd.env("KARAC_LEGACY", "1");
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn karac repl");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait karac repl")
}

/// `karac repl` runs on v2; `--legacy` and `KARAC_LEGACY=1` reach the
/// legacy REPL, whose checker lets a bare parameter be given away.
#[test]
fn repl_v2_is_the_default_and_legacy_stays_reachable() {
    let input = "struct H { n: i64 }\nfn id(h: H) -> H { return h }\n\
                 let a = 40;\nprintln(a + 2);\n:quit\n";
    let out = karac_repl(&[], input, false);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("42\n"), "{stdout}\n{stderr}");
    assert!(!stdout.contains("JIT"), "{stdout}");
    assert!(
        stderr.contains("cannot give away parameter `h`"),
        "{stderr}"
    );
    // `--interp` keeps the legacy runs off the JIT runner; v2 takes it too.
    for (args, env) in [
        (&["--legacy", "--interp"][..], false),
        (&["--interp"][..], true),
    ] {
        let out = karac_repl(args, input, env);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("cannot give away"),
            "{args:?} {env}: {stderr}"
        );
    }
}

#[test]
fn repl_v2_refuses_auto_clone_without_legacy() {
    let out = karac_repl(&["--auto-clone"], ":quit\n", false);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("karac repl --legacy --auto-clone"),
        "{stderr}"
    );
}

/// The number of times the cells have appended to the file `path`.
fn appended(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path).map_or(0, |s| s.len())
}

/// A later cell starts from the values the cells before it ended with, so
/// what those cells did (here, append to a file) is not done again
/// (B-2026-10-23-6).
#[test]
fn repl_v2_does_not_run_an_earlier_cell_again() {
    let dir = std::env::temp_dir().join(format!("karac-repl-v2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("once.txt");
    let _ = std::fs::remove_file(&path);
    // `/` separators: a Windows `\` would read as a Kāra string escape.
    let p = path.display().to_string().replace('\\', "/");
    let mut s = session();
    let r = s.evaluate_cell_captured(&format!(
        "let mut n: i64 = {{ File.append(\"{p}\").unwrap().write(\"x\".bytes()).unwrap(); 5 }};"
    ));
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    for cell in ["n = n + 1;", "n = n * 10;", "println(n);"] {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
    }
    assert_eq!(s.evaluate_cell_captured("println(n);").stdout, "60\n");
    assert_eq!(appended(&path), 1, "the first cell ran again");
    // A binding moved away is out of scope after its cell, and a use of
    // it names the cell that moved it; neither runs the first cell again.
    let r = s.evaluate_cell_captured("let w = String.from(\"a\"); let t = w;");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let r = s.evaluate_cell_captured("println(w);");
    assert!(
        r.errors.join("\n").contains("moved at cell 6:1:"),
        "{:?}",
        r.errors
    );
    let r = s.evaluate_cell_captured("let w = 7;");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(s.evaluate_cell_captured("println(n + w);").stdout, "67\n");
    assert_eq!(s.evaluate_cell_captured("println(t);").stdout, "a\n");
    assert_eq!(appended(&path), 1, "the first cell ran again");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Each kind of value a cell can end with reaches the next cell intact.
#[test]
fn repl_v2_carries_each_kind_of_value() {
    let dir = std::env::temp_dir().join(format!("karac-repl-v2k-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("once.txt");
    let _ = std::fs::remove_file(&path);
    let mut s = session();
    let mut out = String::new();
    let first = format!(
        "let mut s = {{ File.append(\"{}\").unwrap().write(\"x\".bytes()).unwrap(); \"a\\\"b\\n\\u{{301}}\".to_string() }};",
        path.display().to_string().replace('\\', "/")
    );
    for cell in [
        "struct P { name: String, xs: Vec[u8] }",
        first.as_str(),
        "enum E { A, B(i64, String), C { w: f64 } }",
        "let m: Map[String, Vec[i64]] = [\"k\": [1, 2], \"j\": Vec.new()];",
        "let st: Set[i64] = Set.from([3, 4]);",
        "let o: Option[(i64, String)] = Some((1, \"x\".to_string()));",
        "let p = P { name: \"n\".to_string(), xs: [1, 2] };",
        "let es: Vec[E] = [E.A, E.B(-3, \"q\".to_string()), E.C { w: 0.1 }];",
        "let t: (u8, char, bool) = (7, '\\'', true);",
        "let sm: SortedMap[i64, String] = [2: \"b\".to_string(), 1: \"a\".to_string()];",
        "let f: f32 = 0.1; let g: f64 = 1e300; let big: u128 = 340282366920938463463374607431768211455u128;",
        "let mn: i64 = -9223372036854775808; let a: Array[i64, 2] = [5, 6];",
        "let r: Result[i64, String] = Err(\"bad\".to_string());",
        "println(s.len()); println(m[\"k\"][1]); println(m[\"j\"].len()); println(st.contains(4));",
        "println(p.name); println(p.xs[1]); println(es.len()); println(t.0); println(t.1); println(sm[1]);",
        "println(f); println(g); println(big); println(mn); println(a[1]);",
        "match o { Some((n, w)) => println(f\"{n} {w}\"), None => println(\"none\") }",
        "match es[1] { E.B(n, w) => println(f\"{n} {w}\"), _ => println(\"other\") }",
        "match es[2] { E.C { w } => println(w), _ => println(\"other\") }",
        "match r { Ok(_) => println(\"ok\"), Err(e) => println(e) }",
    ] {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "cell {cell:?}: {:?}", r.errors);
        out.push_str(&r.stdout);
    }
    assert_eq!(
        out,
        "6\n2\n0\ntrue\nn\n2\n3\n7\n'\na\n0.1\n1e+300\n\
         340282366920938463463374607431768211455\n-9223372036854775808\n6\n\
         1 x\n-3 q\n0.1\nbad\n"
    );
    assert_eq!(appended(&path), 1, "an earlier cell ran again");
    // A closure cannot be written as source: the cell after it replays.
    s.evaluate_cell_captured("let add = |x: i64| x + 1;");
    assert_eq!(
        s.evaluate_cell_captured("println(add(s.len()));").stdout,
        "7\n"
    );
    assert_eq!(appended(&path), 2);
    let _ = std::fs::remove_dir_all(&dir);
}
