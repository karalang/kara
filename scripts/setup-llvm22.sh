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

# The release's llvm-config names libzstd by the build machine's absolute
# path (`/usr/lib/x86_64-linux-gnu/libzstd.a`), which llvm-sys cannot parse:
# the wrapper rewrites such a path to `-l<name>` in `--system-libs`. It also
# answers llvm-sys's `--link-shared` queries with the shared library
# `shared_lib` builds, which the release's llvm-config does not know of.
write_wrapper() {
    cat > "$1/bin/llvm-config" <<'WRAP'
#!/bin/sh
here="$(dirname "$(readlink -f "$0")")"
shared=no
for a in "$@"; do [ "$a" = --link-shared ] && shared=yes; done
if [ "$shared" = yes ] && [ -f "$here/../lib/libLLVM-22.so" ]; then
    for a in "$@"; do
        case "$a" in
            --libnames) echo libLLVM-22.so; exit 0 ;;
            --libs) echo -lLLVM-22; exit 0 ;;
            --libfiles) echo "$(readlink -f "$here/../lib")/libLLVM-22.so"; exit 0 ;;
        esac
    done
    first=yes
    for a in "$@"; do
        [ "$first" = yes ] && { set --; first=no; }
        [ "$a" = --link-shared ] && a=--link-static
        set -- "$@" "$a"
    done
fi
out=$("$here/llvm-config.real" "$@") || exit $?
case " $* " in
    *" --system-libs "*) printf '%s\n' "$out" | sed -E 's#(^| )/[^ ]*/lib([A-Za-z0-9_+-]+)\.(a|so)( |$)#\1-l\2\4#g' ;;
    *) printf '%s\n' "$out" ;;
esac
WRAP
    chmod +x "$1/bin/llvm-config"
}

# The release ships static libraries only. Linked statically, LLVM puts
# about 130 MB into karac (157 MB against 26 MB on LLVM 18's shared
# library) and into every test binary that reaches the backend, which is
# what a test leg's disk is spent on. So the static libraries are linked
# once into `libLLVM-22.so` (about 160 MB), which karac's default
# `prefer-dynamic` linking then takes, and the loader finds through
# /etc/ld.so.conf.d. The static libraries stay for `--features
# llvm-static`.
shared_lib() {
    [ -f "$1/lib/libLLVM-22.so" ] && return 0
    echo "Linking $1/lib/libLLVM-22.so from the static libraries"
    # Exactly the libraries llvm-config names for every component (Polly
    # among them, which LLVMExtensions calls). The tarball's other archives
    # are tools' (BOLT, OptDriver, Exegesis), and linking them in registers
    # a command-line option twice, which aborts at load ("Option 'p'
    # registered more than once"). `--no-undefined` refuses a library that
    # would only fail at karac's link.
    local libs
    libs=$("$1/bin/llvm-config.real" --link-static --libnames) || return 1
    # shellcheck disable=SC2086
    (cd "$1/lib" && c++ -shared -o libLLVM-22.so.tmp -Wl,-soname,libLLVM-22.so -Wl,--no-undefined \
        -Wl,--whole-archive $libs -Wl,--no-whole-archive -lz -lzstd -lxml2 -lpthread -ldl)
    mv "$1/lib/libLLVM-22.so.tmp" "$1/lib/libLLVM-22.so"
}

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
    echo "$PREFIX/lib" > /etc/ld.so.conf.d/llvm-22.conf
    ldconfig
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
    if [ ! -f "$PREFIX/lib/libLLVM-22.so" ]; then
        # An install from before the shared library: add it, and say that
        # target/ still holds a statically linked llvm-sys.
        write_wrapper "$PREFIX"
        shared_lib "$PREFIX"
        echo "Added the shared library: \`cargo clean -p llvm-sys\` (or a fresh target/) makes karac link it."
    fi
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
mv "$PREFIX.new/bin/llvm-config" "$PREFIX.new/bin/llvm-config.real"
write_wrapper "$PREFIX.new"
shared_lib "$PREFIX.new"
rm -rf "${PREFIX:?}"
mv "$PREFIX.new" "$PREFIX"
"$PREFIX/bin/llvm-config" --version >/dev/null
configure
echo "LLVM $("$PREFIX/bin/llvm-config" --version) installed at $PREFIX ($(( $(used_kib "$PREFIX") / 1024 )) MiB, shared mode $("$PREFIX/bin/llvm-config" --shared-mode))."
echo "LLVM_SYS_221_PREFIX is set in ~/.cargo/config.toml; opt, llc and llvm-config are linked into /usr/local/bin."
