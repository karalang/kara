//! A method called on a closure parameter nothing has typed yet
//! (`let digits = |time| { time.chars()..count() }; digits(a)`) used to check
//! clean and fail in lowering with only E0516 "a type that failed to check is
//! not lowered yet". It is now E0238 at the call, naming the parameter, with
//! the type a call of the closure gave it as the fix (B-2026-11-17-144).

use karac::{check_source, run_playground};

const SRC: &str = "fn count_digits(a: String, b: String) -> i64 {\n    let digits = |time| { time.chars().filter(|c| c.is_ascii_digit()).count() };\n    digits(a) + digits(b)\n}\nfn main() { println(count_digits(\"12:3a\".to_string(), \"x9\".to_string())); }\n";

#[test]
fn a_method_on_an_untyped_closure_parameter_asks_for_its_annotation() {
    let diags = check_source(SRC);
    let errs: Vec<_> = diags.iter().filter(|d| d.severity == "error").collect();
    assert_eq!(errs.len(), 1, "{diags:#?}");
    let d = errs[0];
    assert_eq!(d.code.as_deref(), Some("E0238"), "{d:#?}");
    assert!(d.message.contains("closure parameter `time`"), "{d:#?}");
    assert!(d.message.contains("`|time: String|`"), "{d:#?}");
    assert_eq!(d.fixes.len(), 1, "{d:#?}");
    let f = &d.fixes[0];
    let mut out = SRC.to_string();
    out.replace_range(f.offset..f.offset + f.length, &f.replacement);
    assert!(out.contains("|time: String|"), "{out}");
    let after = run_playground(&out);
    assert!(after.ok, "{out}\n{:#?}", after.diagnostics);
    assert_eq!(after.stdout.concat(), "4\n");
}
