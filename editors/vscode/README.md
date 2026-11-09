# Kāra for Visual Studio Code

Language support for [Kāra](https://github.com/karalang/kara).

- **Syntax highlighting** for `.kara` files, with or without the server.
- **Diagnostics** as you type, from the same checks `karac check` runs
  (parse, resolve, types, effects, ownership). No code is executed.
- **Completion**: after `.`, the fields and methods of the value's type,
  the standard library's included; after `std.`, its modules and their
  items; anywhere else, the locals and parameters in scope and the file's
  own items.
- **Signature help** inside a call's parentheses: the function's
  declaration, the parameter you are on, and what each parameter's mode
  means for the argument (borrowed, `own`, `mut ref`).
- **Inlay hints**: the inferred type of a `let` written without one, and,
  when `kara.inlayHints.effects` is on, the inferred effects of a private
  function written without a `with` clause.
- **Go to definition**, **find references** and **rename** within a file.
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
| `kara.inlayHints.types` | `true` | Inferred types of `let` bindings. |
| `kara.inlayHints.effects` | `false` | Inferred effects of private functions. |
| `kara.trace.server` | `"off"` | `messages` or `verbose` logs the protocol to the **Kāra Language Server** output. |

## Commands

- **Kāra: Restart Language Server** (also happens when a `kara.server.*` or
  `kara.inlayHints.*` setting changes).
- **Kāra: Show Language Server Output**.

## Limits in this release

Go to definition, references and rename work within the open file;
reaching across modules comes later (see `docs/roadmap.md` § Track 3).
Signature help has nothing to show for a builtin type's method that has no
Kāra declaration, such as `Vec.push`.

## Developing

```sh
cd editors/vscode
npm install
npm test             # grammar scope assertions
npm run test:corpus  # grammar balance over every corpus/example program (a few minutes)
cargo build -p kara-lsp && npm run test:lsp   # the server over stdio, as the extension runs it
npm run package      # builds kara-0.8.1.vsix
```

Press F5 in VS Code with this folder open to launch an Extension Development
Host. `code --install-extension kara-0.8.1.vsix` installs a packaged build.
