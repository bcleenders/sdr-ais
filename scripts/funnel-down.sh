#!/usr/bin/env bash
#
# Withdraw the API from the internet. The VM keeps running and stays reachable
# on loopback; only the public route goes away.

source "$(dirname "${BASH_SOURCE[0]}")/config.sh"

command -v tailscale >/dev/null || die "tailscale not found in PATH"

log "Withdrawing funnel on port $FUNNEL_PORT"
tailscale funnel --https="$FUNNEL_PORT" off || warn "funnel was not running"

log "Current serve/funnel config:"
tailscale funnel status || true
