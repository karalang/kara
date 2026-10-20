# mini_mailbox

Python 3.13's `mailbox` module, the standard library's reader and writer
for the mailboxes mail clients and servers keep on disk. It sits on top of
the compat32 `email` package ported for mini_mailparser (the feed parser,
`Message`, the `Generator` and `email.header`). The port follows
`mailbox.py`:
- the single-file mailboxes: `mbox`, `MMDF` and `Babyl` (Emacs Rmail).
  Each builds its table of contents the way `_generate_toc` does: a
  "From " line starts an mbox message, and the blank line before it is
  not part of it. Changes go to the end of the file at once, and `flush`
  rewrites the file in key order. From_ lines are quoted as `>From` on
  the way in;
- `MH`: numbered files, `.mh_sequences` read with its range syntax and
  written back as `1-3 5`, `pack`, and folders;
- `Maildir`: `cur/` and `new/` with the info suffix after the colon,
  `get_info`/`set_info` and the flag methods that rename the file,
  Maildir++ folders, and `remove_folder`'s checks;
- the message kinds `MaildirMessage`, `mboxMessage`, `MMDFMessage`,
  `MHMessage` and `BabylMessage`, with the tables `_explain_to` uses to
  carry flags, sequences and labels from one kind to another;
- `email.utils.parsedate_tz` and `mktime_tz`, and `time.asctime` of
  `time.gmtime`, for the envelope dates.

`mk.py` writes `data/`: an mbox inbox, an MMDF archive, an Rmail file, a
Maildir tree with five folders and an MH tree with five folders, three raw
messages, and `jobs.tsv`, the 21 jobs. Each job copies what it needs under
`data/work/` first.

The jobs list and read every format (`get_message`, `get_bytes` and
`get_string`, with and without the From_ line), set and clear flags and
write the file back, deliver raw messages, convert messages between all
the kinds, move mail from mbox to MH and back, edit and pack MH
sequences, make and remove folders, and parse dates. Five are programs a
mail client would run: an unread count across every mailbox, a search of
subjects and bodies, a digest with the envelope dates, an archive of old
mail into the MMDF file, and a dedupe of the MH inbox by Message-ID.

`oracle.py` runs the jobs through Python's own `mailbox` and wrote
`expected.out`. `gen_kara.py` writes the data section.

The quirks are kept. Some examples:
- an unquoted "From the shell, run:" line in a body starts a new mbox
  message, whose sender is `the shell, run:`;
- an MMDF message without a From_ line loses its first header to the
  From_ line, so its sender reads `n-Path: <ci@builds.example.net>`;
- mbox `set_flags` adds an empty `X-Status:` header when no X-Status
  flag is set;
- labels read from a Babyl file are bytes, so `'unseen' in labels` is
  false, rewriting such a file fails with `AttributeError: 'bytes' object
  has no attribute 'encode'`, and converting such a message to another
  Babyl message fails with `TypeError`;
- a `BabylMessage` written to a Babyl file loses its visible headers (the
  buffer is never rewound), so reading it back takes the first lines of
  the body as the visible headers, or fails an `assert` when the body has
  no blank line;
- an MH file named `007` lists as key 7, but `get(7)` raises `KeyError`
  and `pack` stops with `FileNotFoundError` half way;
- `MH.remove_folder` names the parent folder in its `NotEmptyError`, and
  `Maildir.remove_folder` swaps the folder and subdirectory names in its
  message.

Maildir keys of delivered messages come from the clock and the process
id, so the jobs never print them, and the dates `Maildir` keeps are file
modification times, which the jobs never print either.

It matched on every backend. Two closures first compared a captured
`ref String` without the `*`, because `*key` there failed MIR validation
(B-2026-10-20-21). That was fixed in kara e97d43b5df, and the port now
writes `*key`.
