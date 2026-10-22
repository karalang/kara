# mini_colorama

A port of [colorama](https://github.com/tartley/colorama) 0.4.6 (BSD-3,
Jonathan Hartley) — the near-ubiquitous library for coloured terminal output.
This port covers colorama's portable, cross-platform core: the ANSI
escape-code builders and its CSI-stripping path.

## What it does

`mk.py` writes `data/jobs.tsv`, one operation per line. `oracle.py` runs each
through the vendored library (`vendor/colorama/`) to produce `expected.out`.
`mini_colorama.kara` reimplements the builders and the strip, and must match
that output byte-for-byte on every backend.

Covered: `Fore` / `Back` / `Style` (all SGR codes, through
`code_to_chars`), `Cursor.UP/DOWN/FORWARD/BACK/POS`, `ansi.set_title`,
`clear_screen`, `clear_line`, a composed `Fore+Style` string, and the
CSI-stripping that `AnsiToWin32(strip=True)` performs — removing every
`ESC [ … letter` control sequence (including empty-parameter and 256-colour
forms) while leaving an incomplete `ESC [` intact, exactly as the library's
`ANSI_CSI_RE` does.

Kāra string literals accept `\x1b`, so the port emits real ESC (and BEL)
bytes; `expected.out` holds them verbatim and the comparison is a true
byte-for-byte match. The jobs file carries real control bytes too — ESC
(`0x1b`) and BEL (`0x07`) are neither tab, newline, nor any `str.splitlines`
boundary, so the tab/newline framing is unambiguous.

## Scope

OSC (Operating System Command) stripping is out of scope: colorama's convert
path for a title OSC calls into the Windows terminal API and raises on a
non-Windows host, so only CSI sequences — the portable half of the stripper —
are handled. `set_title` is still exercised as a *builder* (it just emits the
`ESC ] 2 ; … BEL` sequence). The Windows console conversion (`win32`,
`winterm`) is platform-specific and not part of this port.

## Running

```bash
karac run mini_colorama.kara          # interpret
karac build --mir mini_colorama.kara  # native binary
python3 -s -P -B mk.py                # regenerate data/jobs.tsv
python3 -I oracle.py                   # regenerate expected.out
```
