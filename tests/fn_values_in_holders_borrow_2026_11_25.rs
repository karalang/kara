//! A `Fn(T)` type held in a collection, an `Option` or a tuple borrows its
//! bare parameter as a `Fn(T)` parameter does (D5): `let fs: Vec[Fn(String) ->
//! i64] = [nf]; fs[0](s); println(s)` is accepted and prints `s` afterwards.
//! The D5 rewrite reached only signatures, so a `let` annotation kept the
//! parameter owned: `karac check` refused the later use of `s` as moved
//! (B-2026-11-25-42) and the interpreter, handed the borrow by the call,
//! failed with "deref of non-reference Box" (B-2026-11-25-41). A closure
//! literal in an annotated tuple slot also takes its parameter types from
//! the slot now, as at a `let`. The cells are probe matrix wk48b's.

mod common;

use std::process::Command;

fn karac(args: &[&str], name: &str, src: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-fn-holders-{name}-{}-{}",
        args[0].trim_start_matches('_'),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// `karac check` accepts `src`, and the interpreter (and, with the backend,
/// the compiled program) prints `want`.
fn accepted_and_prints(name: &str, src: &str, want: &str) {
    let check = karac(&["check"], name, src);
    assert!(
        check.status.success(),
        "{name}: check: {}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for cmd in backends {
        let out = karac(&[cmd], name, src);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        if *cmd != "__mir-run"
            && common::link_or_skip(if out.status.success() {
                Ok(())
            } else {
                Err(err.clone())
            })
            .is_none()
        {
            continue;
        }
        assert!(out.status.success(), "{name}: {cmd}: {err}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{name}: {cmd}\n{src}"
        );
    }
}

#[test]
fn fn_param_borrows_in_a_vec_element() {
    accepted_and_prints(
        "c_bstr_named_vecidx",
        "fn nf(s: String) -> i64 { s.len() as i64 }\nfn main() { let s = \"abcd\".to_string(); let fs: Vec[Fn(String) -> i64] = [nf]; let r = fs[0](s); println(r); println(s); }\n",
        "4\nabcd\n",
    );
}

#[test]
fn fn_param_borrows_in_a_vec_walked_by_iter() {
    accepted_and_prints(
        "c_bstr_named_veciter",
        "fn nf(s: String) -> i64 { s.len() as i64 }\nfn main() { let s = \"abcd\".to_string(); let fs: Vec[Fn(String) -> i64] = [nf, nf]; for f in fs.iter() { let r = f(s); println(r); } println(s); }\n",
        "4\n4\nabcd\n",
    );
}

#[test]
fn fn_param_borrows_in_a_map_value() {
    accepted_and_prints(
        "c_bstr_named_map",
        "fn nf(s: String) -> i64 { s.len() as i64 }\nfn main() { let s = \"abcd\".to_string(); let mut m: Map[i64, Fn(String) -> i64] = Map.new(); m.insert(1, nf); match m.get(1) { Some(f) => { let r = f(s); println(r); } None => {} } println(s); }\n",
        "4\nabcd\n",
    );
}

#[test]
fn fn_param_borrows_in_an_option_payload() {
    accepted_and_prints(
        "c_bstr_named_option",
        "fn nf(s: String) -> i64 { s.len() as i64 }\nfn main() { let s = \"abcd\".to_string(); let o: Option[Fn(String) -> i64] = Some(nf); match o { Some(f) => { let r = f(s); println(r); } None => {} } println(s); }\n",
        "4\nabcd\n",
    );
}

#[test]
fn fn_param_borrows_in_an_option_payload_bound_by_ref() {
    accepted_and_prints(
        "c_bstr_named_optref",
        "fn nf(s: String) -> i64 { s.len() as i64 }\nfn main() { let s = \"abcd\".to_string(); let o: Option[Fn(String) -> i64] = Some(nf); if let Some(ref f) = o { let r = f(s); println(r); } println(s); }\n",
        "4\nabcd\n",
    );
}

#[test]
fn fn_param_borrows_in_a_tuple_slot() {
    accepted_and_prints(
        "c_bstr_named_tuple",
        "fn nf(s: String) -> i64 { s.len() as i64 }\nfn main() { let s = \"abcd\".to_string(); let t: (Fn(String) -> i64, i64) = (nf, 1); let r = (t.0)(s); println(r); println(s); }\n",
        "4\nabcd\n",
    );
}

#[test]
fn fn_param_borrows_in_a_nested_vec_element() {
    accepted_and_prints(
        "c_bstr_named_vecvec",
        "fn nf(s: String) -> i64 { s.len() as i64 }\nfn main() { let s = \"abcd\".to_string(); let fs: Vec[Vec[Fn(String) -> i64]] = [[nf]]; let r = fs[0][0](s); println(r); println(s); }\n",
        "4\nabcd\n",
    );
}

#[test]
fn fn_param_borrows_in_a_closure_literal_in_a_tuple_slot() {
    accepted_and_prints(
        "c_bstr_clo_tuple",
        "fn nf(s: String) -> i64 { s.len() as i64 }\nfn main() { let s = \"abcd\".to_string(); let t: (Fn(String) -> i64, i64) = (|s| s.len() as i64, 1); let r = (t.0)(s); println(r); println(s); }\n",
        "4\nabcd\n",
    );
}

#[test]
fn fn_param_borrows_in_a_method_value_in_a_map() {
    accepted_and_prints(
        "c_bstr_meth_map",
        "fn nf(s: String) -> i64 { s.len() as i64 }\nfn main() { let s = \"abcd\".to_string(); let mut m: Map[i64, Fn(String) -> i64] = Map.new(); m.insert(1, String.len); match m.get(1) { Some(f) => { let r = f(s); println(r); } None => {} } println(s); }\n",
        "4\nabcd\n",
    );
}

#[test]
fn fn_param_borrows_in_closure_literals_in_a_vec() {
    accepted_and_prints(
        "c_bi64_clo_veciter",
        "fn nf(x: i64) -> i64 { x * 2 }\nfn main() { let s = -21; let fs: Vec[Fn(i64) -> i64] = [|x| x * 2, |x| x * 2]; for f in fs.iter() { let r = f(s); println(r); } println(s); }\n",
        "-42\n-42\n-21\n",
    );
}

#[test]
fn fn_param_borrows_in_a_closure_taking_a_vec_in_an_option() {
    accepted_and_prints(
        "c_bvec_clo_option",
        "fn nf(v: Vec[i64]) -> i64 { v.len() as i64 + v[0] }\nfn main() { let s: Vec[i64] = [5, 6, 7]; let o: Option[Fn(Vec[i64]) -> i64] = Some(|v| v.len() as i64 + v[0]); match o { Some(f) => { let r = f(s); println(r); } None => {} } println(s); }\n",
        "8\n[5, 6, 7]\n",
    );
}

#[test]
fn fn_param_borrows_in_a_mut_ref_param_through_a_map() {
    accepted_and_prints(
        "c_mutv_named_map",
        "fn nf(v: mut ref Vec[i64]) -> i64 { v.push(9); v.len() as i64 }\nfn main() { let mut s: Vec[i64] = [1, 2]; let mut m: Map[i64, Fn(mut ref Vec[i64]) -> i64] = Map.new(); m.insert(1, nf); match m.get(1) { Some(f) => { let r = f(mut s); println(r); } None => {} } println(s); }\n",
        "3\n[1, 2, 9]\n",
    );
}

#[test]
fn fn_param_borrows_in_a_string_to_string_fn_in_a_nested_vec() {
    accepted_and_prints(
        "c_bstrstr_named_vecvec",
        "fn nf(s: String) -> String { s.to_uppercase() }\nfn main() { let s = \"abcd\".to_string(); let fs: Vec[Vec[Fn(String) -> String]] = [[nf]]; let r = fs[0][0](s); println(r); println(s); }\n",
        "ABCD\nabcd\n",
    );
}

/// `own` in the held type still moves: the later read of `s` is refused.
#[test]
fn fn_own_param_in_a_vec_element_still_moves() {
    let out = karac(&["check"], "own_control", "fn nf(s: own String) -> i64 { s.len() as i64 }\nfn main() { let s = \"abcd\".to_string(); let fs: Vec[Fn(own String) -> i64] = [nf]; let r = fs[0](s); println(r); println(s); }\n");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "accepted a use after an own move");
    assert!(err.contains("E0500"), "{err}");
}

/// The same `own` cell with a clone at the call runs and keeps `s`.
#[test]
fn fn_own_param_in_a_vec_element_runs_with_a_clone() {
    accepted_and_prints(
        "c_own_named_vecidx",
        "fn nf(s: own String) -> i64 { s.len() as i64 }\nfn main() { let s = \"abcd\".to_string(); let fs: Vec[Fn(own String) -> i64] = [nf]; let r = fs[0](s.clone()); println(r); println(s); }\n",
        "4\nabcd\n",
    );
}
