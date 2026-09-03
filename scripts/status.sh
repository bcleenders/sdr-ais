#!/usr/bin/env bash
#
# Show what is running, whether it is healthy, and where the logs are.
#
#   ./scripts/status.sh        one-shot summary
#   ./scripts/status.sh -f     then follow the guest console
#
# Needs no privileges.

source "$(dirname "${BASH_SOURCE[0]}")/config.sh"

ok()   { printf '  \033[32m*\033[0m %s\n' "$*"; }
bad()  { printf '  \033[31mx\033[0m %s\n' "$*"; }

echo "processes"
for spec in "I/Q tap:$TAP_PID_FILE" "microVM:$FC_PID_FILE" "proxy:$PROXY_PID_FILE" "logger:$LOGGER_PID_FILE"; do
    name="${spec%%:*}"; file="${spec#*:}"
    if pid_alive "$file"; then ok "$name (pid $(cat "$file"))"; else bad "$name not running"; fi
done

echo
echo "radio"
if rtlsdr_present; then
    # Which driver holds interface .0 decides whether we can stream: "usbfs"
    # means our tap has claimed it, a dvb_* name means the kernel took it back.
    driver=""
    for dev in /sys/bus/usb/devices/*; do
        [[ -r "$dev/idVendor" ]] || continue
        [[ "$(cat "$dev/idVendor")" == "0bda" && "$(cat "$dev/idProduct" 2>/dev/null)" == "2838" ]] || continue
        if [[ -e "$dev:1.0/driver" ]]; then
            driver="$(basename "$(readlink "$dev:1.0/driver")")"
        else
            driver="(none)"
        fi
        ok "RTL-SDR at $(basename "$dev"), interface driver: $driver"
    done
    [[ "$driver" == dvb* ]] && bad "the DVB driver has it; run sudo ./scripts/setup-host.sh"
else
    bad "no RTL-SDR (0bda:2838) on USB"
fi

if [[ -f "$TAP_LOG" ]] && grep -q "dropped" "$TAP_LOG"; then
    bad "$(grep -c dropped "$TAP_LOG") dropped-chunk warnings — see $TAP_LOG"
fi

echo
echo "api (loopback)"
if curl -fsS --max-time 3 "http://127.0.0.1:$LOCAL_HTTP_PORT/healthz" >/dev/null 2>&1; then
    ok "healthz"
    snapshot="$(curl -fsS --max-time 3 "http://127.0.0.1:$LOCAL_HTTP_PORT/v1/vessels" 2>/dev/null)"
    if command -v jq >/dev/null && [[ -n "$snapshot" ]]; then
        count="$(jq length <<<"$snapshot")"
        newest="$(jq -r '[.[].timestamp] | max // "never"' <<<"$snapshot")"
        ok "$count vessels, newest report $newest UTC (now $(date -u '+%Y-%m-%d %H:%M:%S'))"
    fi
else
    bad "no response on 127.0.0.1:$LOCAL_HTTP_PORT"
fi

echo
echo "public exposure"
if command -v tailscale >/dev/null; then
    handlers="$(tailscale serve status --json 2>/dev/null | jq -r '.Web // {} | to_entries[] | .value.Handlers | keys[]' 2>/dev/null)"
    if [[ -z "$handlers" ]]; then
        ok "nothing published (funnel off)"
    else
        while read -r path; do
            [[ "$path" == "/ws" ]] && ok "wss://$(tailscale status --json | jq -r '.Self.DNSName' | sed 's/\.$//')$path" \
                                   || bad "$path is public — expected only /ws"
        done <<<"$handlers"
    fi
fi

echo
echo "origin policy"
if [[ -f "$RUN_DIR/vm-config.json" ]] && grep -q 'sdr\.origin=[^ ]*\*' "$RUN_DIR/vm-config.json"; then
    bad "origin checking DISABLED — any website can read the stream"
    bad "restart without SDR_AIS_EXTRA_ORIGINS to restore it"
elif [[ -f "$RUN_DIR/vm-config.json" ]]; then
    ok "$(sed -n 's/.*sdr\.origin=\([^ ]*\).*/\1/p' "$RUN_DIR/vm-config.json")"
fi

echo
echo "logs  ($RUN_DIR, cleared on host reboot)"
for spec in "guest console:$FC_LOG" "tap:$TAP_LOG" "proxy:$PROXY_LOG" "logger:$LOGGER_LOG"; do
    name="${spec%%:*}"; file="${spec#*:}"
    printf '  %-14s %s\n' "$name" "$file"
done

if [[ "${1:-}" == "-f" ]]; then
    echo
    echo "following guest console (ctrl-c to stop)"
    tail -f "$FC_LOG"
fi
