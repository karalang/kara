# Changelog

## 0.8.2 (unreleased)

- Signature help inside a call's parentheses, with what each parameter's
  mode (borrowed, `own`, `mut ref`) means for the argument.
- Inlay hints: inferred `let` types (`kara.inlayHints.types`, on) and a
  private function's inferred effects (`kara.inlayHints.effects`, off).
- Completion and rename, and diagnostics that re-check only the edited
  function, from `kara-lsp`.

## 0.8.1

Ships with Kāra 0.8.1; the bundled `kara-lsp` carries 0.8.1's fixes. No change to the extension itself.

## 0.8.0

First release, with Kāra 0.8.0 (beta).

- Syntax highlighting for `.kara` files (TextMate grammar).
- Diagnostics as you type, hover with type and effect signature,
  go-to-definition, find references, document outline, formatting and quick
  fixes, all from `kara-lsp`.
- Commands: **Kāra: Restart Language Server**, **Kāra: Show Language Server Output**.
