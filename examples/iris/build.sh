#!/usr/bin/env bash
# Build Iris for the browser (threaded WASM) and serve it cross-origin isolated
# so SharedArrayBuffer + the Web Worker pool are available.
#
#   ./build.sh           # build, then serve on http://localhost:8000
#   ./build.sh --build   # build only
#   ./build.sh --native  # also build + run the native checksum oracle
#
# Iris is ONE Kāra package built to two targets from the same source tree — only
# the target flag changes. The browser build selects the `host_wasm` module; the
# native build selects `host_macos`/`host_linux`. The filter kernels (filters.kara)
# are shared verbatim.
set -euo pipefail
cd "$(dirname "$0")"

KARAC="${KARAC:-karac}"

if [[ "${1:-}" == "--native" ]]; then
  echo "==> building native oracle (karac build)"
  "$KARAC" build >/dev/null
  echo "==> ./iris (per-filter checksums — the A/B ground truth):"
  ./iris
  exit 0
fi

echo "==> building iris (wasm_browser + wasm-threads)"
"$KARAC" build --target=wasm_browser --features wasm-threads

# The project build emits under dist/wasm/; copy the artifacts next to
# index.html so the page can import ./iris.js directly.
cp dist/wasm/iris.wasm dist/wasm/iris.threads.wasm dist/wasm/iris.js dist/wasm/iris.d.ts ./

echo "==> artifacts:"
ls -la iris.wasm iris.threads.wasm iris.js 2>/dev/null || true

if [[ "${1:-}" == "--build" ]]; then
  echo "==> build only; open index.html via a COOP/COEP server to run."
  exit 0
fi

echo "==> serving on http://localhost:8000 (Ctrl-C to stop)"
echo "    (cross-origin isolated — required for SharedArrayBuffer)"
exec python3 serve.py
