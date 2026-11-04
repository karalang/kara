# Local mirror of CI's "memory-sanitizer" job (Linux ASAN + LeakSanitizer).
#
# macOS has NO LeakSanitizer (`-fsanitize=address` there catches use-after-free /
# double-free only), so the whole codegen-ownership LEAK class is invisible on a
# Mac and only CI catches it. This image reproduces the authoritative gate locally
# via colima so leak-class bugs can be debugged without a CI round-trip.
#
# Driven by scripts/lsan-local.sh. Mirrors .github/workflows/ci.yml `memory-sanitizer`:
# ubuntu-24.04 + llvm-22 (apt.llvm.org) + rust stable, build lean->full runtime archives, then
# `cargo test --features llvm --test mir_asan`.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential pkg-config libssl-dev libzstd-dev zlib1g-dev libxml2-dev \
        curl ca-certificates git gnupg \
    && install -d -m 0755 /etc/apt/keyrings \
    && curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key -o /etc/apt/keyrings/apt.llvm.org.asc \
    && echo "deb [signed-by=/etc/apt/keyrings/apt.llvm.org.asc] http://apt.llvm.org/noble/ llvm-toolchain-noble-22 main" \
        > /etc/apt/sources.list.d/llvm-22.list \
    && apt-get update && apt-get install -y --no-install-recommends \
        llvm-22 llvm-22-dev clang-22 lld-22 \
    && rm -rf /var/lib/apt/lists/*

# CI sets these from `llvm-config-22 --prefix` / `--libdir`.
ENV LLVM_SYS_221_PREFIX=/usr/lib/llvm-22
ENV LD_LIBRARY_PATH=/usr/lib/llvm-22/lib

# Rust stable — matches CI's dtolnay/rust-toolchain@stable.
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/usr/lib/llvm-22/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
        --default-toolchain stable --profile minimal \
    && rustc --version && cargo --version

WORKDIR /work
