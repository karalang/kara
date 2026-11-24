//! TypeScript's / JavaScript's and Java's / C#'s first-day habits, mended
//! where the Kāra form is one-to-one (B-2026-11-22-36's sibling sweep,
//! B-2026-11-24-8): `===`, template literals, `console.log`,
//! `System.out.println`, `Console.WriteLine`, `Math.max(a, b)`, `new`,
//! `this`, arrow functions, `c ? a : b`, `for (..; ..; ..)`, `for .. of`,
//! `switch` with `break`, Java's typed locals, parameters and headers.
//!
//! The rule is the Python and Go sweeps': a habit whose Kāra form means the
//! same thing is refused with that form and carries the edit; one whose
//! meaning differs (`throw`, `try`/`catch`, `for .. in` over keys,
//! `parseInt`) is told, with no edit. Each rewrite is also made in the token
//! stream, so the parse goes on as if the Kāra form had been written.

use crate::token::{Span, Token};

use super::Parser;

/// Java's and C#'s type names, as Kāra writes them: `int` -> `i32`,
/// `long` -> `i64`, `String[]` -> `Vec[String]`, `List<Integer>` ->
/// `Vec[i32]`. `None` for a type with no one-to-one form.
pub(super) fn java_type(text: &str) -> Option<String> {
    let t = text.trim();
    if let Some(inner) = t.strip_suffix("[]") {
        return Some(format!("Vec[{}]", java_type(inner)?));
    }
    if let Some(open) = t.find('<') {
        let inner = t[open + 1..].strip_suffix('>')?;
        let head = match &t[..open] {
            "List" | "ArrayList" | "LinkedList" | "IList" => "Vec",
            "Map" | "HashMap" | "Dictionary" | "IDictionary" => "Map",
            "TreeMap" | "SortedDictionary" => "SortedMap",
            "Set" | "HashSet" | "ISet" => "Set",
            "TreeSet" | "SortedSet" => "SortedSet",
            "Deque" | "ArrayDeque" | "Queue" => "VecDeque",
            _ => return None,
        };
        let mut depth = 0;
        let mut parts = Vec::new();
        let mut start = 0;
        for (i, c) in inner.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(java_type(&inner[start..i])?);
                    start = i + 1;
                }
                _ => {}
            }
        }
        parts.push(java_type(&inner[start..])?);
        return Some(format!("{head}[{}]", parts.join(", ")));
    }
    let mapped = match t {
        "int" | "Integer" => "i32",
        "long" | "Long" => "i64",
        "short" | "Short" => "i16",
        "byte" | "Byte" => "i8",
        "double" | "Double" => "f64",
        "float" | "Float" => "f32",
        "boolean" | "Boolean" | "bool" => "bool",
        "char" | "Character" => "char",
        "String" | "string" => "String",
        _ => return None,
    };
    Some(mapped.to_string())
}

/// Java's primitive and `String` type names: a statement or a parameter
/// that starts with one of these and then a name is Java's typed binding.
fn java_binding_type(name: &str) -> bool {
    matches!(
        name,
        "int" | "long" | "short" | "byte" | "double" | "float" | "boolean" | "char" | "String"
    )
}

impl Parser {
    /// Whether the program binds `word` itself anywhere: a `let`, a
    /// parameter, a field, an item, a closure parameter, a loop variable.
    /// A habit keyed on a name (`console`, `Math`, `this`, `new`) is never
    /// mended where the program has its own.
    pub(super) fn word_is_bound(&self, word: &str) -> bool {
        (0..self.tokens.len()).any(|i| {
            self.ident_at(i) == Some(word)
                && ((matches!(self.tok_at(i + 1), Some(Token::Colon))
                    && !matches!(self.tok_at(i + 2), Some(Token::Equal)))
                    || i > 0
                        && matches!(
                            self.tok_at(i - 1),
                            Some(
                                Token::Let
                                    | Token::Mut
                                    | Token::Fn
                                    | Token::Struct
                                    | Token::Enum
                                    | Token::Trait
                                    | Token::Type
                                    | Token::Const
                                    | Token::Impl
                                    | Token::Pipe
                                    | Token::For
                                    | Token::As
                                    | Token::Import
                            )
                        ))
        })
    }

    /// `switch x {` (Go) or `switch (x) {` (C, Java, JavaScript) at the
    /// parser's position, where `switch` is not a name the program uses
    /// (`switch = 1`, `switch(x)` as a call).
    pub(super) fn at_switch_statement(&self) -> bool {
        let p = self.pos;
        if self.ident_at(p) != Some("switch") {
            return false;
        }
        match self.tok_at(p + 1) {
            Some(Token::LeftParen) => {
                self.matching_close(p + 1)
                    .is_some_and(|c| matches!(self.tok_at(c + 1), Some(Token::LeftBrace)))
                    && !self.word_is_bound("switch")
            }
            Some(Token::Equal | Token::Dot | Token::Semicolon | Token::RightBrace) => false,
            _ => true,
        }
    }

    /// `class Name {` at the parser's position, whose body holds a method,
    /// a constructor or a function: anything followed by `(`, or `fn`.
    pub(super) fn class_has_methods(&self) -> bool {
        let brace = self.pos + 2;
        let Some(close) = self.matching_brace(brace) else {
            return false;
        };
        (brace + 1..close).any(|k| {
            matches!(self.tok_at(k), Some(Token::Fn))
                || self.ident_at(k).is_some()
                    && matches!(self.tok_at(k + 1), Some(Token::LeftParen))
        })
    }

    /// Past the `class Name { .. }` at the parser's position.
    pub(super) fn skip_class(&mut self) {
        if let Some(close) = self.matching_brace(self.pos + 2) {
            self.pos = close + 1;
        }
    }

    /// The token index after a `<..>` that opens at `open`, or `open` when
    /// none does.
    fn skip_angles(&self, open: usize) -> Option<usize> {
        if !matches!(self.tok_at(open), Some(Token::LessThan)) {
            return Some(open);
        }
        let mut depth = 0i32;
        let mut i = open;
        loop {
            match self.tok_at(i)? {
                Token::LessThan => depth += 1,
                Token::GreaterThan => depth -= 1,
                Token::GreaterGreater => depth -= 2,
                Token::Identifier { .. }
                | Token::Comma
                | Token::LeftBracket
                | Token::RightBracket => {}
                Token::Question => {}
                _ => return None,
            }
            i += 1;
            if depth <= 0 {
                return (depth == 0).then_some(i);
            }
        }
    }

    /// The whole-file pass, run once before parsing: habits whose shape is
    /// told by the tokens alone, wherever they sit.
    pub(super) fn mend_ts_java_tokens(&mut self) {
        if self.source.is_none() {
            return;
        }
        let mut i = 0;
        while i < self.tokens.len() {
            if !self.mend_ts_java_token_at(i) {
                i += 1;
            }
        }
    }

    /// One habit at token `i`; `true` when the tokens there changed (the
    /// same index is then looked at again).
    fn mend_ts_java_token_at(&mut self, i: usize) -> bool {
        let lexed = |me: &Self, at: usize, c: char| matches!(me.tok_at(at), Some(Token::Error(m)) if *m == format!("Unexpected character: '{c}'"));
        // `===` / `!==`: JavaScript's strict equality.
        if matches!(self.tok_at(i), Some(Token::EqualEqual | Token::BangEqual))
            && matches!(self.tok_at(i + 1), Some(Token::Equal))
            && self.touching(i, i + 1)
            && self.span_at(i).length == 2
        {
            let op = if matches!(self.tok_at(i), Some(Token::EqualEqual)) {
                "=="
            } else {
                "!="
            };
            let at = self.span_at(i);
            let eq = self.span_at(i + 1);
            self.habit(
                &format!(
                    "`{op}=` is JavaScript's strict comparison: Kāra's `{op}` never converts \
                     between types, so it is already strict"
                ),
                at,
                vec![Self::edit(eq.offset, eq.length, "")],
            );
            self.tokens.remove(i + 1);
            return false;
        }
        // `` `n is ${n}` ``: a template literal is an f-string.
        if lexed(self, i, '`') {
            let open = self.span_at(i);
            let close = (i + 1..self.tokens.len()).find(|&j| lexed(self, j, '`'));
            let Some(j) = close else {
                return false;
            };
            let end = self.span_at(j);
            let Some(body) = self.source_between(open.offset + 1, end.offset) else {
                return false;
            };
            let note = "a template literal is JavaScript's: Kāra's interpolated text is an \
                        f-string, `f\"n is {n}\"`";
            // The holes are `${..}`; any other brace, a quote, an escape
            // or a line break has a spelling of its own in an f-string.
            let converted = body.replace("${", "{");
            let plain = !body.contains(['"', '\\', '\n'])
                && converted.matches('{').count() == body.matches("${").count()
                && converted.matches('{').count() == converted.matches('}').count();
            if plain {
                let text = format!("f\"{converted}\"");
                self.habit(
                    note,
                    open,
                    vec![Self::edit(
                        open.offset,
                        end.offset + end.length - open.offset,
                        &text,
                    )],
                );
                self.splice_text(i, j, &text, open);
            } else {
                self.error_at(note, open);
                self.splice_text(i, j, "\"\"", open);
            }
            return true;
        }
        // C#'s `$"n={n}"`: an interpolated string, Kāra's `f"n={n}"`.
        if lexed(self, i, '$')
            && matches!(self.tok_at(i + 1), Some(Token::StringLiteral(_)))
            && self.touching(i, i + 1)
        {
            let at = self.span_at(i);
            let lit = self.span_at(i + 1);
            let Some(text) = self.source_between(lit.offset, lit.offset + lit.length) else {
                return false;
            };
            let note = "`$\"..\"` is C#'s interpolated string: Kāra's is an f-string, \
                        `f\"n={n}\"`";
            // C#'s `{x:N2}` / `{x,8}` formats are not Kāra's.
            let specs = text.split('{').skip(1).any(|h| {
                h.split('}')
                    .next()
                    .is_some_and(|hole| hole.contains([':', ',']))
            });
            if specs {
                self.error_at(note, at);
                self.tokens.remove(i);
            } else {
                self.habit(note, at, vec![Self::edit(at.offset, at.length, "f")]);
                self.splice_text(i, i + 1, &format!("f{text}"), at);
            }
            return true;
        }
        // `p?.x ?? d`: TypeScript's optional chaining. Kāra's `p?` returns
        // from the function at a `None`, so the default is never reached:
        // told, with no edit, before a checker's fix can write `unwrap()`.
        if matches!(self.tok_at(i), Some(Token::QuestionDot)) && self.ident_at(i + 1).is_some() {
            let line = self.span_at(i).line;
            let coalesced = (i + 2..self.tokens.len())
                .take_while(|&k| {
                    self.span_at(k).line == line
                        && !matches!(self.tok_at(k), Some(Token::Semicolon | Token::LeftBrace))
                })
                .any(|k| matches!(self.tok_at(k), Some(Token::QuestionQuestion)));
            if coalesced {
                let at = self.span_at(i);
                self.error_at(
                    "`p?.x ?? d` is TypeScript's optional chaining: in Kāra, `p?` returns from \
                     the function when `p` is `None`, so `??` never supplies its default; \
                     write `match p { Some(v) => v.x, None => d }`, or `p.map(|v| v.x) ?? d`",
                    at,
                );
                return false;
            }
        }
        // `[...a, 3]` / `f(...args)`: JavaScript's spread. A capital name
        // is a type-parameter pack, `Tensor[T, ...S]`.
        if matches!(self.tok_at(i), Some(Token::DotDotDot))
            && i > 0
            && matches!(
                self.tok_at(i - 1),
                Some(Token::LeftBracket | Token::Comma | Token::LeftParen)
            )
            && self
                .ident_at(i + 1)
                .is_some_and(|n| n.starts_with(|c: char| c.is_ascii_lowercase()))
            && self.touching(i, i + 1)
        {
            let at = self.span_at(i);
            self.error_at(
                "`...v` is JavaScript's spread: Kāra builds a list from another with \
                 `let mut b = a.clone(); b.push(3);`, and joins lists with `[a, b].concat()`",
                at,
            );
            self.tokens.remove(i);
            return true;
        }
        // `@Override`: Java's annotation; a Kāra impl's method needs none.
        if matches!(self.tok_at(i), Some(Token::At))
            && self.ident_at(i + 1) == Some("Override")
            && self.touching(i, i + 1)
            && matches!(
                self.tok_at(i + 2),
                Some(Token::Fn | Token::Pub | Token::Private)
            )
        {
            let at = self.span_at(i);
            let next = self.span_at(i + 2);
            self.habit(
                "`@Override` is Java's: a method in `impl Trait for T` is the trait's by \
                 its name, so Kāra writes no annotation",
                at,
                vec![Self::edit(at.offset, next.offset - at.offset, "")],
            );
            self.tokens.drain(i..i + 2);
            return true;
        }
        // `throws E` after a signature: Java's checked exceptions.
        if self.ident_at(i) == Some("throws")
            && i > 0
            && matches!(
                self.tok_at(i - 1),
                Some(Token::RightParen | Token::Identifier { .. } | Token::RightBracket)
            )
        {
            let mut j = i + 1;
            while self.ident_at(j).is_some() {
                match self.tok_at(j + 1) {
                    Some(Token::Comma) | Some(Token::Dot) => j += 2,
                    _ => {
                        j += 1;
                        break;
                    }
                }
            }
            if j > i + 1 && matches!(self.tok_at(j), Some(Token::LeftBrace)) {
                let at = self.span_at(i);
                let brace = self.span_at(j);
                let prev = self.span_at(i - 1);
                let from = prev.offset + prev.length;
                self.habit(
                    "`throws` is Java's: a Kāra function that can fail says so in its \
                     result, `-> Result[T, E]`, so the clause goes",
                    at,
                    vec![Self::edit(from, brace.offset - from, " ")],
                );
                self.tokens.drain(i..j);
                return true;
            }
        }
        if let Some(word) = self.ident_at(i).map(str::to_string) {
            let after_dot = i > 0 && matches!(self.tok_at(i - 1), Some(Token::Dot));
            if !after_dot {
                if let Some(done) = self.mend_foreign_print(i, &word) {
                    return done;
                }
                if word == "Math" {
                    return self.mend_math(i);
                }
                if word == "new" {
                    return self.mend_new(i);
                }
                if word == "this" {
                    return self.mend_this(i);
                }
            }
        }
        false
    }

    /// `console.log(..)`, `System.out.println(..)`, `Console.WriteLine(..)`
    /// and their `print` forms: Kāra's `println(..)` / `print(..)`.
    fn mend_foreign_print(&mut self, i: usize, word: &str) -> Option<bool> {
        let dotted = |me: &Self, names: &[&str]| -> bool {
            names.iter().enumerate().all(|(k, n)| {
                me.ident_at(i + 2 * k) == Some(n)
                    && (k + 1 == names.len()
                        || matches!(me.tok_at(i + 2 * k + 1), Some(Token::Dot)))
            })
        };
        let forms: [(&[&str], &str, &str); 6] = [
            (&["console", "log"], "println", "JavaScript's"),
            (&["System", "out", "println"], "println", "Java's"),
            (&["System", "out", "print"], "print", "Java's"),
            (&["Console", "WriteLine"], "println", "C#'s"),
            (&["Console", "Write"], "print", "C#'s"),
            (&["console", "error"], "eprintln", "JavaScript's"),
        ];
        let (names, to, lang) = forms.iter().find(|(names, _, _)| {
            names[0] == word
                && dotted(self, names)
                && matches!(self.tok_at(i + 2 * names.len() - 1), Some(Token::LeftParen))
        })?;
        if self.word_is_bound(word) {
            return Some(false);
        }
        let last = i + 2 * names.len() - 2;
        let at = self.span_at(i);
        let end = self.span_at(last);
        self.habit(
            &format!(
                "`{}` is {lang}: Kāra prints a line with `{to}(x)`",
                names.join(".")
            ),
            at,
            vec![Self::edit(
                at.offset,
                end.offset + end.length - at.offset,
                to,
            )],
        );
        self.splice_text(i, last, to, at);
        Some(true)
    }

    /// The text of tokens `[a, b)`, parenthesized unless it is one token.
    fn operand_text(&self, a: usize, b: usize) -> Option<String> {
        let t = self.tokens_text(a, b)?;
        Some(if b == a + 1 { t } else { format!("({t})") })
    }

    /// The arguments of the call whose `(` is at `open`: each one's token
    /// range, and the index of the `)`.
    pub(super) fn call_args(&self, open: usize) -> Option<(Vec<(usize, usize)>, usize)> {
        let close = self.matching_close(open)?;
        let mut args = Vec::new();
        let mut depth = 0i32;
        let mut start = open + 1;
        for k in open + 1..close {
            match self.tok_at(k)? {
                Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                Token::RightParen | Token::RightBracket | Token::RightBrace => depth -= 1,
                Token::Comma if depth == 0 => {
                    args.push((start, k));
                    start = k + 1;
                }
                _ => {}
            }
        }
        if start < close {
            args.push((start, close));
        }
        Some((args, close))
    }

    /// `Math.max(a, b)` / `Math.min` / `Math.abs(x)` / `floor` / `ceil` /
    /// `sqrt`: Kāra's numbers carry these as methods, `a.max(b)`.
    fn mend_math(&mut self, i: usize) -> bool {
        let Some(f) = self.ident_at(i + 2).map(str::to_string) else {
            return false;
        };
        if !matches!(self.tok_at(i + 1), Some(Token::Dot))
            || !matches!(self.tok_at(i + 3), Some(Token::LeftParen))
            || self.word_is_bound("Math")
        {
            return false;
        }
        let Some((args, close)) = self.call_args(i + 3) else {
            return false;
        };
        let text = match (f.as_str(), args.as_slice()) {
            ("max" | "min", [(a0, a1), (b0, b1)]) => self
                .operand_text(*a0, *a1)
                .zip(self.tokens_text(*b0, *b1))
                .map(|(a, b)| format!("{a}.{f}({b})")),
            ("abs" | "floor" | "ceil" | "sqrt", [(a0, a1)]) => {
                self.operand_text(*a0, *a1).map(|a| {
                    // `-3.abs()` is `-(3.abs())`.
                    if matches!(self.tok_at(*a0), Some(Token::Minus)) && !a.starts_with('(') {
                        format!("({a}).{f}()")
                    } else {
                        format!("{a}.{f}()")
                    }
                })
            }
            _ => None,
        };
        let Some(text) = text else {
            return false;
        };
        let at = self.span_at(i);
        self.rewrite(
            &format!(
                "`Math.{f}` is JavaScript's and Java's: a Kāra number has `{f}` as a method, \
                 `{text}`"
            ),
            at,
            i,
            close,
            &text,
        );
        true
    }

    /// `new P { x: 1 }`, `new ArrayList<>()`, `new int[n]`: Java's and
    /// JavaScript's `new`. Kāra builds a struct with its literal and a
    /// collection with its constructor; any other `new X(..)` is told.
    fn mend_new(&mut self, i: usize) -> bool {
        // `new` is never a Kāra value followed by a type name, so no binding
        // of the program's can be read this way.
        // After `throw` the statement's own note says it all.
        if i > 0 && matches!(self.tok_at(i - 1), Some(Token::Fn | Token::ColonColon))
            || i > 0 && self.ident_at(i - 1) == Some("throw")
        {
            return false;
        }
        let Some(ty) = self.ident_at(i + 1).map(str::to_string) else {
            return false;
        };
        let at = self.span_at(i);
        let next = self.span_at(i + 1);
        // `new int[n]` / `new int[]{1, 2}`.
        if java_binding_type(&ty) && matches!(self.tok_at(i + 2), Some(Token::LeftBracket)) {
            let Some(rb) = self.matching_close(i + 2) else {
                return false;
            };
            if rb == i + 3 && matches!(self.tok_at(rb + 1), Some(Token::LeftBrace)) {
                let Some(cb) = self.matching_brace(rb + 1) else {
                    return false;
                };
                let inner = if cb == rb + 2 {
                    Some(String::new())
                } else {
                    self.tokens_text(rb + 2, cb)
                };
                if let Some(inner) = inner {
                    let text = format!("[{inner}]");
                    self.rewrite(
                        "`new int[]{..}` is Java's array literal: Kāra's is `[1, 2, 3]`",
                        at,
                        i,
                        cb,
                        &text,
                    );
                    return true;
                }
                return false;
            }
            let zero = match ty.as_str() {
                "int" => "0_i32",
                "long" => "0_i64",
                "short" => "0_i16",
                "byte" => "0_i8",
                "double" => "0.0",
                "float" => "0.0_f32",
                "boolean" => "false",
                _ => "",
            };
            if rb > i + 3 && !zero.is_empty() {
                if let Some(n) = self.tokens_text(i + 3, rb) {
                    let text = format!("Vec.filled({n}, {zero})");
                    self.rewrite(
                        "`new int[n]` is Java's: Kāra makes `n` zeros with \
                         `Vec.filled(n, 0)` (library/collections.md)",
                        at,
                        i,
                        rb,
                        &text,
                    );
                    return true;
                }
            }
            return false;
        }
        if !ty.starts_with(|c: char| c.is_ascii_uppercase()) {
            return false;
        }
        // `new P { .. }`: the struct literal alone.
        if matches!(self.tok_at(i + 2), Some(Token::LeftBrace)) {
            self.habit(
                "`new` is Java's and JavaScript's: Kāra builds a struct with its literal \
                 alone, `P { x: 1 }`",
                at,
                vec![Self::edit(at.offset, next.offset - at.offset, "")],
            );
            self.tokens.remove(i);
            return true;
        }
        // `new ArrayList<>()`: an empty collection.
        let Some(open) = self.skip_angles(i + 2) else {
            return false;
        };
        if !matches!(self.tok_at(open), Some(Token::LeftParen)) {
            return false;
        }
        let empty = matches!(self.tok_at(open + 1), Some(Token::RightParen));
        let kara = match ty.as_str() {
            "Vec" | "ArrayList" | "List" | "LinkedList" | "Array" => "Vec",
            "Map" | "HashMap" | "Dictionary" => "Map",
            "TreeMap" | "SortedDictionary" => "SortedMap",
            "Set" | "HashSet" => "Set",
            "TreeSet" | "SortedSet" => "SortedSet",
            "ArrayDeque" => "VecDeque",
            _ => "",
        };
        if empty && !kara.is_empty() {
            let text = format!("{kara}.new()");
            self.rewrite(
                &format!(
                    "`new {ty}()` is Java's and JavaScript's: Kāra's empty collection is \
                     `{text}`"
                ),
                at,
                i,
                open + 1,
                &text,
            );
            return true;
        }
        // Any other `new X(..)`: told, and parsed on without the `new`.
        self.error_at(
            "`new` is Java's and JavaScript's: Kāra builds a struct with its literal, \
             `P { x: 1 }`, and a type's constructor is a function it declares, called \
             `P.new(..)` by convention",
            at,
        );
        self.tokens.remove(i);
        true
    }

    /// `this.x` in a method whose receiver is `self`: Java's and
    /// JavaScript's name for it.
    fn mend_this(&mut self, i: usize) -> bool {
        if !matches!(self.tok_at(i + 1), Some(Token::Dot)) || self.word_is_bound("this") {
            return false;
        }
        // The nearest `fn` before `this` whose body holds it, and whose
        // parameter list has `self`.
        let in_method = (0..i).rev().any(|f| {
            if !matches!(self.tok_at(f), Some(Token::Fn)) {
                return false;
            }
            let Some(open) = (f + 1..i).find(|&k| matches!(self.tok_at(k), Some(Token::LeftParen)))
            else {
                return false;
            };
            let Some(close) = self.matching_close(open) else {
                return false;
            };
            let has_self =
                (open + 1..close).any(|k| matches!(self.tok_at(k), Some(Token::SelfValue)));
            let Some(body) =
                (close + 1..i).find(|&k| matches!(self.tok_at(k), Some(Token::LeftBrace)))
            else {
                return false;
            };
            has_self && self.matching_brace(body).is_some_and(|end| end > i)
        });
        if !in_method {
            return false;
        }
        let at = self.span_at(i);
        self.habit(
            "`this` is Java's and JavaScript's: a Kāra method names its receiver `self`",
            at,
            vec![Self::edit(at.offset, at.length, "self")],
        );
        self.tokens[i].token = Token::SelfValue;
        true
    }

    /// Item-start habits: `export`, `public` / `static`, a `;` after an
    /// item, `using` / `import java..`, `interface`, `impl X implements T`,
    /// and Java's `int f(int a) { .. }` header. Run before the item's
    /// visibility is read, so a `pub` it writes is the item's.
    pub(super) fn mend_ts_java_item_forms(&mut self) {
        loop {
            let p = self.pos;
            let at = match self.tokens.get(p) {
                Some(t) => t.span,
                None => return,
            };
            // A `;` after an item's `}`.
            if matches!(self.tok_at(p), Some(Token::Semicolon))
                && p > 0
                && matches!(self.tok_at(p - 1), Some(Token::RightBrace))
            {
                self.habit(
                    "a `;` after an item is not Kāra's: an item ends at its `}`",
                    at,
                    vec![Self::edit(at.offset, at.length, "")],
                );
                self.tokens.remove(p);
                continue;
            }
            let word = self.ident_at(p).map(str::to_string);
            // `using System;` (C#) and `import java.util.*;` (Java).
            let using = word.as_deref() == Some("using") && self.ident_at(p + 1).is_some();
            let java_import = matches!(self.tok_at(p), Some(Token::Import))
                && matches!(self.ident_at(p + 1), Some("java" | "javax"))
                && matches!(self.tok_at(p + 2), Some(Token::Dot));
            if using || java_import {
                let Some(semi) = (p + 1..self.tokens.len())
                    .take_while(|&k| self.span_at(k).line == at.line)
                    .find(|&k| matches!(self.tok_at(k), Some(Token::Semicolon)))
                else {
                    return;
                };
                let end = self.span_at(semi);
                self.habit(
                    if using {
                        "`using` is C#'s: the Kāra library's names are in scope as they are, \
                         and a module of it is imported with `import std.<module>`"
                    } else {
                        "Java's library has no Kāra import: the Kāra library's names are in \
                         scope as they are, and a module of it is imported with \
                         `import std.<module>`"
                    },
                    at,
                    vec![Self::edit(
                        at.offset,
                        end.offset + end.length - at.offset,
                        "",
                    )],
                );
                self.tokens.drain(p..=semi);
                continue;
            }
            // `export fn` (TypeScript): Kāra's `pub fn`.
            let item_kw = |me: &Self, k: usize| {
                matches!(
                    me.tok_at(k),
                    Some(
                        Token::Fn
                            | Token::Struct
                            | Token::Enum
                            | Token::Trait
                            | Token::Const
                            | Token::Type
                    )
                ) || matches!(me.ident_at(k), Some("function" | "interface" | "class"))
            };
            if word.as_deref() == Some("export") && item_kw(self, p + 1) {
                self.habit(
                    "`export` is TypeScript's: Kāra makes an item public with `pub`",
                    at,
                    vec![Self::edit(at.offset, at.length, "pub")],
                );
                self.tokens[p].token = Token::Pub;
                return;
            }
            // Java's and C#'s modifiers before an item.
            let modifiers = |me: &Self| {
                let mut k = p;
                while matches!(
                    me.ident_at(k),
                    Some("public" | "static" | "protected" | "internal" | "abstract")
                ) || matches!(me.tok_at(k), Some(Token::Private))
                    || matches!(me.tok_at(k), Some(Token::ReservedFuture("final")))
                {
                    k += 1;
                }
                k
            };
            let k = modifiers(self);
            if k > p && (item_kw(self, k) || self.java_header_at(k).is_some()) {
                let mut edits = Vec::new();
                // `main` is never `pub` (design.md § Entry Point).
                let is_main = self
                    .java_header_at(k)
                    .is_some_and(|(n, _)| self.ident_at(n) == Some("main"))
                    || matches!(self.tok_at(k), Some(Token::Fn))
                        && self.ident_at(k + 1) == Some("main");
                let mut pub_ = is_main;
                for m in p..k {
                    let s = self.span_at(m);
                    let next = self.span_at(m + 1);
                    match self.ident_at(m) {
                        Some("public") if !pub_ => {
                            pub_ = true;
                            edits.push(Self::edit(s.offset, s.length, "pub"));
                        }
                        _ if matches!(self.tok_at(m), Some(Token::Private)) => {}
                        _ => edits.push(Self::edit(s.offset, next.offset - s.offset, "")),
                    }
                }
                if edits.is_empty() {
                    return;
                }
                self.habit(
                    "`public` and `static` are Java's and C#'s: a Kāra item is made public \
                     with `pub`, and a function in no `impl` is already static",
                    at,
                    edits,
                );
                let keep: Vec<_> = (p..k)
                    .filter_map(|m| match self.ident_at(m) {
                        Some("public") if !is_main => Some(Token::Pub),
                        _ if matches!(self.tok_at(m), Some(Token::Private)) => Some(Token::Private),
                        _ => None,
                    })
                    .take(1)
                    .collect();
                let synth: Vec<_> = keep.into_iter().map(|t| Self::synth(t, at)).collect();
                let kept = synth.len();
                self.tokens.splice(p..k, synth);
                if let Some((name_at, open)) = self.java_header_at(p + kept) {
                    self.mend_java_header(p + kept, name_at, open);
                }
                return;
            }
            // `interface Shape { fn area(self) -> i64; }`: a trait.
            if word.as_deref() == Some("interface") && self.ident_at(p + 1).is_some() {
                let brace = match self.tok_at(p + 2) {
                    Some(Token::LeftBrace) => Some(p + 2),
                    Some(Token::LeftBracket) => self.matching_close(p + 2).map(|c| c + 1),
                    _ => None,
                };
                if let Some(brace) =
                    brace.filter(|&b| matches!(self.tok_at(b), Some(Token::LeftBrace)))
                {
                    let close = self.matching_brace(brace).unwrap_or(brace);
                    let fields = self.ident_at(brace + 1).is_some()
                        && matches!(self.tok_at(brace + 2), Some(Token::Colon));
                    let methods =
                        (brace + 1..close).any(|k| matches!(self.tok_at(k), Some(Token::Fn)));
                    let (kw, tok, note) = if fields && !methods {
                        (
                            "struct",
                            Token::Struct,
                            "an `interface` of fields is TypeScript's: Kāra declares the \
                             shape as `struct P { x: T }`",
                        )
                    } else {
                        (
                            "trait",
                            Token::Trait,
                            "`interface` is Java's and TypeScript's: Kāra declares the \
                             methods a type must have with `trait`",
                        )
                    };
                    self.habit(note, at, vec![Self::edit(at.offset, at.length, kw)]);
                    self.tokens[p].token = tok;
                    return;
                }
            }
            // `impl Sq implements Shape {`: Kāra's `impl Shape for Sq {`.
            if matches!(self.tok_at(p), Some(Token::Impl))
                && self.ident_at(p + 1).is_some()
                && self.ident_at(p + 2) == Some("implements")
                && self.ident_at(p + 3).is_some()
                && matches!(self.tok_at(p + 4), Some(Token::LeftBrace))
            {
                let (ty, tr) = (
                    self.ident_at(p + 1).unwrap_or_default().to_string(),
                    self.ident_at(p + 3).unwrap_or_default().to_string(),
                );
                let first = self.span_at(p + 1);
                let last = self.span_at(p + 3);
                let text = format!("{tr} for {ty}");
                self.habit(
                    "`implements` is Java's: Kāra writes `impl Trait for Type { .. }`",
                    self.span_at(p + 2),
                    vec![Self::edit(
                        first.offset,
                        last.offset + last.length - first.offset,
                        &text,
                    )],
                );
                self.splice_text(p + 1, p + 3, &text, first);
                return;
            }
            if let Some((name_at, open)) = self.java_header_at(p) {
                self.mend_java_header(p, name_at, open);
            }
            return;
        }
    }

    /// Java's `T name(` at `k`: a return type (`void`, a primitive,
    /// `String`, `T[]`, `List<T>`) and a function name. The name's index
    /// and the `(`'s.
    fn java_header_at(&self, k: usize) -> Option<(usize, usize)> {
        let ty = self.ident_at(k)?;
        if !(ty == "void" || java_binding_type(ty) || java_type(ty).is_some()) {
            return None;
        }
        let mut n = k + 1;
        if matches!(self.tok_at(n), Some(Token::LeftBracket))
            && matches!(self.tok_at(n + 1), Some(Token::RightBracket))
        {
            n += 2;
        }
        n = self.skip_angles(n)?;
        self.ident_at(n)?;
        if !matches!(self.tok_at(n + 1), Some(Token::LeftParen)) {
            return None;
        }
        let close = self.matching_close(n + 1)?;
        matches!(self.tok_at(close + 1), Some(Token::LeftBrace)).then_some((n, n + 1))
    }

    /// `int add(int a, int b) { .. }` -> `fn add(a: i32, b: i32) -> i32 { .. }`,
    /// and `void main(String[] args)` -> `fn main()`.
    fn mend_java_header(&mut self, p: usize, name_at: usize, open: usize) {
        let at = self.span_at(p);
        let Some(ty_text) = self.tokens_text(p, name_at) else {
            return;
        };
        let ty_text = ty_text.replace(' ', "");
        let result = if ty_text == "void" {
            Some(String::new())
        } else {
            java_type(&ty_text)
        };
        let Some(mut result) = result else {
            return;
        };
        let Some(close) = self.matching_close(open) else {
            return;
        };
        let name = self.ident_at(name_at).unwrap_or_default().to_string();
        // C's `int main() { ..; return 0; }`: Kāra's `main` returns
        // nothing, and ending is success. Any other `return` keeps it.
        let mut c_return_zero = None;
        if name == "main" && result == "i32" {
            let body = close + 1;
            let Some(end) = self.matching_brace(body) else {
                return;
            };
            let last_zero = end >= body + 4
                && matches!(self.tok_at(end - 3), Some(Token::Return))
                && matches!(self.tok_at(end - 2), Some(Token::Integer(0, _)))
                && matches!(self.tok_at(end - 1), Some(Token::Semicolon));
            let returns = (body..end)
                .filter(|&k| matches!(self.tok_at(k), Some(Token::Return)))
                .count();
            if !last_zero || returns != 1 {
                self.error_at(
                    "`int main()` is C's: Kāra's entry point is `fn main()`, and reaching its \
                     end is success; a program that ends with another status returns \
                     `ExitCode` (design.md § The `main` function and exit codes)",
                    at,
                );
                return;
            }
            c_return_zero = Some(end - 3);
            result = String::new();
        }
        let name_span = self.span_at(name_at);
        let close_span = self.span_at(close);
        let mut edits = vec![Self::edit(at.offset, name_span.offset - at.offset, "fn ")];
        // `main(String[] args)` / `main(int argc, char** argv)`: Kāra's
        // `main` takes nothing, and the body must not name them.
        let java_main_args = name == "main" && close > open + 1 && {
            let names: Vec<&str> = (open + 1..close)
                .filter(|&k| matches!(self.tok_at(k + 1), Some(Token::Comma | Token::RightParen)))
                .filter_map(|k| self.ident_at(k))
                .collect();
            let body_end = self.matching_brace(close + 1).unwrap_or(close);
            !names.is_empty()
                && !(close + 1..body_end)
                    .any(|k| self.ident_at(k).is_some_and(|w| names.contains(&w)))
        };
        if java_main_args {
            let o = self.span_at(open);
            edits.push(Self::edit(
                o.offset + o.length,
                close_span.offset - o.offset - o.length,
                "",
            ));
        }
        if !result.is_empty() {
            edits.push(Self::edit(
                close_span.offset + close_span.length,
                0,
                &format!(" -> {result}"),
            ));
        }
        if let Some(r) = c_return_zero {
            let (rs, semi) = (self.span_at(r), self.span_at(r + 2));
            edits.push(Self::edit(
                rs.offset,
                semi.offset + semi.length - rs.offset,
                "",
            ));
        }
        self.habit(
            if c_return_zero.is_some() {
                "`int main() { .. return 0; }` is C's: Kāra's entry point is `fn main()`, \
                 and reaching its end is success"
            } else if java_main_args {
                "`void main(String[] args)` is Java's: Kāra's entry point is `fn main()`, \
                 and the command line is `env.args()`"
            } else {
                "a result type before the name is C's, Java's and C#'s: Kāra declares \
                 `fn name(x: T) -> R { .. }` (no `-> ` for `void`)"
            },
            at,
            edits,
        );
        // Tokens: `fn` name `(` .. `)` [`->` R].
        if let Some(r) = c_return_zero {
            self.tokens.drain(r..r + 3);
        }
        if !result.is_empty() {
            let mut arrow = crate::tokenize(&format!("-> {result}"));
            arrow.retain(|t| !matches!(t.token, Token::EOF));
            for t in &mut arrow {
                t.span = Span {
                    length: 0,
                    ..close_span
                };
            }
            self.tokens.splice(close + 1..close + 1, arrow);
        }
        if java_main_args {
            self.tokens.drain(open + 1..close);
        }
        self.tokens.splice(p..name_at, [Self::synth(Token::Fn, at)]);
    }

    /// Java's parameter `int a` / `String[] names`: Kāra's `a: i32`.
    pub(super) fn mend_java_param(&mut self) {
        let p = self.pos;
        let Some(ty) = self.ident_at(p).map(str::to_string) else {
            return;
        };
        let generic = ty.starts_with(|c: char| c.is_ascii_uppercase())
            && matches!(self.tok_at(p + 1), Some(Token::LessThan));
        if !java_binding_type(&ty) && java_type(&ty).is_none() && !generic {
            return;
        }
        let mut n = p + 1;
        if matches!(self.tok_at(n), Some(Token::LeftBracket))
            && matches!(self.tok_at(n + 1), Some(Token::RightBracket))
        {
            n += 2;
        }
        let Some(n) = self.skip_angles(n) else {
            return;
        };
        if self.ident_at(n).is_none()
            || !matches!(self.tok_at(n + 1), Some(Token::Comma | Token::RightParen))
            || n == p
        {
            return;
        }
        let (Some(ty_text), Some(name)) =
            (self.tokens_text(p, n), self.ident_at(n).map(str::to_string))
        else {
            return;
        };
        let Some(kara) = java_type(&ty_text.replace(' ', "")) else {
            return;
        };
        let at = self.span_at(p);
        let text = format!("{name}: {kara}");
        self.rewrite(
            &format!(
                "a parameter's type before its name is Java's and C#'s: Kāra writes \
                 `{text}`"
            ),
            at,
            p,
            n,
            &text,
        );
    }

    /// TypeScript's result annotation `(..): T {`: Kāra's `-> T`.
    pub(super) fn mend_ts_result_colon(&mut self) {
        let p = self.pos;
        if !matches!(self.tok_at(p), Some(Token::Colon)) || p == 0 {
            return;
        }
        let close = self.span_at(p - 1);
        let at = self.span_at(p);
        if at.line != close.line || self.tok_at(p + 1).is_none() {
            return;
        }
        let mut k = p + 1;
        let mut depth = 0i32;
        loop {
            match self.tok_at(k) {
                Some(Token::LeftParen | Token::LeftBracket) => depth += 1,
                Some(Token::RightParen | Token::RightBracket) => depth -= 1,
                Some(Token::LeftBrace) if depth == 0 => break,
                Some(Token::Semicolon | Token::EOF) | None => return,
                _ => {}
            }
            k += 1;
        }
        if k == p + 1 {
            return;
        }
        self.habit(
            "a function's result type follows `->` in Kāra: TypeScript's `(..): T` is \
             `(..) -> T`",
            at,
            vec![Self::edit(at.offset, at.length, " ->")],
        );
        self.tokens[p].token = Token::Arrow;
    }

    /// Statement-start habits: `const x = v`, Java's typed locals, `throw`.
    /// `true` when the statement was rewritten or consumed.
    pub(super) fn mend_ts_java_statement_forms(&mut self) -> bool {
        let p = self.pos;
        if p >= self.tokens.len() {
            return false;
        }
        let at = self.span_at(p);
        // `foreach (var x in v)` (C#).
        if self.ident_at(p) == Some("foreach")
            && matches!(self.tok_at(p + 1), Some(Token::LeftParen))
        {
            self.mend_paren_for();
            return true;
        }
        // `const x = v` in a body: TypeScript's immutable binding is `let`.
        if matches!(self.tok_at(p), Some(Token::Const))
            && self
                .ident_at(p + 1)
                .is_some_and(|n| n.chars().any(|c| c.is_ascii_lowercase()))
            && matches!(self.tok_at(p + 2), Some(Token::Equal | Token::Colon))
        {
            self.habit(
                "`const x = v` is JavaScript's binding that is never reassigned: Kāra's is \
                 `let x = v` (a Kāra `const` is a compile-time constant)",
                at,
                vec![Self::edit(at.offset, at.length, "let")],
            );
            self.tokens[p].token = Token::Let;
            return true;
        }
        // `final int x = 3;` / `int x = 3;` / `String s = ..;` / `int[] v = {1, 2};`
        let fin = matches!(self.tok_at(p), Some(Token::ReservedFuture("final")));
        let t0 = p + usize::from(fin);
        if let Some(ty) = self.ident_at(t0).map(str::to_string) {
            let typed = java_binding_type(&ty)
                || (ty.starts_with(|c: char| c.is_ascii_uppercase())
                    && matches!(self.tok_at(t0 + 1), Some(Token::LessThan))
                    && java_type(&format!("{ty}<i>")).is_some());
            if typed {
                let mut n = t0 + 1;
                if matches!(self.tok_at(n), Some(Token::LeftBracket))
                    && matches!(self.tok_at(n + 1), Some(Token::RightBracket))
                {
                    n += 2;
                }
                if let Some(n) = self.skip_angles(n) {
                    if self.ident_at(n).is_some()
                        && matches!(self.tok_at(n + 1), Some(Token::Equal | Token::Semicolon))
                        && !matches!(self.tok_at(n + 2), Some(Token::Equal))
                    {
                        if let Some(kara) = self
                            .tokens_text(t0, n)
                            .and_then(|t| java_type(&t.replace(' ', "")))
                        {
                            return self.mend_java_local(p, t0, n, &kara);
                        }
                    }
                }
            }
        }
        false
    }

    /// `[final] T name = v;` -> `let name: K = v;`, a `{..}` array
    /// initializer written `[..]`.
    fn mend_java_local(&mut self, p: usize, t0: usize, n: usize, kara: &str) -> bool {
        let at = self.span_at(p);
        let name = self.ident_at(n).unwrap_or_default().to_string();
        let name_span = self.span_at(n);
        let mut edits = vec![Self::edit(
            at.offset,
            name_span.offset + name_span.length - at.offset,
            &format!("let {name}: {kara}"),
        )];
        // `= {1, 2, 3};`: Java's array initializer.
        let brace_init = matches!(self.tok_at(n + 1), Some(Token::Equal))
            && matches!(self.tok_at(n + 2), Some(Token::LeftBrace))
            && kara.starts_with("Vec[");
        let mut brace_close = None;
        if brace_init {
            if let Some(c) = self
                .matching_brace(n + 2)
                .filter(|&c| matches!(self.tok_at(c + 1), Some(Token::Semicolon)))
            {
                let (o, cl) = (self.span_at(n + 2), self.span_at(c));
                edits.push(Self::edit(o.offset, 1, "["));
                edits.push(Self::edit(cl.offset, 1, "]"));
                brace_close = Some(c);
            }
        }
        self.habit(
            &format!(
                "a type before the name is Java's and C#'s: Kāra binds with \
                 `let {name}: {kara} = v` (`let mut` when it is reassigned)"
            ),
            self.span_at(t0),
            edits,
        );
        if let Some(c) = brace_close {
            self.tokens[c].token = Token::RightBracket;
            self.tokens[n + 2].token = Token::LeftBracket;
        }
        let mut head = crate::tokenize(&format!("let {name}: {kara}"));
        head.retain(|t| !matches!(t.token, Token::EOF));
        for (k, t) in head.iter_mut().enumerate() {
            t.span = if k == 1 {
                name_span
            } else {
                Span { length: 0, ..at }
            };
        }
        self.tokens.splice(p..=n, head);
        true
    }

    /// `for (..)` whose parentheses hold a whole header: `for (let i = 0;
    /// i < n; i++)`, `for (const x of v)`, `for (int x : v)`,
    /// `foreach (var x in v)`. The C form is left to the three-part `for`
    /// mend; the others are `for x in v`. `true` when handled.
    pub(super) fn mend_paren_for(&mut self) -> bool {
        let p = self.pos;
        let at = self.span_at(p);
        let foreach = self.ident_at(p) == Some("foreach");
        if !(matches!(self.tok_at(p), Some(Token::For)) || foreach) {
            return false;
        }
        // `for x of v {` with no parentheses.
        if !foreach && self.ident_at(p + 1).is_some() && self.ident_at(p + 2) == Some("of") {
            let of = self.span_at(p + 2);
            self.habit(
                "`for x of v` is JavaScript's: Kāra's loop reads `for x in v`",
                of,
                vec![Self::edit(of.offset, of.length, "in")],
            );
            self.tokens[p + 2].token = Token::In;
            return false;
        }
        if !matches!(self.tok_at(p + 1), Some(Token::LeftParen)) {
            return false;
        }
        let Some(close) = self.matching_close(p + 1) else {
            return false;
        };
        if !matches!(self.tok_at(close + 1), Some(Token::LeftBrace)) {
            return false;
        }
        let semis: Vec<usize> = (p + 2..close)
            .filter(|&k| matches!(self.tok_at(k), Some(Token::Semicolon)))
            .collect();
        // The loop variable, after `let` / `const` / `var` / `int` / `final`,
        // or a C++ type: `auto`, `const auto&`, `size_t`.
        let mut v = p + 2;
        let mut writes = false;
        if let Some(ty) = self
            .c_type_at(v)
            .filter(|t| t.c_only && self.ident_at(t.end).is_some())
        {
            writes = ty.pass != super::c_cpp_habits::CPass::Value && !ty.konst;
            v = ty.end;
        }
        // JavaScript declares its loop variable; Kotlin never does.
        let declared = matches!(self.tok_at(v), Some(Token::Let | Token::Const))
            || self.ident_at(v) == Some("var");
        while matches!(self.tok_at(v), Some(Token::Let | Token::Const | Token::Mut))
            || matches!(self.tok_at(v), Some(Token::ReservedFuture("final")))
            || self
                .ident_at(v)
                .is_some_and(|w| w == "var" || java_binding_type(w))
                && self.ident_at(v + 1).is_some()
        {
            v += 1;
        }
        if self.ident_at(v).is_none() {
            return false;
        }
        if semis.len() == 2 && !foreach {
            // `( init ; cond ; step )` -> `let mut i = A ; cond ; step`, then
            // the three-part mend writes the range.
            if !matches!(self.tok_at(v + 1), Some(Token::Equal)) {
                return false;
            }
            let var_tok = self.tokens[v].clone();
            let mut head = vec![
                Self::synth(Token::Let, at),
                Self::synth(Token::Mut, at),
                var_tok,
            ];
            head.extend(self.tokens[v + 1..close].iter().cloned());
            self.tokens.splice(p + 1..=close, head);
            return false;
        }
        if !semis.is_empty() {
            return false;
        }
        let sep = v + 1;
        let (lang, keys) = match (self.tok_at(sep), self.ident_at(sep)) {
            (_, Some("of")) => ("`for (const x of v)` is JavaScript's", false),
            (Some(Token::Colon), _) => ("`for (int x : v)` is Java's", false),
            (Some(Token::In), _) if foreach => ("`foreach (var x in v)` is C#'s", false),
            (Some(Token::In), _) if declared => ("`for (const k in o)` is JavaScript's", true),
            (Some(Token::In), _) => return self.mend_kotlin_for(p, v, sep, close),
            _ => return false,
        };
        if sep + 1 >= close {
            return false;
        }
        let Some(iter_text) = self.tokens_text(sep + 1, close) else {
            return false;
        };
        let name = self.ident_at(v).unwrap_or_default().to_string();
        let mut head = vec![self.tokens[v].clone(), Self::synth(Token::In, at)];
        head.extend(self.tokens[sep + 1..close].iter().cloned());
        if writes {
            self.error_at(
                "`for (auto& x : v)` is C++'s loop that writes through `x`: Kāra's is \
                 `for x in v.iter_mut() { *x = .. }`",
                at,
            );
        } else if keys {
            // JavaScript's `for .. in` walks an object's keys, or an array's
            // indices as text: Kāra's loop takes the values, so it is told.
            self.error_at(
                "`for (k in o)` is JavaScript's loop over keys: Kāra writes \
                 `for k in m.keys()` for a map's keys and `for i in 0..v.len()` for an \
                 array's indices; `for x in v` takes the values",
                at,
            );
        } else {
            let end = self.span_at(close);
            self.habit(
                &format!("{lang}: Kāra's loop reads `for {name} in {iter_text}`"),
                at,
                vec![Self::edit(
                    at.offset,
                    end.offset + end.length - at.offset,
                    &format!("for {name} in {iter_text}"),
                )],
            );
        }
        self.tokens[p].token = Token::For;
        self.tokens.splice(p + 1..=close, head);
        false
    }

    /// Kotlin's `for (x in v)`, `for (i in 1..3)` (both ends included),
    /// `for (i in 0 until n)`: Kāra's `for x in v`, `1..=3`, `0..n`.
    fn mend_kotlin_for(&mut self, p: usize, v: usize, sep: usize, close: usize) -> bool {
        let at = self.span_at(p);
        let mut parts = Vec::new();
        let mut depth = 0i32;
        let mut range = None;
        for k in sep + 1..close {
            match self.tok_at(k) {
                Some(Token::LeftParen | Token::LeftBracket) => depth += 1,
                Some(Token::RightParen | Token::RightBracket) => depth -= 1,
                Some(Token::DotDot) if depth == 0 => range = Some((k, "..=")),
                _ if depth == 0 && self.ident_at(k) == Some("until") => range = Some((k, "..")),
                _ if depth == 0 && matches!(self.ident_at(k), Some("downTo" | "step")) => {
                    self.error_at(
                        "`downTo` and `step` are Kotlin's: Kāra counts down with \
                         `(a..=b).rev()` and steps with `(a..b).step_by(n)`",
                        at,
                    );
                    // Told: the loop is parsed on over an empty range.
                    let name = self.ident_at(v).unwrap_or_default().to_string();
                    self.splice_text(p + 1, close, &format!("{name} in 0..0"), at);
                    return false;
                }
                _ => {}
            }
        }
        let iter_text = match range {
            Some((k, dots)) => {
                let (Some(a), Some(b)) =
                    (self.tokens_text(sep + 1, k), self.tokens_text(k + 1, close))
                else {
                    return false;
                };
                parts.push(if dots == "..=" {
                    "Kotlin's `a..b` includes `b`, as Kāra's `a..=b` does"
                } else {
                    "Kotlin's `a until b` is Kāra's `a..b`"
                });
                format!("{a}{dots}{b}")
            }
            None => match self.tokens_text(sep + 1, close) {
                Some(t) => t,
                None => return false,
            },
        };
        let name = self.ident_at(v).unwrap_or_default().to_string();
        let end = self.span_at(close);
        let text = format!("for {name} in {iter_text}");
        let extra = parts.first().map(|t| format!("; {t}")).unwrap_or_default();
        self.habit(
            &format!("`for (x in v)` is Kotlin's: Kāra's loop reads `{text}`{extra}"),
            at,
            vec![Self::edit(
                at.offset,
                end.offset + end.length - at.offset,
                &text,
            )],
        );
        self.tokens[p].token = Token::For;
        self.splice_text(p + 1, close, &text["for ".len()..], at);
        false
    }

    /// JavaScript's and Java's arrow function: `(a, b) => a + b`,
    /// `x => x * 2`, `(x) => { .. }`. Kāra's closure, `|a, b| a + b`.
    pub(super) fn mend_arrow_fn(&mut self) {
        let p = self.pos;
        if p > 0 && matches!(self.tok_at(p - 1), Some(Token::Pipe)) || self.in_if_condition(p) {
            return;
        }
        let (params, arrow) = match self.tok_at(p) {
            Some(Token::Identifier { .. })
                if matches!(self.tok_at(p + 1), Some(Token::FatArrow)) =>
            {
                (self.tokens_text(p, p + 1), p + 1)
            }
            Some(Token::LeftParen) => {
                let Some(close) = self.matching_close(p) else {
                    return;
                };
                if !matches!(self.tok_at(close + 1), Some(Token::FatArrow)) {
                    return;
                }
                // Names, each with an optional `: T`.
                let mut k = p + 1;
                let mut ok = true;
                while k < close {
                    if self.ident_at(k).is_none() {
                        ok = false;
                        break;
                    }
                    k += 1;
                    if matches!(self.tok_at(k), Some(Token::Colon)) {
                        while k < close && !matches!(self.tok_at(k), Some(Token::Comma)) {
                            k += 1;
                        }
                    }
                    if matches!(self.tok_at(k), Some(Token::Comma)) {
                        k += 1;
                    }
                }
                if !ok {
                    return;
                }
                let text = if close == p + 1 {
                    Some(String::new())
                } else {
                    self.tokens_text(p + 1, close)
                };
                (text, close + 1)
            }
            _ => return,
        };
        let Some(params) = params else {
            return;
        };
        if self.tok_at(arrow + 1).is_none() {
            return;
        }
        let at = self.span_at(arrow);
        let text = format!("|{params}|");
        self.rewrite(
            &format!(
                "`=>` here is an arrow function, JavaScript's and Java's: Kāra writes a \
                 closure as `{text} body`"
            ),
            at,
            p,
            arrow,
            &text,
        );
    }

    /// JavaScript's object literal `{ a: 1, b: 2 }` where a value is due:
    /// Kāra's record is a declared struct, so it is told, and the literal
    /// is parsed on as `()`.
    pub(super) fn mend_object_literal(&mut self) {
        let p = self.pos;
        if !matches!(self.tok_at(p), Some(Token::LeftBrace))
            || self.ident_at(p + 1).is_none()
            || !matches!(self.tok_at(p + 2), Some(Token::Colon))
            || self.ident_at(1) == Some("__interp__")
        {
            return;
        }
        let Some(close) = self.matching_brace(p) else {
            return;
        };
        if self.key_value_pairs(p + 1, close).is_none() {
            return;
        }
        let at = self.span_at(p);
        self.error_at(
            "`{ a: 1, b: 2 }` is JavaScript's object literal: Kāra declares the record's \
             shape, `struct O { a: i64, b: i64 }`, and builds it as `O { a: 1, b: 2 }`; a \
             map of keys is `Map.from([(\"a\", 1)])`",
            at,
        );
        self.splice_text(p, close, "()", at);
    }

    /// Whether token `p` sits in an `if` condition or a match arm's guard
    /// (`Some(c) if not first => ..`), where a `=>` ends the guard: walked
    /// back to whatever starts the expression.
    fn in_if_condition(&self, p: usize) -> bool {
        let mut depth = 0i32;
        let mut k = p;
        while k > 0 {
            k -= 1;
            match self.tok_at(k) {
                Some(Token::RightParen | Token::RightBracket) => depth += 1,
                Some(Token::LeftParen | Token::LeftBracket) => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                }
                Some(Token::If) if depth == 0 => return true,
                Some(
                    Token::LeftBrace
                    | Token::RightBrace
                    | Token::Semicolon
                    | Token::FatArrow
                    | Token::Equal,
                ) => return false,
                Some(Token::Comma) if depth == 0 => return false,
                None => return false,
                _ => {}
            }
        }
        false
    }

    /// The text of an alternative `[a, e)`, itself a conditional when it
    /// holds one at depth 0 (`x > 0 ? "pos" : x < 0 ? "neg" : "zero"`).
    fn conditional_alternative(&self, a: usize, e: usize) -> Option<String> {
        let mut depth = 0i32;
        let mut q = None;
        let mut colon = None;
        for k in a..e {
            match self.tok_at(k)? {
                Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                Token::RightParen | Token::RightBracket | Token::RightBrace => depth -= 1,
                Token::Question if depth == 0 && q.is_none() => q = Some(k),
                Token::Colon if depth == 0 && q.is_some() && colon.is_none() => colon = Some(k),
                _ => {}
            }
        }
        match (q, colon) {
            (Some(q), Some(c)) if q > a && c > q + 1 && c + 1 < e => Some(format!(
                "if {} {{ {} }} else {{ {} }}",
                self.tokens_text(a, q)?,
                self.tokens_text(q + 1, c)?,
                self.conditional_alternative(c + 1, e)?
            )),
            (Some(_), _) => None,
            _ => self.tokens_text(a, e),
        }
    }

    /// The conditional operator `c ? a : b`: Kāra's `if` is an expression,
    /// `if c { a } else { b }`. Found where the expression starts; a `?`
    /// Kāra reads as propagation (`f()?`, `x?.y`) has no `:` after it.
    pub(super) fn mend_conditional_operator(&mut self) {
        if self.ident_at(1) == Some("__interp__") {
            return;
        }
        let p = self.pos;
        if matches!(
            self.tok_at(p),
            Some(
                Token::Return
                    | Token::Break
                    | Token::Continue
                    | Token::If
                    | Token::Match
                    | Token::Loop
                    | Token::While
                    | Token::For
                    | Token::Let
                    | Token::Pipe
                    | Token::LeftBrace
                    | Token::Else
                    | Token::EOF
            ) | None
        ) {
            return;
        }
        // `stop` at depth 0, before anything that ends the expression.
        let scan = |me: &Self, from: usize, stop: &Token| -> Option<usize> {
            let mut depth = 0i32;
            let mut i = from;
            loop {
                let t = me.tok_at(i)?;
                match t {
                    Token::LeftParen | Token::LeftBracket => depth += 1,
                    Token::RightParen | Token::RightBracket => {
                        depth -= 1;
                        if depth < 0 {
                            return None;
                        }
                    }
                    t if depth == 0 && t == stop => return Some(i),
                    Token::LeftBrace
                    | Token::RightBrace
                    | Token::Semicolon
                    | Token::Equal
                    | Token::FatArrow
                    | Token::Arrow
                    | Token::PlusEqual
                    | Token::MinusEqual
                    | Token::StarEqual
                    | Token::SlashEqual
                    | Token::PercentEqual
                    | Token::QuestionQuestion
                    | Token::QuestionDot
                    | Token::EOF => return None,
                    Token::Comma | Token::Colon if depth == 0 => return None,
                    _ => {}
                }
                i += 1;
            }
        };
        let Some(q) = scan(self, p, &Token::Question).filter(|&q| q > p) else {
            return;
        };
        // What follows a conditional's `?` starts an expression.
        if !matches!(
            self.tok_at(q + 1),
            Some(
                Token::Identifier { .. }
                    | Token::Integer(..)
                    | Token::Float(..)
                    | Token::StringLiteral(_)
                    | Token::InterpolatedStringLiteral(_)
                    | Token::CharLiteral(_)
                    | Token::True
                    | Token::False
                    | Token::LeftParen
                    | Token::LeftBracket
                    | Token::Bang
                    | Token::SelfValue
            )
        ) {
            return;
        }
        let Some(colon) = scan(self, q + 1, &Token::Colon).filter(|&c| c > q + 1) else {
            return;
        };
        // The alternative runs to whatever ends the expression.
        let mut depth = 0i32;
        let mut end = colon + 1;
        loop {
            match self.tok_at(end) {
                Some(Token::LeftParen | Token::LeftBracket | Token::LeftBrace) => depth += 1,
                Some(Token::RightParen | Token::RightBracket | Token::RightBrace) if depth > 0 => {
                    depth -= 1
                }
                Some(
                    Token::RightParen
                    | Token::RightBracket
                    | Token::RightBrace
                    | Token::Semicolon
                    | Token::Comma
                    | Token::EOF,
                )
                | None => break,
                _ => {}
            }
            end += 1;
        }
        if end == colon + 1 {
            return;
        }
        let (Some(c), Some(a), Some(b)) = (
            self.tokens_text(p, q),
            self.tokens_text(q + 1, colon),
            self.conditional_alternative(colon + 1, end),
        ) else {
            return;
        };
        let at = self.span_at(q);
        self.rewrite(
            "`c ? a : b` is the conditional operator of C, Java and JavaScript: Kāra's \
             `if` is an expression, `if c { a } else { b }`",
            at,
            p,
            end - 1,
            &format!("if {c} {{ {a} }} else {{ {b} }}"),
        );
    }
}
