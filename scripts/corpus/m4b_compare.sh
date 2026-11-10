#!/usr/bin/env bash
# The M4b auto-par comparison: a release karac and every runtime archive
# built at HEAD, then mir-llvm over the whole corpus with KARAC_AUTO_PAR=1
# and =0, compared by scripts/corpus/m4b_compare.py. The bar is zero
# differences. About 15 minutes of build and 35 minutes per lane on 4 cores.
#
#   bash scripts/corpus/m4b_compare.sh <out-dir> [run.py args, e.g. --filter x]
#
# Needs LLVM 22 (scripts/setup-llvm22.sh). Exits with the comparison's code.
set -u
out=${1:?usage: m4b_compare.sh <out-dir> [run.py args]}
shift
mkdir -p "$out"
out=$(cd "$out" && pwd)
cd "$(dirname "$0")/../.."
echo "sha $(git rev-parse --short HEAD) start $(date -u +%FT%TZ)"
cargo build -q --release --features llvm || { echo "BUILD FAILED"; exit 3; }
b() { cargo rustc -q -p karac-runtime --release "$@" --crate-type staticlib; }
# Lean first, then the opt-in archives, then the full one at the canonical name.
b --no-default-features --features net && cp target/release/libkarac_runtime.a target/release/libkarac_runtime_min.a
for f in regex unicode arrow gpu; do
  if b --features "$f"; then
    cp target/release/libkarac_runtime.a "target/release/libkarac_runtime_$f.a"
  else
    echo "archive $f FAILED"
  fi
done
b || { echo "FULL ARCHIVE FAILED"; exit 3; }
echo "archives rebuilt at $(git rev-parse --short HEAD)"
karac=$(pwd)/target/release/karac
"$karac" --version
for lane in on off; do
  case $lane in on) par=1 ;; off) par=0 ;; esac
  echo "lane $lane start $(date -u +%FT%TZ)"
  KARAC_AUTO_PAR=$par python3 scripts/corpus/run.py --karac "$karac" --backend mir-llvm --jobs 4 "$@" > "$out/$lane.log" 2>&1
  echo "lane $lane rc=$? end $(date -u +%FT%TZ)"
  grep '^corpus-run' "$out/$lane.log"
done
python3 scripts/corpus/m4b_compare.py "$out/on.log" "$out/off.log"
