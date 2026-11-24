//! C's and C++'s first-day habits, mended where the Kāra form computes the
//! same value (B-2026-11-24-31, the fourth habit sweep): `#include`,
//! `printf` with a format, `std::cout << .. << std::endl`, `p->x`,
//! `std::sort(v.begin(), v.end())`, typed locals and parameters
//! (`std::vector<int> v`, `const std::string& s`, `auto x`), C struct
//! fields and initializers, `++i`, `for (;;)`, `int main() { .. return 0; }`.
//!
//! The rule is the earlier sweeps': a habit whose Kāra form means the same
//! thing is refused with that form and carries the edit; one whose meaning
//! differs (`printf("%e")`, `std::to_string(1.5)`, a reference variable, a
//! raw pointer) is told, with no edit.

use crate::token::Token;

use super::Parser;

/// How a C or C++ type is passed: by value, or through a `&` / `*`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CPass {
    Value,
    Ref,
    Ptr,
}

/// A C or C++ type written at a token: where it ends, its Kāra form
/// (`None` for `auto`), whether it is `const`, and how it is passed.
pub(super) struct CType {
    pub end: usize,
    pub kara: Option<String>,
    pub konst: bool,
    pub pass: CPass,
    /// The type is one only C or C++ writes (`auto`, `const`, `std::`,
    /// `unsigned`, `size_t`, a `&` or `*`): Java's own forms are left to
    /// Java's mends.
    pub c_only: bool,
}

/// C's and C++'s scalar type names, as Kāra writes them.
fn c_scalar(words: &str) -> Option<&'static str> {
    Some(match words {
        "int" | "signed" | "signed int" | "int32_t" => "i32",
        "unsigned" | "unsigned int" | "uint32_t" => "u32",
        "long" | "long int" | "long long" | "long long int" | "int64_t" => "i64",
        "unsigned long" | "unsigned long long" | "unsigned long int" | "uint64_t" => "u64",
        // Kāra's lengths and indices are `i64`, so `size_t i < v.len()`
        // compares like with like.
        "size_t" => "i64",
        "short" | "short int" | "int16_t" => "i16",
        "unsigned short" | "uint16_t" => "u16",
        "int8_t" | "signed char" => "i8",
        "uint8_t" | "unsigned char" => "u8",
        "double" | "long double" => "f64",
        "float" => "f32",
        "bool" => "bool",
        "char" => "char",
        "string" => "String",
        _ => return None,
    })
}

/// A C++ container's Kāra name: `std::map` is ordered, as `SortedMap` is.
fn c_container(name: &str) -> Option<&'static str> {
    Some(match name {
        "vector" => "Vec",
        "map" => "SortedMap",
        "unordered_map" => "Map",
        "set" => "SortedSet",
        "unordered_set" => "Set",
        "deque" => "VecDeque",
        _ => return None,
    })
}

/// The Kāra form of a C++ type's text with no `const`, `&` or `*`:
/// `std::vector<std::string>` -> `Vec[String]`.
fn c_type_text(text: &str) -> Option<String> {
    let t = text.trim().replace("std::", "");
    if let Some(open) = t.find('<') {
        let head = c_container(t[..open].trim())?;
        let inner = t[open + 1..].trim_end().strip_suffix('>')?;
        let mut depth = 0;
        let mut parts = Vec::new();
        let mut start = 0;
        for (i, c) in inner.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(c_type_text(&inner[start..i])?);
                    start = i + 1;
                }
                _ => {}
            }
        }
        parts.push(c_type_text(&inner[start..])?);
        return Some(format!("{head}[{}]", parts.join(", ")));
    }
    let words = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(k) = c_scalar(&words) {
        return Some(k.to_string());
    }
    // A program's own type, `P`.
    (words.starts_with(|c: char| c.is_ascii_uppercase())
        && words.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    .then_some(words)
}

/// The zero a C++ container of numbers is filled with: `vector<int> v(n)`.
fn c_zero(kara: &str) -> Option<&'static str> {
    Some(match kara {
        "i32" => "0_i32",
        "i64" => "0",
        "u32" => "0_u32",
        "u64" => "0_u64",
        "i16" => "0_i16",
        "u16" => "0_u16",
        "i8" => "0_i8",
        "u8" => "0_u8",
        "f64" => "0.0",
        "f32" => "0.0_f32",
        "bool" => "false",
        _ => return None,
    })
}

const SCALAR_WORDS: &[&str] = &[
    "unsigned", "signed", "long", "short", "int", "char", "double", "float",
];

impl Parser {
    /// A C or C++ type at `k`, as [`CType`].
    pub(super) fn c_type_at(&self, k: usize) -> Option<CType> {
        let mut i = k;
        let mut konst = false;
        let mut c_only = false;
        if matches!(self.tok_at(i), Some(Token::Const)) {
            konst = true;
            c_only = true;
            i += 1;
        }
        let base_start = i;
        let mut auto = false;
        if self.ident_at(i) == Some("std") && matches!(self.tok_at(i + 1), Some(Token::ColonColon))
        {
            c_only = true;
            i += 2;
            self.ident_at(i)?;
            i += 1;
        } else if self.ident_at(i) == Some("auto") {
            auto = true;
            c_only = true;
            i += 1;
        } else if self.ident_at(i).is_some_and(|w| SCALAR_WORDS.contains(&w)) {
            while self.ident_at(i).is_some_and(|w| SCALAR_WORDS.contains(&w)) {
                i += 1;
            }
            let words = self.tokens_text(base_start, i)?;
            // Java's own words are left to Java's mends.
            if !matches!(
                words.as_str(),
                "int" | "long" | "short" | "double" | "float" | "char"
            ) {
                c_only = true;
            }
        } else {
            let w = self.ident_at(i)?;
            if c_scalar(w).is_some() || c_container(w).is_some() {
                c_only = true;
            } else if !w.starts_with(|c: char| c.is_ascii_uppercase()) {
                return None;
            }
            i += 1;
        }
        // `<..>`, whose arguments may be `std::` paths.
        if matches!(self.tok_at(i), Some(Token::LessThan)) {
            let mut depth = 0i32;
            loop {
                match self.tok_at(i)? {
                    Token::LessThan => depth += 1,
                    Token::GreaterThan => depth -= 1,
                    Token::GreaterGreater => depth -= 2,
                    Token::Identifier { .. } | Token::Comma | Token::ColonColon => {}
                    _ => return None,
                }
                i += 1;
                if depth <= 0 {
                    if depth < 0 {
                        return None;
                    }
                    break;
                }
            }
        }
        let base_end = i;
        if matches!(self.tok_at(i), Some(Token::Const)) {
            konst = true;
            c_only = true;
            i += 1;
        }
        let pass = match self.tok_at(i) {
            Some(Token::Amp) => {
                i += 1;
                CPass::Ref
            }
            Some(Token::Star) => {
                i += 1;
                if matches!(self.tok_at(i), Some(Token::Star)) {
                    i += 1;
                }
                CPass::Ptr
            }
            _ => CPass::Value,
        };
        if pass != CPass::Value {
            c_only = true;
        }
        let kara = if auto {
            None
        } else {
            Some(c_type_text(&self.tokens_text(base_start, base_end)?)?)
        };
        Some(CType {
            end: i,
            kara,
            konst,
            pass,
            c_only,
        })
    }

    /// The whole-file pass for C and C++: habits whose shape the tokens
    /// alone tell, wherever they sit.
    pub(super) fn mend_c_cpp_tokens(&mut self) {
        if self.source.is_none() {
            return;
        }
        let mut i = 0;
        while i < self.tokens.len() {
            if !self.mend_c_cpp_token_at(i) {
                i += 1;
            }
        }
    }

    fn mend_c_cpp_token_at(&mut self, i: usize) -> bool {
        // `#include <stdio.h>` / `#include "x.h"`.
        if matches!(self.tok_at(i), Some(Token::Pound))
            && self.ident_at(i + 1) == Some("include")
            && self.touching(i, i + 1)
        {
            let line = self.span_at(i).line;
            let end = match self.tok_at(i + 2) {
                Some(Token::StringLiteral(_)) => Some(i + 2),
                Some(Token::LessThan) => (i + 3..self.tokens.len())
                    .take_while(|&k| self.span_at(k).line == line)
                    .find(|&k| matches!(self.tok_at(k), Some(Token::GreaterThan))),
                _ => None,
            };
            if let Some(end) = end {
                let at = self.span_at(i);
                let last = self.span_at(end);
                self.habit(
                    "`#include` is C's and C++'s: the Kāra library's names are in scope as \
                     they are, and a module of it is imported with `import std.<module>`",
                    at,
                    vec![Self::edit(
                        at.offset,
                        last.offset + last.length - at.offset,
                        "",
                    )],
                );
                self.tokens.drain(i..=end);
                return true;
            }
        }
        // `p->x` / `p->f()`: a member through a pointer. Kāra's `->` only
        // follows a `)` or a `|` (a result type), never a name.
        if matches!(self.tok_at(i), Some(Token::Arrow))
            && i > 0
            && matches!(
                self.tok_at(i - 1),
                Some(Token::Identifier { .. } | Token::SelfValue | Token::RightBracket)
            )
            && self.ident_at(i + 1).is_some()
            && self.touching(i - 1, i)
        {
            let at = self.span_at(i);
            self.habit(
                "`p->x` is C's and C++'s member access through a pointer: Kāra reaches a \
                 field or a method the same way through a value or a reference, `p.x`",
                at,
                vec![Self::edit(at.offset, at.length, ".")],
            );
            self.tokens[i].token = Token::Dot;
            return false;
        }
        // `p: P*` / `p: P&`: a pointer or reference mark on a parameter's
        // type, which Kāra lends by default.
        if matches!(self.tok_at(i), Some(Token::Star | Token::Amp))
            && matches!(self.tok_at(i + 1), Some(Token::Comma | Token::RightParen))
            && i > 1
            && self.ident_at(i - 1).is_some()
            && matches!(self.tok_at(i - 2), Some(Token::Colon))
        {
            let at = self.span_at(i);
            self.habit(
                "a `*` or `&` after a type is C's and C++'s: a Kāra parameter is lent by \
                 default, so `p: P` takes it without a copy",
                at,
                vec![Self::edit(at.offset, at.length, "")],
            );
            self.tokens.remove(i);
            return true;
        }
        // `P { .x = 1, .y = 2 }`: C's designated initializer.
        if matches!(self.tok_at(i), Some(Token::Dot))
            && self.ident_at(i + 1).is_some()
            && matches!(self.tok_at(i + 2), Some(Token::Equal))
            && !matches!(self.tok_at(i + 3), Some(Token::Equal))
            && i > 0
            && self.designated_brace(i - 1)
        {
            let at = self.span_at(i);
            let eq = self.span_at(i + 2);
            let name = self.ident_at(i + 1).unwrap_or_default().to_string();
            self.habit(
                &format!(
                    "`.{name} = v` is C's designated initializer: a Kāra struct literal names \
                     the field as `{name}: v`"
                ),
                at,
                vec![Self::edit(
                    at.offset,
                    eq.offset + eq.length - at.offset,
                    &format!("{name}:"),
                )],
            );
            self.tokens[i + 2].token = Token::Colon;
            self.tokens.remove(i);
            return true;
        }
        let after_dot = i > 0 && matches!(self.tok_at(i - 1), Some(Token::Dot | Token::ColonColon));
        let Some(word) = self.ident_at(i).map(str::to_string) else {
            return false;
        };
        if after_dot {
            return false;
        }
        // `std::cout << ..;`, `std::sort(..)`, `std::to_string(..)`: the
        // name after an optional `std::`.
        let (name_at, std) =
            if word == "std" && matches!(self.tok_at(i + 1), Some(Token::ColonColon)) {
                (i + 2, true)
            } else {
                (i, false)
            };
        let Some(name) = self.ident_at(name_at).map(str::to_string) else {
            return false;
        };
        if !matches!(
            name.as_str(),
            "cout" | "cerr" | "printf" | "puts" | "sort" | "reverse" | "to_string"
        ) || !std && self.word_is_bound(&name)
        {
            return false;
        }
        match name.as_str() {
            "cout" | "cerr" if matches!(self.tok_at(name_at + 1), Some(Token::LessLess)) => {
                self.mend_cout(i, name_at, name == "cerr")
            }
            "printf" | "puts" if matches!(self.tok_at(name_at + 1), Some(Token::LeftParen)) => {
                self.mend_printf(i, name_at, name == "puts")
            }
            "sort" | "reverse" if matches!(self.tok_at(name_at + 1), Some(Token::LeftParen)) => {
                self.mend_std_range_call(i, name_at, &name)
            }
            "to_string" if std && matches!(self.tok_at(name_at + 1), Some(Token::LeftParen)) => {
                let at = self.span_at(i);
                self.error_at(
                    "`std::to_string(x)` is C++'s: Kāra's is `x.to_string()`; a float prints \
                     its shortest form there (`1.5`), where C++ prints six places \
                     (`1.500000`)",
                    at,
                );
                // Told: the `std::` goes too, so no path edit is offered.
                self.tokens.drain(i..name_at);
                false
            }
            _ => false,
        }
    }

    /// `std::cout << "x=" << x << std::endl;` -> `println(f"x={x}")`.
    fn mend_cout(&mut self, i: usize, name_at: usize, err: bool) -> bool {
        // The operands, `<<`-separated, up to the `;`.
        let mut parts: Vec<(usize, usize)> = Vec::new();
        let mut depth = 0i32;
        let mut start = name_at + 2;
        let mut k = start;
        let semi = loop {
            match self.tok_at(k) {
                Some(Token::LeftParen | Token::LeftBracket | Token::LeftBrace) => depth += 1,
                Some(Token::RightParen | Token::RightBracket | Token::RightBrace) => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                }
                Some(Token::LessLess) if depth == 0 => {
                    parts.push((start, k));
                    start = k + 1;
                }
                Some(Token::Semicolon) if depth == 0 => break k,
                Some(Token::EOF) | None => return false,
                _ => {}
            }
            k += 1;
        };
        parts.push((start, semi));
        let at = self.span_at(i);
        let lang = "`std::cout << ..` is C++'s stream output";
        let mut text = String::new();
        let mut holes = 0;
        let mut ok = !parts.iter().any(|&(a, b)| a >= b);
        for &(a, b) in &parts {
            if !ok {
                break;
            }
            let endl = (b == a + 1 && self.ident_at(a) == Some("endl"))
                || (b == a + 3
                    && self.ident_at(a) == Some("std")
                    && self.ident_at(a + 2) == Some("endl"));
            if endl {
                text.push_str("\\n");
                continue;
            }
            if b == a + 1 {
                if let Some(Token::StringLiteral(_)) = self.tok_at(a) {
                    let s = self.span_at(a);
                    match self.source_between(s.offset, s.offset + s.length) {
                        Some(lit)
                            if lit.starts_with('"') && lit.ends_with('"') && lit.len() >= 2 =>
                        {
                            let body = &lit[1..lit.len() - 1];
                            text.push_str(&body.replace('{', "{{").replace('}', "}}"));
                        }
                        _ => ok = false,
                    }
                    continue;
                }
                if let Some(Token::CharLiteral(_)) = self.tok_at(a) {
                    // `'\n'` / `' '`: the character itself.
                    let s = self.span_at(a);
                    match self.source_between(s.offset, s.offset + s.length) {
                        Some(lit) if lit.len() >= 3 && lit != "'\"'" => {
                            let c = &lit[1..lit.len() - 1];
                            text.push_str(&c.replace('{', "{{").replace('}', "}}"));
                        }
                        _ => ok = false,
                    }
                    continue;
                }
            }
            match self.tokens_text(a, b).map(|e| e.replace("->", ".")) {
                Some(e) if !e.contains(['{', '}', '\n']) => {
                    text.push('{');
                    text.push_str(&e);
                    text.push('}');
                    holes += 1;
                }
                _ => ok = false,
            }
        }
        if !ok {
            self.error_at(
                &format!("{lang}: Kāra prints with `println(f\"x={{x}}\")`"),
                at,
            );
            return false;
        }
        let (body, line) = match text.strip_suffix("\\n") {
            Some(b) => (b.to_string(), true),
            None => (text.clone(), false),
        };
        let f = match (err, line) {
            (false, true) => "println",
            (false, false) => "print",
            (true, true) => "eprintln",
            (true, false) => "eprint",
        };
        let lit = if holes > 0 {
            format!("f\"{body}\"")
        } else {
            format!("\"{}\"", body.replace("{{", "{").replace("}}", "}"))
        };
        let call = format!("{f}({lit})");
        self.rewrite(
            &format!("{lang}: Kāra prints with `{call}`"),
            at,
            i,
            semi - 1,
            &call,
        );
        true
    }

    /// `printf("x=%d\n", x)` -> `println(f"x={x}")`; `puts("hi")` ->
    /// `println("hi")`. A conversion whose Kāra text differs (`%e`, `%g`,
    /// `%p`, `%c` of a number) is told.
    fn mend_printf(&mut self, i: usize, name_at: usize, puts: bool) -> bool {
        let open = name_at + 1;
        let Some((args, close)) = self.call_args(open) else {
            return false;
        };
        let at = self.span_at(i);
        let Some(&(f0, f1)) = args.first() else {
            return false;
        };
        if f1 != f0 + 1 || !matches!(self.tok_at(f0), Some(Token::StringLiteral(_))) {
            return false;
        }
        let s = self.span_at(f0);
        let Some(lit) = self.source_between(s.offset, s.offset + s.length) else {
            return false;
        };
        if !(lit.starts_with('"') && lit.ends_with('"') && lit.len() >= 2) {
            return false;
        }
        let fmt = &lit[1..lit.len() - 1];
        if puts {
            if args.len() != 1 {
                return false;
            }
            let call = format!("println(\"{fmt}\")");
            self.rewrite(
                "`puts(s)` is C's: Kāra prints a line with `println(s)`",
                at,
                i,
                close,
                &call,
            );
            return true;
        }
        let note_told = "`printf` is C's: Kāra prints with `println(f\"x={x}\")`, whose \
                         holes take `{x:>5}`, `{x:05}`, `{x:.2}` and `{x:x}`; this format has \
                         a conversion whose Kāra text differs, so it is written by hand";
        // Walk the format: literal text, and one hole per conversion.
        let mut out = String::new();
        let mut holes = Vec::new();
        let b = fmt.as_bytes();
        let mut k = 0;
        let mut bad = false;
        while k < b.len() {
            let c = b[k] as char;
            if c == '{' || c == '}' {
                out.push(c);
                out.push(c);
                k += 1;
                continue;
            }
            if c != '%' {
                let ch = fmt[k..].chars().next().unwrap_or(' ');
                out.push(ch);
                k += ch.len_utf8();
                continue;
            }
            k += 1;
            if b.get(k) == Some(&b'%') {
                out.push('%');
                k += 1;
                continue;
            }
            let mut flags = String::new();
            while let Some(&f) = b.get(k) {
                if b"-+0 #".contains(&f) {
                    flags.push(f as char);
                    k += 1;
                } else {
                    break;
                }
            }
            let mut width = String::new();
            while b.get(k).is_some_and(u8::is_ascii_digit) {
                width.push(b[k] as char);
                k += 1;
            }
            let mut prec = None;
            if b.get(k) == Some(&b'.') {
                k += 1;
                let mut p = String::new();
                while b.get(k).is_some_and(u8::is_ascii_digit) {
                    p.push(b[k] as char);
                    k += 1;
                }
                prec = Some(if p.is_empty() { "0".to_string() } else { p });
            }
            while b.get(k).is_some_and(|c| b"hlLqjzt".contains(c)) {
                k += 1;
            }
            let Some(&conv) = b.get(k) else {
                bad = true;
                break;
            };
            k += 1;
            if flags.contains([' ', '#']) {
                bad = true;
                break;
            }
            let arg = holes.len() + 1;
            let Some(&(a0, a1)) = args.get(arg) else {
                bad = true;
                break;
            };
            let mut spec = String::new();
            if !width.is_empty() {
                if flags.contains('-') {
                    spec.push('<');
                } else if !flags.contains('0') {
                    spec.push('>');
                }
            }
            if flags.contains('+') {
                spec.push('+');
            }
            if flags.contains('0') && !flags.contains('-') {
                spec.push('0');
            }
            spec.push_str(&width);
            match conv {
                b'd' | b'i' | b'u' => {
                    if prec.is_some() {
                        bad = true;
                    }
                }
                b's' => {
                    if prec.is_some() || flags.contains(['+', '0']) {
                        bad = true;
                    }
                }
                b'f' | b'F' => {
                    spec.push('.');
                    spec.push_str(prec.as_deref().unwrap_or("6"));
                }
                b'x' | b'X' | b'o' => {
                    if prec.is_some() || flags.contains('+') {
                        bad = true;
                    }
                    spec.push(conv as char);
                }
                b'c' => {
                    // A `char` prints itself; C's `%c` of a number prints
                    // the character with that code, Kāra's the number.
                    if a1 != a0 + 1 || !matches!(self.tok_at(a0), Some(Token::CharLiteral(_))) {
                        bad = true;
                    }
                }
                _ => bad = true,
            }
            if bad {
                break;
            }
            let Some(e) = self
                .tokens_text(a0, a1)
                .map(|e| e.replace("->", "."))
                .filter(|e| !e.contains(['{', '}', '\n']))
            else {
                bad = true;
                break;
            };
            out.push('{');
            out.push_str(&e);
            if !spec.is_empty() {
                out.push(':');
                out.push_str(&spec);
            }
            out.push('}');
            holes.push(e);
        }
        if bad || holes.len() + 1 != args.len() {
            self.error_at(note_told, at);
            return false;
        }
        let (body, line) = match out.strip_suffix("\\n") {
            Some(b) => (b.to_string(), true),
            None => (out.clone(), false),
        };
        let f = if line { "println" } else { "print" };
        let lit = if holes.is_empty() {
            format!("\"{}\"", body.replace("{{", "{").replace("}}", "}"))
        } else {
            format!("f\"{body}\"")
        };
        let call = format!("{f}({lit})");
        self.rewrite(
            &format!("`printf` is C's: Kāra prints with `{call}`"),
            at,
            i,
            close,
            &call,
        );
        true
    }

    /// `std::sort(v.begin(), v.end())` -> `v.sort()`, and `std::reverse`.
    fn mend_std_range_call(&mut self, i: usize, name_at: usize, name: &str) -> bool {
        let Some((args, close)) = self.call_args(name_at + 1) else {
            return false;
        };
        let range = |me: &Self, (a, b): (usize, usize), end: &str| -> Option<String> {
            (b >= a + 5
                && matches!(me.tok_at(b - 4), Some(Token::Dot))
                && me.ident_at(b - 3) == Some(end)
                && matches!(me.tok_at(b - 2), Some(Token::LeftParen))
                && matches!(me.tok_at(b - 1), Some(Token::RightParen)))
            .then(|| me.tokens_text(a, b - 4))
            .flatten()
        };
        let at = self.span_at(i);
        let ([first, last] | [first, last, _]) = args.as_slice() else {
            return false;
        };
        let (Some(v), Some(w)) = (range(self, *first, "begin"), range(self, *last, "end")) else {
            return false;
        };
        if v != w {
            return false;
        }
        if args.len() == 3 {
            self.error_at(
                &format!(
                    "`std::{name}` with a comparator is C++'s: Kāra sorts by a key with \
                     `v.sort_by(|a, b| b.cmp(a))`"
                ),
                at,
            );
            self.tokens.drain(i..name_at);
            return false;
        }
        let call = format!("{v}.{name}()");
        self.rewrite(
            &format!(
                "`std::{name}(v.begin(), v.end())` is C++'s: a Kāra list {name}s itself, \
                 `{call}`"
            ),
            at,
            i,
            close,
            &call,
        );
        true
    }

    /// Statement-start habits of C and C++: `++i;`, `auto x = v;`,
    /// `const int x = 3;`, `std::vector<int> v;`, `P p = {1, 2};`.
    pub(super) fn mend_c_cpp_statement_forms(&mut self) -> bool {
        let p = self.pos;
        if p >= self.tokens.len() {
            return false;
        }
        // `++i;` / `--i;`.
        let pre = match (self.tok_at(p), self.tok_at(p + 1)) {
            (Some(Token::Plus), Some(Token::Plus)) => Some(("++", "+= 1")),
            (Some(Token::Minus), Some(Token::Minus)) => Some(("--", "-= 1")),
            _ => None,
        };
        if let Some((sym, kara)) = pre {
            if self.touching(p, p + 1)
                && self.ident_at(p + 2).is_some()
                && matches!(
                    self.tok_at(p + 3),
                    Some(Token::Semicolon | Token::RightBrace)
                )
            {
                let name = self.ident_at(p + 2).unwrap_or_default().to_string();
                let at = self.span_at(p);
                let text = format!("{name} {kara}");
                self.rewrite(
                    &format!("`{sym}{name}` is not Kāra: a counter steps with `{text}`"),
                    at,
                    p,
                    p + 2,
                    &text,
                );
                return true;
            }
        }
        let Some(ty) = self.c_type_at(p) else {
            return false;
        };
        let n = ty.end;
        let user = self
            .ident_at(p)
            .is_some_and(|w| w.starts_with(|c: char| c.is_ascii_uppercase()));
        if !(ty.c_only || user) || self.ident_at(n).is_none() {
            return false;
        }
        let after = self.tok_at(n + 1);
        if !matches!(
            after,
            Some(Token::Equal | Token::Semicolon | Token::LeftParen)
        ) {
            return false;
        }
        if matches!(after, Some(Token::Equal)) && matches!(self.tok_at(n + 2), Some(Token::Equal)) {
            return false;
        }
        let at = self.span_at(p);
        let name = self.ident_at(n).unwrap_or_default().to_string();
        if ty.pass != CPass::Value {
            self.error_at(
                &format!(
                    "a C or C++ {} variable is an alias of another place: Kāra binds a \
                     value with `let {name} = v`, and lends a place to a function as its \
                     argument",
                    if ty.pass == CPass::Ref {
                        "reference"
                    } else {
                        "pointer"
                    }
                ),
                at,
            );
            return false;
        }
        let ann = ty
            .kara
            .as_ref()
            .map(|k| format!(": {k}"))
            .unwrap_or_default();
        let container = ty
            .kara
            .as_deref()
            .is_some_and(|k| k.ends_with(']') && !k.starts_with('['));
        let head_note = |kara: &str| {
            format!(
                "a type before the name is C's and C++'s: Kāra binds with `let {name}{kara} = \
                 v` (`let mut` when it is reassigned)"
            )
        };
        match after {
            // `T name;`
            Some(Token::Semicolon) => {
                let Some(kara) = ty.kara.clone() else {
                    return false;
                };
                let text = if container {
                    let head = kara.split('[').next().unwrap_or_default();
                    format!("let mut {name}: {kara} = {head}.new()")
                } else if kara == "String" {
                    format!("let mut {name}: String = String.new()")
                } else if c_scalar_kara(&kara) {
                    format!("let mut {name}: {kara}")
                } else {
                    self.error_at(
                        &format!(
                            "`{kara} {name};` is C's and C++'s: a Kāra struct is built with \
                             all its fields, `let {name} = {kara} {{ x: 1 }}`"
                        ),
                        at,
                    );
                    return false;
                };
                self.rewrite(&head_note(&ann), at, p, n, &text);
                true
            }
            // `std::vector<int> v(n, 0);` / `v(n)`.
            Some(Token::LeftParen) => {
                let Some(kara) = ty.kara.clone().filter(|k| k.starts_with("Vec[")) else {
                    return false;
                };
                let Some((args, close)) = self.call_args(n + 1) else {
                    return false;
                };
                if !matches!(self.tok_at(close + 1), Some(Token::Semicolon)) {
                    return false;
                }
                let elem = &kara[4..kara.len() - 1];
                let fill = match args.as_slice() {
                    [(a, b)] => self
                        .tokens_text(*a, *b)
                        .zip(c_zero(elem).map(str::to_string)),
                    [(a, b), (c, d)] => self.tokens_text(*a, *b).zip(self.tokens_text(*c, *d)),
                    _ => None,
                };
                let Some((count, value)) = fill else {
                    return false;
                };
                let text = format!("let mut {name}: {kara} = Vec.filled({count}, {value})");
                self.rewrite(
                    "`vector<T> v(n, x)` is C++'s: Kāra makes `n` copies with \
                     `Vec.filled(n, x)` (library/collections.md)",
                    at,
                    p,
                    close,
                    &text,
                );
                true
            }
            // `T name = v;`, `= {a, b}`.
            _ => {
                if matches!(self.tok_at(n + 2), Some(Token::LeftBrace)) {
                    let Some(cb) = self.matching_brace(n + 2) else {
                        return false;
                    };
                    let inner = if cb == n + 3 {
                        Some(String::new())
                    } else {
                        self.tokens_text(n + 3, cb)
                    };
                    let Some(inner) = inner else {
                        return false;
                    };
                    let kara = ty.kara.clone().unwrap_or_default();
                    let value = if kara.starts_with("Vec[") {
                        Some(format!("[{inner}]"))
                    } else if user {
                        self.c_struct_init(&kara, n + 3)
                    } else {
                        None
                    };
                    let Some(value) = value else {
                        self.error_at(
                            "a `{..}` initializer is C's and C++'s: Kāra builds a list as \
                             `[1, 2]` and a struct as `P { x: 1, y: 2 }`",
                            at,
                        );
                        return false;
                    };
                    let text = if user {
                        format!("let {name} = {value}")
                    } else {
                        format!("let {name}{ann} = {value}")
                    };
                    self.rewrite(&head_note(&ann), at, p, cb, &text);
                    return true;
                }
                let text = format!("let {name}{ann}");
                self.rewrite(&head_note(&ann), at, p, n, &text);
                true
            }
        }
    }

    /// Whether token `k` (a `{` or a `,`) is in a struct literal's braces,
    /// `P {` or C's `= {`.
    fn designated_brace(&self, k: usize) -> bool {
        let open = match self.tok_at(k) {
            Some(Token::LeftBrace) => Some(k),
            Some(Token::Comma) => {
                let mut depth = 0i32;
                (0..k).rev().find(|&j| match self.tok_at(j) {
                    Some(Token::RightBrace | Token::RightParen | Token::RightBracket) => {
                        depth += 1;
                        false
                    }
                    Some(Token::LeftParen | Token::LeftBracket) => {
                        depth -= 1;
                        false
                    }
                    Some(Token::LeftBrace) => {
                        if depth == 0 {
                            true
                        } else {
                            depth -= 1;
                            false
                        }
                    }
                    _ => false,
                })
            }
            _ => None,
        };
        open.is_some_and(|o| {
            o > 0
                && (self
                    .ident_at(o - 1)
                    .is_some_and(|w| w.starts_with(|c: char| c.is_ascii_uppercase()))
                    || matches!(self.tok_at(o - 1), Some(Token::Equal)))
        })
    }

    /// `{1, 2}` for the struct `ty` the file declares: its fields named in
    /// order, `P { x: 1, y: 2 }`. `None` when the counts differ.
    fn c_struct_init(&self, ty: &str, from: usize) -> Option<String> {
        let decl = (0..self.tokens.len()).find(|&k| {
            matches!(self.tok_at(k), Some(Token::Struct))
                && self.ident_at(k + 1) == Some(ty)
                && matches!(self.tok_at(k + 2), Some(Token::LeftBrace))
        })?;
        let close = self.matching_brace(decl + 2)?;
        // A field's name is the word before its `:` (Kāra) or `;` (C).
        let mut fields = Vec::new();
        let mut depth = 0i32;
        for k in decl + 3..close {
            match self.tok_at(k)? {
                Token::LeftParen | Token::LeftBracket | Token::LessThan => depth += 1,
                Token::RightParen | Token::RightBracket | Token::GreaterThan => depth -= 1,
                Token::Colon | Token::Semicolon if depth == 0 && k > 0 => {
                    fields.push(self.ident_at(k - 1)?.to_string());
                }
                _ => {}
            }
        }
        let (args, _) = self.call_args(from - 1)?;
        if args.len() != fields.len() || fields.is_empty() {
            return None;
        }
        let parts: Option<Vec<String>> = args
            .iter()
            .zip(&fields)
            .map(|(&(a, b), f)| self.tokens_text(a, b).map(|v| format!("{f}: {v}")))
            .collect();
        Some(format!("{ty} {{ {} }}", parts?.join(", ")))
    }

    /// A C or C++ parameter: `const std::string& s` -> `s: String`,
    /// `int& n` -> `n: mut ref i32`, `P* p` -> `p: P`.
    pub(super) fn mend_c_param(&mut self) {
        let p = self.pos;
        let Some(ty) = self.c_type_at(p) else {
            return;
        };
        let user = self
            .ident_at(p)
            .is_some_and(|w| w.starts_with(|c: char| c.is_ascii_uppercase()));
        if !(ty.c_only || user) {
            return;
        }
        let n = ty.end;
        if self.ident_at(n).is_none()
            || !matches!(self.tok_at(n + 1), Some(Token::Comma | Token::RightParen))
        {
            return;
        }
        let Some(kara) = ty.kara else {
            return;
        };
        let name = self.ident_at(n).unwrap_or_default().to_string();
        let writes = ty.pass != CPass::Value && !ty.konst;
        let text = if writes {
            format!("{name}: mut ref {kara}")
        } else {
            format!("{name}: {kara}")
        };
        let at = self.span_at(p);
        self.rewrite(
            &format!(
                "a parameter's type before its name is C's and C++'s: Kāra writes `{text}`; a \
                 parameter is lent by default, and `mut ref` lends it to be written"
            ),
            at,
            p,
            n,
            &text,
        );
    }

    /// A C struct's field `int x;`: Kāra's `x: i32,`.
    pub(super) fn mend_c_field(&mut self) {
        let p = self.pos;
        let Some(ty) = self.c_type_at(p) else {
            return;
        };
        let n = ty.end;
        if ty.pass != CPass::Value
            || self.ident_at(n).is_none()
            || !matches!(self.tok_at(n + 1), Some(Token::Semicolon))
        {
            return;
        }
        let Some(kara) = ty.kara else {
            return;
        };
        let name = self.ident_at(n).unwrap_or_default().to_string();
        let text = format!("{name}: {kara},");
        let at = self.span_at(p);
        self.rewrite(
            &format!("a C field is `int x;`: a Kāra field is `{text}`"),
            at,
            p,
            n + 1,
            &text,
        );
    }

    /// `for (;;) {`: C's endless loop, Kāra's `loop`. `true` when mended.
    pub(super) fn mend_c_forever(&mut self) -> bool {
        let p = self.pos;
        if !(matches!(self.tok_at(p), Some(Token::For))
            && matches!(self.tok_at(p + 1), Some(Token::LeftParen))
            && matches!(self.tok_at(p + 2), Some(Token::Semicolon))
            && matches!(self.tok_at(p + 3), Some(Token::Semicolon))
            && matches!(self.tok_at(p + 4), Some(Token::RightParen))
            && matches!(self.tok_at(p + 5), Some(Token::LeftBrace)))
        {
            return false;
        }
        let at = self.span_at(p);
        let end = self.span_at(p + 4);
        self.habit(
            "`for (;;)` is C's endless loop: Kāra writes `loop { .. }`",
            at,
            vec![Self::edit(
                at.offset,
                end.offset + end.length - at.offset,
                "loop",
            )],
        );
        self.tokens.drain(p + 1..=p + 4);
        self.tokens[p].token = Token::Loop;
        true
    }
}

/// A Kāra scalar type a C local may be declared without a value.
fn c_scalar_kara(k: &str) -> bool {
    matches!(
        k,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "f32"
            | "f64"
            | "bool"
            | "char"
            | "String"
    )
}
