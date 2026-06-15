#!/usr/bin/env bash
# Run the codegen LEAK gate (Linux ASAN + LeakSanitizer) locally on macOS, via a
# colima Linux container — the authoritative equivalent of CI's "memory-sanitizer"
# job. macOS has no LeakSanitizer, so this is the only way to catch the
# codegen-ownership leak class without pushing and waiting for CI.
#
# Usage:
#   scripts/lsan-local.sh                       # full memory_sanitizer suite
#   scripts/lsan-local.sh <name-filter>         # only tests matching the filter
#   scripts/lsan-local.sh --shell               # interactive shell in the container
#
# Leaks are architecture-independent (a missing free in the emitted drop logic),
# so this runs a NATIVE arm64 Linux container on Apple Silicon — same leaks as
# CI's x86_64, at native speed (no qemu). Reach for an x86 image only for a
# suspected arch-specific codegen bug.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE=kara-lsan

if ! colima status >/dev/null 2>&1; then
  echo ">> starting colima (arm64 Linux VM) ..."
  colima start --cpu 8 --memory 12 --disk 80
fi

echo ">> building toolchain image (cached after first build) ..."
docker build -t "$IMAGE" -f "$REPO/docker/lsan.Dockerfile" "$REPO/docker"

docker volume create kara-lsan-target >/dev/null   # Linux cargo target (off the mount, fast + persistent)
docker volume create kara-lsan-cargo  >/dev/null   # cargo registry cache

# Source is bind-mounted at /work; the target volume overlays <repo>/target so
# cargo/karac find the runtime archives at the path they expect, WITHOUT a
# CARGO_TARGET_DIR override (which the harness's archive resolution may not honor).
RUN_ARGS=(--rm
  -v "$REPO:/work"
  -v kara-lsan-target:/work/target
  -v kara-lsan-cargo:/opt/cargo/registry
  -w /work "$IMAGE")

if [[ "${1:-}" == "--shell" ]]; then
  exec docker run -it "${RUN_ARGS[@]}" bash
fi

FILTER="${1:-}"
docker run "${RUN_ARGS[@]}" bash -euo pipefail -c '
  echo ">> building runtime staticlib archives (lean -> full, per CLAUDE.md order) ..."
  cargo rustc -p karac-runtime --release --no-default-features --features net --crate-type staticlib
  cp target/release/libkarac_runtime.a target/release/libkarac_runtime_min.a
  cargo rustc -p karac-runtime --release --crate-type staticlib
  echo ">> cargo test --features llvm --test memory_sanitizer '"$FILTER"' ..."
  cargo test --features llvm --test memory_sanitizer '"$FILTER"'
'
