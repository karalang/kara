//! Kotlin's and Swift's first-day habits, mended where the Kāra form
//! computes the same value (B-2026-11-24-47, the fifth habit sweep): `val`,
//! `fun f(): T = e`, `?:`, `!!` and `!`, `T?`, `when`, `guard let` and
//! `if let x = opt`, `listOf(..)`, `v.forEach { it }`, `{ x -> e }`,
//! `"\(x)"`, `1...3` and `0..<3`, Kotlin's `for (i in 1..3)`, `data class`,
//! Swift's `_` parameter labels and `let` / `var` fields, and the
//! conditional expression `if (c) a else b`.
//!
//! The rule is the earlier sweeps': a habit whose Kāra form means the same
//! thing is refused with that form and carries the edit; one whose meaning
//! differs (`mapOf` keeps insertion order, `downTo`, `?.let`) is told, with
//! no edit.

use crate::token::Token;

use super::Parser;

impl Parser {
    /// The whole-file pass for Kotlin and Swift.
    pub(super) fn mend_kotlin_swift_tokens(&mut self) {
        if self.source.is_none() {
            return;
        }
        let mut i = 0;
        while i < self.tokens.len() {
            if !self.mend_kotlin_swift_token_at(i) {
                i += 1;
            }
        }
    }

    /// Whether token `k` ends an operand: a name, a literal, `)` or `]`.
    fn ends_operand(&self, k: usize) -> bool {
        matches!(
            self.tok_at(k),
            Some(
                Token::Identifier { .. }
                    | Token::Integer(..)
                    | Token::Float(..)
                    | Token::StringLiteral(_)
                    | Token::RightParen
                    | Token::RightBracket
                    | Token::SelfValue
            )
        )
    }

    fn mend_kotlin_swift_token_at(&mut self, i: usize) -> bool {
        // `"n is \(n)"`: Swift's interpolation, Kāra's f-string.
        if let Some(Token::Error(m)) = self.tok_at(i) {
            if m == "Unknown escape sequence: \\(" {
                return self.mend_swift_interpolation(i);
            }
        }
        // `a ?: b`: Kotlin's elvis operator, Kāra's `??`.
        if matches!(self.tok_at(i), Some(Token::Question))
            && matches!(self.tok_at(i + 1), Some(Token::Colon))
            && self.touching(i, i + 1)
            && i > 0
            && self.ends_operand(i - 1)
        {
            let at = self.span_at(i);
            let colon = self.span_at(i + 1);
            self.habit(
                "`?:` is Kotlin's elvis operator: Kāra's is `??`, the value inside an \
                 `Option`, or the default when it is `None`",
                at,
                vec![Self::edit(
                    at.offset,
                    colon.offset + colon.length - at.offset,
                    "??",
                )],
            );
            self.tokens[i].token = Token::QuestionQuestion;
            self.tokens.remove(i + 1);
            return false;
        }
        // `x!!` (Kotlin) / `x!` (Swift): the value, or a crash at `None`;
        // Kāra's `x.unwrap()`.
        if matches!(self.tok_at(i), Some(Token::Bang))
            && i > 0
            && self.ends_operand(i - 1)
            && self.touching(i - 1, i)
        {
            let double = matches!(self.tok_at(i + 1), Some(Token::Bang)) && self.touching(i, i + 1);
            let next = i + 1 + usize::from(double);
            if matches!(
                self.tok_at(next),
                Some(
                    Token::Semicolon
                        | Token::RightParen
                        | Token::RightBracket
                        | Token::RightBrace
                        | Token::Comma
                        | Token::Dot
                        | Token::EOF
                )
            ) || self.span_at(next.min(self.tokens.len() - 1)).line != self.span_at(i).line
            {
                let at = self.span_at(i);
                let last = self.span_at(next - 1);
                let (sym, lang) = if double {
                    ("!!", "Kotlin's")
                } else {
                    ("!", "Swift's")
                };
                self.habit(
                    &format!(
                        "`x{sym}` is {lang}: Kāra takes the value out of an `Option` with \
                         `x.unwrap()`, which stops the program at `None` as `{sym}` does"
                    ),
                    at,
                    vec![Self::edit(
                        at.offset,
                        last.offset + last.length - at.offset,
                        ".unwrap()",
                    )],
                );
                self.splice_text(i, next - 1, ".unwrap()", at);
                return false;
            }
        }
        // `x: Int?` / `-> String?`: Kotlin's and Swift's nullable type.
        if matches!(self.tok_at(i), Some(Token::Question))
            && i > 1
            && self.touching(i - 1, i)
            && self
                .ident_at(i - 1)
                .is_some_and(|w| w.starts_with(|c: char| c.is_ascii_uppercase()))
            && matches!(self.tok_at(i - 2), Some(Token::Colon | Token::Arrow))
            && matches!(
                self.tok_at(i + 1),
                Some(
                    Token::Comma
                        | Token::RightParen
                        | Token::Equal
                        | Token::LeftBrace
                        | Token::Semicolon
                )
            )
        {
            let ty = self.ident_at(i - 1).unwrap_or_default().to_string();
            let at = self.span_at(i - 1);
            let text = format!("Option[{ty}]");
            self.rewrite(
                &format!(
                    "`{ty}?` is Kotlin's and Swift's nullable type: Kāra's is `{text}`, `None` \
                     where they write `null` / `nil`"
                ),
                at,
                i - 1,
                i,
                &text,
            );
            return true;
        }
        // `m: [String: Int]`: Swift's dictionary type, Kāra's `Map[K, V]`.
        if matches!(self.tok_at(i), Some(Token::LeftBracket))
            && i > 0
            && matches!(self.tok_at(i - 1), Some(Token::Colon | Token::Arrow))
            && self.ident_at(i + 1).is_some()
            && matches!(self.tok_at(i + 2), Some(Token::Colon))
            && self.ident_at(i + 3).is_some()
            && matches!(self.tok_at(i + 4), Some(Token::RightBracket))
        {
            let (k, v) = (
                self.ident_at(i + 1).unwrap_or_default().to_string(),
                self.ident_at(i + 3).unwrap_or_default().to_string(),
            );
            let text = format!("Map[{k}, {v}]");
            let at = self.span_at(i);
            self.rewrite(
                &format!("`[{k}: {v}]` is Swift's dictionary type: Kāra's is `{text}`"),
                at,
                i,
                i + 4,
                &text,
            );
            return true;
        }
        // Swift's ranges `1...3` and `0..<3`.
        if i > 0 && self.ends_operand(i - 1) {
            if matches!(self.tok_at(i), Some(Token::DotDotDot))
                && self.tok_at(i + 1).is_some()
                && !matches!(self.tok_at(i + 1), Some(Token::RightBracket | Token::Comma))
            {
                let at = self.span_at(i);
                self.habit(
                    "`a...b` is Swift's closed range: Kāra's is `a..=b`",
                    at,
                    vec![Self::edit(at.offset, at.length, "..=")],
                );
                self.tokens[i].token = Token::DotDotEq;
                return false;
            }
            if matches!(self.tok_at(i), Some(Token::DotDot))
                && matches!(self.tok_at(i + 1), Some(Token::LessThan))
                && self.touching(i, i + 1)
            {
                let at = self.span_at(i);
                let lt = self.span_at(i + 1);
                self.habit(
                    "`a..<b` is Swift's half-open range: Kāra's is `a..b`",
                    at,
                    vec![Self::edit(lt.offset, lt.length, "")],
                );
                self.tokens.remove(i + 1);
                return false;
            }
        }
        // `_ a: Int`: a Swift parameter with no argument label.
        if matches!(self.tok_at(i), Some(Token::Underscore))
            && i > 0
            && matches!(self.tok_at(i - 1), Some(Token::LeftParen | Token::Comma))
            && self.ident_at(i + 1).is_some()
            && matches!(self.tok_at(i + 2), Some(Token::Colon))
        {
            let at = self.span_at(i);
            let name = self.span_at(i + 1);
            self.habit(
                "`_ a: T` is Swift's parameter with no label: Kāra's arguments are \
                 positional, so it is `a: T`",
                at,
                vec![Self::edit(at.offset, name.offset - at.offset, "")],
            );
            self.tokens.remove(i);
            return true;
        }
        if self.mend_braceless_if(i) {
            return true;
        }
        // `fun f(x: Int): Int = x * x`: an expression body.
        if self.mend_expression_body(i) {
            return true;
        }
        // `data class P(val x: Int)`.
        if self.mend_data_class(i) {
            return true;
        }
        // `struct P { let x: Int; var y: Int }`: Swift's fields.
        if matches!(self.tok_at(i), Some(Token::Struct)) {
            self.mend_swift_fields(i);
        }
        let after_dot = i > 0 && matches!(self.tok_at(i - 1), Some(Token::Dot));
        let Some(word) = self.ident_at(i).map(str::to_string) else {
            return false;
        };
        if after_dot {
            return self.mend_trailing_lambda(i, &word);
        }
        if matches!(self.tok_at(i + 1), Some(Token::LeftParen))
            && matches!(
                word.as_str(),
                "listOf"
                    | "mutableListOf"
                    | "arrayListOf"
                    | "emptyList"
                    | "mutableList"
                    | "mapOf"
                    | "mutableMapOf"
                    | "setOf"
                    | "mutableSetOf"
                    | "emptyMap"
                    | "emptySet"
            )
            && !self.word_is_bound(&word)
        {
            return self.mend_kotlin_collection(i, &word);
        }
        false
    }

    /// `"n is \(n)"` -> `f"n is {n}"`.
    fn mend_swift_interpolation(&mut self, i: usize) -> bool {
        let at = self.span_at(i);
        let Some(lit) = self.source_between(at.offset, at.offset + at.length) else {
            return false;
        };
        if !(lit.starts_with('"') && lit.ends_with('"') && lit.len() >= 2) {
            return false;
        }
        let body = &lit[1..lit.len() - 1];
        let mut out = String::new();
        let mut rest = body;
        while let Some(k) = rest.find("\\(") {
            // A `\\(` is an escaped backslash and a `(`.
            let backslashes = rest[..k].chars().rev().take_while(|&c| c == '\\').count();
            if backslashes % 2 == 1 {
                out.push_str(&rest[..k + 2]);
                rest = &rest[k + 2..];
                continue;
            }
            out.push_str(&rest[..k].replace('{', "{{").replace('}', "}}"));
            let inner = &rest[k + 2..];
            let mut depth = 1;
            let mut end = None;
            for (j, c) in inner.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(end) = end else {
                return false;
            };
            let hole = &inner[..end];
            if hole.contains(['{', '}', '"']) || hole.trim().is_empty() {
                return false;
            }
            out.push('{');
            out.push_str(hole);
            out.push('}');
            rest = &inner[end + 1..];
        }
        out.push_str(&rest.replace('{', "{{").replace('}', "}}"));
        let text = format!("f\"{out}\"");
        self.rewrite(
            &format!("`\\(x)` is Swift's interpolation: Kāra's is an f-string, `{text}`"),
            at,
            i,
            i,
            &text,
        );
        true
    }

    /// `fun f(x: Int): Int = x * x` (Kotlin) -> `{ x * x }`, at an item's
    /// `fun` / `fn`: the expression runs to the end of its line.
    fn mend_expression_body(&mut self, i: usize) -> bool {
        let starts = matches!(self.tok_at(i), Some(Token::Fn))
            || matches!(self.ident_at(i), Some("fun" | "func"));
        if !starts
            || i > 0
                && !matches!(
                    self.tok_at(i - 1),
                    Some(Token::RightBrace | Token::Semicolon | Token::Pub)
                )
            || self.ident_at(i + 1).is_none()
            || !matches!(self.tok_at(i + 2), Some(Token::LeftParen))
        {
            return false;
        }
        let Some(close) = self.matching_close(i + 2) else {
            return false;
        };
        // The result type, to the `=` on the header's line.
        let line = self.span_at(close).line;
        let mut eq = close + 1;
        let mut depth = 0i32;
        loop {
            match self.tok_at(eq) {
                Some(Token::LeftBracket | Token::LeftParen | Token::LessThan) => depth += 1,
                Some(Token::RightBracket | Token::RightParen | Token::GreaterThan) => depth -= 1,
                Some(Token::Equal) if depth == 0 => break,
                Some(Token::LeftBrace | Token::Semicolon | Token::EOF) | None => return false,
                _ if self.span_at(eq).line != line => return false,
                _ => {}
            }
            eq += 1;
        }
        if matches!(self.tok_at(eq + 1), Some(Token::Equal | Token::LeftBrace)) {
            return false;
        }
        let eq_line = self.span_at(eq).line;
        let mut end = eq + 1;
        let mut depth = 0i32;
        while let Some(t) = self.tok_at(end) {
            match t {
                Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                Token::RightParen | Token::RightBracket | Token::RightBrace => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                Token::Semicolon | Token::EOF if depth == 0 => break,
                _ if depth == 0 && self.span_at(end).line != eq_line => break,
                _ => {}
            }
            end += 1;
        }
        if end == eq + 1 {
            return false;
        }
        let Some(body) = self.tokens_text(eq + 1, end) else {
            return false;
        };
        let at = self.span_at(eq);
        let semi = matches!(self.tok_at(end), Some(Token::Semicolon));
        let last = if semi { end } else { end - 1 };
        let text = format!("{{ {body} }}");
        self.rewrite(
            &format!(
                "`fun f(..) = e` is Kotlin's expression body: a Kāra function's body is a \
                 block, `{text}`"
            ),
            at,
            eq,
            last,
            &text,
        );
        true
    }

    /// `data class P(val x: Int, val y: Int)` -> `struct P { x: Int, y: Int }`.
    fn mend_data_class(&mut self, i: usize) -> bool {
        let data = self.ident_at(i) == Some("data");
        let c = i + usize::from(data);
        if self.ident_at(c) != Some("class")
            || self.ident_at(c + 1).is_none()
            || !matches!(self.tok_at(c + 2), Some(Token::LeftParen))
            || i > 0
                && !matches!(
                    self.tok_at(i - 1),
                    Some(Token::RightBrace | Token::Semicolon | Token::Pub)
                )
        {
            return false;
        }
        let Some(close) = self.matching_close(c + 2) else {
            return false;
        };
        if matches!(
            self.tok_at(close + 1),
            Some(Token::LeftBrace | Token::Colon)
        ) {
            return false;
        }
        let Some((params, _)) = self.call_args(c + 2) else {
            return false;
        };
        let mut fields = Vec::new();
        for (a, b) in params {
            let a = if matches!(self.tok_at(a), Some(Token::Let))
                || self.ident_at(a) == Some("val")
                || self.ident_at(a) == Some("var")
            {
                a + 1
            } else {
                a
            };
            if self.ident_at(a).is_none() || !matches!(self.tok_at(a + 1), Some(Token::Colon)) {
                return false;
            }
            let Some(f) = self.tokens_text(a, b) else {
                return false;
            };
            fields.push(f);
        }
        let name = self.ident_at(c + 1).unwrap_or_default().to_string();
        let semi = matches!(self.tok_at(close + 1), Some(Token::Semicolon));
        let last = if semi { close + 1 } else { close };
        let text = format!("struct {name} {{ {} }}", fields.join(", "));
        let at = self.span_at(i);
        self.rewrite(
            &format!(
                "a `class` whose parameters are its fields is Kotlin's: Kāra declares it as \
                 `{text}`, built with `{name} {{ x: 1 }}`"
            ),
            at,
            i,
            last,
            &text,
        );
        true
    }

    /// Swift's `struct P { let x: Int; var y: Int }`: the fields lose
    /// their `let` / `var`, and are separated by `,`.
    fn mend_swift_fields(&mut self, s: usize) {
        if self.ident_at(s + 1).is_none() || !matches!(self.tok_at(s + 2), Some(Token::LeftBrace)) {
            return;
        }
        let Some(close) = self.matching_brace(s + 2) else {
            return;
        };
        let is_field = |me: &Self, k: usize| {
            (matches!(me.tok_at(k), Some(Token::Let)) || me.ident_at(k) == Some("var"))
                && me.ident_at(k + 1).is_some()
                && matches!(me.tok_at(k + 2), Some(Token::Colon))
        };
        let starts: Vec<usize> = (s + 3..close).filter(|&k| is_field(self, k)).collect();
        if starts.is_empty() {
            return;
        }
        // Each field runs to the next one's start, or the `}`.
        let mut edits = Vec::new();
        let mut drop = Vec::new();
        let mut commas = Vec::new();
        for (n, &k) in starts.iter().enumerate() {
            let kw = self.span_at(k);
            let name = self.span_at(k + 1);
            edits.push(Self::edit(kw.offset, name.offset - kw.offset, ""));
            drop.push(k);
            let end = starts.get(n + 1).copied().unwrap_or(close);
            let last = end - 1;
            match self.tok_at(last) {
                Some(Token::Semicolon) => {
                    let sp = self.span_at(last);
                    edits.push(Self::edit(sp.offset, sp.length, ","));
                    commas.push((last, true));
                }
                Some(Token::Comma) => {}
                _ if n + 1 < starts.len() => {
                    let sp = self.span_at(last);
                    edits.push(Self::edit(sp.offset + sp.length, 0, ","));
                    commas.push((last, false));
                }
                _ => {}
            }
        }
        let at = self.span_at(starts[0]);
        self.habit(
            "a field written `let x: T` or `var x: T` is Swift's: a Kāra field is `x: T`, and \
             fields are separated by `,`",
            at,
            edits,
        );
        // Tokens, from the end back so the indices hold.
        let mut ops: Vec<(usize, u8)> = drop.iter().map(|&k| (k, 0)).collect();
        ops.extend(commas.iter().map(|&(k, r)| (k, if r { 1 } else { 2 })));
        ops.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        for (k, op) in ops {
            match op {
                0 => {
                    self.tokens.remove(k);
                }
                1 => self.tokens[k].token = Token::Comma,
                _ => {
                    let sp = self.span_at(k);
                    self.tokens.insert(k + 1, Self::synth(Token::Comma, sp));
                }
            }
        }
    }

    /// `v.forEach { println(it) }`, `v.map { it * 2 }`, `v.filter { x -> x > 1 }`:
    /// Kotlin's trailing lambda. `word` is the method, at `i`, after a `.`.
    fn mend_trailing_lambda(&mut self, i: usize, word: &str) -> bool {
        let (kara, collect) = match word {
            "forEach" => ("for_each", false),
            "map" => ("map", true),
            "filter" => ("filter", true),
            "any" => ("any", false),
            "all" => ("all", false),
            _ => return false,
        };
        if !matches!(self.tok_at(i + 1), Some(Token::LeftBrace)) {
            return false;
        }
        let Some(close) = self.matching_brace(i + 1) else {
            return false;
        };
        let Some((params, body)) = self.kotlin_lambda_parts(i + 1, close) else {
            return false;
        };
        let text = if collect {
            format!(".iter().{kara}(|{params}| {body}).collect()")
        } else {
            format!(".iter().{kara}(|{params}| {body})")
        };
        // The `.` before the method goes with it.
        let at = self.span_at(i);
        self.rewrite(
            &format!(
                "`v.{word} {{ .. }}` is Kotlin's trailing lambda: Kāra passes a closure in \
                 parentheses, over the list's iterator, `v{text}`"
            ),
            at,
            i - 1,
            close,
            &text,
        );
        true
    }

    /// A Kotlin lambda `{ x -> body }` or `{ body with it }` between the
    /// braces at `open` and `close`: its parameter text and its body.
    pub(super) fn kotlin_lambda_parts(
        &self,
        open: usize,
        close: usize,
    ) -> Option<(String, String)> {
        let mut depth = 0i32;
        let mut arrow = None;
        for k in open + 1..close {
            match self.tok_at(k)? {
                Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                Token::RightParen | Token::RightBracket | Token::RightBrace => depth -= 1,
                Token::Arrow if depth == 0 => {
                    arrow = Some(k);
                    break;
                }
                _ => {}
            }
        }
        if let Some(a) = arrow {
            // Names, each with an optional `: T`.
            let mut k = open + 1;
            while k < a {
                self.ident_at(k)?;
                k += 1;
                if matches!(self.tok_at(k), Some(Token::Colon)) {
                    k += 2;
                }
                if k < a {
                    if !matches!(self.tok_at(k), Some(Token::Comma)) {
                        return None;
                    }
                    k += 1;
                }
            }
            let params = self.tokens_text(open + 1, a)?;
            let body = self.tokens_text(a + 1, close)?;
            return Some((params, body.trim().to_string()));
        }
        let uses_it = (open + 1..close).any(|k| self.ident_at(k) == Some("it"));
        if !uses_it {
            return None;
        }
        let body = self.tokens_text(open + 1, close)?;
        if body.contains(';') {
            return Some(("it".to_string(), format!("{{ {} }}", body.trim())));
        }
        Some(("it".to_string(), body.trim().to_string()))
    }

    /// `listOf(1, 2)` / `mutableListOf()` / `arrayListOf(..)` /
    /// `emptyList()`: Kāra's list. `mapOf` and `setOf` keep their
    /// insertion order, which Kāra's `Map` and `Set` do not: told.
    fn mend_kotlin_collection(&mut self, i: usize, word: &str) -> bool {
        let list = matches!(
            word,
            "listOf" | "mutableListOf" | "arrayListOf" | "emptyList" | "mutableList"
        );
        let unordered = matches!(
            word,
            "mapOf" | "mutableMapOf" | "setOf" | "mutableSetOf" | "emptyMap" | "emptySet"
        );
        if !list && !unordered {
            return false;
        }
        let Some(close) = self.matching_close(i + 1) else {
            return false;
        };
        let at = self.span_at(i);
        if unordered {
            self.error_at(
                &format!(
                    "`{word}` is Kotlin's: its map or set keeps the order things were added, \
                     and Kāra's `Map` and `Set` do not, so the program is written by hand: \
                     `Map.from([(k, v)])`, `Set.from([..])`, or `SortedMap` / `SortedSet` \
                     for an order by key"
                ),
                at,
            );
            return false;
        }
        let text = if close == i + 2 {
            "Vec.new()".to_string()
        } else {
            let Some(inner) = self.tokens_text(i + 2, close) else {
                return false;
            };
            format!("[{inner}]")
        };
        self.rewrite(
            &format!("`{word}(..)` is Kotlin's: Kāra's list is `{text}` (`let mut` when it grows)"),
            at,
            i,
            close,
            &text,
        );
        true
    }

    /// `val x = 3` -> `let x = 3`; Swift's `guard let x = e else { .. }` ->
    /// `let Some(x) = e else { .. };` and `guard c else { .. }` ->
    /// `if not (c) { .. }`.
    pub(super) fn mend_kotlin_swift_statement_forms(&mut self) -> bool {
        let p = self.pos;
        if p >= self.tokens.len() {
            return false;
        }
        let at = self.span_at(p);
        if self.ident_at(p) == Some("val")
            && self.ident_at(p + 1).is_some()
            && matches!(self.tok_at(p + 2), Some(Token::Equal | Token::Colon))
            && !self.word_is_bound("val")
        {
            self.habit(
                "`val` is Kotlin's binding that is never reassigned: Kāra's is `let`",
                at,
                vec![Self::edit(at.offset, at.length, "let")],
            );
            self.tokens[p].token = Token::Let;
            return true;
        }
        if self.ident_at(p) == Some("guard") && !self.word_is_bound("guard") {
            // The `else` at depth 0, and its block.
            let mut depth = 0i32;
            let mut k = p + 1;
            let els = loop {
                match self.tok_at(k) {
                    Some(Token::LeftParen | Token::LeftBracket) => depth += 1,
                    Some(Token::RightParen | Token::RightBracket) => depth -= 1,
                    Some(Token::Else) if depth == 0 => break k,
                    Some(Token::LeftBrace | Token::Semicolon | Token::EOF) | None => return false,
                    _ => {}
                }
                k += 1;
            };
            if !matches!(self.tok_at(els + 1), Some(Token::LeftBrace)) {
                return false;
            }
            let Some(end) = self.matching_brace(els + 1) else {
                return false;
            };
            let semi = matches!(self.tok_at(end + 1), Some(Token::Semicolon));
            let Some(block) = self.tokens_text(els + 1, end + 1) else {
                return false;
            };
            let text = if matches!(self.tok_at(p + 1), Some(Token::Let))
                && self.ident_at(p + 2).is_some()
                && matches!(self.tok_at(p + 3), Some(Token::Equal))
            {
                let name = self.ident_at(p + 2).unwrap_or_default().to_string();
                let Some(value) = self.tokens_text(p + 4, els) else {
                    return false;
                };
                format!("let Some({name}) = {value} else {block};")
            } else {
                let Some(cond) = self.tokens_text(p + 1, els) else {
                    return false;
                };
                format!("if not ({cond}) {block}")
            };
            let last = if semi { end + 1 } else { end };
            self.rewrite(
                &format!(
                    "`guard` is Swift's: Kāra writes `{}`",
                    if text.starts_with("let") {
                        "let Some(x) = e else { return };"
                    } else {
                        "if not (c) { return }"
                    }
                ),
                at,
                p,
                last,
                &text,
            );
            return true;
        }
        false
    }

    /// Swift's `if let x {`: Kāra's `if let Some(x) = x {`. At the `let`.
    /// Swift's `if let x = opt {` is left alone: it is Kāra too, a binding
    /// that matches anything (lint W0302), so it cannot be refused.
    pub(super) fn mend_swift_if_let(&mut self) {
        let p = self.pos;
        if !matches!(self.tok_at(p), Some(Token::Let)) {
            return;
        }
        let Some(name) = self.ident_at(p + 1).map(str::to_string) else {
            return;
        };
        if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
            return;
        }
        let at = self.span_at(p + 1);
        if matches!(self.tok_at(p + 2), Some(Token::LeftBrace)) {
            let text = format!("Some({name}) = {name}");
            self.rewrite(
                &format!("`if let x {{` is Swift's: Kāra writes `if let {text} {{`"),
                at,
                p + 1,
                p + 1,
                &text,
            );
        }
    }

    /// Kotlin's `when (x) { 1 -> a; 2, 3 -> b; else -> c }` -> `match x {
    /// 1 => a, 2 | 3 => b, _ => c }`, and `when { c -> a; else -> b }` ->
    /// an `if` chain. At the parser's position, in an expression.
    pub(super) fn mend_kotlin_when(&mut self) {
        let p = self.pos;
        if self.ident_at(p) != Some("when") || self.word_is_bound("when") {
            return;
        }
        let (subject, open) = match self.tok_at(p + 1) {
            Some(Token::LeftParen) => {
                let Some(c) = self.matching_close(p + 1) else {
                    return;
                };
                (self.tokens_text(p + 2, c), c + 1)
            }
            Some(Token::LeftBrace) => (None, p + 1),
            _ => return,
        };
        if !matches!(self.tok_at(open), Some(Token::LeftBrace)) {
            return;
        }
        if matches!(self.tok_at(p + 1), Some(Token::LeftParen)) && subject.is_none() {
            return;
        }
        let Some(close) = self.matching_brace(open) else {
            return;
        };
        // Arms: `pattern -> body`, each ending at a `;`, a `,` after a
        // body, or a new line, at depth 0.
        let mut arms: Vec<(usize, usize, usize)> = Vec::new();
        let mut k = open + 1;
        while k < close {
            let start = k;
            let mut depth = 0i32;
            let mut arrow = None;
            while k < close {
                match self.tok_at(k) {
                    Some(Token::LeftParen | Token::LeftBracket | Token::LeftBrace) => depth += 1,
                    Some(Token::RightParen | Token::RightBracket | Token::RightBrace) => depth -= 1,
                    Some(Token::Arrow) if depth == 0 && arrow.is_none() => arrow = Some(k),
                    Some(Token::Semicolon) if depth == 0 => break,
                    _ => {}
                }
                // An arm ends at its line's end, or at the `}`.
                let line_end = depth == 0
                    && arrow.is_some_and(|a| k > a + 1)
                    && self.span_at(k + 1).line != self.span_at(k).line
                    && !matches!(self.tok_at(k + 1), Some(Token::RightBrace));
                if (line_end || depth == 0 && k + 1 == close) && arrow.is_some_and(|a| k > a) {
                    k += 1;
                    break;
                }
                k += 1;
            }
            let Some(a) = arrow else {
                return;
            };
            let end = if matches!(self.tok_at(k), Some(Token::Semicolon)) {
                k
            } else {
                k.min(close)
            };
            if a == start || end <= a + 1 {
                return;
            }
            arms.push((start, a, end));
            if matches!(self.tok_at(k), Some(Token::Semicolon)) {
                k += 1;
            }
        }
        if arms.is_empty() {
            return;
        }
        let mut parts = Vec::new();
        for &(s, a, e) in &arms {
            let e = if matches!(self.tok_at(e - 1), Some(Token::Comma)) {
                e - 1
            } else {
                e
            };
            let (Some(pat), Some(body)) = (self.tokens_text(s, a), self.tokens_text(a + 1, e))
            else {
                return;
            };
            if pat.starts_with("in ") || pat.starts_with("is ") || pat.starts_with('!') {
                let at = self.span_at(p);
                self.error_at(
                    "`when` with an `in` or `is` arm is Kotlin's: Kāra matches a range with \
                     `match x { 1..=5 => .., _ => .. }` (`in a..b` includes `b`), and a type \
                     with an `enum`'s cases",
                    at,
                );
                // Told: parsed on as `()`.
                self.splice_text(p, close, "()", at);
                return;
            }
            parts.push((pat, body));
        }
        let text = if let Some(subject) = subject {
            let arms: Vec<String> = parts
                .iter()
                .map(|(pat, body)| {
                    let pat = if pat == "else" {
                        "_".to_string()
                    } else {
                        pat.split(',')
                            .map(str::trim)
                            .collect::<Vec<_>>()
                            .join(" | ")
                    };
                    format!("{pat} => {body}")
                })
                .collect();
            format!("match {subject} {{ {} }}", arms.join(", "))
        } else {
            let mut text = String::new();
            for (n, (cond, body)) in parts.iter().enumerate() {
                if cond == "else" {
                    text.push_str(&format!(" else {{ {body} }}"));
                    break;
                }
                if n > 0 {
                    text.push_str(" else ");
                }
                text.push_str(&format!("if {cond} {{ {body} }}"));
            }
            text
        };
        let at = self.span_at(p);
        self.rewrite(
            "`when` is Kotlin's: Kāra branches on a value with `match x { 1 => a, _ => b }`, \
             and on conditions with `if c { a } else { b }`",
            at,
            p,
            close,
            &text,
        );
    }
    /// Kotlin's lambda as a value, `{ x: Int -> x * 2 }`: Kāra's closure,
    /// `|x: Int| x * 2`. At the parser's position, a `{` where a value is due.
    pub(super) fn mend_kotlin_lambda_value(&mut self) {
        let p = self.pos;
        if !matches!(self.tok_at(p), Some(Token::LeftBrace))
            || self.ident_at(p + 1).is_none()
            || self.ident_at(1) == Some("__interp__")
        {
            return;
        }
        let Some(close) = self.matching_brace(p) else {
            return;
        };
        // Only the arrow form: a block may name a variable `it`.
        if !(p + 1..close).any(|k| matches!(self.tok_at(k), Some(Token::Arrow))) {
            return;
        }
        let Some((params, body)) = self.kotlin_lambda_parts(p, close) else {
            return;
        };
        if params == "it" {
            return;
        }
        let text = format!("|{params}| {body}");
        let at = self.span_at(p);
        self.rewrite(
            &format!("`{{ x -> e }}` is Kotlin's lambda: Kāra's closure is `{text}`"),
            at,
            p,
            close,
            &text,
        );
    }

    /// `if (c) a else b` / `if (x > 0) return;`: a branch with no braces,
    /// Kotlin's, C's and Java's. Kāra's branches are blocks, `if c { a }
    /// else { b }`. At the `if` token `i`, in the whole-file pass.
    fn mend_braceless_if(&mut self, i: usize) -> bool {
        if !matches!(self.tok_at(i), Some(Token::If))
            || !matches!(self.tok_at(i + 1), Some(Token::LeftParen))
        {
            return false;
        }
        let Some(close) = self.matching_close(i + 1) else {
            return false;
        };
        // What follows the condition must start a value or a statement.
        if !matches!(
            self.tok_at(close + 1),
            Some(
                Token::Integer(..)
                    | Token::Float(..)
                    | Token::StringLiteral(_)
                    | Token::InterpolatedStringLiteral(_)
                    | Token::True
                    | Token::False
                    | Token::Return
                    | Token::Break
                    | Token::Continue
            )
        ) && !self
            .ident_at(close + 1)
            .is_some_and(|w| !matches!(w, "and" | "or" | "as" | "is" | "not" | "in"))
        {
            return false;
        }
        // The branch ends at `else`, a `;`, or what closes the expression.
        let branch_end = |me: &Self, from: usize| -> Option<(usize, bool)> {
            let mut depth = 0i32;
            let mut k = from;
            let line = me.span_at(from).line;
            loop {
                match me.tok_at(k)? {
                    Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                    Token::RightParen | Token::RightBracket | Token::RightBrace => {
                        if depth == 0 {
                            return Some((k, false));
                        }
                        depth -= 1;
                    }
                    Token::Else if depth == 0 => return Some((k, true)),
                    Token::Semicolon | Token::Comma if depth == 0 => return Some((k, false)),
                    Token::EOF => return Some((k, false)),
                    // A binary operator never follows a whole statement.
                    _ if depth == 0 && k > from && me.span_at(k).line != line => {
                        return Some((k, false))
                    }
                    _ => {}
                }
                k += 1;
            }
        };
        let Some((a_end, has_else)) = branch_end(self, close + 1) else {
            return false;
        };
        let (Some(cond), Some(a)) = (
            self.tokens_text(i + 2, close),
            self.tokens_text(close + 1, a_end),
        ) else {
            return false;
        };
        let at = self.span_at(i);
        let note = "a branch with no braces is Kotlin's, C's and Java's: a Kāra branch is a \
                    block, `if c { a } else { b }`";
        if has_else {
            if matches!(self.tok_at(a_end + 1), Some(Token::LeftBrace | Token::If)) {
                // `else {` / `else if`: only the first branch needs braces.
                let text = format!("if {cond} {{ {a} }}");
                self.rewrite(note, at, i, a_end - 1, &text);
                return true;
            }
            let Some((b_end, _)) = branch_end(self, a_end + 1) else {
                return false;
            };
            let Some(b) = self.tokens_text(a_end + 1, b_end) else {
                return false;
            };
            let semi = matches!(self.tok_at(b_end), Some(Token::Semicolon))
                && matches!(
                    self.tok_at(close + 1),
                    Some(Token::Return | Token::Break | Token::Continue)
                );
            let text = if semi {
                format!("if {cond} {{ {a}; }} else {{ {b}; }}")
            } else {
                format!("if {cond} {{ {a} }} else {{ {b} }}")
            };
            let last = if semi { b_end } else { b_end - 1 };
            self.rewrite(note, at, i, last, &text);
            return true;
        }
        let semi = matches!(self.tok_at(a_end), Some(Token::Semicolon));
        let text = if semi {
            format!("if {cond} {{ {a}; }}")
        } else {
            format!("if {cond} {{ {a} }}")
        };
        let last = if semi { a_end } else { a_end - 1 };
        self.rewrite(note, at, i, last, &text);
        true
    }
}
