# REPL cells

Each `<name>.repl` is fed to `karac repl` on stdin, in a scratch directory.
The expectation comes from the REPL's own contract: the book's "Two Surfaces"
chapter (`docs/book/src/ch01b-two-surfaces.md`), `karac repl --help` and
`:help`. design.md has no REPL section of its own.

| File | Meaning |
|---|---|
| `.out`, `.err` | The exact stdout and stderr (no `.err`: stderr is empty) |
| `.args` | Flags for `karac repl`, space-separated |
| `.saved` | The session ran `:save saved.kara`: the file's exact text |
| `.savedout` | What `karac run saved.kara` prints |

Run by `cargo test --test repl_conformance`.
