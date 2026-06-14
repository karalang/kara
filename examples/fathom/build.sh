#!/usr/bin/env bash
# Build Fathom for the browser (threaded WASM) and serve it cross-origin
# isolated so SharedArrayBuffer + the Web Worker pool are available.
#
#   ./build.sh          # build, then serve on http://localhost:8000
#   ./build.sh --build  # build only
set -euo pipefail
cd "$(dirname "$0")"

KARAC="${KARAC:-karac}"

echo "==> building mandelbrot.kara (wasm_browser + wasm-threads)"
"$KARAC" build mandelbrot.kara --target=wasm_browser --features wasm-threads

echo "==> artifacts:"
ls -la mandelbrot.wasm mandelbrot.threads.wasm mandelbrot.js 2>/dev/null || true

if [[ "${1:-}" == "--build" ]]; then
  echo "==> build only; open index.html via a COOP/COEP server to run."
  exit 0
fi

echo "==> serving on http://localhost:8000 (Ctrl-C to stop)"
echo "    (cross-origin isolated — required for SharedArrayBuffer)"
exec python3 serve.py
