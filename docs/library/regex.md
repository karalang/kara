# Regular expressions: `std.regex`

`std.regex` holds `Regex`, a compiled pattern; `RegexError`, why a pattern did not compile; `Match`, one match; and `Captures`, the groups of one match.

```kara
match Regex.compile("(?P<key>\\w+)=(?P<value>\\d+)") {
    Ok(re) => {
        for c in re.captures_all("a=1, b=22") {
            match (c.name("key"), c.name("value")) {
                (Some(k), Some(v)) => println(f"{k.text} -> {v.text}"),
                _ => {}
            }
        }
    }
    Err(e) => eprintln(e.message),
}
```

## Syntax

The pattern syntax is RE2's, as the Rust `regex` crate implements it. There are no backreferences and no lookaround, which is what lets every search run in time linear in the length of the pattern and the text: a pattern from an untrusted source cannot make a search take exponential time.

Matching is Unicode-aware by default. `.` matches one character, not one byte, and `\d`, `\w` and `\s` are Unicode classes: `\d` matches any Unicode decimal digit, including non-ASCII ones. Write `[0-9]` (or `(?-u:\d)`) to match ASCII digits only. Flags are set inside the pattern: `(?i)` for case-insensitive, `(?m)` for `^` and `$` at line ends, `(?s)` for `.` matching a newline, `(?x)` for whitespace and comments.

## Offsets

Offsets are byte offsets into the searched string, as everywhere in [strings.md](strings.md): `m.start` and `m.end` delimit the match, and `s[m.start..m.end]` is `m.text`. A match always begins and ends on a character boundary.

## `Regex`

| Method | Signature | Notes |
|---|---|---|
| `compile` | `fn compile(pattern: Str) -> Result[Regex, RegexError]` | `Err` when `pattern` is not a valid pattern. Compiling is not free; compile once, outside loops |
| `escape` | `fn escape(s: Str) -> String` | `s` with every metacharacter escaped, so that `Regex.compile(Regex.escape(s))` matches `s` literally |
| `is_match` | `fn is_match(self, s: Str) -> bool` | Whether the pattern matches anywhere in `s` |
| `find` | `fn find(self, s: Str) -> Option[Match]` | The leftmost match |
| `find_all` | `fn find_all(self, s: Str) -> Vec[Match]` | Every match, left to right, none overlapping |
| `replace` | `fn replace(self, s: Str, rep: Str) -> String` | `s` with its first match replaced by `rep` (see [Replacement](#replacement)) |
| `replace_all` | `fn replace_all(self, s: Str, rep: Str) -> String` | `s` with every match replaced by `rep` |
| `split` | `fn split(self, s: Str) -> impl Iterator[Item = Str]` | The pieces of `s` between matches, as views into `s`, like `Str.split`. A match at either end gives an empty first or last piece. **Not yet implemented** as an iterator: it returns `Vec[String]`, as `Str.split` does today |
| `captures` | `fn captures(self, s: Str) -> Option[Captures]` | The groups of the leftmost match |
| `captures_all` | `fn captures_all(self, s: Str) -> Vec[Captures]` | The groups of every match, one `Captures` per match, left to right, none overlapping |

A `Regex` is immutable, so one value can be shared by `ref` across `par` branches and tasks. Compiling a pattern once and sharing it is the pattern for a server that matches every request.

An empty match is reported once at each position: `find_all`, `captures_all` and `split` skip an empty match that ends where the previous match ended. `a*` in `"baaab"` matches at `0..0`, `1..4` and `5..5`.

## `Match`

```kara
struct Match {
    text: String,
    start: i64,
    end: i64,
}
```

## `Captures`

Group 0 is the whole match; groups are numbered from 1 in the order of their opening parentheses.

| Method | Signature | Notes |
|---|---|---|
| `get` | `fn get(self, i: i64) -> Option[Match]` | Group `i`. `None` when the group did not take part in the match (`(a)|b` matching `"b"`) or there is no group `i` |
| `name` | `fn name(self, n: Str) -> Option[Match]` | The group named `n` (`(?P<n>...)` or `(?<n>...)`). `None` when it did not take part or there is no such group |
| `len` | `fn len(self) -> i64` | The number of groups, group 0 included |

## Replacement

In `replace` and `replace_all`, `rep` may refer to the match's groups: `$1` or `${1}` by number, `$name` or `${name}` by name, and `$$` for a literal `$`. A group that did not take part in the match, or does not exist, is replaced by the empty string. `$name` takes the longest run of letters, digits and `_` after the `$`, so write `${1}a` rather than `$1a`.

## `RegexError`

```kara
struct RegexError {
    message: String,
}
```

`message` is the engine's description, which shows the pattern and marks where the error is.

## Effects

None. Every method is pure: it reads its arguments and allocates the values it returns.

## Not in v1

Regexes over bytes (`Slice[u8]`) rather than text; matching a set of patterns at once (`RegexSet`); streaming matches over a reader; a flag API beyond the inline `(?i)` flags; and a builder for size and nesting limits. See [deferred.md](../deferred.md#stdlib-and-ecosystem-security-conventions).
