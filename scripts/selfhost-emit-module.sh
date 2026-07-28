#!/usr/bin/env bash
# Emit ONE self-host module through the SELF-HOSTED emitter
# (selfhost/src/codegen.kara), with its IMPORT CLOSURE supplied.
#
# Usage:
#   scripts/selfhost-emit-module.sh <module>          # e.g. ast, typechecker
#   scripts/selfhost-emit-module.sh <module> --keep   # keep the emitted IR
#   scripts/selfhost-emit-module.sh --all             # every module, one line each
#
# WHY THE CLOSURE. `emit_program` walks the items of ONE parsed module and
# ignores `Import`, so nothing an import names is registered. Until
# B-2026-07-28-15 that was SILENT — `kind_of_ty` returned 0 (i64) for the
# unresolved name, so `Span` (a four-field struct) lowered to a bare integer and
# the emitter produced well-formed IR for a program the seed REFUSES. It now
# refuses and prints the name it could not resolve.
#
# The fix on this side is to hand the emitter the declarations, which needs no
# emitter change at all: concatenating a module with its transitive imports
# gives one program in which every name is declared, and `import` lines are
# inert (the emitter ignores them, and duplicates are harmless). That is what
# this script does, and it is what moves ast/token/lexer/typechecker from
# invalid IR to IR `llvm-as` accepts.
#
# SCOPE. This checks that a module EMITS and that the IR PARSES. It does not run
# it — these modules are libraries with no `main`. For behavioural parity on
# runnable programs use the differential oracle (tests/selfhost_codegen.rs); for
# memory verdicts use scripts/selfhost-memcheck.sh.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$REPO/selfhost/src"
KARAC="$REPO/target/debug/karac"
[ -x "$KARAC" ] || { echo "no $KARAC — run: cargo build --features llvm"; exit 2; }

# Transitive import closure of a module, in dependency order (deps first).
# Reads `import <mod>.…;` lines; only modules that exist in selfhost/src count,
# so stdlib-ish imports are ignored.
closure() {
  local m="$1"; shift
  local seen=" $* "
  case "$seen" in *" $m "*) return 0;; esac
  local dep
  for dep in $(sed -n 's/^import \([a-z_][a-z_0-9]*\)\..*/\1/p' "$SRC/$m.kara" | sort -u); do
    [ -f "$SRC/$dep.kara" ] || continue
    closure "$dep" $seen $m
  done
  echo "$m"
}

emit_one() {
  local m="$1" keep="${2:-}"
  local tmp; tmp="$(mktemp -d)"
  [ "$keep" = "--keep" ] || trap 'rm -rf "$tmp"' RETURN

  # Deduplicate the closure, keeping first occurrence (dependency order).
  local mods; mods="$(closure "$m" | awk '!seen[$0]++')"
  mkdir -p "$tmp/src"
  printf '[package]\nname = "cg"\nversion = "0.1.0"\n' > "$tmp/kara.toml"
  for f in span.kara token.kara lexer.kara ast.kara parser.kara codegen.kara; do
    cp "$SRC/$f" "$tmp/src/$f"
  done
  : > "$tmp/closure.kara"
  local d
  for d in $mods; do cat "$SRC/$d.kara" >> "$tmp/closure.kara"; done

  python3 - "$tmp/closure.kara" > "$tmp/src/main.kara" <<'PY'
import sys
src = open(sys.argv[1]).read()
esc = src.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")
print("import parser.parse_program;")
print("import codegen.emit_program;")
print("")
print("fn main() with panics {")
print(f'    print(emit_program(parse_program("{esc}")));')
print("}")
PY

  ( cd "$tmp" && "$KARAC" build ) > "$tmp/build.log" 2>&1 || true
  if [ ! -x "$tmp/cg" ]; then
    printf '%-12s DRIVER BUILD FAILED\n' "$m"; tail -20 "$tmp/build.log"; return 1
  fi
  if ! "$tmp/cg" > "$tmp/out.ll" 2>"$tmp/emit.err"; then
    local why
    why="$(grep -o 'unresolved type name: .*' "$tmp/out.ll" | head -1 \
           || tail -1 "$tmp/out.ll" | cut -c1-100)"
    printf '%-12s REFUSED  closure[%s]  %s\n' "$m" "$(echo $mods | tr ' ' ,)" "$why"
    return 1
  fi
  if llvm-as -o /dev/null "$tmp/out.ll" 2>"$tmp/as.err"; then
    printf '%-12s IR VALID  closure[%s]  %s lines\n' \
      "$m" "$(echo $mods | tr ' ' ,)" "$(wc -l < "$tmp/out.ll")"
  else
    printf '%-12s IR INVALID  closure[%s]  %s\n' "$m" "$(echo $mods | tr ' ' ,)" \
      "$(head -1 "$tmp/as.err" | sed 's/.*error: //' | cut -c1-70)"
    [ "$keep" = "--keep" ] && echo "   kept: $tmp/out.ll"
    return 1
  fi
  [ "$keep" = "--keep" ] && echo "   kept: $tmp/out.ll"
  return 0
}

if [ "${1:-}" = "--all" ]; then
  rc=0
  for f in "$SRC"/*.kara; do
    m="$(basename "$f" .kara)"
    [ "$m" = "main" ] && continue      # the lexer harness, not a library module
    emit_one "$m" || rc=1
  done
  exit $rc
fi

emit_one "${1:?usage: selfhost-emit-module.sh <module> [--keep] | --all}" "${2:-}"
