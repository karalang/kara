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
#     bin/kara-lsp                # the language server editors launch
#     README.md
#
# Output: dist/karac-<version>-<target>.tar.gz + .sha256, where
# <version> is the stamped `karac --version` (with `+` sanitized to `-`
# for filename/URL friendliness; on a release build, KARAC_RELEASE_TAG=
# v<crate version>, the bare version), and the VS Code extension for the same
# platform, dist/kara-vscode-<version>-<vscode-target>.vsix + .sha256,
# with `kara-lsp` bundled in its `server/` (editors/vscode: the client
# launches the bundled server before one on PATH).
#
# Usage: scripts/package-release.sh <target-label>
#   e.g. scripts/package-release.sh x86_64-linux
#        scripts/package-release.sh aarch64-linux
#        scripts/package-release.sh aarch64-macos
#
# Requirements: LLVM 18 with static archives (apt llvm-18-dev / brew
# llvm@18) reachable by llvm-sys (LLVM_SYS_181_PREFIX), a full-depth
# git checkout — a shallow clone stamps `dev.shallow`, which the check
# below rejects for release artifacts (fetch-depth: 0 in CI) — and
# Node 18+ with npm for the extension (`npm ci` fetches `vsce`).

set -euo pipefail
cd "$(dirname "$0")/.."

TARGET_LABEL="${1:?usage: package-release.sh <target-label>}"

# The VS Code platform the label's .vsix is for; checked before any build.
case "$TARGET_LABEL" in
    x86_64-linux) VSCODE_TARGET=linux-x64 ;;
    aarch64-linux) VSCODE_TARGET=linux-arm64 ;;
    x86_64-macos) VSCODE_TARGET=darwin-x64 ;;
    aarch64-macos) VSCODE_TARGET=darwin-arm64 ;;
    *)
        echo "ERROR: no VS Code target for '$TARGET_LABEL'" >&2
        exit 1 ;;
esac
command -v npm > /dev/null || { echo "ERROR: npm is required to package the VS Code extension" >&2; exit 1; }

echo "==> building runtime archives (lean → full)"
cargo rustc -p karac-runtime --release --no-default-features --features net --crate-type staticlib
cp target/release/libkarac_runtime.a target/release/libkarac_runtime_min.a
cargo rustc -p karac-runtime --release --crate-type staticlib

echo "==> building karac + karac_jit_runner (release, static LLVM)"
cargo build --release --no-default-features --features llvm,llvm-static --bin karac --bin karac_jit_runner

# Its own build: the server analyses source and links no LLVM at all.
echo "==> building kara-lsp (release)"
cargo build --release -p kara-lsp --bin kara-lsp

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
# A release tag (`v<base>`, no `-dev`) must have built a release stamp:
# build.rs only drops `-dev` when KARAC_RELEASE_TAG equals v<crate
# version>, so a dev stamp here means the tag and Cargo.toml disagree.
RELEASE_TAG="${KARAC_RELEASE_TAG:-}"
if [[ "$RELEASE_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+ && "$RELEASE_TAG" != *-dev* ]]; then
    if [[ "$VERSION_RAW" == *-dev.* || "v${VERSION_RAW%%+*}" != "$RELEASE_TAG" ]]; then
        echo "ERROR: release tag '$RELEASE_TAG' but karac --version is '$VERSION_RAW';" >&2
        echo "the tag must be v<version in Cargo.toml>." >&2
        exit 1
    fi
fi
# File names: a release names its version alone (`karac-0.9.0-<target>`);
# a dev build keeps its whole stamp, `+` sanitized to `-`.
if [[ "$VERSION_RAW" == *-dev.* ]]; then
    VERSION="${VERSION_RAW//+/-}"
else
    VERSION="${VERSION_RAW%%+*}"
fi

# Linux leg: verify the static-LLVM promise before packaging — a bundle
# that still links libLLVM.so would fail on every user machine without
# LLVM installed, which is the exact failure this pipeline exists to
# prevent. (The macOS equivalent runs post-staging, below: brew's LLVM
# leaves non-libLLVM keg references too — libunwind bit the first
# dry-run — so that leg relocates them into lib/ and then rejects ANY
# surviving brew/local reference, not just libLLVM.)
if [[ "$(uname -s)" == Linux ]]; then
    for bin in karac karac_jit_runner kara-lsp; do
        if ldd "target/release/$bin" | grep -q 'libLLVM'; then
            echo "ERROR: target/release/$bin still links libLLVM dynamically:" >&2
            ldd "target/release/$bin" | grep libLLVM >&2
            exit 1
        fi
    done
fi

BUNDLE="karac-${VERSION}-${TARGET_LABEL}"
STAGE="dist/${BUNDLE}"
echo "==> assembling ${STAGE}"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/lib"
cp target/release/karac "$STAGE/bin/"
cp target/release/karac_jit_runner "$STAGE/bin/"
cp target/release/kara-lsp "$STAGE/bin/"
cp target/release/libkarac_runtime.a "$STAGE/lib/"
cp target/release/libkarac_runtime_min.a "$STAGE/lib/"

# macOS: brew's LLVM links its OWN support dylibs from the keg —
# libunwind at minimum (brew builds LLVM against LLVM's libunwind, and
# the static-LLVM link still records `/opt/homebrew/opt/llvm@18/lib/
# libunwind.1.dylib`, which no user machine has; the first dry-run's
# clean-runner smoke died on exactly this dyld abort). Relocate every
# brew-/usr/local-prefixed dylib into the bundle's lib/ and rewrite the
# references to @executable_path/../lib (@loader_path between dylibs),
# then re-sign ad hoc — install_name_tool invalidates the signature and
# arm64 macOS kills unsigned binaries outright.
if [[ "$(uname -s)" == Darwin ]]; then
    relocate() { # relocate <macho-path> <ref-prefix>
        local macho="$1" prefix="$2" dep base deps
        deps="$(otool -L "$macho" | tail -n +2 | awk '{print $1}' \
            | grep -E '^(/opt/homebrew|/usr/local)/' || true)"
        for dep in $deps; do
            base="$(basename "$dep")"
            [[ "$base" == "$(basename "$macho")" ]] && continue  # LC_ID line
            if [[ ! -f "$STAGE/lib/$base" ]]; then
                cp "$dep" "$STAGE/lib/$base"   # follows the keg symlink
                chmod u+w "$STAGE/lib/$base"
                install_name_tool -id "@loader_path/$base" "$STAGE/lib/$base"
                relocate "$STAGE/lib/$base" "@loader_path"
            fi
            install_name_tool -change "$dep" "$prefix/$base" "$macho"
        done
        codesign --force --sign - "$macho"
    }
    relocate "$STAGE/bin/karac" "@executable_path/../lib"
    relocate "$STAGE/bin/karac_jit_runner" "@executable_path/../lib"
    relocate "$STAGE/bin/kara-lsp" "@executable_path/../lib"

    # The staged verification: NOTHING in the bundle may reference a
    # brew or /usr/local path — catch-all, not just libLLVM.
    for macho in "$STAGE"/bin/* "$STAGE"/lib/*.dylib; do
        [[ -f "$macho" && "$macho" != *.a ]] || continue
        if otool -L "$macho" | tail -n +2 | grep -E '^\s*(/opt/homebrew|/usr/local)/'; then
            echo "ERROR: $macho still references a non-system dylib (above)" >&2
            exit 1
        fi
    done
fi

if [[ "$VERSION_RAW" == *-dev.* ]]; then
    KIND="development preview"
    KIND_LINE="Pre-v1 development build of the Kāra compiler."
else
    KIND="beta"
    KIND_LINE="Pre-v1 beta release of the Kāra compiler."
fi
cat > "$STAGE/README.md" <<EOF
# karac ${VERSION_RAW} — ${KIND} (${TARGET_LABEL})

${KIND_LINE} Expect breakage; the
compiler's own defect record is public at
https://github.com/karalang/kara/blob/main/docs/bugs-v2.md — please
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

## Editor

\`bin/kara-lsp\` is the language server. For VS Code, install the
release's \`kara-vscode-${VERSION}-${VSCODE_TARGET}.vsix\`, which bundles
it (Extensions view, "Install from VSIX...", or
\`code --install-extension <file>.vsix\`). Other editors can launch
\`kara-lsp\` over stdio.

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

# The VS Code extension, with the staged (on macOS, relocated and
# re-signed) kara-lsp in `server/`. `server/` is gitignored and removed
# again, so the checkout stays clean for the next label.
VSIX="kara-vscode-${VERSION}-${VSCODE_TARGET}.vsix"
EXT=editors/vscode
echo "==> packaging ${VSIX}"
rm -rf "$EXT/server"
mkdir -p "$EXT/server"
cp "$STAGE/bin/kara-lsp" "$EXT/server/"
(
    cd "$EXT"
    npm ci --no-audit --no-fund
    npx vsce package --target "$VSCODE_TARGET" --out "../../dist/${VSIX}"
)
rm -rf "$EXT/server"
# The extension must carry the server, executable.
if ! unzip -Z "dist/${VSIX}" | grep -Eq '^-rwx.* extension/server/kara-lsp$'; then
    echo "ERROR: dist/${VSIX} has no executable extension/server/kara-lsp:" >&2
    unzip -l "dist/${VSIX}" >&2
    exit 1
fi
if command -v sha256sum > /dev/null; then
    (cd dist && sha256sum "${VSIX}" > "${VSIX}.sha256")
else
    (cd dist && shasum -a 256 "${VSIX}" > "${VSIX}.sha256")
fi
echo "==> $(du -h "dist/${VSIX}" | cut -f1) dist/${VSIX}"
