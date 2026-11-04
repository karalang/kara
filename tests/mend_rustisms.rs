//! The Mend loop over blind-authored Rust-isms: each cell is what a model that
//! never saw Kāra's diagnostics wrote for a one-line task (the "Probe core
//! semantics by spec section" thread's 30-cell grid, `probe-core/fixgrid.py`
//! in the project's shared folder). Each cell goes through `karac check`, then
//! `karac fix` to a fixpoint (at most four rounds), then `karac check` again
//! and a run, and must print what the task asked for. A cell that `karac fix`
//! carries to a working program stays that way: this is the regression net
//! for the Rust-ism fix-its (CLAUDE.md § Writing Kāra).

use std::path::Path;
use std::process::{Command, Stdio};

const CELLS: &[(&str, &str, &str)] = &[
    (
        "r01_impl_fn_param",
        "fn apply(f: impl Fn(i64) -> i64, x: i64) -> i64 { f(x) }\nfn main() { println(apply(|v| v + 1, 4)); }",
        "5",
    ),
    (
        "r02_lowercase_fn_type",
        "fn apply(f: fn(i64) -> i64, x: i64) -> i64 { f(x) }\nfn inc(v: i64) -> i64 { v + 1 }\nfn main() { println(apply(inc, 4)); }",
        "5",
    ),
    (
        "r03_closure_return_annot",
        "fn main() { let f = |x: i64| -> i64 { x * 2 }; println(f(4)); }",
        "8",
    ),
    (
        "r04_turbofish_parse",
        "fn main() { let n = \"5\".parse::<i64>().unwrap(); println(n + 1); }",
        "6",
    ),
    (
        "r05_turbofish_collect_angle",
        "fn main() { let v = [1, 2, 3].to_vec(); let w = v.iter().map(|x| x * 2).collect::<Vec<i64>>(); println(w.len()); }",
        "3",
    ),
    (
        "r06_angle_generic_type",
        "fn total(v: Vec<i64>) -> i64 { let mut t = 0; for x in v { t += x; } t }\nfn main() { println(total([1, 2].to_vec())); }",
        "3",
    ),
    (
        "r07_ampersand_call_site",
        "fn total(v: Vec[i64]) -> i64 { let mut t = 0; for x in v { t += x; } t }\nfn main() { let v = [1, 2].to_vec(); println(total(&v)); }",
        "3",
    ),
    (
        "r08_amp_mut_call_site",
        "fn add(v: mut ref Vec[i64]) { v.push(1); }\nfn main() { let mut v: Vec[i64] = Vec.new(); add(&mut v); println(v.len()); }",
        "1",
    ),
    (
        "r09_logical_ops",
        "fn main() { let a = true; let b = false; if a && !b || b { println(\"y\"); } }",
        "y",
    ),
    (
        "r10_double_colon_paths",
        "fn main() { let s = String::from(\"ab\"); let v: Vec[i64] = Vec::new(); println(s.len() + v.len()); }",
        "2",
    ),
    (
        "r11_vec_macro",
        "fn main() { let v = vec![1, 2, 3]; println(v.len()); }",
        "3",
    ),
    (
        "r12_println_macro",
        "fn main() { let x = 3; println!(\"x = {}\", x); }",
        "x = 3",
    ),
    (
        "r13_format_macro",
        "fn main() { let a = 1; let b = 2; let s = format!(\"{}-{}\", a, b); println(s); }",
        "1-2",
    ),
    (
        "r14_impl_generic_angle",
        "struct Bx<T> { v: T }\nimpl<T> Bx<T> { fn get(self) -> T { self.v } }\nfn main() { let b = Bx { v: 4 }; println(b.get()); }",
        "4",
    ),
    (
        "r15_ref_str_param",
        "fn hello(name: &str) -> String { \"hi \".to_string() + name }\nfn main() { println(hello(\"bo\")); }",
        "hi bo",
    ),
    (
        "r16_amp_mut_self",
        "struct C { n: i64 }\nimpl C { fn bump(&mut self) { self.n += 1; } fn get(&self) -> i64 { self.n } }\nfn main() { let mut c = C { n: 0 }; c.bump(); println(c.get()); }",
        "1",
    ),
    (
        "r17_closure_amp_pattern",
        "fn main() { let v = [1, 2, 3].to_vec(); let w: Vec[i64] = v.iter().map(|&x| x + 1).collect(); println(w[2]); }",
        "4",
    ),
    (
        "r18_usize_index_cast",
        "fn main() { let v = [5, 6, 7].to_vec(); let i: i64 = 1; println(v[i as usize]); }",
        "6",
    ),
    (
        "r19_i64_max_path",
        "fn main() { let m = i64::MAX; println(m - 9223372036854775806); }",
        "1",
    ),
    (
        "r20_box_dyn_fn",
        "fn mk(k: i64) -> Box<dyn Fn(i64) -> i64> { Box::new(move |x| x + k) }\nfn main() { let f = mk(2); println(f(3)); }",
        "5",
    ),
    (
        "r21_move_closure",
        "fn main() { let k = 2; let f = move |x: i64| x + k; println(f(3)); }",
        "5",
    ),
    (
        "r22_slice_ref_type",
        "fn first(s: &[i64]) -> i64 { s[0] }\nfn main() { let v = [9, 8].to_vec(); println(first(&v[..])); }",
        "9",
    ),
    (
        "r23_derive_debug_print",
        "#[derive(Debug)]\nstruct P { x: i64 }\nfn main() { let p = P { x: 1 }; println(f\"{p:?}\"); }",
        "P { x: 1 }",
    ),
    (
        "r24_const_usize",
        "const N: usize = 3;\nfn main() { let v = Vec.filled(N, 0); println(v.len()); }",
        "3",
    ),
    (
        "r25_result_angle_return",
        "fn half(n: i64) -> Result<i64, String> { if n % 2 == 0 { Ok(n / 2) } else { Err(\"odd\".to_string()) } }\nfn main() { println(half(8).unwrap()); }",
        "4",
    ),
    (
        "r26_option_angle_some_q",
        "fn f(o: Option<i64>) -> Option<i64> { let v = o?; Some(v + 1) }\nfn main() { println(f(Some(1)).unwrap()); }",
        "2",
    ),
    (
        "r27_unwrap_or_default_string_ref",
        "fn main() { let o: Option<String> = None; println(o.unwrap_or_default().len()); }",
        "0",
    ),
    (
        "r28_hashmap_name",
        "use std::collections::HashMap;\nfn main() { let mut m: HashMap<i64, i64> = HashMap::new(); m.insert(1, 2); println(m[1]); }",
        "2",
    ),
    (
        "r29_iter_sum_annot",
        "fn main() { let v = [1, 2, 3].to_vec(); let s: i64 = v.iter().sum::<i64>(); println(s); }",
        "6",
    ),
    (
        "r30_let_else_rust",
        "fn f(o: Option[i64]) -> i64 { let Some(v) = o else { return 0; }; v }\nfn main() { println(f(Some(3))); }",
        "3",
    ),
];

/// Cells `karac fix` cannot carry to a working program yet, with the open
/// ledger row that tracks each. They run and report, but do not fail the test;
/// once the row is fixed the cell moves out of this list.
const OPEN: &[(&str, &str)] = &[];

/// Cells refused with a named diagnostic that has no single right edit: the
/// diagnostic must stay.
const DESCRIPTIVE: &[(&str, &str)] = &[
    // `{p:?}`: Debug formatting is reserved; `{p}` needs a Display impl.
    ("r23_derive_debug_print", "is Debug formatting"),
    // `const N`: a one-letter name is Type-class whatever its case.
    ("r24_const_usize", "const names must be Const-class"),
];

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// One cell through the loop: `Ok` with the run's stdout, or `Err` with why
/// it stopped.
fn mend(dir: &Path, src: &str) -> (String, Result<String, String>) {
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let (_, _, first) = karac(dir, &["check", "cell.kara"]);
    for _ in 0..4 {
        let before = std::fs::read_to_string(&file).unwrap();
        karac(dir, &["fix", "cell.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (ok, _, err) = karac(dir, &["check", "cell.kara"]);
    if !ok {
        return (
            first,
            Err(format!("still refused:\n{err}\nfixed text:\n{text}")),
        );
    }
    if err.contains("warning[deprecated]") {
        return (
            first,
            Err(format!(
                "a deprecation is left:\n{err}\nfixed text:\n{text}"
            )),
        );
    }
    let (_, stdout, stderr) = karac(dir, &["__mir-run", "cell.kara"]);
    if stdout.is_empty() {
        return (
            first,
            Err(format!("no output: {stderr}\nfixed text:\n{text}")),
        );
    }
    (first, Ok(stdout))
}

#[test]
fn mend_loop_carries_each_blind_rust_ism_to_a_working_program() {
    let root = std::env::temp_dir().join(format!("karac-mend-rustisms-{}", std::process::id()));
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = CELLS
            .iter()
            .map(|&(name, src, want)| {
                let dir = root.join(name);
                s.spawn(move || {
                    std::fs::create_dir_all(&dir).unwrap();
                    let (first, got) = mend(&dir, src);
                    (name, want, first, got)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(results.len(), 30);
    let mut bad = Vec::new();
    let mut fixed = 0;
    for (name, want, first, got) in results {
        if let Some((_, phrase)) = DESCRIPTIVE.iter().find(|(n, _)| *n == name) {
            if !first.contains(phrase) {
                bad.push(format!("{name}: first check lost {phrase:?}:\n{first}"));
            }
            continue;
        }
        let open = OPEN.iter().find(|(n, _)| *n == name);
        match (got, open) {
            (Ok(out), None) if out.trim_end() == want => fixed += 1,
            (Ok(out), None) => bad.push(format!("{name}: printed {out:?}, want {want:?}")),
            (Err(why), None) => bad.push(format!("{name}: {why}")),
            (Ok(out), Some((_, row))) if out.trim_end() == want => {
                eprintln!("{name}: now mends ({row}); move it out of OPEN")
            }
            (_, Some(_)) => {}
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
    assert_eq!(fixed, CELLS.len() - OPEN.len() - DESCRIPTIVE.len());
}
