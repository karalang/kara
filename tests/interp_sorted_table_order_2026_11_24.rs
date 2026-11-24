//! The MIR interpreter keeps a changed sorted table's key order in a
//! chunked index of slots ([`Order`] in src/mir/interp.rs), so an insert no
//! longer shifts every later entry (B-2026-11-21-50: 10^5 front inserts
//! went quadratic). This drives that index through chunk splits, removals
//! that leave holes and compact them, ranges, floors, pops, `retain`,
//! `entry`, clones, equality, and the drop of values in key order. The
//! numbers were checked against a Python mirror; the interpreter and a
//! compiled build print the same.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn karac(mode: &str, src: &str) -> Output {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-sorted-order-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

const PROGRAM: &str = r#"struct Noisy {
    id: i64,
}

impl Drop for Noisy {
    fn drop(mut ref self) {
        print(f"d{self.id} ");
    }
}

fn digest(m: SortedMap[i64, i64]) -> i64 {
    let mut h = 0;
    let mut prev = -1000000000;
    let mut ordered = true;
    for (k, v) in m.iter() {
        if *k <= prev {
            ordered = false;
        }
        prev = *k;
        h = (h * 31 + *k * 7 + *v) % 1000000007;
    }
    if ordered { h } else { -1 }
}

fn main() {
    let mut m: SortedMap[i64, i64] = SortedMap.new();
    let mut i = 5000;
    while i > 0 {
        m.insert(i * 2, i);
        i -= 1;
    }
    println(f"{m.len()} {digest(m)}");
    let mut s = 12345;
    for _ in 0..3000 {
        s = (s * 1103515245 + 12345) % 2147483648;
        m.insert((s % 20000) * 2 + 1, s % 97);
    }
    println(f"{m.len()} {digest(m)}");
    let mut k = 1;
    while k < 20000 {
        m.remove(k);
        k += 3;
    }
    println(f"{m.len()} {digest(m)}");
    let mut r = 0;
    for (a, b) in m.range(4000, 4100) {
        r += *a + *b;
    }
    let mut ri = 0;
    for (a, _) in m.range_inclusive(4000, 4100) {
        ri += *a;
    }
    println(f"{r} {ri}");
    let fl = m.floor(7777).map(|(a, _)| *a).unwrap_or(-1);
    let ce = m.ceiling(7777).map(|(a, _)| *a).unwrap_or(-1);
    let lo = m.lower(8000).map(|(a, _)| *a).unwrap_or(-1);
    let hi = m.higher(8000).map(|(a, _)| *a).unwrap_or(-1);
    println(f"{fl} {ce} {lo} {hi}");
    let f = m.first().map(|(a, _)| *a).unwrap_or(-1);
    let l = m.last().map(|(a, _)| *a).unwrap_or(-1);
    println(f"{f} {l}");
    for _ in 0..40 {
        m.pop_first();
        m.pop_last();
    }
    m.retain(|key, v| key % 5 != 0);
    if let Some(v) = m.get_mut(5002) {
        *v += 1000;
    }
    m[3] = 33;
    *m.entry(1).or_insert(0) += 7;
    println(f"{m.len()} {digest(m)} {m.get(5002) ?? -1} {m[1]}");
    let c = m.clone();
    println(c == m);
    let mut c2 = m.clone();
    c2.insert(-5, 0);
    println(c2 == m);
    c2.remove(-5);
    println(c2 == m);
    let mut vs = 0;
    for v in m.values_mut() {
        *v += 1;
        vs += *v;
    }
    let ks: Vec[i64] = m.into_keys().take(5).collect();
    println(f"{vs} {ks}");

    let mut t: SortedSet[String] = SortedSet.new();
    for j in 0..2000 {
        t.insert(f"w{(1999 - j) * 7 % 2000}");
    }
    for j in 0..2000 {
        if j % 4 == 1 {
            t.remove(f"w{j}");
        }
    }
    let mut prev = "".to_string();
    let mut ok = true;
    for w in t.iter() {
        if *w <= prev {
            ok = false;
        }
        prev = w.clone();
    }
    let rs: Vec[String] = t.range("w10".to_string(), "w11".to_string()).map(|w| w.clone()).collect();
    println(f"{t.len()} {ok} {t.min() ?? ""} {t.max() ?? ""} {rs.len()} {t.contains("w13")} {t.contains("w12")}");

    let mut d: SortedMap[i64, Noisy] = SortedMap.new();
    let mut j = 1200;
    while j > 0 {
        d.insert(j, Noisy { id: j });
        j -= 1;
    }
    let mut q = 1;
    while q <= 1200 {
        if q > 6 {
            d.remove(q);
        }
        q += 1;
    }
    println("");
    d.insert(0, Noisy { id: 0 });
    d.insert(3, Noisy { id: 33 });
    println("");
}
"#;

fn want() -> String {
    let mut s = String::from(
        "5000 927656796\n7786 622678399\n5676 469722350\n208651 145824\n\
         7776 7778 7998 8004\n2 39985\n4489 451717218 -1 7\ntrue\nfalse\ntrue\n\
         6761225 [1, 3, 104, 108, 113]\n1500 true w0 w999 83 false true\n",
    );
    for i in 7..=1200 {
        s.push_str(&format!("d{i} "));
    }
    s.push_str("\nd3 \nd0 d1 d2 d33 d4 d5 d6 ");
    s
}

fn runs(mode: &str) {
    let out = karac(mode, PROGRAM);
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), want(), "{mode}");
}

#[test]
fn interp_sorted_table_order_index() {
    runs("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_sorted_table_order_index() {
    runs("__mir-llvm-run");
}
