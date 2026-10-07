# Strings

Kāra has two string types, both UTF-8:

| Type | What it is | When to use |
|---|---|---|
| `String` | Owned, heap-allocated text | The default: to own and change text |
| `Str` | A borrowed view of UTF-8 text (pointer and length) | Reading, parsing and splitting without copying |

What the types are, how a `String` argument passes as `Str`, why `s[i]` is a compile error, and what `s[a..b]` yields are in [design.md §5](../design.md#strings). This file lists the methods.

**String literals.** A string literal is a `String` by default and a `Str` where a `Str` is expected, as an integer literal is `i64` by default and takes an expected type ([design.md §3](../design.md#string-literals)). A literal typed `Str` is a view of static data: it allocates nothing and borrows nothing, so it may go anywhere, a `const` included. `const NAME: Str = "x";` is the form for string constants. `f"..."` interpolation is in [design.md §3](../design.md#interpolated-strings).

## `Str` and `String`

Every reading method below is defined on `Str`. A `String` receiver finds them too: method lookup on a `String` searches `String`'s own methods first, then `Str`'s, through the same coercion that passes a `String` argument as `Str` ([design.md §9](../design.md#method-resolution)). When a result borrows, it borrows from the receiver ([core-semantics.md §5.4](../core-semantics.md#5-references-and-views-c5)). A `Str` cannot outlive the text it views; to keep one longer, take an owned copy with `to_string()`.

```kara
fn first_word(s: Str) -> Str {
    let end = s.find(" ") ?? s.len();
    s[0..end]                              // no allocation: a view into s
}

let line = "alice,30,engineer";
let w = first_word(line);                  // Str, borrows from line
let owned: String = w.to_string();         // an owned copy, to keep
```

`first_word` has a single view parameter, so its result borrows from `s`. A view of an owned local cannot be returned, because it would dangle.

Splitters return lazy iterators of views, so splitting copies nothing until asked to:

```kara
let parts: Vec[Str] = line.split(",").collect();                         // views into line
let fields: Vec[String] = line.split(",").map(|p| p.to_string()).collect();  // owned copies
```

## Reading: `Str` and `String`

| Method | Signature | Notes |
|---|---|---|
| `len` | `fn len(self) -> i64` | Length in bytes. O(1) |
| `is_empty` | `fn is_empty(self) -> bool` | `self.len() == 0` |
| `chars` | `fn chars(self) -> impl Iterator[Item = char]` | Unicode scalar values, decoded from UTF-8. `s.chars().count()` is the number of scalars |
| `bytes` | `fn bytes(self) -> Slice[u8]` | The raw UTF-8 bytes. `s.bytes()[i]` is a `u8` and panics if out of bounds. Use it for protocols and binary formats, not text |
| `char_at` | `fn char_at(self, i: i64) -> Option[char]` | The `i`-th scalar value. O(n); `None` if out of range. For many indexed reads, collect once (`let cs: Vec[char] = s.chars().collect();`) and index the `Vec`, where indexing really is O(1). For a pass over every character, `for c in s.chars()` is O(n) in total |
| `contains` | `fn contains(self, pat: Str) -> bool` | Substring search |
| `starts_with` | `fn starts_with(self, pat: Str) -> bool` | |
| `ends_with` | `fn ends_with(self, pat: Str) -> bool` | |
| `find` | `fn find(self, pat: Str) -> Option[i64]` | Byte offset of the first match. A valid place to cut with `s[a..b]` |
| `split` | `fn split(self, sep: Str) -> impl Iterator[Item = Str]` | Lazy; each piece is a view into `self` |
| `lines` | `fn lines(self) -> impl Iterator[Item = Str]` | Lazy; splits at line ends |
| `split_whitespace` | `fn split_whitespace(self) -> impl Iterator[Item = Str]` | Lazy; splits at runs of whitespace |
| `trim` | `fn trim(self) -> Str` | Strips leading and trailing whitespace; a view into `self`, like the splitters |
| `replace` | `fn replace(self, from: Str, to: Str) -> String` | Replaces every occurrence |
| `to_uppercase` | `fn to_uppercase(self) -> String` | Unicode uppercasing |
| `to_lowercase` | `fn to_lowercase(self) -> String` | Unicode lowercasing |
| `normalize` | `fn normalize(self, form: NormalizationForm) -> String` | See [Equality and normalization](#equality-and-normalization) |
| `to_string` | `fn to_string(self) -> String` | An owned copy of a `Str`. For a `String`, use `clone()` |
| `[]` | `s[a..b]` | A `Str` over the byte range `[a, b)`, through `Index` ([design.md §5](../design.md#5-types)). Panics if either end falls inside a multi-byte code point |

Byte offsets (`len`, `find`, `s[a..b]`, `bytes()`) count bytes, not characters. Locate a valid cut with `find`, or work in characters with `chars()` and `char_at`.

Parsing text into a number is `T.parse(s)` on the target type, which returns `Result[T, ParseError]` ([core-types.md](core-types.md#parsing)).

## Building: `String` only

| Method | Signature | Notes |
|---|---|---|
| `new` | `fn new() -> String` | Empty string |
| `with_capacity` | `fn with_capacity(cap: i64) -> String` | Preallocates `cap` bytes |
| `from_utf8` | `fn from_utf8(bytes: Slice[u8]) -> Result[String, Utf8Error]` | Checks that the bytes are valid UTF-8 |
| `as_str` | `fn as_str(self) -> Str` | The whole string as a `Str` value, for places other than call boundaries |
| `push` | `fn push(mut ref self, c: char)` | Appends one scalar value, encoded as UTF-8 |
| `push_str` | `fn push_str(mut ref self, s: Str)` | Appends text |
| `clear` | `fn clear(mut ref self)` | Empties the string |
| `reserve` | `fn reserve(mut ref self, additional: i64)` | Makes room for `additional` more bytes. A hint: `additional <= 0` does nothing, and there is no `capacity()` |
| `+` | `fn add(self, other: String) -> String` | Concatenation into a new `String`. Borrows both operands, so `a + b` leaves both usable ([core-semantics.md §4.7](../core-semantics.md#4-parameters-calls-and-patterns)). In a loop, prefer `push_str` |

Each allocating method has a `try_*` twin ([collections.md](collections.md#fallible-allocation)).

## Equality and normalization

`String` and `Str` equality (`==`) compares the raw UTF-8 bytes. Two strings that look the same but use different Unicode normalization forms (NFC and NFD, for example) are **not** equal. For a normalization-aware comparison, normalize both sides:

```kara
let same = a.normalize(NormalizationForm.Nfc) == b.normalize(NormalizationForm.Nfc);
```

`normalize` takes a `NormalizationForm` and returns a new `String`:

```kara
enum NormalizationForm { Nfc, Nfd, Nfkc, Nfkd }
```

`NormalizationForm` is in the prelude; its variants are written qualified, as `NormalizationForm.Nfc`. The compatibility forms (`Nfkc`, `Nfkd`) also fold formatting distinctions (the ligature `ﬁ` becomes `fi`), which loses information and cannot be undone, so prefer `Nfc` unless that folding is what you want.

`Hash` is consistent with `==`: it hashes the raw bytes. A `Map` keyed by un-normalized strings has the same trap, with the same remedy.

## C strings: `CStr` and `CString`

These types exist for FFI. `CStr` is a run of bytes ending in a NUL that its surface does not show. `c"..."` literals have type `ref CStr` and point into read-only data ([design.md §3](../design.md#c-string-literals)). `CString` is the owning, heap-allocated counterpart: a `String`-shaped buffer with a guaranteed trailing NUL. `CString` owns and `ref CStr` borrows, as `String` owns and `Str` borrows.

| Method | Signature | Notes |
|---|---|---|
| `as_ptr` | `fn as_ptr(self) -> *const u8` | The pointer to pass to an `extern "C"` function. For a literal it is valid for the whole program; otherwise while the `CStr` reference is held |
| `len` | `fn len(self) -> i64` | Bytes, not counting the NUL. O(1) for a `c"..."` literal; O(n) for a `CStr` built from a raw `*const u8` (which needs `unsafe`), because it walks to the NUL |
| `is_empty` | `fn is_empty(self) -> bool` | |
| `bytes` | `fn bytes(self) -> Slice[u8]` | The bytes, without the NUL |
| `to_str` | `fn to_str(self) -> Result[Str, Utf8Error]` | A `Str` view, if the bytes are valid UTF-8 |
| `to_string` | `fn to_string(self) -> Result[String, Utf8Error]` | An owned copy, if the bytes are valid UTF-8 |

A `CStr` never converts implicitly, even when its bytes are valid UTF-8.

`String.to_cstring(self) -> Result[CString, NulError]` copies the text into a new buffer with a NUL appended, and fails if the text contains a NUL. A `CString` can also be built from a `Slice[u8]`. It drops normally, and its `as_ptr()` returns the `*const u8` to pass to C.
