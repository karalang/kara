# Local mirror of CI's "memory-sanitizer" job (Linux ASAN + LeakSanitizer).
#
# macOS has NO LeakSanitizer (`-fsanitize=address` there catches use-after-free /
# double-free only), so the whole codegen-ownership LEAK class is invisible on a
# Mac and only CI catches it. This image reproduces the authoritative gate locally
# via colima so leak-class bugs can be debugged without a CI round-trip.
#
# Driven by scripts/lsan-local.sh. Mirrors .github/workflows/ci.yml `memory-sanitizer`:
# ubuntu-24.04 + llvm-18 + rust stable, build lean->full runtime archives, then
# `cargo test --features llvm --test memory_sanitizer`.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        llvm-18 llvm-18-dev clang-18 lld-18 \
        build-essential pkg-config libssl-dev \
        curl ca-certificates git \
    && rm -rf /var/lib/apt/lists/*

# CI sets these from `llvm-config-18 --prefix` / `--libdir`.
ENV LLVM_SYS_181_PREFIX=/usr/lib/llvm-18
ENV LD_LIBRARY_PATH=/usr/lib/llvm-18/lib

# Rust stable — matches CI's dtolnay/rust-toolchain@stable.
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/usr/lib/llvm-18/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
        --default-toolchain stable --profile minimal \
    && rustc --version && cargo --version

WORKDIR /work
