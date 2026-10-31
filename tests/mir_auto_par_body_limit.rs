//! A statement group's join makes its whole function a coroutine, so the
//! auto-par cost gate declines a group in a body over its size limit
//! (B-2026-10-28-10): `mini_inflect`'s `new_tab`, 28,271 MIR statements,
//! built in 10.9 s with its groups as tasks and 6.1 s without, against
//! 5.3 s in order. `karac query concurrency` reports each candidate's
//! verdict, the size refusal included. `KARAC_AUTO_PAR_BODY_MAX` moves the
//! limit, here below `main`'s size so a small program shows it.

use std::process::Command;

const SRC: &str = r#"fn work(n: i64) -> i64 {
    let mut s = 0;
    for i in 0..n {
        for j in 0..n {
            s = s + i * j % 7;
        }
    }
    s
}
fn main() {
    let a = work(300);
    let b = work(400);
    println(f"{a} {b}");
}
"#;

const WANT: &str = "231169 409374\n";

fn karac(limit: Option<&str>) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env_remove("KARAC_AUTO_PAR")
        .env_remove("KARAC_AUTO_PAR_SIZE");
    match limit {
        Some(n) => c.env("KARAC_AUTO_PAR_BODY_MAX", n),
        None => c.env_remove("KARAC_AUTO_PAR_BODY_MAX"),
    };
    c
}

/// `main`'s statement group in `karac query concurrency`'s `auto_par`
/// list: whether it runs as tasks, and why not.
fn main_group(file: &str, limit: Option<&str>) -> (bool, String) {
    let out = karac(limit)
        .args(["query", "concurrency", file, "main"])
        .output()
        .unwrap();
    let json = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{json}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let groups: Vec<&serde_json::Value> = v["auto_par"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["function"] == "main" && e["kind"] == "statements")
        .collect();
    assert_eq!(groups.len(), 1, "{json}");
    (
        groups[0]["tasks"].as_bool().unwrap(),
        groups[0]["reason"].as_str().unwrap_or("").to_string(),
    )
}

#[test]
fn mir_auto_par_declines_a_statement_group_in_a_body_over_the_limit() {
    let dir = std::env::temp_dir().join(format!("karac-body-limit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let file = file.to_str().unwrap();

    assert_eq!(main_group(file, None), (true, String::new()));
    let (tasks, why) = main_group(file, Some("5"));
    assert!(!tasks, "{why}");
    assert!(
        why.starts_with("main is ")
            && why.ends_with(
                " MIR statements, more than a statement group may make a coroutine of (5)"
            ),
        "{why}"
    );
    assert_eq!(main_group(file, Some("0")), (true, String::new()));

    for (limit, grouped) in [(None, true), (Some("5"), false)] {
        let run = karac(limit)
            .env("KARAC_AUTO_PAR_DEBUG", "1")
            .args(["__mir-run", file])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), WANT);
        let err = String::from_utf8_lossy(&run.stderr);
        assert_eq!(
            err.lines()
                .any(|l| l == "auto-par: statements as tasks: main@11"),
            grouped,
            "{err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A function specialised to its providers is the MIR instance
/// `report{OrderDB=Fixed, UserDB=Fixed}`, and `query concurrency <file>.report`
/// lists its candidates under it (the README's program).
#[test]
fn mir_auto_par_query_lists_a_provider_specialised_function() {
    let dir = std::env::temp_dir().join(format!("karac-body-limit-prov-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("app.kara");
    std::fs::write(&file, APP).unwrap();
    let out = karac(None)
        .args(["query", "concurrency", file.to_str().unwrap(), "report"])
        .output()
        .unwrap();
    let json = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{json}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let list = v["auto_par"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{json}");
    assert_eq!(list[0]["function"], "report{OrderDB=Fixed, UserDB=Fixed}");
    assert_eq!(list[0]["kind"], "statements");
    assert_eq!(list[0]["tasks"], false);
    assert_eq!(
        list[0]["reason"],
        "0 of its 2 statements nest loops or recurse"
    );
}

const APP: &str = r#"// Two data sources behind one trait. A provider supplies each at run time.
trait Source {
    fn load(self) -> Vec[i64];
}
effect resource UserDB: Source;
effect resource OrderDB: Source;

struct Fixed { rows: Vec[i64] }
impl Source for Fixed {
    fn load(self) -> Vec[i64] { self.rows.clone() }
}

// Private functions: the compiler infers each one's effects from its body.
//   fetch_users  -> reads(UserDB)
//   fetch_orders -> reads(OrderDB)
fn fetch_users() -> Vec[i64] { UserDB.load() }
fn fetch_orders() -> Vec[i64] { OrderDB.load() }

// Parameters borrow by default: no copy, and no lifetime annotation.
fn total(users: Vec[i64], orders: Vec[i64]) -> i64 {
    users.len() + orders.len()
}

fn report() {
    // Two reads of two different resources: nothing shared, no conflict.
    // The compiler proves them independent and runs them concurrently.
    let users = fetch_users();            // statement 0
    let orders = fetch_orders();          // statement 1
    println(f"{total(users, orders)}");   // statement 2: waits on 0 and 1
}

fn main() {
    providers {
        UserDB => Fixed { rows: [1, 2, 3] },
        OrderDB => Fixed { rows: [10, 20] },
    } in {
        report();
    }
}
"#;
