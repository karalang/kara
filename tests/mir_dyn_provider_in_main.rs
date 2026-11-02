//! A `dyn` value `main` makes of a type whose method uses a resource with
//! no default is refused at the coercion as `E_DYN_PROVIDER` (E0423,
//! design.md § dyn Trait, core semantics §14.5), as it is in any other
//! function: the value has no room to carry a provider, so a
//! `with_provider` around its call cannot reach the method. `main` takes
//! no hidden provider parameter, so its coercion reached the generic
//! no-provider check first and was reported as E0419, "`main` takes no
//! hidden provider parameter", which points at the wrong fix
//! (B-2026-11-02-33).

use std::process::{Command, Output, Stdio};

const PRELUDE: &str = "effect resource Clk: ClkP;
trait ClkP {
    fn now(ref self) -> i64;
}
struct F { t: i64 }
impl ClkP for F {
    fn now(ref self) -> i64 { self.t }
}
trait At { fn at(self) -> i64; }
struct S {}
impl At for S { fn at(self) -> i64 { Clk.now() } }
";

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-dyn-provider-main-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

/// `karac check --output=json`: each diagnostic's code, line, column and
/// message.
fn check(tag: &str, src: &str) -> Vec<(String, u64, u64, String)> {
    let out = karac(tag, &["check", "--output=json"], src);
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{tag}: {e}"));
    json["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| {
            (
                d["code"].as_str().unwrap_or("").to_string(),
                d["line"].as_u64().unwrap_or(0),
                d["column"].as_u64().unwrap_or(0),
                d["message"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// The probe's program (cell p27): the coercion in `main`, its call
/// inside a `with_provider`. One E0423 at the coercion, and the
/// interpreter refuses it with the same text.
#[test]
fn a_dyn_main_makes_is_refused_at_the_coercion_as_a_dyn_provider() {
    let src = format!(
        "{PRELUDE}fn main() {{ let d: dyn At = S {{}}; \
         let r = with_provider[Clk](F {{ t: 9 }}, || d.at()); println(r); }}\n"
    );
    let found = check("p27", &src);
    assert_eq!(found.len(), 1, "{found:?}");
    let (code, line, col, msg) = &found[0];
    assert_eq!((code.as_str(), *line, *col), ("E0423", 12, 29), "{msg}");
    assert!(
        msg.contains(
            "`S` as a `dyn At` cannot carry a provider, and its `S.at` uses `Clk`, \
             which has no default"
        ) && msg.contains("take a generic parameter (`[T: At]`)"),
        "{msg}"
    );
    let out = karac("p27", &["__mir-run"], &src);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && out.stdout.is_empty(), "ran: {err}");
    assert!(err.contains("E_DYN_PROVIDER"), "{err}");
}

/// Controls: the same method called on the value itself in `main`, with
/// no `dyn`, is still the no-provider error at the call; and the same
/// coercion in a function `main` calls inside the `with_provider` was
/// already E0423.
#[test]
fn a_plain_call_in_main_stays_a_no_provider_error() {
    let plain = format!("{PRELUDE}fn main() {{ let s = S {{}}; println(s.at()); }}\n");
    let found = check("plain", &plain);
    let at: Vec<(&str, u64, u64)> = found
        .iter()
        .map(|(c, l, k, _)| (c.as_str(), *l, *k))
        .collect();
    assert_eq!(at, [("E0419", 12, 35)], "{found:?}");
    let callee = format!(
        "{PRELUDE}fn mk() -> i64 {{ let d: dyn At = S {{}}; d.at() }}\n\
         fn main() {{ let r = with_provider[Clk](F {{ t: 9 }}, || mk()); println(r); }}\n"
    );
    let found = check("callee", &callee);
    let at: Vec<(&str, u64, u64)> = found
        .iter()
        .map(|(c, l, k, _)| (c.as_str(), *l, *k))
        .collect();
    assert_eq!(at, [("E0423", 12, 34)], "{found:?}");
}
