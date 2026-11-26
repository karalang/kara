# crypto.md sentence map

Each normative sentence of the page, split into units, and the cell or script that checks it. Cells run with `python3 conformance/cases.py <this dir>` on the interpreter and the compiled build. conformance/crypto.py (published vectors, hashlib, hmac) and conformance/ct_timing.py (the timing promise) are cited by name.

| # | Section | Sentence | Cell(s) |
|---|---|---|---|
| cry.1 | Cryptographic hashing (`std.crypto`) | `std.crypto` holds the digests that existing protocols and file formats require: | crypto.py (FIPS 180-4 vectors for every SHA-2 digest, RFC 4231 for every HMAC, random inputs against hashlib and hmac); page_example |
| cry.2 | Cryptographic hashing (`std.crypto`) | SHA-256, SHA-384 and SHA-512 (FIPS 180-4), HMAC over each of them (RFC 2104, RFC 4231), and a constant-time comparison for checking a MAC. | crypto.py (FIPS 180-4 vectors for every SHA-2 digest, RFC 4231 for every HMAC, random inputs against hashlib and hmac); page_example |
| cry.3 | Cryptographic hashing (`std.crypto`) | SHA-1 is in a separate module, `std.crypto.legacy`. | crypto.py (sha1 imported from std.crypto.legacy); crypto_sha1_only_in_legacy_refused |
| cry.4 | Cryptographic hashing (`std.crypto`) | The hashing is done by the `ring` library on both the interpreter and compiled programs, so the two give the same bytes. | crypto.py runs every case on the interpreter and built (same bytes), and `--lanes interp,build,asan` under ASan too (2026-11-26: 2475 passed); implementation note |
| cry.5 | Cryptographic hashing (`std.crypto`) | code: import std.crypto.sha2.{sha256}; import std.crypto.hmac.{hmac_sha256}; import std.crypto.{constant_time_eq}; import std.encoding.{hex_ | page_example (the page's program, its digest comment) |
| cry.6 | Cryptographic hashing (`std.crypto`) | These are the interoperability digests. | advice and deferred items (BLAKE3, Argon2id, HKDF: deferred.md), no cell |
| cry.7 | Cryptographic hashing (`std.crypto`) | For a new design that needs a general cryptographic hash, the committed default is BLAKE3, which has not shipped yet ([deferred.md § std.cry | advice and deferred items (BLAKE3, Argon2id, HKDF: deferred.md), no cell |
| cry.8 | Not a password hash | None of these functions is fit for storing passwords or deriving a key from one. | advice and deferred items (BLAKE3, Argon2id, HKDF: deferred.md), no cell |
| cry.9 | Not a password hash | A SHA-2 digest or an HMAC is fast by design, so an attacker holding the stored values can test guesses at the same speed. | advice and deferred items (BLAKE3, Argon2id, HKDF: deferred.md), no cell |
| cry.10 | Not a password hash | Password hashing needs a deliberately slow, memory-hard function; the committed choice is Argon2id, which has not shipped yet. | advice and deferred items (BLAKE3, Argon2id, HKDF: deferred.md), no cell |
| cry.11 | Not a password hash | The same applies to key derivation: an HMAC is a building block of HKDF, not a substitute for it. | advice and deferred items (BLAKE3, Argon2id, HKDF: deferred.md), no cell |
| cry.12 | Inputs are bytes | Every function takes `Slice[u8]`. | crypto.py (every function called with Slice[u8]) |
| cry.13 | Inputs are bytes | There is no `Str` overload, because a digest is defined over bytes and a text has more than one byte encoding. | crypto_str_argument_refused (a Str argument refused, the diagnostic points at `.bytes()`); page_example (`.bytes()`) |
| cry.14 | Inputs are bytes | Hash the UTF-8 bytes of a string with `.bytes()`, as the examples here do. | crypto_str_argument_refused (a Str argument refused, the diagnostic points at `.bytes()`); page_example (`.bytes()`) |
| cry.15 | Inputs are bytes | The output is a fixed-size `Array[u8, N]`; `hex_encode` or `base64_encode` from [`std.encoding`](encoding.md) turns it into text. | page_example (hex_encode of the Array[u8, 32]); crypto.py (digest lengths 32, 48, 64, 20); hashers_dropped_unfinished (each one-shot function's result held at its declared `Array[u8, N]`) |
| cry.16 | `std.crypto.sha2` | row: Item / Signature / Notes | table header, no cell |
| cry.17 | `std.crypto.sha2` | row: `sha256` / `fn sha256(data: Slice[u8]) -> Array[u8, 32]` | crypto.py (sha256, sha384, sha512) |
| cry.18 | `std.crypto.sha2` | row: `sha384` / `fn sha384(data: Slice[u8]) -> Array[u8, 48]` | crypto.py (sha256, sha384, sha512) |
| cry.19 | `std.crypto.sha2` | row: `sha512` / `fn sha512(data: Slice[u8]) -> Array[u8, 64]` | crypto.py (sha256, sha384, sha512) |
| cry.20 | `std.crypto.sha2` | row: `Sha256` / `struct Sha256` / A digest over data given in parts: `Sha256.new()`, then `update(mut ref self, data: Slice[u8])` any number | crypto.py (Sha256, Sha384, Sha512 over random splits equal the one-shot digest) |
| cry.21 | `std.crypto.sha2` | row: `Sha384` / `struct Sha384` / As `Sha256`; `finish` returns `Array[u8, 48]` | crypto.py (Sha256, Sha384, Sha512 over random splits equal the one-shot digest) |
| cry.22 | `std.crypto.sha2` | row: `Sha512` / `struct Sha512` / As `Sha256`; `finish` returns `Array[u8, 64]` | crypto.py (Sha256, Sha384, Sha512 over random splits equal the one-shot digest) |
| cry.23 | `std.crypto.sha2` | Feeding the same bytes in any split gives the same digest as the one-shot function. | crypto.py (Sha256, Sha384, Sha512 over random splits equal the one-shot digest) |
| cry.24 | `std.crypto.sha2` | `finish` consumes the value, so a finished hash cannot be updated again. | finish_consumes (update after finish refused) |
| cry.25 | `std.crypto.sha2` | A hash dropped without `finish` frees its state. | crypto_websocket_accept_example (a Sha256 dropped without finish; the ASan lane reports a leak otherwise); hashers_dropped_unfinished (all seven streaming types dropped unfinished, empty and fed, in a struct field and in a Vec, clean under ASan) |
| cry.26 | `std.crypto.hmac` | row: Item / Signature / Notes | crypto.py (hmac_sha256 against RFC 4231 cases 1-4, 6, 7; cases 6 and 7 use a 131-byte key, longer than the block) |
| cry.27 | `std.crypto.hmac` | row: `hmac_sha256` / `fn hmac_sha256(key: Slice[u8], data: Slice[u8]) -> Array[u8, 32]` / Any key length; a key longer than the block is has | crypto.py (hmac_sha256 against RFC 4231 cases 1-4, 6, 7; cases 6 and 7 use a 131-byte key, longer than the block) |
| cry.28 | `std.crypto.hmac` | row: `hmac_sha384` / `fn hmac_sha384(key: Slice[u8], data: Slice[u8]) -> Array[u8, 48]` | crypto.py (hmac_sha384, hmac_sha512) |
| cry.29 | `std.crypto.hmac` | row: `hmac_sha512` / `fn hmac_sha512(key: Slice[u8], data: Slice[u8]) -> Array[u8, 64]` | crypto.py (hmac_sha384, hmac_sha512) |
| cry.30 | `std.crypto.hmac` | row: `HmacSha256` / `struct HmacSha256` / `HmacSha256.new(key: Slice[u8])`, then `update` and `finish` as for `Sha256` | crypto.py (HmacSha256, HmacSha384, HmacSha512 over random splits) |
| cry.31 | `std.crypto.hmac` | row: `HmacSha384` / `struct HmacSha384` / As `HmacSha256`; `finish` returns `Array[u8, 48]` | crypto.py (HmacSha256, HmacSha384, HmacSha512 over random splits) |
| cry.32 | `std.crypto.hmac` | row: `HmacSha512` / `struct HmacSha512` / As `HmacSha256`; `finish` returns `Array[u8, 64]` | crypto.py (HmacSha256, HmacSha384, HmacSha512 over random splits) |
| cry.33 | `std.crypto` | row: Item / Signature / Notes | table header, no cell |
| cry.34 | `std.crypto` | row: `constant_time_eq` / `fn constant_time_eq(a: Slice[u8], b: Slice[u8]) -> bool` / `true` when the two hold the same bytes. Slices of dif | crypto.py (constant_time_eq: same bytes, different bytes, different lengths); ct_timing.py (time independent of where equal-length inputs differ, with `==` as the positive control) |
| cry.35 | `std.crypto` | Check a received MAC with `constant_time_eq`, never with `==`. | page_example (a received MAC checked with constant_time_eq); advice |
| cry.36 | `std.crypto` | An ordinary comparison stops at the first differing byte, so its timing tells an attacker how much of a forged tag is right. | page_example (a received MAC checked with constant_time_eq); advice |
| cry.37 | `std.crypto` | The length is not secret: a MAC's length is fixed by its algorithm. | page_example (a received MAC checked with constant_time_eq); advice |
| cry.38 | `std.crypto` | `ConstantTimeEq` on `Secret[T]` ([secret.md](secret.md)) is the post-v1 form of the same guarantee. | post-v1 cross-reference (secret.md's ct_eq: conformance/secret redacted_and_access, ct_timing.py), no cell |
| cry.39 | `std.crypto.legacy` | row: Item / Signature / Notes | table header, no cell |
| cry.40 | `std.crypto.legacy` | row: `sha1` / `fn sha1(data: Slice[u8]) -> Array[u8, 20]` | crypto.py (sha1 on the FIPS examples and random inputs; Sha1 over random splits); crypto_websocket_accept_example (Sha1 in two parts) |
| cry.41 | `std.crypto.legacy` | row: `Sha1` / `struct Sha1` / `Sha1.new()`, `update`, and `finish(own self) -> Array[u8, 20]` | crypto.py (sha1 on the FIPS examples and random inputs; Sha1 over random splits); crypto_websocket_accept_example (Sha1 in two parts) |
| cry.42 | `std.crypto.legacy` | SHA-1 is broken for collision resistance: two different inputs with the same SHA-1 digest can be computed, so it must not be used for signat | advice; crypto_sha1_only_in_legacy_refused (the import is visible: std.crypto does not export it) |
| cry.43 | `std.crypto.legacy` | It ships only in `std.crypto.legacy`, so every use is a visible import in review ([deferred.md § Security-related conventions](../deferred.m | advice; crypto_sha1_only_in_legacy_refused (the import is visible: std.crypto does not export it) |
| cry.44 | `std.crypto.legacy` | It is still the correct choice where a protocol fixes it and does not rely on collision resistance. | advice; crypto_sha1_only_in_legacy_refused (the import is visible: std.crypto does not export it) |
| cry.45 | `std.crypto.legacy` | The WebSocket handshake (RFC 6455 §1.3) is one: | crypto_websocket_accept_example (the page's accept_value, RFC 6455's sample key) |
| cry.46 | `std.crypto.legacy` | code: import std.crypto.legacy.{sha1}; import std.encoding.{base64_encode}; fn accept_value(key: Str) -> String { base64_encode(sha1(f"{key} | crypto_websocket_accept_example (the page's accept_value, RFC 6455's sample key) |
| cry.47 | `std.crypto.legacy` | Git's object ids are another. | statement, no cell |
| cry.48 | Targets | The digests are available on native targets. | crypto.py (native targets) |
| cry.49 | Targets | They are not yet available on WebAssembly targets: | crypto_import_refused_on_wasm (`karac check --target=wasm_wasi` refuses the import, naming the module and the target; native run passes) |
| cry.50 | Targets | `karac check` and `karac build` refuse an import of `std.crypto` or any of its submodules on `wasm_wasi`, naming the module and the target ( | crypto_import_refused_on_wasm (`karac check --target=wasm_wasi` refuses the import, naming the module and the target; native run passes) |
| cry.51 | Targets | A digest written for wasm is in [deferred.md](../deferred.md#stdcrypto). | cross-reference (deferred.md), no cell |
