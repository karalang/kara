//! A built-in type's method named as a value (`i64.checked_add`,
//! `Vec.windows`, `SortedSet.range`) when the library has no hand-written
//! `method_value_{T}_{m}` for it (runtime/stdlib/source/method_values.kara):
//! the wrapper is generated from the method's family, for the methods the
//! program names that way and for no others (B-2026-11-26-17).
//!
//! A family is a rule: the methods it covers, the receiver types it applies
//! to, and the signature after the receiver, written once with placeholders
//! for the receiver type. The signatures are the ones docs/library gives
//! (core-types.md, strings.md, collections.md); the wrapper calls the method
//! on its first parameter, so it behaves as the call `x.m(..)` does.
//! `tests/method_value_families_2026_11_26.rs` generates every pair the
//! method tables name and checks each wrapper, so a rule that drifts from
//! the checker fails there rather than in a program.

/// The receiver types a rule applies to.
#[derive(Clone, Copy)]
enum On {
    /// Every integer type.
    Int,
    /// The signed integer types.
    Signed,
    /// The unsigned integer types.
    Unsigned,
    /// The integer types with a fixed width (not `isize` / `usize`).
    FixedInt,
    /// The integer types up to 64 bits.
    Int64,
    /// `f32` and `f64`.
    Float,
    /// Every integer and float type.
    Number,
    /// `char`.
    Char,
    /// `u8` and `char`, the ASCII surface they share.
    Ascii,
    /// `String` and `Str`.
    Text,
    /// `String`, for the methods that build it.
    String,
    /// `Vec`.
    Vec,
    /// `VecDeque`.
    Deque,
    /// `Map`.
    Map,
    /// `Set`.
    Set,
    /// `SortedMap`.
    SortedMap,
    /// `SortedSet`.
    SortedSet,
}

const SIGNED: &[&str] = &["i8", "i16", "i32", "i64", "i128", "isize"];
const UNSIGNED: &[&str] = &["u8", "u16", "u32", "u64", "u128", "usize"];

impl On {
    fn covers(self, ty: &str) -> bool {
        let signed = SIGNED.contains(&ty);
        let unsigned = UNSIGNED.contains(&ty);
        let int = signed || unsigned;
        let float = matches!(ty, "f32" | "f64");
        match self {
            On::Int => int,
            On::Signed => signed,
            On::Unsigned => unsigned,
            On::FixedInt => int && !matches!(ty, "isize" | "usize"),
            On::Int64 => int && !matches!(ty, "i128" | "u128"),
            On::Float => float,
            On::Number => int || float,
            On::Char => ty == "char",
            On::Ascii => matches!(ty, "u8" | "char"),
            On::Text => matches!(ty, "String" | "Str"),
            On::String => ty == "String",
            On::Vec => ty == "Vec",
            On::Deque => ty == "VecDeque",
            On::Map => ty == "Map",
            On::Set => ty == "Set",
            On::SortedMap => ty == "SortedMap",
            On::SortedSet => ty == "SortedSet",
        }
    }
}

/// One family. `params` and `ret` spell `S` for the receiver type, `U` for
/// the unsigned type of the same width, `N`
/// for an integer's byte width, `T` / `K` / `V` for a container's element,
/// key and value. `generics` is the wrapper's generic list with its bounds
/// (`[T: Ord]`), empty for a scalar. A parameter is `name: Type`, comma
/// separated; the call passes them in order. A `ret` that is empty is the
/// unit result.
struct Rule {
    methods: &'static str,
    on: On,
    /// `mut ref ` for a method that changes its receiver, else empty.
    recv: &'static str,
    generics: &'static str,
    params: &'static str,
    ret: &'static str,
}

const fn r(
    methods: &'static str,
    on: On,
    generics: &'static str,
    params: &'static str,
    ret: &'static str,
) -> Rule {
    Rule {
        methods,
        on,
        recv: "",
        generics,
        params,
        ret,
    }
}

/// [`r`] for a method that changes its receiver, which the wrapper takes as
/// `mut ref`: `Vec.push` is `fn(mut ref Vec[T], own T)`. An empty `ret` is
/// the unit result.
const fn rm(
    methods: &'static str,
    on: On,
    generics: &'static str,
    params: &'static str,
    ret: &'static str,
) -> Rule {
    Rule {
        methods,
        on,
        recv: "mut ref ",
        generics,
        params,
        ret,
    }
}

/// The families (docs/library/core-types.md § Integers and floats,
/// strings.md, collections.md).
const RULES: &[Rule] = &[
    // Checked, overflowing, wrapping and saturating arithmetic.
    r(
        "checked_add checked_sub checked_mul checked_div checked_rem",
        On::Int,
        "",
        "y: S",
        "Option[S]",
    ),
    r("checked_neg", On::Int, "", "", "Option[S]"),
    r("checked_abs", On::Signed, "", "", "Option[S]"),
    r("checked_pow", On::Int, "", "e: u32", "Option[S]"),
    r(
        "checked_shl checked_shr",
        On::Int,
        "",
        "n: u32",
        "Option[S]",
    ),
    r("checked_ilog", On::Int, "", "b: S", "Option[i64]"),
    r(
        "checked_ilog2 checked_ilog10",
        On::Int,
        "",
        "",
        "Option[i64]",
    ),
    r(
        "overflowing_add overflowing_sub overflowing_mul overflowing_div overflowing_rem",
        On::Int,
        "",
        "y: S",
        "(S, bool)",
    ),
    r("overflowing_neg", On::Int, "", "", "(S, bool)"),
    r("overflowing_abs", On::Signed, "", "", "(S, bool)"),
    r("overflowing_pow", On::Int, "", "e: u32", "(S, bool)"),
    r(
        "overflowing_shl overflowing_shr",
        On::Int,
        "",
        "n: u32",
        "(S, bool)",
    ),
    r(
        "wrapping_div wrapping_rem saturating_div div_ceil",
        On::Int,
        "",
        "y: S",
        "S",
    ),
    r("div_euclid rem_euclid", On::Number, "", "y: S", "S"),
    r("wrapping_neg", On::Int, "", "", "S"),
    r(
        "wrapping_abs saturating_abs saturating_neg",
        On::Signed,
        "",
        "",
        "S",
    ),
    r("wrapping_pow saturating_pow", On::Int, "", "e: u32", "S"),
    r(
        "wrapping_shl wrapping_shr rotate_left rotate_right",
        On::Int,
        "",
        "n: u32",
        "S",
    ),
    r("abs_diff", On::Int, "", "y: S", "U"),
    r("unsigned_abs", On::Signed, "", "", "U"),
    r("clamp", On::Number, "", "lo: S, hi: S", "S"),
    // Logarithms, roots and bits.
    r("ilog", On::Int, "", "b: S", "i64"),
    r("ilog2 ilog10", On::Int, "", "", "i64"),
    r("isqrt", On::Int64, "", "", "S"),
    r("reverse_bits swap_bytes", On::Int, "", "", "S"),
    r("leading_ones trailing_ones", On::Int, "", "", "i64"),
    r("next_power_of_two", On::Unsigned, "", "", "S"),
    r(
        "to_be_bytes to_le_bytes to_ne_bytes",
        On::FixedInt,
        "",
        "",
        "Array[u8, N]",
    ),
    r("to_f32", On::Int, "", "", "f32"),
    r("to_f64", On::Int, "", "", "f64"),
    // Floats.
    r(
        "acos acosh asin asinh atan atanh cbrt cos cosh exp exp2 exp_m1 fract ln ln_1p \
         log10 log2 recip sin sinh tan tanh to_degrees to_radians",
        On::Float,
        "",
        "",
        "S",
    ),
    r("atan2 copysign hypot", On::Float, "", "y: S", "S"),
    r(
        "is_finite is_infinite is_nan is_sign_negative is_sign_positive",
        On::Float,
        "",
        "",
        "bool",
    ),
    r("to_bits", On::Float, "", "", "u64"),
    r("to_bits32", On::Float, "", "", "u32"),
    r("bits_as_f64", On::Int, "", "", "f64"),
    r("bits_as_f32", On::Int, "", "", "f32"),
    // Every scalar copies and compares itself.
    r("clone", On::Number, "", "", "S"),
    r("clone", On::Char, "", "", "S"),
    r("eq", On::Int, "", "y: S", "bool"),
    r("eq", On::Char, "", "y: S", "bool"),
    r("cmp", On::Int, "", "y: S", "Ordering"),
    r("cmp", On::Char, "", "y: S", "Ordering"),
    // Characters.
    r("is_digit", On::Char, "", "radix: u32", "bool"),
    r("to_digit", On::Char, "", "radix: u32", "Option[u32]"),
    r("len_utf8", On::Char, "", "", "i64"),
    r("eq_ignore_ascii_case", On::Ascii, "", "o: S", "bool"),
    r("is_ascii_hexdigit", On::Ascii, "", "", "bool"),
    r("is_ascii", On::Char, "", "", "bool"),
    r("to_uppercase to_lowercase", On::Char, "", "", "char"),
    // Text (docs/library/strings.md).
    r("as_str", On::Text, "", "", "Str"),
    r("char_count", On::Text, "", "", "i64"),
    r("char_at", On::Text, "", "i: i64", "Option[char]"),
    r("find", On::Text, "", "p: Str", "Option[i64]"),
    r("eq_ignore_ascii_case", On::Text, "", "o: Str", "bool"),
    r(
        "strip_prefix strip_suffix",
        On::Text,
        "",
        "p: Str",
        "Option[Str]",
    ),
    r("repeat", On::Text, "", "n: i64", "String"),
    r("replace", On::Text, "", "from: Str, to: Str", "String"),
    r(
        "replacen",
        On::Text,
        "",
        "from: Str, to: Str, n: i64",
        "String",
    ),
    r("chars", On::Text, "", "", "impl Iterator[Item = char]"),
    r(
        "lines split_whitespace",
        On::Text,
        "",
        "",
        "impl Iterator[Item = Str]",
    ),
    r(
        "split",
        On::Text,
        "",
        "sep: Str",
        "impl Iterator[Item = Str]",
    ),
    r("bytes", On::Text, "", "", "Slice[u8]"),
    r("clone", On::Text, "", "", "String"),
    r(
        "to_ascii_uppercase to_ascii_lowercase",
        On::Text,
        "",
        "",
        "String",
    ),
    r("cmp", On::Text, "", "o: Str", "Ordering"),
    r("substring", On::Text, "", "a: i64, b: i64", "String"),
    rm("push", On::String, "", "c: char", ""),
    rm("push_str", On::String, "", "t: Str", ""),
    rm("pop", On::String, "", "", "Option[char]"),
    rm("clear", On::String, "", "", ""),
    rm("truncate", On::String, "", "n: i64", ""),
    rm("reserve", On::String, "", "n: i64", ""),
    // Vec (docs/library/collections.md).
    r("windows chunks", On::Vec, "[T]", "n: i64", "Vec[Slice[T]]"),
    r("get", On::Vec, "[T]", "i: i64", "Option[ref T]"),
    r("last", On::Vec, "[T]", "", "Option[ref T]"),
    r("is_sorted", On::Vec, "[T: Ord]", "", "bool"),
    r("to_vec", On::Vec, "[T: Clone]", "", "Vec[T]"),
    r("split_at", On::Vec, "[T]", "i: i64", "(Slice[T], Slice[T])"),
    r("iter", On::Vec, "[T]", "", "impl Iterator[Item = ref T]"),
    r("join", On::Vec, "[T: Display]", "sep: Str", "String"),
    rm("push", On::Vec, "[T]", "v: own T", ""),
    rm("pop", On::Vec, "[T]", "", "Option[T]"),
    rm("insert", On::Vec, "[T]", "i: i64, v: own T", ""),
    rm("remove", On::Vec, "[T]", "i: i64", "T"),
    rm("truncate", On::Vec, "[T]", "n: i64", ""),
    rm("swap", On::Vec, "[T]", "i: i64, j: i64", ""),
    rm("reverse", On::Vec, "[T]", "", ""),
    rm("rotate_left rotate_right", On::Vec, "[T]", "n: i64", ""),
    rm("sort", On::Vec, "[T: Ord]", "", ""),
    // VecDeque.
    r("front back", On::Deque, "[T]", "", "Option[ref T]"),
    r("get", On::Deque, "[T]", "i: i64", "Option[ref T]"),
    r("iter", On::Deque, "[T]", "", "impl Iterator[Item = ref T]"),
    r("clone", On::Deque, "[T: Clone]", "", "VecDeque[T]"),
    rm("push_back push_front", On::Deque, "[T]", "v: own T", ""),
    rm("pop_back pop_front", On::Deque, "[T]", "", "Option[T]"),
    rm("insert", On::Deque, "[T]", "i: i64, v: own T", ""),
    rm("remove", On::Deque, "[T]", "i: i64", "T"),
    rm("truncate", On::Deque, "[T]", "n: i64", ""),
    rm("swap", On::Deque, "[T]", "i: i64, j: i64", ""),
    rm("clear", On::Deque, "[T]", "", ""),
    // Map.
    r("get", On::Map, "[K: Hash + Eq, V]", "k: K", "Option[ref V]"),
    r(
        "keys",
        On::Map,
        "[K: Hash + Eq, V]",
        "",
        "impl Iterator[Item = ref K]",
    ),
    r(
        "values",
        On::Map,
        "[K: Hash + Eq, V]",
        "",
        "impl Iterator[Item = ref V]",
    ),
    r("entries", On::Map, "[K: Hash + Eq, V]", "", "Vec[(K, V)]"),
    rm(
        "insert",
        On::Map,
        "[K: Hash + Eq, V]",
        "k: own K, v: own V",
        "Option[V]",
    ),
    rm("remove", On::Map, "[K: Hash + Eq, V]", "k: K", "Option[V]"),
    rm("clear", On::Map, "[K: Hash + Eq, V]", "", ""),
    rm("reserve", On::Map, "[K: Hash + Eq, V]", "n: i64", ""),
    // Set.
    r(
        "iter",
        On::Set,
        "[T: Hash + Eq]",
        "",
        "impl Iterator[Item = ref T]",
    ),
    rm("insert", On::Set, "[T: Hash + Eq]", "v: own T", "bool"),
    rm("remove", On::Set, "[T: Hash + Eq]", "v: T", "bool"),
    rm("clear", On::Set, "[T: Hash + Eq]", "", ""),
    rm("reserve", On::Set, "[T: Hash + Eq]", "n: i64", ""),
    r(
        "is_disjoint is_subset is_superset",
        On::Set,
        "[T: Hash + Eq]",
        "o: Set[T]",
        "bool",
    ),
    // SortedMap.
    r(
        "range range_inclusive",
        On::SortedMap,
        "[K: Ord, V]",
        "lo: K, hi: K",
        "impl Iterator[Item = (ref K, ref V)]",
    ),
    r("get", On::SortedMap, "[K: Ord, V]", "k: K", "Option[ref V]"),
    r(
        "keys",
        On::SortedMap,
        "[K: Ord, V]",
        "",
        "impl Iterator[Item = ref K]",
    ),
    r(
        "values",
        On::SortedMap,
        "[K: Ord, V]",
        "",
        "impl Iterator[Item = ref V]",
    ),
    r("entries", On::SortedMap, "[K: Ord, V]", "", "Vec[(K, V)]"),
    r(
        "min max",
        On::SortedMap,
        "[K: Ord, V]",
        "",
        "Option[(K, V)]",
    ),
    rm(
        "insert",
        On::SortedMap,
        "[K: Ord, V]",
        "k: own K, v: own V",
        "Option[V]",
    ),
    rm("remove", On::SortedMap, "[K: Ord, V]", "k: K", "Option[V]"),
    rm("clear", On::SortedMap, "[K: Ord, V]", "", ""),
    // SortedSet.
    r(
        "range range_inclusive",
        On::SortedSet,
        "[T: Ord]",
        "lo: T, hi: T",
        "impl Iterator[Item = ref T]",
    ),
    r(
        "is_disjoint is_subset is_superset",
        On::SortedSet,
        "[T: Ord]",
        "o: SortedSet[T]",
        "bool",
    ),
    rm("insert", On::SortedSet, "[T: Ord]", "v: own T", "bool"),
    rm("remove", On::SortedSet, "[T: Ord]", "v: T", "bool"),
];

/// The receiver type a rule's `S` stands for.
fn receiver(ty: &str) -> String {
    match ty {
        "Vec" | "VecDeque" => format!("{ty}[T]"),
        "Set" | "SortedSet" => format!("{ty}[T]"),
        "Map" | "SortedMap" => format!("{ty}[K, V]"),
        _ => ty.to_string(),
    }
}

/// The unsigned integer type of `ty`'s width.
fn unsigned_twin(ty: &str) -> &str {
    match ty {
        "i8" => "u8",
        "i16" => "u16",
        "i32" => "u32",
        "i64" => "u64",
        "i128" => "u128",
        "isize" => "usize",
        _ => ty,
    }
}

/// The byte width of a fixed-width integer type.
fn byte_width(ty: &str) -> &str {
    match ty {
        "i8" | "u8" => "1",
        "i16" | "u16" => "2",
        "i32" | "u32" => "4",
        "i64" | "u64" => "8",
        "i128" | "u128" => "16",
        _ => "8",
    }
}

/// Fill a rule's placeholders for `ty`. Only whole words are replaced, so
/// `Str` and `Slice` keep their `S`.
fn fill(text: &str, ty: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        match word.as_str() {
            "S" => out.push_str(&receiver(ty)),
            "U" => out.push_str(unsigned_twin(ty)),
            "N" => out.push_str(byte_width(ty)),
            w => out.push_str(w),
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// The rule that covers `ty.method`, if one does.
fn rule(ty: &str, method: &str) -> Option<&'static Rule> {
    RULES
        .iter()
        .find(|r| r.on.covers(ty) && r.methods.split_whitespace().any(|m| m == method))
}

/// Kāra source for `method_value_{ty}_{method}`, the function `ty.method`
/// named as a value stands for, when a family covers it.
pub fn wrapper_source(ty: &str, method: &str) -> Option<String> {
    let rule = rule(ty, method)?;
    let params = fill(rule.params, ty);
    let names: Vec<&str> = params
        .split(',')
        .filter_map(|p| p.split(':').next())
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .collect();
    let mut list = format!("x: {}{}", rule.recv, receiver(ty));
    if !params.trim().is_empty() {
        list.push_str(", ");
        list.push_str(&params);
    }
    let ret = match rule.ret {
        "" => String::new(),
        ret => format!(" -> {}", fill(ret, ty)),
    };
    Some(format!(
        "\nfn method_value_{ty}_{method}{generics}({list}){ret} {{\n    x.{method}({args})\n}}\n",
        generics = rule.generics,
        args = names.join(", "),
    ))
}

/// The built-in type names a wrapper can be generated for.
pub const TYPES: &[&str] = &[
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "f32",
    "f64",
    "char",
    "String",
    "Str",
    "Vec",
    "VecDeque",
    "Map",
    "Set",
    "SortedMap",
    "SortedSet",
];

/// The wrappers a program whose tokens are `toks` needs: one for each
/// `T.m` it writes that is not a call (`T.m(..)` is the receiver call and
/// needs none), whose method a family covers, and that `library` (the
/// hand-written wrappers) does not already define. Each pair once.
pub fn wrappers_for(toks: &[String], library: &str) -> String {
    let mut seen: std::collections::BTreeSet<(&str, &str)> = Default::default();
    for (i, w) in toks.windows(3).enumerate() {
        let (ty, dot, m) = (w[0].as_str(), w[1].as_str(), w[2].as_str());
        let called = toks.get(i + 3).is_some_and(|t| t == "(");
        if dot != "." || called || !TYPES.contains(&ty) {
            continue;
        }
        seen.insert((ty, m));
    }
    let mut out = String::new();
    for (ty, m) in seen {
        let name = format!("fn method_value_{ty}_{m}");
        if library.contains(&format!("{name}(")) || library.contains(&format!("{name}[")) {
            continue;
        }
        if let Some(src) = wrapper_source(ty, m) {
            out.push_str(&src);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_add_on_i32_takes_and_gives_its_type() {
        assert_eq!(
            wrapper_source("i32", "checked_add").as_deref(),
            Some("\nfn method_value_i32_checked_add(x: i32, y: i32) -> Option[i32] {\n    x.checked_add(y)\n}\n")
        );
    }

    #[test]
    fn a_placeholder_is_replaced_only_as_a_whole_word() {
        assert_eq!(
            fill("Option[Str], Slice[S]", "u8"),
            "Option[Str], Slice[u8]"
        );
    }

    #[test]
    fn a_family_covers_only_its_types() {
        assert!(wrapper_source("u8", "checked_abs").is_none());
        assert!(wrapper_source("i128", "isqrt").is_none());
        assert!(wrapper_source("usize", "to_be_bytes").is_none());
    }

    #[test]
    fn a_call_form_or_a_hand_written_wrapper_generates_nothing() {
        let toks: Vec<String> = [
            "i64",
            ".",
            "abs",
            ";",
            "i64",
            ".",
            "checked_add",
            ";",
            "i64",
            ".",
            "checked_sub",
            "(",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let out = wrappers_for(&toks, "fn method_value_i64_abs(x: i64) -> i64 {");
        assert!(!out.contains("method_value_i64_abs"));
        assert!(out.contains("fn method_value_i64_checked_add("));
        assert!(!out.contains("checked_sub"));
    }
}
