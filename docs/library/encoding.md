# Encodings (`std.encoding`)

`std.encoding` turns bytes into text and back: Base64 (RFC 4648) and hex. It is written in Kāra and imported by name.

```kara
import std.encoding.{base64_encode, base64_decode, hex_encode};

let b = base64_encode("hello".bytes());       // "aGVsbG8="
let back = base64_decode(b.as_str());         // Ok of the five bytes
let h = hex_encode("hi".bytes());             // "6869"
```

## Items

| Item | Signature | Notes |
|---|---|---|
| `base64_encode` | `fn base64_encode(data: Slice[u8]) -> String` | Standard alphabet (`+`, `/`), padded with `=` |
| `base64_decode` | `fn base64_decode(text: Str) -> Result[Vec[u8], DecodeError]` | Standard alphabet. Padding is required, and the bits after the last whole byte must be zero, so each byte string has exactly one accepted text. No whitespace is skipped |
| `base64url_encode` | `fn base64url_encode(data: Slice[u8]) -> String` | URL-safe alphabet (`-`, `_`), without padding |
| `base64url_decode` | `fn base64url_decode(text: Str) -> Result[Vec[u8], DecodeError]` | URL-safe alphabet, with or without padding |
| `hex_encode` | `fn hex_encode(data: Slice[u8]) -> String` | Two lowercase digits a byte |
| `hex_decode` | `fn hex_decode(text: Str) -> Result[Vec[u8], DecodeError]` | Digits in either case |
| `DecodeError` | `enum DecodeError { InvalidByte(i64), InvalidLength }` | `InvalidByte(at)`: the byte at offset `at` of the text cannot appear there. `InvalidLength`: the length cannot hold whole bytes, or the padding is wrong. `#[non_exhaustive]`; implements `Display` |
