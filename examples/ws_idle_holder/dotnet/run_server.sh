#!/usr/bin/env bash
# Foreground launcher for the bench harness's --server-bin contract.
#
# `exec` replaces this shell with the self-contained .NET binary, so the PID
# the harness spawned IS the process measured by `ps -o rss=` (no wrapper
# indirection). The binary prints `BOUND_PORT=<n>` on stdout once Kestrel
# binds; the harness reads it. cert.pem/key.pem sit next to the binary
# (resolved via AppContext.BaseDirectory).
#
# Build the self-contained publish first (no .NET needed at runtime):
#   dotnet publish -c Release -r <rid> --self-contained -o publish
# where <rid> is linux-arm64 (rig), linux-x64, or osx-arm64 (local smoke).
set -euo pipefail
cd "$(dirname "$0")"

BIN=publish/ws-idle-holder-dotnet
if [[ ! -x "$BIN" ]]; then
  echo "$0: $BIN not built — run: dotnet publish -c Release -r <rid> --self-contained -o publish" >&2
  exit 1
fi

# DOTNET_gcServer is already set via the csproj; allow override (e.g.
# DOTNET_GCHeapHardLimit) through the environment for heap-dial experiments,
# the .NET analog of the JVM -Xmx dial documented in the Netty comparator.
exec "$BIN"
