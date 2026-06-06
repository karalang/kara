#!/usr/bin/env bash
# Foreground launcher for the bench harness's --server-bin contract.
#
# `exec node server.js` replaces this shell with the Node process, so the
# PID the harness spawned IS the process measured by `ps -o rss=` (no
# wrapper indirection). server.js prints `BOUND_PORT=<n>` on stdout once the
# https listener binds; the harness reads it. cert.pem/key.pem sit next to
# server.js (resolved via __dirname).
#
# Unlike the Go/.NET comparators there is no self-contained build step: Node
# is an interpreter, so the rig needs a Node runtime installed and the `ws`
# dependency vendored. One-time, on-box:
#   ( cd examples/ws_idle_holder/node && npm ci --omit=dev )
# `npm ci` installs the exact tree from package-lock.json (committed).
#
# NODE_OPTIONS passes through, so a heap-dial cross-check is a drop-in:
#   NODE_OPTIONS=--max-old-space-size=512 run_server.sh
# the V8 analog of the JVM -Xmx / .NET DOTNET_GCHeapHardLimit dials
# documented in the sibling comparators.
set -euo pipefail
cd "$(dirname "$0")"

if [[ ! -d node_modules/ws ]]; then
  echo "$0: ws not installed — run: npm ci --omit=dev" >&2
  exit 1
fi

exec node server.js
