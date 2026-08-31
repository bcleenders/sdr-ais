#!/usr/bin/env bash
# Shared configuration, sourced by every other script.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Per-host overrides, not tracked in git. Use conditional assignment inside it
# (`: "${VAR:=value}"`) so a variable set on the command line still wins.
# This is how a setting survives a restart — anything passed inline is lost the
# next time someone runs vm-up.sh without it.
if [[ -f "$REPO_ROOT/scripts/local.env" ]]; then
    source "$REPO_ROOT/scripts/local.env"
fi

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

# Additional browser origins permitted to read the stream, comma-separated.
# Empty in production. To run the bundled map locally against the live API:
#
#     SDR_AIS_EXTRA_ORIGINS=http://localhost:8000 ./scripts/vm-up.sh
#     ./scripts/serve-ui.sh
#
# Widening this is low risk — the Origin check only stops *other websites*
# reading the feed in a browser, any non-browser client can send whatever
# Origin it likes, and the data is public vessel positions. It is opt-in so
# that production does not quietly carry a development origin.
# Set to "*" to turn origin checking off entirely while developing:
#     SDR_AIS_EXTRA_ORIGINS='*' ./scripts/vm-up.sh
# vm-up.sh and status.sh both flag this loudly, because on a funnelled host it
# lets any website read the stream from a visitor's browser.
EXTRA_ORIGINS="${SDR_AIS_EXTRA_ORIGINS:-}"
ALLOWED_ORIGINS="$ALLOWED_ORIGIN${EXTRA_ORIGINS:+,$EXTRA_ORIGINS}"

# Port serve-ui.sh listens on. Must match an entry in EXTRA_ORIGINS, or the
# guest will refuse the upgrade with a 403.
UI_PORT="${SDR_AIS_UI_PORT:-8000}"

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

# ssh non-login shells often omit sbin, which is where lsmod/modprobe live.
case ":$PATH:" in
    *:/usr/sbin:*) ;;
    *) PATH="$PATH:/usr/sbin:/sbin" ;;
esac

# True if a kernel module is loaded. Reads /proc/modules directly rather than
# shelling out to lsmod, which is not always on PATH.
module_loaded() {
    grep -q "^$1 " /proc/modules 2>/dev/null
}

# True if the RTL-SDR (0bda:2838) is attached. Walks sysfs so this does not
# depend on usbutils being installed.
rtlsdr_present() {
    local dev
    for dev in /sys/bus/usb/devices/*; do
        [[ -r "$dev/idVendor" && -r "$dev/idProduct" ]] || continue
        if [[ "$(cat "$dev/idVendor")" == "0bda" && "$(cat "$dev/idProduct")" == "2838" ]]; then
            return 0
        fi
    done
    return 1
}
