#!/usr/bin/env bash
# Relay cross-host bench — Linux host provisioning.
#
# Runs ON a Linux host (Debian/Ubuntu family; apt). Installs the toolchains a
# given ROLE needs and builds that role's bench binaries from the repo, which
# must already be present (the orchestrator `bench-remote.sh --setup` rsyncs it
# to ~/kara before invoking this). ARM (aarch64) and x86-64 are both fine; the
# script is arch-agnostic.
#
# Roles:
#   proxy     — the host under test. Needs Rust + LLVM 18 (to build `karac` and
#               the Kāra proxy) AND go + node (the Go/Node proxies). Builds all
#               three proxies + the runtime archive.
#   client    — runs `wrk`. Needs only wrk.
#   upstream  — the shared origin. Needs only go (builds the upstream binary).
#   all       — everything (handy when co-locating roles on fewer hosts).
#
# Usage (typically invoked over ssh by bench-remote.sh):
#   bash examples/relay/bench/remote/provision.sh proxy
#
# LLVM pin: the compiler links inkwell's `llvm18-1` (see root Cargo.toml), so we
# install LLVM/clang 18 specifically. If the distro's default llvm is a
# different major, we add the official apt.llvm.org 18 channel.
set -euo pipefail

ROLE="${1:-all}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
BENCH_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd -- "$BENCH_DIR/../../.." && pwd)"
LLVM_MAJOR=18

log() { printf '[provision:%s] %s\n' "$ROLE" "$*" >&2; }
have() { command -v "$1" >/dev/null 2>&1; }

apt_install() {
  sudo apt-get update -y
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y "$@"
}

install_wrk() {
  if have wrk; then log "wrk present: $(wrk --version 2>&1 | head -1)"; return; fi
  log "installing wrk (build from source — not packaged on most distros)"
  apt_install build-essential libssl-dev git
  local tmp; tmp="$(mktemp -d)"
  git clone --depth 1 https://github.com/wg/wrk.git "$tmp/wrk"
  make -C "$tmp/wrk" -j"$(nproc)"
  sudo install -m 0755 "$tmp/wrk/wrk" /usr/local/bin/wrk
  rm -rf "$tmp"
  log "wrk installed: $(wrk --version 2>&1 | head -1)"
}

install_go() {
  if have go; then log "go present: $(go version)"; return; fi
  log "installing go via apt (golang-go)"
  apt_install golang-go
  log "go installed: $(go version)"
}

install_node() {
  if have node; then log "node present: $(node --version)"; return; fi
  log "installing node via apt (nodejs)"
  apt_install nodejs
  log "node installed: $(node --version)"
}

install_rust() {
  if have cargo; then log "cargo present: $(cargo --version)"; return; fi
  log "installing rust via rustup"
  apt_install curl build-essential
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1090,SC1091
  . "$HOME/.cargo/env"
  log "cargo installed: $(cargo --version)"
}

install_llvm() {
  # inkwell's llvm-sys needs `llvm-config` for LLVM 18 on PATH.
  if have "llvm-config-${LLVM_MAJOR}" || { have llvm-config && llvm-config --version | grep -q "^${LLVM_MAJOR}\."; }; then
    log "LLVM ${LLVM_MAJOR} present"
  else
    log "installing LLVM ${LLVM_MAJOR} (clang, dev libs, polly)"
    if ! apt-get -s install -y "llvm-${LLVM_MAJOR}-dev" >/dev/null 2>&1; then
      log "llvm-${LLVM_MAJOR} not in distro repos — adding apt.llvm.org channel"
      apt_install wget gnupg lsb-release software-properties-common
      wget -qO- https://apt.llvm.org/llvm.sh | sudo bash -s -- "${LLVM_MAJOR}"
    fi
    apt_install "llvm-${LLVM_MAJOR}-dev" "libpolly-${LLVM_MAJOR}-dev" "clang-${LLVM_MAJOR}" "libclang-${LLVM_MAJOR}-dev" zlib1g-dev libzstd-dev libffi-dev
  fi
  # llvm-sys reads LLVM_SYS_181_PREFIX or finds llvm-config on PATH.
  local cfg; cfg="$(command -v "llvm-config-${LLVM_MAJOR}" || true)"
  if [ -n "$cfg" ]; then
    export LLVM_SYS_181_PREFIX; LLVM_SYS_181_PREFIX="$("$cfg" --prefix)"
    log "LLVM_SYS_181_PREFIX=$LLVM_SYS_181_PREFIX"
    # Persist for later non-login shells / the build step below.
    grep -q LLVM_SYS_181_PREFIX "$HOME/.bashrc" 2>/dev/null || \
      echo "export LLVM_SYS_181_PREFIX=$LLVM_SYS_181_PREFIX" >> "$HOME/.bashrc"
  fi
}

build_kara_proxy() {
  log "building karac + runtime archive + kara proxy (cold build — several minutes)"
  # shellcheck disable=SC1090,SC1091
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  ( cd "$REPO_ROOT" && cargo build --release --features llvm -p karac )
  # Lean-then-full runtime archive, per root CLAUDE.md.
  ( cd "$REPO_ROOT" && cargo rustc -p karac-runtime --release --no-default-features --features net --crate-type staticlib )
  cp "$REPO_ROOT/target/release/libkarac_runtime.a" "$REPO_ROOT/target/release/libkarac_runtime_min.a"
  ( cd "$REPO_ROOT" && cargo rustc -p karac-runtime --release --crate-type staticlib )
  mkdir -p "$BENCH_DIR/kara/.bin"
  ( cd "$BENCH_DIR/kara" \
      && KARAC_RUNTIME="$REPO_ROOT/target/release/libkarac_runtime.a" \
         "$REPO_ROOT/target/release/karac" build server.kara \
      && mv server .bin/server )
  log "kara proxy built: $BENCH_DIR/kara/.bin/server"
}

build_go_proxy()  { ( cd "$BENCH_DIR/go"       && go build -o relay-bench-go . );        log "go proxy built"; }
build_upstream()  { ( cd "$BENCH_DIR/upstream" && go build -o relay-bench-upstream . );  log "upstream built"; }

case "$ROLE" in
  proxy)
    install_rust; install_llvm; install_go; install_node
    build_kara_proxy; build_go_proxy
    ;;
  client)
    install_wrk
    ;;
  upstream)
    install_go; build_upstream
    ;;
  all)
    install_rust; install_llvm; install_go; install_node; install_wrk
    build_kara_proxy; build_go_proxy; build_upstream
    ;;
  *)
    echo "unknown role: $ROLE (want: proxy|client|upstream|all)" >&2
    exit 2
    ;;
esac
log "done"
