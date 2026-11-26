//! Names, modules and methods that docs/deferred.md specifies and 0.8 does
//! not have. A program that reaches for one is refused by the ordinary
//! resolver or typechecker error (an undefined name, an unknown module, no
//! such method); this table adds which deferred.md section holds the
//! feature, so the refusal says the construct is scheduled rather than
//! misspelled (conformance/deferred/SENTENCE_MAP.md).
//!
//! Deferred attributes are the attribute validator's own
//! (`attribute_validator::DEFERRED_ATTRIBUTES`, E_ATTRIBUTE_DEFERRED).
//!
//! Only names that nothing in 0.8 declares belong here: the note is added to
//! an error that already refused the program, never to an accepted one.

/// Type and value names, with the deferred.md section that specifies them.
const NAMES: &[(&str, &str)] = &[
    ("CircularBuffer", "CircularBuffer[T]"),
    (
        "OrderedMap",
        "`OrderedMap[K, V]` / `OrderedSet[T]` — Insertion-Ordered Collections",
    ),
    (
        "OrderedSet",
        "`OrderedMap[K, V]` / `OrderedSet[T]` — Insertion-Ordered Collections",
    ),
    ("Complex", "`Complex[T]` — Complex Number Type"),
    ("CtU8", "Constant-Time Integer Types (`CtU64`, `CtBool`)"),
    ("CtU32", "Constant-Time Integer Types (`CtU64`, `CtBool`)"),
    ("CtU64", "Constant-Time Integer Types (`CtU64`, `CtBool`)"),
    ("CtI32", "Constant-Time Integer Types (`CtU64`, `CtBool`)"),
    ("CtI64", "Constant-Time Integer Types (`CtU64`, `CtBool`)"),
    ("CtBool", "Constant-Time Integer Types (`CtU64`, `CtBool`)"),
    (
        "Untrusted",
        "Taint Tracking (`Untrusted[T]` / `Validated[T]`)",
    ),
    (
        "Validated",
        "Taint Tracking (`Untrusted[T]` / `Validated[T]`)",
    ),
    ("Signal", "Processes beyond v1"),
    (
        "Call",
        "User-Defined Callable Types (`Call` trait / `apply`)",
    ),
];

/// Modules (`import std.fft;`).
const MODULES: &[(&str, &str)] = &[
    ("std.fft", "`std.fft` — Fourier Transforms"),
    ("std.linalg", "`std.linalg` — Linear Algebra Suite"),
    ("std.einsum", "`std.einsum` — Einstein Summation"),
    (
        "std.nn",
        "Neural Network Framework (`std.nn` / `std.optim`) — Decision Deferred",
    ),
    (
        "std.optim",
        "Neural Network Framework (`std.nn` / `std.optim`) — Decision Deferred",
    ),
    (
        "std.terminal",
        "Terminal Control Library (`std.terminal` or `kara-terminal`)",
    ),
];

/// Items of a module that exists (`import std.random.{normal};`).
const ITEMS: &[(&str, &str, &str)] = &[
    (
        "std.random",
        "normal",
        "`std.random` — Distribution Extensions",
    ),
    (
        "std.random",
        "uniform",
        "`std.random` — Distribution Extensions",
    ),
    (
        "std.random",
        "binomial",
        "`std.random` — Distribution Extensions",
    ),
    (
        "std.random",
        "poisson",
        "`std.random` — Distribution Extensions",
    ),
    (
        "std.random",
        "exponential",
        "`std.random` — Distribution Extensions",
    ),
    ("std.io", "npy", "`.npy` / `.npz` Array File I/O"),
];

/// Methods of 0.8 types (`f.read_at(..)`).
const METHODS: &[(&str, &str, &str)] = &[
    (
        "File",
        "read_at",
        "Positional File I/O and Advisory Locking",
    ),
    (
        "File",
        "write_at",
        "Positional File I/O and Advisory Locking",
    ),
    ("File", "lock", "Positional File I/O and Advisory Locking"),
    (
        "File",
        "try_lock",
        "Positional File I/O and Advisory Locking",
    ),
    ("File", "unlock", "Positional File I/O and Advisory Locking"),
    ("UdpSocket", "join_multicast_v4", "Networking beyond v1"),
    ("UdpSocket", "join_multicast_v6", "Networking beyond v1"),
    ("UdpSocket", "set_broadcast", "Networking beyond v1"),
    (
        "Stdin",
        "read_line_bytes",
        "Bytes-Level Stdin and Typed Buffering",
    ),
];

fn note(section: &str) -> String {
    format!(": it is deferred (docs/deferred.md § {section}) and not in 0.8")
}

/// The text between the first pair of `open` / `close` delimiters after
/// `prefix`, when `message` starts with `prefix`.
fn quoted<'a>(message: &'a str, prefix: &str, close: char) -> Option<&'a str> {
    let rest = message.strip_prefix(prefix)?;
    rest.find(close).map(|end| &rest[..end])
}

/// The deferred.md section a refusal's subject belongs to, read off the
/// opening words of the resolver's and typechecker's messages.
fn section_for(message: &str) -> Option<&'static str> {
    for prefix in ["undefined name '", "undefined type '"] {
        if let Some(name) = quoted(message, prefix, '\'') {
            return NAMES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s);
        }
    }
    if let Some(module) = quoted(message, "unknown module `", '`') {
        return MODULES.iter().find(|(m, _)| *m == module).map(|(_, s)| *s);
    }
    if let Some(item) = quoted(message, "unknown item `", '`') {
        let module = message
            .split(" in module `")
            .nth(1)
            .and_then(|rest| rest.split('`').next())?;
        return ITEMS
            .iter()
            .find(|(m, i, _)| *m == module && *i == item)
            .map(|(_, _, s)| *s);
    }
    if let Some(method) = quoted(message, "no method '", '\'') {
        let ty = message
            .split(" on type '")
            .nth(1)
            .and_then(|rest| rest.split('\'').next())?;
        return METHODS
            .iter()
            .find(|(t, m, _)| *t == ty && *m == method)
            .map(|(_, _, s)| *s);
    }
    None
}

/// Adds the deferred.md section to a refusal whose subject is a deferred
/// name. A "did you mean" guess is dropped first: `std.fft` is not a
/// misspelling of `std.net`.
/// Says whether it did, so a caller drops any rename it had offered.
pub(crate) fn annotate(message: &mut String) -> bool {
    let Some(section) = section_for(message) else {
        return false;
    };
    if message.contains("docs/deferred.md") {
        return false;
    }
    if let Some(at) = message.find(", did you mean") {
        message.truncate(at);
    }
    // An unknown item's message goes on to list the module's items: the
    // section belongs right after the item and its module.
    let at = match message.find(" in module `") {
        Some(start) if message.starts_with("unknown item `") => message[start + 12..]
            .find('`')
            .map_or(message.len(), |end| start + 12 + end + 1),
        _ => message.len(),
    };
    message.insert_str(at, &note(section));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annotated(m: &str) -> String {
        let mut s = m.to_string();
        annotate(&mut s);
        s
    }

    #[test]
    fn deferred_names_modules_items_and_methods_name_their_section() {
        assert_eq!(
            annotated("undefined name 'CircularBuffer'"),
            "undefined name 'CircularBuffer': it is deferred (docs/deferred.md § \
             CircularBuffer[T]) and not in 0.8"
        );
        assert!(
            annotated("unknown module `std.fft`, did you mean `std.net`?")
                .starts_with("unknown module `std.fft`: it is deferred")
        );
        assert_eq!(
            annotated("unknown item `normal` in module `std.random`; available: next_u64"),
            "unknown item `normal` in module `std.random`: it is deferred (docs/deferred.md § \
             `std.random` — Distribution Extensions) and not in 0.8; available: next_u64"
        );
        assert!(annotated("no method 'read_at' on type 'File'")
            .contains("§ Positional File I/O and Advisory Locking"));
    }

    #[test]
    fn other_refusals_are_left_alone() {
        for m in [
            "undefined name 'frobnicate'",
            "unknown module `std.nope`",
            "no method 'read_at' on type 'Vec'",
            "unknown item `uuid` in module `std.random`",
        ] {
            assert_eq!(annotated(m), m);
        }
    }
}
