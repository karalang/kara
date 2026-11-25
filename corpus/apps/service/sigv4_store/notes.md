# service/sigv4_store

An S3-style object store on loopback HTTP whose every request is signed with
AWS Signature Version 4, and the clients that sign them. Object bodies are
bytes, both ways.

- **Signing.** The canonical request (method, the path URI-encoded with `/`
  kept, the query sorted by encoded name, the lower-cased headers, the
  signed-header list and the payload hash), the string to sign, and the key
  chain `HMAC("AWS4" + secret, date)` then region, `s3` and `aws4_request`.
  The program first signs AWS's published `get-vanilla` case and prints the
  signature, which is the one AWS documents.
- **The store.** `Server.serve` answers bucket create (409 for one that
  exists, 400 for a bad name), list buckets, object PUT, GET and DELETE, and
  ListObjectsV2 with `prefix`, `delimiter`, `max-keys` and a continuation
  token, plus `/_stats`. Keys are kept in a `SortedMap`, so listings come
  out in key order. A PUT keeps the body's bytes and content type and
  answers the first 32 hex digits of its SHA-256 as the ETag.
- **Authentication.** A request is signed in its `Authorization` header or
  presigned in its query string (`X-Amz-Algorithm`, `X-Amz-Credential`,
  `X-Amz-Date`, `X-Amz-Expires`, `X-Amz-SignedHeaders`,
  `X-Amz-Signature`, `UNSIGNED-PAYLOAD`). The server recomputes the
  signature and refuses, each with S3's error code: an unknown key, a wrong
  secret, a wrong region or algorithm, a clock more than 15 minutes off its
  own fixed time, a body whose SHA-256 is not `x-amz-content-sha256`, a
  read-only key writing, an unsigned request, an expired presign, an
  `X-Amz-Expires` over seven days, and a bad percent escape.
- **The clients.** A writer and a reader (obviously fake keys) create
  buckets, upload text and binary objects (one is all 256 byte values, one
  key has a space, a `+` and an `é`), read each back and compare it,
  page through listings two at a time, send every refused case, presign
  URLs, delete, and print the server's counts.

The mirror `sigv4_store.py` is the same store and clients written
independently with `http.server`, `hmac` and `urllib`; it wrote
`expected.out`.

Written 2026-11-25 by the real-world apps thread; karac carries the fixes
below.

## From sigv4_store.orig.kara to sigv4_store.kara

The draft was written Rust-shaped (`use std::collections::{BTreeMap,
HashMap}`, `Arc<Mutex<Store>>` with `store.lock().unwrap()`, `&str` and
`&[u8]` parameters, `format!` throughout, `Some(&original)` for an
`Option<&str>`, `k.eq_ignore_ascii_case(name)` on header names,
`rb.body(body.to_vec())` for an upload, `TaskGroup::new()` and
`group.spawn(move || Server::serve(..))`, and a `thread::sleep` retry until
the server answers). Three gaps turned up, and each was fixed in the
compiler before the fix passes were rerun:

- B-2026-11-25-80: a request body could only be text, so the clients
  could not upload a binary object. `RequestBuilder.body_bytes` and
  `Request.with_bytes` take a `Vec[u8]` and send it as it is, with
  `Content-Length` its count; `karac fix` rewrites `.body(v)` to
  `.body_bytes(v)` for a `Vec[u8]`.
- B-2026-11-25-81: `String` had no `eq_ignore_ascii_case`, which header
  lookups want. It now does, with Rust's meaning.
- B-2026-11-25-82: `Some(ref original)` handed to an `Option[Str]`
  passed the check, then failed MIR validation, so the program did not
  run. A `String` or `ref String` payload at a `Str` field is now viewed,
  as a call argument is.

After those, `karac fix` rewrote the draft in one pass (752 fixes), with
no hand edits, and the first run matched the mirror byte for byte.
