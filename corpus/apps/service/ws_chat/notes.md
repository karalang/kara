# ws_chat: a WebSocket chat server

A WebSocket chat server (RFC 6455) on `127.0.0.1:47742`. The server:

- does the opening handshake, with `Sec-WebSocket-Accept` computed from the
  program's own SHA-1 and base64. It reproduces the RFC's sample key
  (`s3pPLMBiTxaQ9kYGzzhZRbK+xOo=`);
- refuses a bad upgrade with 400, a wrong version with 426 and a wrong path
  with 404;
- unmasks client frames, joins fragmented messages (with a ping between the
  fragments, which the spec allows) and answers pings;
- runs a room with `/nick`, `/who`, `/history` and broadcasts;
- closes a broken connection with the right code: 1002 for an unmasked
  frame, an unknown opcode or an orphan continuation; 1007 for text that is
  not UTF-8; 1009 for a message over 200 bytes. It echoes a client's close
  frame, and sends 1001 to anyone still connected at the end of a round.

One thread drives every party. In each round, every client connects, writes
its handshake and frames, and shuts down its write side. The server accepts
them all, reads each to end of input, runs the room in connection order,
writes the replies and closes. Then each client decodes what it got.

Oracle: `ws_chat.py`, the same program using `hashlib` and `base64`.
`expected.out` is its output.

## Compiler gaps (filed in kara `docs/bug-ledger.jsonl`)

- **B-2026-10-15-17** (low): the library has no base64, hex or digest
  (SHA-1, SHA-256) functions, so every WebSocket server, HTTP basic-auth
  check or content-addressed store writes its own. This app and mini_git
  each carry a SHA-1.
- Known and worked around: B-2026-10-14-44 (socket addresses are `String`),
  B-2026-10-12-7 (`split` is an eager `Vec[String]`).

No other gaps: the first draft ran on every surface once the type was renamed.

## Author errors

- A struct named `Client` shadows the prelude's (a warning); renamed `Peer`.

`ws_chat.orig.kara` is the first draft.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits, with one more by hand: `head.lines()[0].split(" ")` collects the lines first (B-2026-10-18-35). The app matches expected.out on all four MIR backends again (karac at a4b0967495).
