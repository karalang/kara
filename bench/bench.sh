#!/usr/bin/env bash
# Top-level bench aggregator: run every track's reproduction script in
# sequence. Each track stays independently runnable (cd <track> && ./bench.sh);
# a track failure doesn't stop the sweep — failures are reported at the end
# with a nonzero exit.
set -uo pipefail
cd "$(dirname "$0")"

failed=()
for script in */bench.sh; do
    track="${script%/bench.sh}"
    echo ""
    echo "══════════ $track ══════════"
    (cd "$track" && ./bench.sh) || failed+=("$track")
done

echo ""
if ((${#failed[@]})); then
    echo "failed tracks: ${failed[*]}" >&2
    exit 1
fi
echo "all tracks complete"
