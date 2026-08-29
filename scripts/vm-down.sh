#!/usr/bin/env bash
#
# Stop the sandboxed AIS API and remove its runtime state.
#
# Safe to run at any time, including after a partial or failed startup.
# Does not touch Tailscale Funnel — use scripts/funnel-down.sh for that.

source "$(dirname "${BASH_SOURCE[0]}")/config.sh"

# Stop a process by PID file: SIGTERM, then SIGKILL if it does not go quietly.
stop() {
    local name="$1" pid_file="$2"

    if ! pid_alive "$pid_file"; then
        rm -f "$pid_file"
        return
    fi

    local pid
    pid="$(cat "$pid_file")"
    log "Stopping $name (pid $pid)"
    kill "$pid" 2>/dev/null || true

    for _ in $(seq 1 30); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.1
    done

    if kill -0 "$pid" 2>/dev/null; then
        warn "$name did not stop, sending SIGKILL"
        kill -9 "$pid" 2>/dev/null || true
    fi

    rm -f "$pid_file"
}

# Proxy first so no new requests reach a VM that is going away, then the VM,
# then release the dongle.
stop "proxy"       "$PROXY_PID_FILE"
stop "microVM"     "$FC_PID_FILE"
stop "I/Q tap"     "$TAP_PID_FILE"

# The guest was the only thing holding these; Firecracker does not clean up
# its vsock sockets on exit.
rm -f "$VSOCK_UDS" "${VSOCK_UDS}_${IQ_PORT}" "$RUN_DIR/vm-config.json"

# Any leftover backchannel sockets Firecracker created for other guest ports.
rm -f "${VSOCK_UDS}"_* 2>/dev/null || true

log "Down."

if command -v tailscale >/dev/null && tailscale funnel status 2>/dev/null | grep -q "$LOCAL_HTTP_PORT"; then
    warn "Tailscale Funnel is still advertising port $LOCAL_HTTP_PORT."
    warn "It will now return errors. Run ./scripts/funnel-down.sh to withdraw it."
fi
