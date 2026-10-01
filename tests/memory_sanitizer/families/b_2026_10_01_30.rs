//! B-2026-10-01-30 -- a heap field read out of a local `Array` element is a
//! copy, so the binding and the array each free their own buffer.

use super::*;

/// B-2026-10-01-30 — `let x = a[1].s` over a local `Array[S, 2]` double freed
/// at -O0 (`free(): double free detected in tcache 2`) where the `Vec[S]` twin
/// was clean: the field-read cloners (B-2026-08-12-27 and its tuple sibling)
/// admitted a Vec, slice or map container and declined an `Array` local, so
/// the read was a shallow alias freed through the binding and the array. Both
/// now resolve an `Array` local's element type too. `control:` cells were right
/// before.
#[test]
fn asan_array_element_field_read_is_a_copy() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "let-bind",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}:{self.s.len()}\") } }\nstruct H { v: Vec[S] }\nfn main() { let n = 7; let a: Array[S, 2] = [S { s: f\"one-aaaaaaaaaaaaaaaaaaaaaa-{n}\", k: 1 }, S { s: f\"two-bbbbbbbb-{n}\", k: 5 }]; let x = a[1].s; println(f\"x:{x}\") }\n",
            "x:two-bbbbbbbb-7\n",
        ),
        (
            "let-mutate-copy",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nfn main() { let n = 7; let a: Array[S, 2] = [S { s: f\"one-{n}\", k: 1 }, S { s: f\"two-{n}\", k: 5 }]; let mut x = a[1].s; x.push_str(\"!\"); println(f\"x:{x}:{a[1].s}\") }\n",
            "x:two-7!:two-7\n",
        ),
        (
            "struct-literal",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}:{self.s.len()}\") } }\nstruct H { v: Vec[S] }\nfn main() { let n = 7; let mut a: Array[S, 2] = [S { s: f\"one-aaaaaaaaaaaaaaaaaaaaaa-{n}\", k: 1 }, S { s: f\"two-bbbbbbbb-{n}\", k: 5 }]; let x = S { s: a[1].s, k: 3 }; println(f\"x:{x.s}:{a[1].s}\") }\n",
            "x:two-bbbbbbbb-7:two-bbbbbbbb-7\n",
        ),
        (
            "field-store",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nfn main() { let n = 7; let mut a: Array[S, 2] = [S { s: f\"one-{n}\", k: 1 }, S { s: f\"two-{n}\", k: 5 }]; a[0].s = a[1].s; println(f\"r:{a[0].s}:{a[1].s}\") }\n",
            "r:two-7:two-7\n",
        ),
        (
            "index-store",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}:{self.s.len()}\") } }\nstruct H { v: Vec[S] }\nfn main() { let n = 7; let mut a: Array[S, 2] = [S { s: f\"one-aaaaaaaaaaaaaaaaaaaaaa-{n}\", k: 1 }, S { s: f\"two-bbbbbbbb-{n}\", k: 5 }]; a[0] = S { s: a[1].s, k: 2 }; println(f\"r:{a[0].k}:{a[0].s}:{a[1].s}\") }\n",
            "r:2:two-bbbbbbbb-7:two-bbbbbbbb-7\n",
        ),
        (
            "tuple-elem",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nfn main() { let n = 7; let a: Array[(String, i64), 2] = [(f\"one-{n}\", 1), (f\"two-{n}\", 5)]; let x = a[1].0; println(f\"x:{x}:{a[1].0}\") }\n",
            "x:two-7:two-7\n",
        ),
        (
            "nested-field",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nfn main() { let n = 7; let a: Array[U, 2] = [U { t: T { s: f\"one-{n}\" }, k: 1 }, U { t: T { s: f\"two-{n}\" }, k: 5 }]; let x = a[1].t.s; println(f\"x:{x}:{a[1].t.s}\") }\n",
            "x:two-7:two-7\n",
        ),
        (
            "whole-struct-field",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nfn main() { let n = 7; let a: Array[U, 2] = [U { t: T { s: f\"one-{n}\" }, k: 1 }, U { t: T { s: f\"two-{n}\" }, k: 5 }]; let x = a[1].t; println(f\"x:{x.s}:{a[1].t.s}\") }\n",
            "x:two-7:two-7\n",
        ),
        (
            "by-value-arg",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nfn take(s: String) -> i64 { s.len() }\nfn main() { let n = 7; let a: Array[S, 2] = [S { s: f\"one-{n}\", k: 1 }, S { s: f\"two-{n}\", k: 5 }]; let k = take(a[1].s); println(f\"k:{k}:{a[1].s}\") }\n",
            "k:5:two-7\n",
        ),
        (
            "push",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nfn main() { let n = 7; let a: Array[S, 2] = [S { s: f\"one-{n}\", k: 1 }, S { s: f\"two-{n}\", k: 5 }]; let mut v: Vec[String] = []; v.push(a[1].s); println(f\"v:{v[0]}:{a[1].s}\") }\n",
            "v:two-7:two-7\n",
        ),
        (
            "return",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nfn get(a: ref Array[S, 2]) -> String { a[1].s }\nfn main() { let n = 7; let a: Array[S, 2] = [S { s: f\"one-{n}\", k: 1 }, S { s: f\"two-{n}\", k: 5 }]; let x = get(a); println(f\"x:{x}:{a[1].s}\") }\n",
            "x:two-7:two-7\n",
        ),
        (
            "owned-param",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nfn get(a: Array[S, 2]) -> String { a[1].s }\nfn main() { let n = 7; let a: Array[S, 2] = [S { s: f\"one-{n}\", k: 1 }, S { s: f\"two-{n}\", k: 5 }]; let x = get(a); println(f\"x:{x}\") }\n",
            "x:two-7\n",
        ),
        (
            "drop-elem",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nfn main() { let n = 7; let a: Array[D, 2] = [D { s: f\"one-{n}\", k: 1 }, D { s: f\"two-{n}\", k: 5 }]; let x = a[1].s; println(f\"x:{x}:{a[1].s}\") }\n",
            "x:two-7:two-7\ndD1\ndD5\n",
        ),
        (
            "loop",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nfn main() { let n = 7; let a: Array[S, 2] = [S { s: f\"one-{n}\", k: 1 }, S { s: f\"two-{n}\", k: 5 }]; let mut t = 0; for i in 0..3 { let x = a[1].s; t = t + x.len(); }; println(f\"t:{t}\") }\n",
            "t:15\n",
        ),
        (
            "control:print-only",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nfn main() { let n = 7; let a: Array[S, 2] = [S { s: f\"one-{n}\", k: 1 }, S { s: f\"two-{n}\", k: 5 }]; println(f\"r:{a[1].s}\") }\n",
            "r:two-7\n",
        ),
        (
            "control:scalar",
            "struct S { s: String, k: i64 }\nstruct T { s: String }\nstruct U { t: T, k: i64 }\nstruct D { s: String, k: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD{self.k}\") } }\nfn main() { let n = 7; let a: Array[S, 2] = [S { s: f\"one-{n}\", k: 1 }, S { s: f\"two-{n}\", k: 5 }]; let k = a[1].k; println(f\"k:{k}\") }\n",
            "k:5\n",
        ),
    ];
    for (label, prog, want) in cells {
        let lines: Vec<&str> = want.lines().collect();
        assert_clean_asan_run(prog, &lines, &format!("b2026-10-01-30-{label}"));
    }
}
