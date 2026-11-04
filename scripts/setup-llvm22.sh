#!/usr/bin/env bash
# Install LLVM 22 for `--features llvm` builds in a container that cannot
# reach apt.llvm.org (a cloud session's egress policy refuses it; Ubuntu
# noble's own archive stops at LLVM 20). It takes the official release
# tarball from GitHub, which those sessions can reach, and keeps only what
# the build and the tests use: llvm-config, opt and llc, the headers, and
# the libraries. Idempotent: a second run with the same version is a no-op.
#
#   bash scripts/setup-llvm22.sh
#
# Environment (optional):
#   LLVM22_VERSION=<x.y.z>   release to install (default below)
#   LLVM22_PREFIX=<dir>      where it goes (default /opt/llvm-22)
#   LLVM22_TARBALL=<file>    a tarball already downloaded (kept afterwards)
#
# It also writes `LLVM_SYS_221_PREFIX` into ~/.cargo/config.toml's [env]
# table, so every later `cargo` invocation in this container finds it with
# nothing exported, and links `opt` and `llc` into /usr/local/bin, ahead of
# any older system copies, for the tests that run them on emitted IR.
# CI installs LLVM 22 from apt.llvm.org instead (.github/workflows/ci.yml).
set -euo pipefail

VERSION="${LLVM22_VERSION:-22.1.8}"
PREFIX="${LLVM22_PREFIX:-/opt/llvm-22}"
NAME="LLVM-$VERSION-Linux-X64"
URL="https://github.com/llvm/llvm-project/releases/download/llvmorg-$VERSION/$NAME.tar.xz"

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) ;;
    *) echo "setup-llvm22.sh: only for Linux x86_64 containers (macOS: brew install llvm@22)" >&2; exit 1 ;;
esac

used_kib() { du -sk "$1" 2>/dev/null | cut -f1; }

configure() {
    # Cargo's [env] reaches build scripts, so llvm-sys sees the prefix.
    local cfg="$HOME/.cargo/config.toml"
    mkdir -p "$(dirname "$cfg")"
    touch "$cfg"
    if ! grep -q '^LLVM_SYS_221_PREFIX' "$cfg"; then
        if grep -q '^\[env\]' "$cfg"; then
            sed -i "/^\[env\]/a LLVM_SYS_221_PREFIX = \"$PREFIX\"" "$cfg"
        else
            printf '\n[env]\nLLVM_SYS_221_PREFIX = "%s"\n' "$PREFIX" >> "$cfg"
        fi
    else
        sed -i "s|^LLVM_SYS_221_PREFIX.*|LLVM_SYS_221_PREFIX = \"$PREFIX\"|" "$cfg"
    fi
    for t in opt llc llvm-config; do
        ln -sf "$PREFIX/bin/$t" "/usr/local/bin/$t"
    done
}

# The static libraries link against these system libraries.
missing=""
for pkg in zlib1g-dev libzstd-dev libxml2-dev; do
    dpkg -s "$pkg" >/dev/null 2>&1 || missing="$missing $pkg"
done
if [ -n "$missing" ]; then
    echo "Installing$missing"
    apt-get update -qq >/dev/null 2>&1 || true
    apt-get install -y -qq $missing >/dev/null
fi

if [ -x "$PREFIX/bin/llvm-config.real" ] && [ -f "$PREFIX/lib/libPolly.a" ] && [ "$("$PREFIX/bin/llvm-config" --version)" = "$VERSION" ]; then
    configure
    echo "LLVM $VERSION already at $PREFIX ($(( $(used_kib "$PREFIX") / 1024 )) MiB); nothing to do."
    exit 0
fi

# The download is resumed until whole: a long stream through the session's
# proxy can be reset part-way.
WORK="$(mktemp -d "${TMPDIR:-/tmp}/llvm22.XXXXXX")"
trap 'rm -rf "${WORK:?}"' EXIT
if [ -n "${LLVM22_TARBALL:-}" ]; then
    TARBALL="$LLVM22_TARBALL"
else
    TARBALL="$WORK/$NAME.tar.xz"
    echo "Downloading $URL (about 1.9 GB)"
    for attempt in $(seq 1 15); do
        if curl -fsSL --retry 3 -C - -o "$TARBALL" "$URL"; then
            break
        fi
        echo "download interrupted (attempt $attempt), resuming" >&2
        sleep 2
    done
fi
xz -t "$TARBALL"

rm -rf "${PREFIX:?}.new"
mkdir -p "$PREFIX.new"
tar -xJf "$TARBALL" -C "$PREFIX.new" --strip-components=1 --wildcards \
    "$NAME/bin/llvm-config" "$NAME/bin/opt" "$NAME/bin/llc" \
    "$NAME/include/llvm/*" "$NAME/include/llvm-c/*" \
    "$NAME/lib/libLLVM*" "$NAME/lib/libPolly*" "$NAME/lib/cmake/llvm/*"
[ -n "${LLVM22_TARBALL:-}" ] || rm -f "$TARBALL"
# The release's llvm-config names libzstd by the build machine's absolute
# path (`/usr/lib/x86_64-linux-gnu/libzstd.a`), which llvm-sys cannot parse:
# a wrapper rewrites such a path to `-l<name>` in `--system-libs`.
mv "$PREFIX.new/bin/llvm-config" "$PREFIX.new/bin/llvm-config.real"
cat > "$PREFIX.new/bin/llvm-config" <<'WRAP'
#!/bin/sh
out=$("$(dirname "$(readlink -f "$0")")/llvm-config.real" "$@") || exit $?
case " $* " in
    *" --system-libs "*) printf '%s\n' "$out" | sed -E 's#(^| )/[^ ]*/lib([A-Za-z0-9_+-]+)\.(a|so)( |$)#\1-l\2\4#g' ;;
    *) printf '%s\n' "$out" ;;
esac
WRAP
chmod +x "$PREFIX.new/bin/llvm-config"
rm -rf "${PREFIX:?}"
mv "$PREFIX.new" "$PREFIX"
"$PREFIX/bin/llvm-config" --version >/dev/null
configure
echo "LLVM $("$PREFIX/bin/llvm-config" --version) installed at $PREFIX ($(( $(used_kib "$PREFIX") / 1024 )) MiB, shared mode $("$PREFIX/bin/llvm-config" --shared-mode))."
echo "LLVM_SYS_221_PREFIX is set in ~/.cargo/config.toml; opt, llc and llvm-config are linked into /usr/local/bin."
