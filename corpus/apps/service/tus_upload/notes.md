# tus_upload

A resumable-upload server speaking [tus 1.0.0](https://tus.io/protocols/resumable-upload),
the protocol behind Uppy, tusd and Vimeo's uploads, with a client that drives
it through `std.http` over a real loopback socket (port 47748). Uploads live
on disk under `tus_data/`: `<id>.bin` holds the bytes received so far, so its
size is the upload's offset, and `<id>.info` the declared length and the raw
`Upload-Metadata`.

The server implements the core protocol and three extensions:

- **Core.** `OPTIONS` advertises `Tus-Version`, `Tus-Extension`,
  `Tus-Max-Size` and `Tus-Checksum-Algorithm`. Every other request must carry
  `Tus-Resumable: 1.0.0` or gets 412. `HEAD` returns `Upload-Offset`,
  `Upload-Length`, the metadata and `Cache-Control: no-store`. `PATCH` needs
  `Content-Type: application/offset+octet-stream` (else 415) and an
  `Upload-Offset` equal to the stored offset (else 409); a chunk that would
  run past `Upload-Length` is 400. Unknown uploads are 404 and other methods
  405 with `Allow`.
- **creation.** `POST /files/` with `Upload-Length` (digits only, else 400;
  over 1 MiB is 413) and optional `Upload-Metadata` (`key base64` pairs,
  comma-separated, a key may have no value, keys distinct, values valid
  Base64 of UTF-8, else 400). The answer is 201 with `Location`.
- **termination.** `DELETE` removes the upload; it is 404 afterwards.
- **checksum.** `Upload-Checksum: sha1 <b64>` or `sha256 <b64>` is verified
  before the chunk is stored: a mismatch is 460, another algorithm 400.

The client creates uploads, provokes each error, recovers the offset with
`HEAD` after the failed `PATCH`es and resumes in 1000-byte (sha1) and
700-byte (sha256) chunks, uploads an empty file, checks the stored bytes
against the payload, deletes an upload and lists what is left on disk.

The program declares its own `Response` with `headers`, as io.md § HTTP
server documents for sending headers, and that is also the type the client's
`send` returns, so the client reads the response's headers from the field.

Oracle: `tus_upload.py`, a port over a real socket (`http.server` and
`urllib`). `expected.out` is its output. A status of 400 or more is an `Err`
of the Kāra client carrying only a message, so both sides print that message.

## Edits from tus_upload.orig.kara to tus_upload.kara

All were made by `karac fix` from machine-applicable diagnostics.

Author errors (the diagnostics were right):
- `||` and `&&` are spelled `or` and `and` (8 sites).
- `hosts[seed % 4]` on an array of string literals is a `String` element,
  which is not `Copy`; the binding is only read, so the fix borrows it
  (2 sites).
- `location ?? String.new()` on a borrowed `Option[String]` parameter would
  move the `String` out of the borrow; the fix takes a `.clone()`.

Style (`karac fix`'s `redundant_literal_method` lint, design.md § `karac
fix`): 17 `.to_string()` calls on string literals in `String` slots were
dropped.

Compiler gaps this app found (both fixed in kara):
- `text?` on a borrowed `Option[String]` parameter was refused as "'?'
  operator requires `Result` or `Option`, found 'ref Option[String]'", with
  no fix. It now gets the same `.clone()` repair as `??` (B-2026-11-09-43).
- The program's own `Response` is also what the client returns, and the
  compiled client wrote only two of its three fields, so the program
  crashed freeing the unwritten `headers` (B-2026-11-09-34).
