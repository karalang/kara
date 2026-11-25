# encoding.md sentence map

Each normative sentence of the page, split into units, and the cell or script that checks it. Cells run with `python3 conformance/cases.py <this dir>` on the interpreter and the compiled build. conformance/encoding.py (against Python's base64, binascii and urllib.parse) is cited by name.

| # | Section | Sentence | Cell(s) |
|---|---|---|---|
| enc.1 | Encodings (`std.encoding`) | `std.encoding` turns bytes into text and back: | the rows below |
| enc.2 | Encodings (`std.encoding`) | Base64 (RFC 4648), hex, and percent-encoding (RFC 3986). | the rows below |
| enc.3 | Encodings (`std.encoding`) | It is written in Kāra and imported by name. | page_example_and_errors (imported by name); encoding_without_import_refused (no import, no name) |
| enc.4 | Encodings (`std.encoding`) | code: import std.encoding.{base64_encode, base64_decode, hex_encode}; let b = base64_encode("hello".bytes());       // "aGVsbG8=" let back = | page_example_and_errors (the page's example, its three comments) |
| enc.5 | Items | row: Item / Signature / Notes | table header, no cell |
| enc.6 | Items | row: `base64_encode` / `fn base64_encode(data: Slice[u8]) -> String` / Standard alphabet (`+`, `/`), padded with `=` | encoding.py (random bytes against Python's b64encode, standard alphabet, padded); page_example_and_errors |
| enc.7 | Items | row: `base64_decode` / `fn base64_decode(text: Str) -> Result[Vec[u8], DecodeError]` / Standard alphabet. Padding is required, and the bits  | encoding.py (decode of random and mutated texts against the page's rules: padding required, non-zero trailing bits refused, no whitespace skipped, error kind and offset); page_example_and_errors (InvalidLength through `?`, InvalidByte(0)) |
| enc.8 | Items | row: `base64url_encode` / `fn base64url_encode(data: Slice[u8]) -> String` / URL-safe alphabet (`-`, `_`), without padding | encoding.py (urlsafe_b64encode, unpadded) |
| enc.9 | Items | row: `base64url_decode` / `fn base64url_decode(text: Str) -> Result[Vec[u8], DecodeError]` / URL-safe alphabet, with or without padding | encoding.py (url-safe decode, padded and unpadded); encoding_decode_error_text_and_wildcard (`aGk` and `aGk=` both decode to 2 bytes) |
| enc.10 | Items | row: `hex_encode` / `fn hex_encode(data: Slice[u8]) -> String` / Two lowercase digits a byte | encoding.py (hex_encode against bytes.hex) |
| enc.11 | Items | row: `hex_decode` / `fn hex_decode(text: Str) -> Result[Vec[u8], DecodeError]` / Digits in either case | encoding.py (hex_decode in either case, odd lengths and bad digits) |
| enc.12 | Items | row: `percent_encode` / `fn percent_encode(data: Slice[u8]) -> String` / Every byte outside RFC 3986's unreserved set (`A`–`Z`, `a`–`z`, `0` | encoding.py (percent_encode against urllib.parse.quote with safe='', every byte value) |
| enc.13 | Items | row: `percent_decode` / `fn percent_decode(text: Str) -> Result[Vec[u8], DecodeError]` / `%` and two hex digits in either case become that b | encoding.py (percent_decode of random and mutated texts: `+` kept, `%` without two digits is InvalidByte at the `%`); encoding_decode_error_text_and_wildcard (`a+b%20c%e2%82%AC` through String.from_utf8, `a%2` is InvalidByte(1)) |
| enc.14 | Items | row: `DecodeError` / `enum DecodeError { InvalidByte(i64), InvalidLength }` / `InvalidByte(at)`: the byte at offset `at` of the text cannot  | encoding_decode_error_text_and_wildcard (Display of InvalidByte and InvalidLength, a match with `_`); encoding_decode_error_match_needs_wildcard (`#[non_exhaustive]`: a match without `_` refused); page_example_and_errors (`?` into AnyError) |
