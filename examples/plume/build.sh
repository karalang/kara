#!/usr/bin/env bash
# Build Plume for the browser (threaded WASM) and serve it cross-origin
# isolated so SharedArrayBuffer + the Web Worker pool are available.
#
#   ./build.sh          # build, then serve on http://localhost:8000
#   ./build.sh --build  # build only
set -euo pipefail
cd "$(dirname "$0")"

KARAC="${KARAC:-karac}"

echo "==> building plume.kara (wasm_browser + wasm-threads)"
"$KARAC" build plume.kara --target=wasm_browser --features wasm-threads

echo "==> artifacts:"
ls -la plume.wasm plume.threads.wasm plume.js 2>/dev/null || true

if [[ "${1:-}" == "--build" ]]; then
  echo "==> build only; open index.html via a COOP/COEP server to run."
  exit 0
fi

echo "==> serving on http://localhost:8000 (Ctrl-C to stop)"
echo "    (cross-origin isolated — required for SharedArrayBuffer)"
exec python3 serve.py
