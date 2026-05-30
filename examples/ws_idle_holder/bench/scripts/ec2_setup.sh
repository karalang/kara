#!/usr/bin/env bash
# EC2 / Linux rig setup for the ws_idle_holder M3 idle-hold bench.
# Idempotent — safe to re-run.
#
# Sized for both the 1M and 2M target runs:
#   - 50 loopback aliases × ~50K ephemeral ports ≈ 2.52M source tuples
#     (24% headroom over 2M; comfortable above 1M too).
#   - nofile cap at 3M so a single process can hold 2M+ idle fds with
#     headroom for the server-side fd-per-conn pair.
#
# Captures the sysctl bumps + loopback alias setup + nofile limit
# discovered during the 2026-05-29 Kāra 1M verification on r8g.4xlarge
# (docs/investigations/demo1_m3_verification.json). Pair with ./run_1m.sh
# or ./run_2m.sh, which pull source-IP + nofile state from this setup.
#
# Usage:
#   sudo bash scripts/ec2_setup.sh
#
# Requires root (sudo). On Ubuntu 24.04 arm64 — the AMI used for the
# Kāra 1M Linux verification — `sudo -i` is the simplest way in. On
# other distros: equivalent privileges to run sysctl + ip addr add +
# write /etc/security/limits.d/.

set -euo pipefail

if [[ "$(uname)" != "Linux" ]]; then
    echo "ec2_setup.sh: Linux only (this is the EC2/headline rig)" >&2
    exit 1
fi

if [[ $EUID -ne 0 ]]; then
    SUDO=sudo
else
    SUDO=
fi

# ── Sysctls ──────────────────────────────────────────────────────────
#
# `somaxconn` + `tcp_max_syn_backlog` at 65535: matches the runtime's
# explicit `listen(65535)` in `karac_runtime_tcp_bind` (and the Rust
# comparator's `socket2::listen(65535)` on Linux); Linux caps `listen(2)`
# at `min(backlog, somaxconn)`, so this lifts the kernel side of the
# pair too. The 2026-05-29 pre-fix verification showed `dmesg` SYN-flood
# warnings without these bumps because the listen queue overflowed at
# ~93K held conns.
#
# `ip_local_port_range="15000 65535"`: ≈50K source ports per source IP,
# vs the stock ~28K. Combined with the 50 loopback aliases below (each
# IP has its own ephemeral pool), client-side capacity is ≈2.52M source
# tuples — comfortably above the M3 2M target (and the 1M target too).
#
# `tcp_rmem` / `tcp_wmem` mins lowered to 4K: the Kāra 1M run held
# 7.62 GB server RSS; without trimming the per-socket buffer floors,
# 1M idle conns drag in ~4 KiB receive + ~4 KiB send buffer each by
# default, inflating server-side memory unnecessarily.
echo "[ec2_setup] applying sysctl bumps..."
$SUDO sysctl -w net.core.somaxconn=65535
$SUDO sysctl -w net.ipv4.tcp_max_syn_backlog=65535
$SUDO sysctl -w net.ipv4.ip_local_port_range="15000 65535"
$SUDO sysctl -w net.ipv4.tcp_rmem="4096 87380 6291456"
$SUDO sysctl -w net.ipv4.tcp_wmem="4096 65536 4194304"

# ── Loopback aliases ─────────────────────────────────────────────────
#
# 127.0.0.2 through 127.0.0.51 — 50 IPs. Each held connection picks
# one round-robin via `--source-ips`, so the bench client doesn't pin
# a single (src_ip, dst_ip, dst_port) tuple and exhaust its ~50K port
# pool. With 50 IPs × 50K ports each, the client side has ≈2.52M
# source tuples — comfortably above the M3 2M target. run_1m.sh only
# consumes 127.0.0.2..28 (27 IPs) for its smaller workload; aliases
# 29..51 sit idle on a 1M run.
echo "[ec2_setup] adding loopback aliases 127.0.0.2..51..."
for i in $(seq 2 51); do
    ip="127.0.0.${i}"
    # `ip addr add` returns 2 ("File exists") if already present —
    # silenced to keep this idempotent.
    $SUDO ip addr add "${ip}/8" dev lo 2>/dev/null || true
done

# ── nofile limit ─────────────────────────────────────────────────────
#
# Hard + soft nofile at 3M so a single process can hold 2M+ idle fds
# with headroom for the server-side pair. /etc/security/limits.d/
# only applies on next login, so run_1m.sh / run_2m.sh also call
# `ulimit -n` inline as a safety net for the current shell.
echo "[ec2_setup] writing /etc/security/limits.d/bench.conf..."
$SUDO tee /etc/security/limits.d/bench.conf >/dev/null <<EOF
*    soft nofile 3000000
*    hard nofile 3000000
root soft nofile 3000000
root hard nofile 3000000
EOF

# ── Verification ─────────────────────────────────────────────────────
echo
echo "[ec2_setup] current state:"
echo "  somaxconn          = $(sysctl -n net.core.somaxconn)"
echo "  tcp_max_syn_backlog= $(sysctl -n net.ipv4.tcp_max_syn_backlog)"
echo "  ip_local_port_range= $(sysctl -n net.ipv4.ip_local_port_range)"
echo "  loopback alias cnt = $(ip addr show lo | grep -c 'inet 127\.0\.0\.')"
echo "  ulimit -n (current)= $(ulimit -n)"
echo
echo "[ec2_setup] DONE. ulimit changes from limits.d/ apply on next"
echo "  login; run_1m.sh / run_2m.sh set ulimit -n inline so it works"
echo "  in-shell too."
