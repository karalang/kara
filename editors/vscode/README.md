# Kāra for Visual Studio Code

Language support for [Kāra](https://github.com/karalang/kara).

- **Syntax highlighting** for `.kara` files, with or without the server.
- **Diagnostics** as you type, from the same checks `karac check` runs
  (parse, resolve, types, effects, ownership). No code is executed.
- **Go to definition** and **find references** within a file.
- **Hover** shows the type of the expression under the cursor and, on a
  function, its effect signature.
- **Outline**, **Format Document** (`karac fmt`) and **quick fixes** for the
  machine-applicable suggestions `karac fix` would apply.

## Setup

Everything except highlighting comes from the `kara-lsp` language server.
Install it from a checkout of the Kāra repository:

```sh
cargo install --path lsp
```

The extension finds the server in this order:

1. the `kara.server.path` setting;
2. a `kara-lsp` binary bundled inside the extension (platform builds);
3. `kara-lsp` on your `PATH`, then `~/.cargo/bin/kara-lsp`.

If none is found, highlighting still works and a notice explains how to
install the server.

## Settings

| Setting | Default | Meaning |
|---|---|---|
| `kara.server.path` | `""` | Path to the `kara-lsp` binary. |
| `kara.server.extraEnv` | `{}` | Extra environment variables for the server. |
| `kara.trace.server` | `"off"` | `messages` or `verbose` logs the protocol to the **Kāra Language Server** output. |

## Commands

- **Kāra: Restart Language Server** (also happens when a `kara.server.*` setting changes).
- **Kāra: Show Language Server Output**.

## Limits in this release

Go to definition and references work within the open file; jumping into
another module or the standard library comes later, as do completion,
signature help and rename (see `docs/roadmap.md` § Track 3).

## Developing

```sh
cd editors/vscode
npm install
npm test             # grammar scope assertions
npm run test:corpus  # grammar balance over every corpus/example program (a few minutes)
cargo build -p kara-lsp && npm run test:lsp   # the server over stdio, as the extension runs it
npm run package      # builds kara-0.1.0.vsix
```

Press F5 in VS Code with this folder open to launch an Extension Development
Host. `code --install-extension kara-0.1.0.vsix` installs a packaged build.
