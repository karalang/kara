# Strings and Bytes

Kāra strings are UTF-8. A `String` owns a growable, heap-allocated buffer of
UTF-8 bytes; a `StringSlice` is a borrowed view into one (see
[Variables and Types](./ch02-variables-and-types.md#strings)). Because the
encoding is UTF-8, a character can be one to four bytes wide — and that single
fact shapes the whole API.

## You can't index a `String` directly

```kara
let s = "hello";
// let c = s[0];   // not allowed
```

`s[i]` would have to either return a byte (surprising — you asked for a
character) or scan from the start counting characters (a hidden O(n) cost on
something that *looks* like O(1)). Kāra refuses to guess. Instead you pick a
**view** and the cost becomes explicit:

- **Characters** — `s.chars()` / `s.char_at(i)`, working in Unicode scalar values.
- **Bytes** — `s.bytes()`, working in raw `u8` with O(1) indexing.

Algorithmic code over ASCII almost always wants the byte view; text that has to
respect non-ASCII characters wants the character view.

## Substrings — a *range* index copies

Scalar indexing is out, but a **range** index is in — and it means something
different from range-indexing a `Vec`. `s[a..b]` returns a **fresh, owned
`String`** holding a copy of the bytes in that range (not a borrowed `Slice`,
and not a `StringSlice` view):

```kara,ignore
let s = "HelloWorld";
let head = s[0..5];       // "Hello" — a new String
let tail = s[5..];        // "World"
let all  = s[..];         // "HelloWorld" (a full copy)
```

Every range form works — `a..b`, `a..=b`, `a..`, `..b`, `..`. The method
spelling `s.substring(a, b)` is identical in behaviour to `s[a..b]`.

Two things to keep in mind:

- **The offsets are byte positions, not character positions** — the same units
  as `s.bytes()`. For ASCII the two coincide; for text with multi-byte
  characters they don't.
- **A range must land on UTF-8 character boundaries.** Slicing through the
  middle of a multi-byte character panics at runtime
  (`E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY`) rather than handing back invalid
  UTF-8:

  ```kara,ignore
  let s = "héllo";         // 'é' occupies bytes 1..3
  let bad = s[0..2];        // panics — byte 2 is mid-character
  ```

Because the result is an owned `String`, it outlives the source and can be
pushed, returned, or stored freely. When you only need to *read* a borrowed
window without copying, reach for a `StringSlice` via `s.slice(a, b)` instead
(see [Variables and Types](./ch02-variables-and-types.md#strings)).

## The character view

`for c in s` and `s.chars()` both yield `char` — one Unicode scalar value at a
time:

```kara,ignore
let mut vowels = 0;
for c in s.chars() {
    if c == 'a' or c == 'e' or c == 'i' or c == 'o' or c == 'u' {
        vowels = vowels + 1;
    }
}
```

For random access, snapshot the characters into a `Vec[char]` once, then index
that:

```kara,ignore
let chars: Vec[char] = s.chars().collect();
println(chars[0]);        // 'h'  — a char
println(chars.len());     // 5
```

`s.char_at(i)` returns the i-th character as an `Option[char]` — `None` when `i`
is out of range — and is O(n), since it counts characters from the front. Reach
for it for a one-off lookup; if you index repeatedly, collect into a `Vec[char]`
instead.

## The byte view — `s.bytes()`

`s.bytes()` returns a `Slice[u8]`: a borrowed, O(1)-indexable view over the
string's underlying storage, with no per-call allocation. This is the workhorse
for scanning ASCII input.

```kara,ignore
let bytes = s.bytes();
let n = bytes.len();
let b = bytes[0];         // u8
```

### Byte literals

A `b'x'` literal is a single ASCII byte — a `u8`, not a `char`. Compare and do
arithmetic on bytes directly:

```kara,ignore
let b = bytes[i];
if b >= b'0' and b <= b'9' {
    let digit = (b - b'0') as i64;   // '7' - '0' == 7
}
```

`b - b'0'` — the gap between a digit's byte and the byte for `'0'` — is the
canonical "parse one ASCII digit" move. The same range trick classifies letters
(`b >= b'a' and b <= b'z'`).

### Worked example: Roman numerals

Scanning bytes left to right, subtracting a smaller value that precedes a larger
one (`IV` is 4):

```kara
fn value(b: u8) -> i64 {
    if b == b'I' { return 1i64; }
    if b == b'V' { return 5i64; }
    if b == b'X' { return 10i64; }
    if b == b'L' { return 50i64; }
    if b == b'C' { return 100i64; }
    if b == b'D' { return 500i64; }
    if b == b'M' { return 1000i64; }
    0i64
}

fn roman_to_int(s: ref String) -> i64 {
    let bytes = s.bytes();
    let n = bytes.len();
    let mut total = 0i64;
    let mut i = 0i64;
    while i < n {
        let cur = value(bytes[i]);
        if i + 1 < n and cur < value(bytes[i + 1]) {
            total = total - cur;
        } else {
            total = total + cur;
        }
        i = i + 1;
    }
    total
}
```

Note the parameter is `ref String` — the function borrows the string to read it,
it doesn't take ownership (see [Ownership](./ch12-ownership.md)). A string
literal passes straight to a `ref String` parameter, so `roman_to_int("MCMXCIV")`
just works.

## Matching on bytes

Because a byte is a plain integer, `match` arms can be byte literals — handy when
one byte maps to another:

```kara
fn closer_for(b: u8) -> u8 {
    match b {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        _    => 0u8,
    }
}
```

## Building strings

You *scan* with bytes, but you *build* with characters. Start from an empty
`String` and append:

```kara
let mut out = String.new();
out.push('h');            // push a single char
out.push_str("ello");     // append a string
```

`push` takes a `char`, so you build from `char` literals or from characters you
pulled out with `.chars()`:

```kara
fn reverse(s: ref String) -> String {
    let chars: Vec[char] = s.chars().collect();
    let n = chars.len();
    let mut out = String.new();
    let mut i = n - 1;
    while i >= 0 {
        out.push(chars[i]);
        i = i - 1;
    }
    out
}
```

A `u8` is **not** a `char` — `b as char` is rejected, because not every integer
is a valid Unicode scalar. When you need a character computed from a number, go
through `char.try_from`, which returns a `Result[char, _]`:

```kara
fn digit_char(d: i64) -> char {
    match char.try_from(b'0' + d as u8) {
        Ok(c)  => c,
        Err(_) => '?',
    }
}
```

## Which view should I use?

| You want… | Use | Cost | Element |
|---|---|---|---|
| Scan/parse ASCII left to right | `s.bytes()` then index | O(1) per access | `u8` |
| Iterate characters once | `for c in s.chars()` | O(n) total | `char` |
| Random access by character index | `s.chars().collect()` → `Vec[char]` | O(n) once, O(1) after | `char` |
| One-off i-th character | `s.char_at(i)` | O(n) | `Option[char]` |
| A copied substring (byte range) | `s[a..b]` / `s.substring(a, b)` | O(k) copy | `String` |
| A borrowed substring view | `s.slice(a, b)` | O(1) | `StringSlice` |
| Build up output | `String.new()` + `push` / `push_str` | amortized O(1) per push | — |

The rule of thumb: **read as bytes, build with chars.** Byte scanning keeps the
inner loop to single-byte comparisons; character building keeps the output
UTF-8-correct without you tracking encoding by hand.
