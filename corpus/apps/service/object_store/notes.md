# object_store

A small object store in the shape of S3, served on loopback
(`127.0.0.1:47755`). Objects live under `/o/<key>` (the key is
percent-decoded, so `a%20b%2Fc.txt` is `a b/c.txt`). Each object keeps
its bytes, a content type, `x-meta-*` metadata and an `ETag`: the first
32 hex digits of its SHA-256, quoted.

- **GET** answers byte ranges (RFC 9110 §14): `bytes=a-b`, `bytes=a-` and
  `bytes=-n` get a 206 with `Content-Range`; a range that starts past the
  end, or `bytes=-0`, gets a 416 with `Content-Range: bytes */<len>`; a
  header that does not parse, names another unit, or asks for several
  ranges is ignored and the whole object is sent.
- **Conditional requests.** `If-None-Match` on a read answers 304 (weak
  comparison, so `W/"…"` matches); `If-Match` that names no current tag is
  a 412. On a `PUT`, `If-None-Match: *` makes it create-only and
  `If-Match: "<etag>"` makes it a compare-and-swap, both 412 otherwise.
- **Listing.** `GET /o/?prefix=&delimiter=&max-keys=&start-after=` lists
  keys in byte order, folding keys past the delimiter into common
  prefixes, and marks a truncated page with the marker to continue from.
- **Multipart upload.** `POST ?uploads` opens an upload, `PUT
  ?uploadId=&partNumber=` stores a part and answers its ETag, and `POST
  ?uploadId=` completes it from a body of `<part> <etag>` lines: parts must
  be ascending, uploaded and unchanged, and every part but the last at
  least 5 bytes (S3's `InvalidPartOrder`, `InvalidPart`,
  `EntityTooSmall`). The object's ETag is the digest of the parts'
  digests, suffixed with the part count. `DELETE ?uploadId=` aborts.
- **Persistence.** Every change writes the object's file
  (`objstore_data/objects/<hash of key>`) and replaces the index
  (`index.tsv`, written to a temporary file and renamed over it). A
  restart reloads the directory and checks each object against its ETag:
  a file that was changed or removed behind the store's back is reported
  and left out.

The state is one `Mutex` in a `sync struct`, shared by the server's
connection tasks.

**Self-test.** The handler is called directly with `Request` values:
ranges of every shape, conditional reads and writes, an encoded key, a
key that does not decode, listings with a delimiter and pages, the error
answers, and two multipart uploads, one completed after three rejected
completions and one aborted after `EntityTooSmall`.

**Live run.** A real client writes a small site, reads it back whole, by
range and conditionally, lists it, uploads a two-part backup, and
deletes a log. The run prints the data directory's index, then reloads
it twice: as written, and after one object file is tampered with and
another deleted.

Oracle: `object_store.py`, the same store with `http.server` and
`http.client`. `expected.out` is its output. The Kāra client turns a
status of 400 or more into an `Err` carrying only "<url>: status code
<n>" (B-2026-11-09-44), so the live run's 412, 416 and 404 print that
message; the mirror's client does the same. A 304 comes back as a
response.

## Edits from object_store.orig.kara to object_store.kara

By `karac fix`, from machine-applicable diagnostics:
- Rust's turbofish `s.parse::<i64>()` is `i64.parse(s)`.
- `&key[a..]` takes no `&`: a range of a `String` is already a view.
- Char arguments to `split`, `split_once` and `contains` are `Str`s.
- `vec![..]` is an array literal, and the `.to_string()`s on literals
  bound for `String` slots are dropped.
- `for (k, v) in req.query()` and `req.headers()` walk `.into_iter()`.
- `new_store` keeps its `dir`: `own Option[String]`.
- `prefixes.last() == Some(p)` compares `Some(ref p)`; `*prev` on a
  copied `i64` loses its `*`; `let (n, tag) = wanted[i]` is `ref
  wanted[i]`; a tag used after it moves into a request is `.clone()`d.

By hand:
- `etag.trim_matches('"')`: Kāra has no `trim_matches` (library/strings.md
  strips with `strip_prefix` and `strip_suffix`); `etag.replace("\"", "")`.

The orig had one logic slip of its own, fixed before any run: a range
`bytes=50-` on a 13-byte object read as a reversed range (sent whole)
rather than as starting past the end (416).

Compiler gaps this app found: none. Every diagnostic was the one the
code needed, and all but one had a machine fix.
