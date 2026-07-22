#!/usr/bin/env bash
# Cold compile-elapsed benchmark: karac build vs rustc -O per corpus member.
# Protocol: ../README.md (hyperfine discipline). Gate consumption: compare.py
# + .github/workflows/compile-speed.yml.
#
#   ./bench.sh                          # human tables + merged latest.json
#   KARAC=/path/to/karac ./bench.sh     # override the karac under test
#
# Corpus members (workload → runs, per the short/long-workload discipline):
#   synthetic  — ~10K LOC front-end stress (gen_synthetic.py)   → 10 runs
#   seed_kata  — two-sum hash-map kata (~40 LOC, algorithmic)   → 30 runs
set -euo pipefail
cd "$(dirname "$0")"

REPO_ROOT="$(cd ../.. && pwd)"
KARAC="${KARAC:-$REPO_ROOT/target/release/karac}"

require() {
    command -v "$1" >/dev/null 2>&1 || { echo "error: $1 not found — install: $2" >&2; exit 1; }
}
require hyperfine "cargo install hyperfine  (or: apt-get install hyperfine / brew install hyperfine)"
require rustc "rustup (https://rustup.rs)"
require python3 "system package manager"
[[ -x "$KARAC" ]] || {
    echo "error: karac not found at $KARAC" >&2
    echo "build it: cargo build --release --features llvm --bin karac" >&2
    exit 1
}
[[ -f "$(dirname "$KARAC")/libkarac_runtime.a" ]] || {
    echo "error: runtime archive missing next to $KARAC — karac build links it" >&2
    echo "build it: cargo rustc -p karac-runtime --release --crate-type staticlib" >&2
    exit 1
}

# Guard the llvm-feature footgun: a karac built WITHOUT --features llvm
# silently degrades `karac build` to a type check ("All checks passed."),
# which would produce absurdly-fast garbage numbers. Probe for "Built:".
probe_dir="$(mktemp -d)"
trap 'rm -rf "$probe_dir"' EXIT
echo 'fn main() { println(1); }' > "$probe_dir/probe.kara"
if ! (cd "$probe_dir" && "$KARAC" build probe.kara 2>/dev/null | grep -q "^Built:"); then
    echo "error: $KARAC cannot AOT-build (built without --features llvm?)" >&2
    exit 1
fi

# Twin-equivalence oracle (synthetic only; the seed kata's main is a runtime
# benchmark, so it is compile-only here): both twins must print the same
# checksum before their compile times are worth comparing.
echo "── twin-equivalence oracle (synthetic) ──"
rm -f synthetic synthetic_rs
"$KARAC" build synthetic.kara >/dev/null
rustc -O -o synthetic_rs synthetic.rs
kara_out="$(./synthetic)"
rust_out="$(./synthetic_rs)"
if [[ "$kara_out" != "$rust_out" ]]; then
    echo "error: twin divergence — kara printed '$kara_out', rust printed '$rust_out'" >&2
    echo "the corpus member is miscompiled somewhere; fix before benching" >&2
    exit 1
fi
echo "ok: both twins print $kara_out"

# Per-workload hyperfine runs. --prepare is positional (one per command):
# each deletes that command's artifact so every run is a full cold compile.
echo "── synthetic (~10K LOC, 10 runs) ──"
hyperfine --warmup 1 --runs "${RUNS_LONG:-10}" --shell=none \
    --export-json .synthetic.json \
    --prepare 'rm -f synthetic' \
    --command-name 'karac:synthetic' \
    "$KARAC build synthetic.kara" \
    --prepare 'rm -f synthetic_rs' \
    --command-name 'rustc:synthetic' \
    "rustc -O -o synthetic_rs synthetic.rs"

echo "── seed_kata (two-sum, 30 runs) ──"
hyperfine --warmup 3 --runs "${RUNS_SHORT:-30}" --shell=none \
    --export-json .seed_kata.json \
    --prepare 'rm -f seed_kata' \
    --command-name 'karac:seed_kata' \
    "$KARAC build seed_kata.kara" \
    --prepare 'rm -f seed_kata_rs' \
    --command-name 'rustc:seed_kata' \
    "rustc -O -o seed_kata_rs seed_kata.rs"

# Merge the per-workload exports into one latest.json (hyperfine schema).
python3 - <<'EOF'
import json
results = []
for f in (".synthetic.json", ".seed_kata.json"):
    results.extend(json.load(open(f))["results"])
json.dump({"results": results}, open("latest.json", "w"), indent=2)
print(f"latest.json: {len(results)} results")
EOF
rm -f .synthetic.json .seed_kata.json

echo "── ratios ──"
python3 compare.py --latest latest.json --ratios-only
