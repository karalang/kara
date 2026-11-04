#!/usr/bin/env bash
# Install LLVM 22 on a GitHub Actions runner and export what llvm-sys needs
# (`LLVM_SYS_221_PREFIX`, and the library path a dynamic libLLVM is found
# on at run time) through $GITHUB_ENV. Every workflow job that builds
# `--features llvm` runs this one script, so the version lives here.
#
#   bash scripts/ci-install-llvm22.sh            # dev packages
#   bash scripts/ci-install-llvm22.sh --static   # also libPolly, for llvm-static
#
# Linux takes apt.llvm.org's llvm-toolchain-<codename>-22 (Ubuntu's own
# archive stops at LLVM 20 on noble); macOS takes Homebrew's llvm@22, or
# `llvm` when that is 22. A cloud container that cannot reach
# apt.llvm.org uses scripts/setup-llvm22.sh instead.
set -euo pipefail

MAJOR=22
STATIC=0
[ "${1:-}" = "--static" ] && STATIC=1
ENV_FILE="${GITHUB_ENV:-/dev/stdout}"

case "$(uname -s)" in
    Linux)
        codename="$(. /etc/os-release && echo "$VERSION_CODENAME")"
        sudo install -d -m 0755 /etc/apt/keyrings
        curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key | sudo tee /etc/apt/keyrings/apt.llvm.org.asc >/dev/null
        echo "deb [signed-by=/etc/apt/keyrings/apt.llvm.org.asc] http://apt.llvm.org/$codename/ llvm-toolchain-$codename-$MAJOR main" \
            | sudo tee /etc/apt/sources.list.d/llvm-$MAJOR.list >/dev/null
        sudo apt-get update
        pkgs="llvm-$MAJOR llvm-$MAJOR-dev libzstd-dev zlib1g-dev libxml2-dev"
        [ "$STATIC" = 1 ] && pkgs="$pkgs libpolly-$MAJOR-dev"
        # shellcheck disable=SC2086
        sudo apt-get install -y $pkgs
        prefix="$(llvm-config-$MAJOR --prefix)"
        echo "LLVM_SYS_${MAJOR}1_PREFIX=$prefix" >> "$ENV_FILE"
        echo "LD_LIBRARY_PATH=$(llvm-config-$MAJOR --libdir)" >> "$ENV_FILE"
        echo "$prefix/bin" >> "${GITHUB_PATH:-/dev/null}"
        ;;
    Darwin)
        if brew install "llvm@$MAJOR"; then
            prefix="$(brew --prefix "llvm@$MAJOR")"
        else
            brew install llvm
            prefix="$(brew --prefix llvm)"
        fi
        echo "LLVM_SYS_${MAJOR}1_PREFIX=$prefix" >> "$ENV_FILE"
        echo "DYLD_FALLBACK_LIBRARY_PATH=$prefix/lib" >> "$ENV_FILE"
        ;;
    *)
        echo "ci-install-llvm22.sh: no recipe for $(uname -s)" >&2
        exit 1
        ;;
esac

got="$("$prefix/bin/llvm-config" --version)"
case "$got" in
    "$MAJOR".*) echo "LLVM $got at $prefix" ;;
    *) echo "ci-install-llvm22.sh: wanted LLVM $MAJOR, $prefix has $got" >&2; exit 1 ;;
esac
