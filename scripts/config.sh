#!/usr/bin/env bash
# Shared configuration, sourced by every other script.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# --- Runtime state -----------------------------------------------------------
# Sockets and PID files live on a tmpfs owned by us, mode 0700. Nothing here
# survives a reboot, which is what we want.
RUN_DIR="${SDR_AIS_RUN_DIR:-/run/user/$(id -u)/sdr-ais}"

# Firecracker's vsock Unix socket. Two derived paths matter:
#   $VSOCK_UDS            host connects here, then says CONNECT <guest port>
#   ${VSOCK_UDS}_$IQ_PORT firecracker connects here when the guest dials out
VSOCK_UDS="$RUN_DIR/vsock.sock"

FC_PID_FILE="$RUN_DIR/firecracker.pid"
TAP_PID_FILE="$RUN_DIR/tap.pid"
PROXY_PID_FILE="$RUN_DIR/proxy.pid"
FC_LOG="$RUN_DIR/firecracker.log"
TAP_LOG="$RUN_DIR/tap.log"
PROXY_LOG="$RUN_DIR/proxy.log"

# --- vsock ports -------------------------------------------------------------
# Guest dials out to the host on this port to receive raw I/Q.
IQ_PORT="${SDR_AIS_IQ_PORT:-5000}"
# Guest listens on this port for HTTP.
GUEST_HTTP_PORT="${SDR_AIS_GUEST_HTTP_PORT:-8000}"
# Loopback port the proxy exposes, and what Tailscale Funnel forwards to.
LOCAL_HTTP_PORT="${SDR_AIS_LOCAL_HTTP_PORT:-8080}"

# --- microVM -----------------------------------------------------------------
GUEST_CID="${SDR_AIS_GUEST_CID:-3}"
# One vCPU carries the DSP, one the HTTP side.
VCPUS="${SDR_AIS_VCPUS:-2}"
MEM_MIB="${SDR_AIS_MEM_MIB:-256}"

VM_DIR="$REPO_ROOT/vm"
KERNEL="${SDR_AIS_KERNEL:-$VM_DIR/vmlinux}"
ROOTFS="${SDR_AIS_ROOTFS:-$VM_DIR/rootfs.ext4}"

GUEST_TARGET="x86_64-unknown-linux-musl"
GUEST_BIN="$REPO_ROOT/target/$GUEST_TARGET/release/sdr-ais-guest"
HOST_BIN="$REPO_ROOT/target/release/sdr-ais-host"

# --- Public exposure ---------------------------------------------------------
# The only browser origin allowed to read the API. Passed to the guest on the
# kernel command line, so changing it needs only a VM restart, not a rebuild.
ALLOWED_ORIGIN="${SDR_AIS_ALLOWED_ORIGIN:-https://amsterdam-vessel-map--bcleenders.replit.app}"

# Funnel can only listen on 443, 8443 or 10000.
FUNNEL_PORT="${SDR_AIS_FUNNEL_PORT:-443}"

# --- Radio -------------------------------------------------------------------
SDR_DEVICE="${SDR_AIS_DEVICE:-0}"
SDR_GAIN="${SDR_AIS_GAIN:-}"          # empty = 49.6 dB
SDR_BIAS_TEE="${SDR_AIS_BIAS_TEE:-0}" # 1 to power an active antenna

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m warn:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# True if the PID in $1 names a live process.
pid_alive() {
    local pid_file="$1"
    [[ -f "$pid_file" ]] || return 1
    local pid
    pid="$(cat "$pid_file" 2>/dev/null)" || return 1
    [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}
