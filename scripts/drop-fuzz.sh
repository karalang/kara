#!/usr/bin/env bash
# One-command driver for the drop-soundness fuzzer (ownership-model-mechanization
# spike, Slice 1 — see docs/spikes/ownership-model-mechanization.md).
#
# The fuzzer generates well-typed heap-core Kāra programs, compiles each with the
# AOT path `karac build` ships, links it under AddressSanitizer + LeakSanitizer,
# runs it, and lets the sanitizer be the judge (double-free / UAF via ASan, leaks
# via LSan). It measures a drop-bug rate and emits a shrunk, bucketed repro
# corpus. This script guarantees the two hard prerequisites are in place first:
#
#   1. The runtime staticlib archives (`libkarac_runtime.a` + the lean
#      `libkarac_runtime_min.a`), which the ASan link step consumes. Built in the
#      CLAUDE.md-mandated lean-then-full order.
#   2. A `cc` that supports `-fsanitize=address` (clang or gcc with ASan).
#
# LeakSanitizer (the leak arm) ships with upstream LLVM's ASan on **Linux**;
# Apple-clang macOS has no LSan, so a Mac run catches double-free / UAF only. On
# macOS, drive the Linux/LSan container gate via scripts/lsan-local.sh --shell
# and run this script inside it for full leak coverage.
#
# Usage:
#   scripts/drop-fuzz.sh                         # default 200-program run
#   scripts/drop-fuzz.sh --count 500 --seed 1000 # forwarded to the binary
#   scripts/drop-fuzz.sh --out target/df --verbose
#
# All arguments are forwarded verbatim to the `drop_fuzz` binary
# (`--count`, `--seed`, `--out`, `--no-shrink`, `--keep-going`, `--verbose`).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

echo ">> building runtime staticlib archives (lean -> full, per CLAUDE.md order) ..."
cargo rustc -p karac-runtime --release --no-default-features --features net --crate-type staticlib
cp target/release/libkarac_runtime.a target/release/libkarac_runtime_min.a
cargo rustc -p karac-runtime --release --crate-type staticlib

echo ">> building the drop_fuzz binary (--features llvm) ..."
cargo build --release --features llvm --bin drop_fuzz

echo ">> running the fuzzer ..."
exec ./target/release/drop_fuzz "$@"
