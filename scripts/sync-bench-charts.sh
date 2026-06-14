#!/usr/bin/env bash
# Mirror the headline benchmark charts from the kara-katas repo into
# docs/assets/ so the README renders them inline (GitHub does not reliably
# render hotlinked external SVGs, so they must live in-repo).
#
# Two sequential-lane charts (runtime-seq, binary-seq) plus the cross-language
# parallel-lane runtime chart (runtime-par — Kāra auto-par vs Rust rayon vs Go
# goroutines vs a C-pthreads floor).
#
# Run this after regenerating the charts in kara-katas:
#     (in kara-katas)  python3 scripts/bench-graph.py
#     (in kara)        scripts/sync-bench-charts.sh
#
# kara-katas is expected as a sibling checkout (../kara-katas); override
# with KARA_KATAS_DIR=/path/to/kara-katas.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
KATAS="${KARA_KATAS_DIR:-$REPO/../kara-katas}"
DEST="$REPO/docs/assets"

mkdir -p "$DEST"
for chart in runtime-seq binary-seq runtime-par; do
    src="$KATAS/graphs/$chart.svg"
    if [ ! -f "$src" ]; then
        echo "error: $src not found — set KARA_KATAS_DIR or regenerate the charts in kara-katas" >&2
        exit 1
    fi
    cp "$src" "$DEST/$chart.svg"
    echo "synced docs/assets/$chart.svg  <-  $src"
done
echo "done. commit docs/assets/*.svg if they changed."
