#!/usr/bin/env bash
# Build the browser playground wasm module (tracker line 703).
#
# Output: `web/pkg/` — JS glue + `.wasm`, loadable by `web/index.html`
# as `import init, { run } from './pkg/karac_playground.js'`.
#
# Local-dev workflow after this script:
#   python3 -m http.server -d web 8000
#   open http://localhost:8000/
#
# wasm-pack is the canonical wasm-bindgen build tool; install via
#   cargo install wasm-pack
# if not on PATH.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"
exec wasm-pack build --target web --out-dir web/pkg "$@"
