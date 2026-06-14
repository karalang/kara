#!/usr/bin/env bash
# Cartographer Studio — build the WASM and serve the live editor.
#
# The studio runs the Kāra compiler's whole-program effect/concurrency
# analysis IN THE BROWSER (compiled to WASM via the `karac-playground`
# crate), so editing the Kāra source re-draws the effect graph with no
# server round-trip and no local `karac` process.
#
#   ./studio.sh           # build wasm + serve on http://localhost:8000/studio.html
#
# Requires `wasm-pack` (cargo install wasm-pack). A static file server is
# needed because browsers won't fetch a .wasm module over file:// — this
# is a static server only, NOT a karac backend.
set -euo pipefail
cd "$(dirname "$0")"

if ! command -v wasm-pack >/dev/null 2>&1; then
    echo "error: wasm-pack not found. Install with: cargo install wasm-pack" >&2
    exit 1
fi

PLAYGROUND="../../playground"
echo "Building the karac WASM module (the compiler's analysis pipeline) ..."
( cd "$PLAYGROUND" && wasm-pack build --target web --out-dir web/pkg )

# Stage the built pkg next to studio.html so its `import` resolves.
rm -rf pkg && cp -R "$PLAYGROUND/web/pkg" pkg

PORT="${PORT:-8000}"
echo
echo "Studio ready. Open:  http://localhost:${PORT}/studio.html"
echo "(Ctrl-C to stop the server.)"
exec python3 -m http.server "$PORT"
