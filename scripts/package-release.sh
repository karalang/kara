#!/usr/bin/env bash
# Assemble a relocatable karac release bundle (release-pipeline step 2).
#
# Builds the compiler + JIT runner with STATIC LLVM (`llvm-static`
# feature — a downloaded binary must not require libLLVM.so on the
# user's machine), builds the runtime archives (lean → full, the
# CLAUDE.md order — the canonical name is overwritten by design), and
# lays them out in the installed-distribution shape
# `driver.rs::link_executable` resolves (`<bin-dir>/../lib/`):
#
#   karac-<version>-<target>/
#     bin/karac
#     bin/karac_jit_runner        # `karac run` (JIT) spawns this sibling
#     lib/libkarac_runtime.a      # full runtime (TLS on)
#     lib/libkarac_runtime_min.a  # lean runtime (auto-selected for
#                                 #  programs with no TLS-only symbols)
#     README.md
#
# Output: dist/karac-<version>-<target>.tar.gz + .sha256, where
# <version> is the stamped `karac --version` (with `+` sanitized to `-`
# for filename/URL friendliness).
#
# Usage: scripts/package-release.sh <target-label>
#   e.g. scripts/package-release.sh x86_64-linux
#        scripts/package-release.sh aarch64-linux
#        scripts/package-release.sh aarch64-macos
#
# Requirements: LLVM 18 with static archives (apt llvm-18-dev / brew
# llvm@18) reachable by llvm-sys (LLVM_SYS_181_PREFIX), and a full-depth
# git checkout — a shallow clone stamps `dev.shallow`, which the check
# below rejects for release artifacts (fetch-depth: 0 in CI).

set -euo pipefail
cd "$(dirname "$0")/.."

TARGET_LABEL="${1:?usage: package-release.sh <target-label>}"

echo "==> building runtime archives (lean → full)"
cargo rustc -p karac-runtime --release --no-default-features --features net --crate-type staticlib
cp target/release/libkarac_runtime.a target/release/libkarac_runtime_min.a
cargo rustc -p karac-runtime --release --crate-type staticlib

echo "==> building karac + karac_jit_runner (release, static LLVM)"
cargo build --release --no-default-features --features llvm,llvm-static --bin karac --bin karac_jit_runner

VERSION_RAW="$(target/release/karac --version | awk '{print $2}')"
if [[ "$VERSION_RAW" == *"dev.shallow"* || "$VERSION_RAW" == *"dev.unknown"* ]]; then
    echo "ERROR: version stamp is '$VERSION_RAW' — release artifacts require a" >&2
    echo "full-depth git checkout (CI: actions/checkout with fetch-depth: 0)." >&2
    exit 1
fi
if [[ "$VERSION_RAW" == *".dirty"* ]]; then
    echo "ERROR: version stamp is '$VERSION_RAW' — refusing to package an" >&2
    echo "uncommitted tree as a release artifact." >&2
    exit 1
fi
VERSION="${VERSION_RAW//+/-}"

# Linux leg: verify the static-LLVM promise before packaging — a bundle
# that still links libLLVM.so would fail on every user machine without
# LLVM installed, which is the exact failure this pipeline exists to
# prevent. (macOS `otool` check is the equivalent; keep both cheap.)
case "$(uname -s)" in
Linux)
    if ldd target/release/karac | grep -q 'libLLVM'; then
        echo "ERROR: target/release/karac still links libLLVM dynamically:" >&2
        ldd target/release/karac | grep libLLVM >&2
        exit 1
    fi
    ;;
Darwin)
    if otool -L target/release/karac | grep -q 'libLLVM'; then
        echo "ERROR: target/release/karac still links libLLVM dynamically:" >&2
        otool -L target/release/karac | grep libLLVM >&2
        exit 1
    fi
    ;;
esac

BUNDLE="karac-${VERSION}-${TARGET_LABEL}"
STAGE="dist/${BUNDLE}"
echo "==> assembling ${STAGE}"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/lib"
cp target/release/karac "$STAGE/bin/"
cp target/release/karac_jit_runner "$STAGE/bin/"
cp target/release/libkarac_runtime.a "$STAGE/lib/"
cp target/release/libkarac_runtime_min.a "$STAGE/lib/"

cat > "$STAGE/README.md" <<EOF
# karac ${VERSION_RAW} — development preview (${TARGET_LABEL})

Pre-v1 development build of the Kāra compiler. Expect breakage; the
compiler's own defect record is public at
https://github.com/karalang/kara/blob/main/docs/bug-ledger.md — please
report issues with the exact version string from \`karac --version\`
(its \`+g<sha>\` suffix identifies the commit this binary was built
from).

## Install

Unpack anywhere and put \`bin/\` on PATH — the layout is relocatable
(\`karac\` finds its runtime libraries relative to its own location):

    tar xzf ${BUNDLE}.tar.gz
    export PATH="\$PWD/${BUNDLE}/bin:\$PATH"

macOS: Gatekeeper quarantines unsigned downloads. Clear it once:

    xattr -dr com.apple.quarantine ${BUNDLE}

## Try it

    cat > hello.kara <<'KARA'
    fn main() {
        println("hello from Kāra")
    }
    KARA
    karac run hello.kara     # JIT
    karac build hello.kara   # native binary (needs a C linker: cc/clang)
    ./hello

## Requirements

- A C toolchain for AOT linking (\`cc\`/\`clang\` — build-essential on
  Debian/Ubuntu, Xcode CLT on macOS). \`karac run\` needs nothing extra.
- Common system libraries LLVM links dynamically (zlib, zstd, terminfo)
  — present by default on mainstream distros and macOS.
- WASM targets (\`--target=wasm_wasi\` / \`wasm_browser\`) are not
  bundled in this preview; build from source for those.

Docs: https://github.com/karalang/kara/blob/main/docs/design.md
EOF

mkdir -p dist
tar -C dist -czf "dist/${BUNDLE}.tar.gz" "$BUNDLE"
if command -v sha256sum > /dev/null; then
    (cd dist && sha256sum "${BUNDLE}.tar.gz" > "${BUNDLE}.tar.gz.sha256")
else
    (cd dist && shasum -a 256 "${BUNDLE}.tar.gz" > "${BUNDLE}.tar.gz.sha256")
fi
echo "==> $(du -h "dist/${BUNDLE}.tar.gz" | cut -f1) dist/${BUNDLE}.tar.gz"
