//! `karac query effects` shows the parameter an inferred effect acts on for
//! a function with no clause, as B-2026-11-25-35 did for one with a clause
//! (B-2026-11-25-59): `fn inner(tx: ref Sender[i64]) { tx.send(2) }` shows
//! `{"verb":"sends","resource":"Channel","roots":["tx"]}`. The roots are
//! the MIR effect pass's summary keys, kept when every key of that verb and
//! class is a parameter; an effect on a value the body makes, or a function
//! `main` never reaches, shows its class alone.

use std::process::{Command, Output, Stdio};

const SRC: &str = "\
fn inner(tx: ref Sender[i64]) { let _ = tx.send(2); }
fn outer(tx: ref Sender[i64], rx: ref Receiver[i64]) { inner(tx); let _ = rx.recv(); }
fn put(f: mut ref File, w: ref Sender[i64], n: i64) { let _ = f.write(\"x\".bytes()); let _ = w.send(n); }
fn both(a: ref Sender[i64], b: ref Sender[i64]) { let _ = a.send(1); let _ = b.send(2); }
fn mixed(a: ref Sender[i64]) { let _ = a.send(1); let (t, r) = Channel.new(); let _ = t.send(2); let _ = r.recv(); }
fn never(tx: ref Sender[i64]) { let _ = tx.send(9); }
fn declared(tx: ref Sender[i64]) with sends(tx) blocks { let _ = tx.send(3); }
fn main() {
    let (tx, rx) = Channel.new();
    let (t2, r2) = Channel.new();
    outer(tx, r2);
    both(tx, t2);
    mixed(tx);
    declared(tx);
    let mut f = File.create(\"q-inferred-roots.txt\").unwrap();
    put(mut f, tx, 1);
    println(rx.recv().unwrap());
}
";

fn query(tag: &str, target: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-query-inferred-roots-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), SRC).unwrap();
    let out: Output = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["query", "effects", target])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    text
}

const SENDS: &str = "{\"verb\":\"sends\",\"resource\":\"Channel\"";

#[test]
fn an_inferred_effect_on_a_parameter_names_it() {
    let t = query("inner", "m.kara.inner");
    assert!(t.contains(&format!("{SENDS},\"roots\":[\"tx\"]}}")), "{t}");
    assert!(t.contains("\"declared_effects\":null"), "{t}");
    // Through a call, each verb names its own parameter.
    let t = query("outer", "m.kara.outer");
    assert!(t.contains(&format!("{SENDS},\"roots\":[\"tx\"]}}")), "{t}");
    assert!(
        t.contains("{\"verb\":\"receives\",\"resource\":\"Channel\",\"roots\":[\"rx\"]}"),
        "{t}"
    );
    // A `File` parameter's write is the file system's class, rooted at it.
    let t = query("put", "m.kara.put");
    assert!(
        t.contains("{\"verb\":\"writes\",\"resource\":\"FileSystem\",\"roots\":[\"f\"]}"),
        "{t}"
    );
    assert!(t.contains(&format!("{SENDS},\"roots\":[\"w\"]}}")), "{t}");
    // Two parameters of one class are both named.
    let t = query("both", "m.kara.both");
    assert!(
        t.contains(&format!("{SENDS},\"roots\":[\"a\",\"b\"]}}")),
        "{t}"
    );
}

#[test]
fn an_effect_not_only_on_parameters_or_unreached_shows_its_class() {
    // `mixed` also sends on a channel it makes.
    let t = query("mixed", "m.kara.mixed");
    assert!(t.contains(&format!("{SENDS}}}")), "{t}");
    assert!(!t.contains("roots"), "{t}");
    // `main` never calls `never`: no MIR instance, so no roots.
    let t = query("never", "m.kara.never");
    assert!(t.contains(&format!("{SENDS}}}")), "{t}");
    assert!(!t.contains("roots"), "{t}");
    // A clause's roots stand as before.
    let t = query("declared", "m.kara.declared");
    assert!(
        t.contains(&format!(
            "\"declared_effects\":[{SENDS},\"roots\":[\"tx\"]}}"
        )),
        "{t}"
    );
}

#[test]
fn the_whole_program_graph_carries_the_roots() {
    let t = query("whole", "m.kara");
    assert!(
        t.contains(&format!(
            "{{\"function\":\"inner\",\"line\":1,\"is_test\":false,\"inferred_effects\":[{{\"verb\":\"allocates\",\"resource\":\"Heap\"}},{{\"verb\":\"blocks\",\"resource\":\"\"}},{SENDS},\"roots\":[\"tx\"]}}]"
        )),
        "{t}"
    );
}
