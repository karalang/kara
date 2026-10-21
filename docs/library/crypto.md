# Cryptographic hashing (`std.crypto`)

`std.crypto` holds the digests that existing protocols and file formats require: SHA-256, SHA-384 and SHA-512 (FIPS 180-4), HMAC over each of them (RFC 2104, RFC 4231), and a constant-time comparison for checking a MAC. SHA-1 is in a separate module, `std.crypto.legacy`. The hashing is done by the `ring` library on both the interpreter and compiled programs, so the two give the same bytes.

```kara
import std.crypto.sha2.{sha256};
import std.crypto.hmac.{hmac_sha256};
import std.crypto.{constant_time_eq};
import std.encoding.{hex_encode};

let d = sha256("abc".bytes());
println(hex_encode(d));   // ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad

fn verify(message: Slice[u8], tag: Slice[u8]) -> bool {
    constant_time_eq(hmac_sha256("key".bytes(), message), tag)
}
```

These are the interoperability digests. For a new design that needs a general cryptographic hash, the committed default is BLAKE3, which has not shipped yet ([deferred.md § std.crypto](../deferred.md#stdcrypto)).

## Not a password hash

None of these functions is fit for storing passwords or deriving a key from one. A SHA-2 digest or an HMAC is fast by design, so an attacker holding the stored values can test guesses at the same speed. Password hashing needs a deliberately slow, memory-hard function; the committed choice is Argon2id, which has not shipped yet. The same applies to key derivation: an HMAC is a building block of HKDF, not a substitute for it.

## Inputs are bytes

Every function takes `Slice[u8]`. There is no `Str` overload, because a digest is defined over bytes and a text has more than one byte encoding. Hash the UTF-8 bytes of a string with `.bytes()`, as the examples here do. The output is a fixed-size `Array[u8, N]`; `hex_encode` or `base64_encode` from [`std.encoding`](encoding.md) turns it into text.

## `std.crypto.sha2`

| Item | Signature | Notes |
|---|---|---|
| `sha256` | `fn sha256(data: Slice[u8]) -> Array[u8, 32]` | |
| `sha384` | `fn sha384(data: Slice[u8]) -> Array[u8, 48]` | |
| `sha512` | `fn sha512(data: Slice[u8]) -> Array[u8, 64]` | |
| `Sha256` | `struct Sha256` | A digest over data given in parts: `Sha256.new()`, then `update(mut ref self, data: Slice[u8])` any number of times, then `finish(own self) -> Array[u8, 32]` |
| `Sha384` | `struct Sha384` | As `Sha256`; `finish` returns `Array[u8, 48]` |
| `Sha512` | `struct Sha512` | As `Sha256`; `finish` returns `Array[u8, 64]` |

Feeding the same bytes in any split gives the same digest as the one-shot function. `finish` consumes the value, so a finished hash cannot be updated again. A hash dropped without `finish` frees its state.

## `std.crypto.hmac`

| Item | Signature | Notes |
|---|---|---|
| `hmac_sha256` | `fn hmac_sha256(key: Slice[u8], data: Slice[u8]) -> Array[u8, 32]` | Any key length; a key longer than the block is hashed first, as RFC 2104 specifies |
| `hmac_sha384` | `fn hmac_sha384(key: Slice[u8], data: Slice[u8]) -> Array[u8, 48]` | |
| `hmac_sha512` | `fn hmac_sha512(key: Slice[u8], data: Slice[u8]) -> Array[u8, 64]` | |
| `HmacSha256` | `struct HmacSha256` | `HmacSha256.new(key: Slice[u8])`, then `update` and `finish` as for `Sha256` |
| `HmacSha384` | `struct HmacSha384` | As `HmacSha256`; `finish` returns `Array[u8, 48]` |
| `HmacSha512` | `struct HmacSha512` | As `HmacSha256`; `finish` returns `Array[u8, 64]` |

## `std.crypto`

| Item | Signature | Notes |
|---|---|---|
| `constant_time_eq` | `fn constant_time_eq(a: Slice[u8], b: Slice[u8]) -> bool` | `true` when the two hold the same bytes. Slices of different lengths are unequal at once; for equal lengths the time taken depends only on the length, not on where the bytes differ |

Check a received MAC with `constant_time_eq`, never with `==`. An ordinary comparison stops at the first differing byte, so its timing tells an attacker how much of a forged tag is right. The length is not secret: a MAC's length is fixed by its algorithm. `ConstantTimeEq` on `Secret[T]` ([secret.md](secret.md)) is the post-v1 form of the same guarantee.

## `std.crypto.legacy`

| Item | Signature | Notes |
|---|---|---|
| `sha1` | `fn sha1(data: Slice[u8]) -> Array[u8, 20]` | |
| `Sha1` | `struct Sha1` | `Sha1.new()`, `update`, and `finish(own self) -> Array[u8, 20]` |

SHA-1 is broken for collision resistance: two different inputs with the same SHA-1 digest can be computed, so it must not be used for signatures, certificates, or any check against a chosen input. It ships only in `std.crypto.legacy`, so every use is a visible import in review ([deferred.md § Security-related conventions](../deferred.md)). It is still the correct choice where a protocol fixes it and does not rely on collision resistance. The WebSocket handshake (RFC 6455 §1.3) is one:

```kara
import std.crypto.legacy.{sha1};
import std.encoding.{base64_encode};

fn accept_value(key: Str) -> String {
    base64_encode(sha1(f"{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11".bytes()))
}

// accept_value("dGhlIHNhbXBsZSBub25jZQ==") == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
```

Git's object ids are another.

## Targets

The digests are available on native targets. They are not yet available on WebAssembly targets: a program that imports `std.crypto` fails to link there.
