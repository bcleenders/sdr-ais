#!/usr/bin/env bash
#
# Start the sandboxed AIS API. Needs no privileges.
#
# Brings up three processes, in this order:
#
#   1. tap         owns the RTL-SDR, waits on ${VSOCK_UDS}_${IQ_PORT}
#   2. firecracker boots the microVM; the guest dials out for I/Q
#   3. proxy       exposes the guest's HTTP on 127.0.0.1:${LOCAL_HTTP_PORT}
#
# The tap must be listening before the VM boots, otherwise the guest's first
# I/Q connection attempt fails (it retries, but starting in order is tidier).
#
# This script does NOT touch Tailscale. Run scripts/funnel-up.sh separately to
# make the API reachable from the internet — going public should be a
# deliberate, separate act.

source "$(dirname "${BASH_SOURCE[0]}")/config.sh"

# --- Preflight ---------------------------------------------------------------
command -v firecracker >/dev/null || die "firecracker not found in PATH"
[[ -r /dev/kvm && -w /dev/kvm ]] || die "no read/write access to /dev/kvm (are you in the kvm group?)"
[[ -f "$KERNEL" ]] || die "kernel missing: $KERNEL — run scripts/build.sh"
[[ -f "$ROOTFS" ]] || die "rootfs missing: $ROOTFS — run scripts/build.sh"
[[ -x "$HOST_BIN" ]] || die "host binary missing: $HOST_BIN — run scripts/build.sh"

if module_loaded dvb_usb_rtl28xxu; then
    # Not fatal: rs-rtl issues USBDEVFS_DISCONNECT before claiming the
    # interface, so it takes the dongle off the DVB driver by itself.
    # The blacklist still matters, to stop it rebinding on replug.
    warn "dvb_usb_rtl28xxu is loaded; the tap will detach it from the dongle"
    warn "run: sudo ./scripts/setup-host.sh   (stops it rebinding on replug)"
fi

if rtlsdr_present; then
    log "RTL-SDR detected"
else
    warn "no RTL-SDR (0bda:2838) found on USB — the API will serve an empty vessel list"
fi

if pid_alive "$FC_PID_FILE"; then
    die "a microVM is already running (pid $(cat "$FC_PID_FILE")) — run scripts/vm-down.sh first"
fi

mkdir -p "$RUN_DIR"
chmod 700 "$RUN_DIR"
rm -f "$VSOCK_UDS" "${VSOCK_UDS}_${IQ_PORT}"

# --- 1. I/Q tap --------------------------------------------------------------
log "Starting I/Q tap (USB -> vsock port $IQ_PORT)"
tap_args=(tap --uds-path "$VSOCK_UDS" --port "$IQ_PORT" --device "$SDR_DEVICE")
[[ -n "$SDR_GAIN" ]] && tap_args+=(--gain "$SDR_GAIN")
[[ "$SDR_BIAS_TEE" == "1" ]] && tap_args+=(--bias-tee)

"$HOST_BIN" "${tap_args[@]}" >"$TAP_LOG" 2>&1 &
echo $! > "$TAP_PID_FILE"

# Wait for the listening socket to appear so the guest's first dial succeeds.
for _ in $(seq 1 50); do
    [[ -S "${VSOCK_UDS}_${IQ_PORT}" ]] && break
    sleep 0.1
done
[[ -S "${VSOCK_UDS}_${IQ_PORT}" ]] || {
    warn "tap socket did not appear; last output:"
    tail -5 "$TAP_LOG" >&2
    die "tap failed to start"
}

# --- 2. microVM --------------------------------------------------------------
# Note what is absent: there is no "network-interfaces" key, so the guest has
# no NIC of any kind. vsock is its only channel to the outside world.
#
# root= and ro are deliberately NOT in boot_args: Firecracker appends them
# itself based on is_root_device / is_read_only, and would duplicate them.
CONFIG_JSON="$RUN_DIR/vm-config.json"
cat > "$CONFIG_JSON" <<EOF
{
  "boot-source": {
    "kernel_image_path": "$KERNEL",
    "boot_args": "console=ttyS0 reboot=k panic=1 pci=off i8042.noaux i8042.nomux i8042.nopnp i8042.dumbkbd init=/init sdr.origin=$ALLOWED_ORIGINS sdr.iq_port=$IQ_PORT sdr.http_port=$GUEST_HTTP_PORT"
  },
  "drives": [
    {
      "drive_id": "rootfs",
      "path_on_host": "$ROOTFS",
      "is_root_device": true,
      "is_read_only": true
    }
  ],
  "machine-config": {
    "vcpu_count": $VCPUS,
    "mem_size_mib": $MEM_MIB,
    "smt": false
  },
  "vsock": {
    "guest_cid": $GUEST_CID,
    "uds_path": "$VSOCK_UDS"
  }
}
EOF
chmod 600 "$CONFIG_JSON"

log "Booting microVM (${VCPUS} vCPU, ${MEM_MIB} MiB, no network device)"
# --no-api removes the control-plane HTTP socket entirely. Nothing can
# reconfigure this VM once it is running; teardown is a signal.
firecracker --no-api --config-file "$CONFIG_JSON" >"$FC_LOG" 2>&1 &
echo $! > "$FC_PID_FILE"

sleep 0.5
pid_alive "$FC_PID_FILE" || {
    warn "firecracker exited immediately:"
    tail -20 "$FC_LOG" >&2
    die "microVM failed to boot"
}

# --- 3. HTTP proxy -----------------------------------------------------------
log "Starting proxy (127.0.0.1:$LOCAL_HTTP_PORT -> guest vsock $GUEST_HTTP_PORT)"
"$HOST_BIN" proxy \
    --uds-path "$VSOCK_UDS" \
    --guest-port "$GUEST_HTTP_PORT" \
    --listen "127.0.0.1:$LOCAL_HTTP_PORT" >"$PROXY_LOG" 2>&1 &
echo $! > "$PROXY_PID_FILE"

# --- Readiness ---------------------------------------------------------------
log "Waiting for the guest API"
ready=0
for _ in $(seq 1 100); do
    if curl -fsS --max-time 2 "http://127.0.0.1:$LOCAL_HTTP_PORT/healthz" >/dev/null 2>&1; then
        ready=1
        break
    fi
    pid_alive "$FC_PID_FILE" || { warn "microVM died during boot:"; tail -20 "$FC_LOG" >&2; die "boot failed"; }
    sleep 0.2
done

if [[ $ready -ne 1 ]]; then
    warn "guest did not answer /healthz in 20s"
    warn "--- guest console ---"; tail -20 "$FC_LOG" >&2
    warn "--- proxy ---";         tail -10 "$PROXY_LOG" >&2
    die "startup failed (run scripts/vm-down.sh to clean up)"
fi

log "Up."
cat <<EOF

  local API      http://127.0.0.1:$LOCAL_HTTP_PORT
    /healthz         liveness
    /v1/vessels      current vessel table (JSON)
    /ws              live position stream (WebSocket)

  allowed origins $ALLOWED_ORIGINS
  logs           $RUN_DIR/{firecracker,tap,proxy}.log

  expose publicly  ./scripts/funnel-up.sh
  tear down        ./scripts/vm-down.sh
EOF
