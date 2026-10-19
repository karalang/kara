# mini_mailparser

The message half of Python 3.13's `email` package under the `compat32`
policy, which is what `email.message_from_string` and `email.mime` use.
It ports the feed parser (`email/feedparser.py`) with all of its
defects, `Message` (`email/message.py`) with its header and parameter
methods, the `Generator` that writes a message back out with its headers
folded through `email.header`, and the `email.mime` classes `MIMEText`,
`MIMEApplication`, `MIMEMultipart` and `MIMEMessage`. The address, header
and RFC 2231 code is taken from mini_mailheaders. The base64,
quoted-printable and uuencode decoders are ported from `binascii`.

The run writes 16 messages under `data/mail/`, two files to attach under
`data/outbox/`, and 11 edit jobs to `data/jobs.jsonl`. Then it does four
things:
- It parses each message and prints every part: its content type,
  parameters, filename, charset, disposition, defects, preamble and
  epilogue, and its payload raw, decoded and as text. Then it prints the
  message written back with `as_string()`, and again folded at 40
  columns with `From ` lines mangled.
- It makes the edits in `data/jobs.jsonl` to one part of a parsed
  message (`set_param`, `del_param`, `set_type`, `set_boundary`,
  `add_header`, `replace_header` and the getters), and prints what each
  returns and the headers that result.
- It builds seven `MIMEText` parts and a multipart message with an HTML
  alternative, two attachments and a forwarded message. It prints them,
  parses the multipart back and writes it out again.
- It reads the mailbox the way a mail client would. For each message it
  prints the sender, the decoded subject, the first line of the body
  and the recipients, and it saves every attachment under `data/saved/`
  with a CRC-32.

`oracle.py` does the same with the real `email` package (CPython 3.13)
and wrote `expected.out`.

The library's quirks are kept. Some examples:
- A message in raw UTF-8 parses, but `as_string()` writes each non-ASCII
  header as one base64 encoded word, the address included:
  `From: =?utf-8?b?RMO2cnRlIDxkb2VydGVAZXhhbXBsZS5kZT4=?=`.
- `get_payload(decode=True)` on that message's 8bit body falls back to
  `raw-unicode-escape`, so `ü` becomes the byte `\xfc` and `—` becomes
  the six bytes `—`. Decoding that as UTF-8 gives `Gr��e`.
- The newline before a boundary belongs to the boundary, so the last
  line of each part loses its newline. In a forwarded `message/rfc822`
  part, that is the inner message's body.
- The parts of a `multipart/digest` default to `message/rfc822`, even a
  part with its own `Content-Type: text/plain`.
- The status blocks of a `message/delivery-status` part parse as
  messages with headers and an empty body.
- A `multipart/mixed` with no boundary, or whose boundary never appears,
  keeps its body as one string and gets two defects. Base64 with missing
  padding or stray characters decodes anyway and records a defect.
- `set_param` and `del_param` move `Content-Type` to the end of the
  headers, under the name the call spelled: `del_param` writes
  `content-type`.
- The RFC 2231 continuations `filename*0*` and `filename*1*` join to
  `Informe trimestral — Q3.pdf`. `get_param` returns the value as
  Latin-1 code points, `'Informe trimestral â\x80\x94 Q3.pdf'`.
- `charset*=us-ascii'en'%41%42C` makes `get_content_charset()` return
  `'abc'`, and decoding the text with it raises `LookupError`.
- The parts inside a `multipart/signed` are written without folding,
  so the signed text goes out as it came in.

Limits of the port:
- Only the `compat32` policy is ported, and only `message_from_string`.
  The bytes parser and the newer `EmailMessage` API are not.
- Python keeps an undecodable byte as a lone surrogate. A Kāra String
  cannot hold one, so the port keeps the byte as the private-use code
  point U+10FF00 + byte.
- The generator does not re-encode 8-bit payloads or unknown-8bit
  headers, and does not make up a random boundary. No job needs either.
- `Charset` conversions such as EUC-JP to ISO-2022-JP are not ported,
  and only the six codecs of mini_mailheaders are known.

It matched the oracle on the first full run on every backend. It needed
one workaround, for the open B-2026-10-19-6: in
`Gen.handle_delivery_status`, a `Vec[Str]` split from a String keeps
that String borrowed, so the String is cloned instead of moved. The code
taken from mini_mailheaders keeps that app's workarounds for
B-2026-10-19-4 and -5.

Written 2026-10-19 by the real-world apps thread; karac at 3934e06a45.
