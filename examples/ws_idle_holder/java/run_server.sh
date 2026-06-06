#!/usr/bin/env bash
# Foreground JVM launcher for the bench harness's --server-bin contract.
#
# `exec` replaces this shell with the JVM, so the PID the harness spawned IS
# the java PID — `ps -o rss=` measures the JVM (heap + Netty direct buffers +
# metaspace + thread stacks) directly, no wrapper indirection. The server
# prints `BOUND_PORT=<n>` on stdout once bound; the harness reads it.
#
# GC config is a pure runtime flag (no rebuild) via JAVA_OPTS:
#   (unset)                  -> G1GC  (JDK 21 default — the broad prod default)
#   JAVA_OPTS="-XX:+UseZGC -XX:+ZGenerational"  -> generational ZGC sidebar
#
# Heap/direct sizing below is box-sizing, not GC tuning: -Xms stays small so
# committed heap (and thus RSS) tracks actual live set; the ceilings just
# prevent an artificial OOM at 250K. Per-conn-bytes = RSS-delta / N subtracts
# the JVM baseline measured before any connection, so the ceilings don't
# inflate the density number.
set -euo pipefail
cd "$(dirname "$0")"

JAR=target/ws-idle-holder-netty.jar
if [[ ! -f "$JAR" ]]; then
  echo "$0: $JAR not built — run: mvn -q package" >&2
  exit 1
fi

exec java \
  -Xms256m -Xmx24g -XX:MaxDirectMemorySize=48g \
  ${JAVA_OPTS:-} \
  -jar "$JAR"
