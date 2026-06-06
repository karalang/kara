#!/usr/bin/env bash
# Foreground Phoenix launcher for the bench harness's --server-bin contract.
#
# `exec` replaces this shell with the BEAM, so the PID the harness spawned
# IS the beam.smp PID — `ps -o rss=` on it measures the VM node directly
# (no wrapper/child indirection). The app prints `BOUND_PORT=<n>` on stdout
# once the endpoint is up; the harness reads that to find the port.
#
# Pre-compile first (`mix deps.get && mix compile`) so `mix run` starts
# fast enough to print BOUND_PORT inside the harness's 15s window.
#
# PRESENCE=off  -> presence-disabled (sidebar) run; default is presence-on.
set -euo pipefail
cd "$(dirname "$0")"

# +Q (max ports): one port per TCP connection — the default 65536 caps us
#    well below 250K, so raise it.
# +P (max processes): each connection spawns a transport process + a
#    channel process (+ presence churn), so budget generously.
exec elixir --erl "+Q 2000000 +P 8000000" -S mix run --no-halt
