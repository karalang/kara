#!/usr/bin/env bash
# Cold compile-elapsed benchmark for karac vs rustc -O (vs optional clang -O2).
# Companion to ../README.md (bench-setup protocol) and ./README.md (this track).
#
# PLACEHOLDER scaffold — fills out during implementation per docs/roadmap.md
# § Phase 8.5 Track 5. Today this script lists the planned steps as comments
# so reviewers can see the intended shape.

set -euo pipefail
cd "$(dirname "$0")"

require() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "$1 not found — install with: $2" >&2
        exit 1
    fi
}

require hyperfine "brew install hyperfine"
require rustc     "rustup (https://rustup.rs)"
require karac     "cargo install --path ../.. --features llvm  (from karac-rust checkout)"

# Workload set (filled in once the seed kata + synthetic land):
#   - synthetic.kara / synthetic.rs   (front-end stress, ~10K LOC, v1-required)
#   - <seed-kata>.kara / <seed-kata>.rs (algorithmic shape, evolves over time)
#   - <backend-kata>.kara / <backend-kata>.rs (real-shape backend, v1-required
#       from kara-katas/PLAN.md priority #1 when it lands)
#
# Per (workload, compiler) pair, run:
#   hyperfine --warmup 1 --runs 10 --shell=none \
#       --prepare 'rm -f <artifact>' \
#       --export-json latest.json \
#       --command-name '<label>' \
#       '<command>'

echo "compile_speed bench — scaffold; fills out per docs/roadmap.md § Phase 8.5 Track 5"
echo "see README.md for the planned corpus and CI workflow shape"
