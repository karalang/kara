#!/usr/bin/env bash
# Build Slipstream for the browser (threaded WASM) and serve it cross-origin
# isolated so SharedArrayBuffer + the Web Worker pool are available.
#
#   ./build.sh           # build, then serve on http://localhost:8000
#   ./build.sh --build   # build only
#   ./build.sh --native  # build + run the native LBM checksum oracle
#
# Slipstream is ONE Kāra package built to two targets from the same source tree —
# only the target flag changes. The browser build selects the `host_wasm` module;
# the native build selects `host_macos`/`host_linux`. The fluid kernel (sim.kara)
# is shared verbatim.
set -euo pipefail
cd "$(dirname "$0")"

KARAC="${KARAC:-karac}"

if [[ "${1:-}" == "--native" ]]; then
  echo "==> building native oracle (karac build)"
  "$KARAC" build >/dev/null
  echo "==> ./slipstream (milestone framebuffer checksums — the kernel gate):"
  ./slipstream
  exit 0
fi

echo "==> building slipstream (wasm_browser + wasm-threads)"
"$KARAC" build --target=wasm_browser --features wasm-threads

# The project build emits under dist/wasm/; copy the artifacts next to
# index.html so the page can import ./slipstream.js directly.
cp dist/wasm/slipstream.wasm dist/wasm/slipstream.threads.wasm \
   dist/wasm/slipstream.js dist/wasm/slipstream.d.ts ./

echo "==> artifacts:"
ls -la slipstream.wasm slipstream.threads.wasm slipstream.js 2>/dev/null || true

if [[ "${1:-}" == "--build" ]]; then
  echo "==> build only; open index.html via a COOP/COEP server to run."
  exit 0
fi

echo "==> serving on http://localhost:8000 (Ctrl-C to stop)"
echo "    (cross-origin isolated — required for SharedArrayBuffer)"
exec python3 serve.py
