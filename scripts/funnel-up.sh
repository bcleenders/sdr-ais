#!/usr/bin/env bash
#
# Publish the API to the internet via Tailscale Funnel.
#
# This is separate from vm-up.sh on purpose: starting the VM is local and
# reversible, whereas this makes the service reachable by anyone.
#
# Funnel terminates TLS using a Let's Encrypt certificate Tailscale provisions
# for your node's MagicDNS name, and forwards plaintext HTTP to loopback. It can
# only serve *.ts.net names, and only on ports 443, 8443 and 10000.

source "$(dirname "${BASH_SOURCE[0]}")/config.sh"

command -v tailscale >/dev/null || die "tailscale not found in PATH"

# Refuse to advertise a port that nothing is serving — that would publish a
# hostname that only returns errors.
if ! curl -fsS --max-time 2 "http://127.0.0.1:$LOCAL_HTTP_PORT/healthz" >/dev/null 2>&1; then
    die "nothing healthy on 127.0.0.1:$LOCAL_HTTP_PORT — run ./scripts/vm-up.sh first"
fi

NODE_DNS="$(tailscale status --json 2>/dev/null | sed -n 's/.*"DNSName": *"\([^"]*\)".*/\1/p' | head -1)"
NODE_DNS="${NODE_DNS%.}"
[[ -n "$NODE_DNS" ]] || die "could not determine this node's MagicDNS name (is tailscaled up?)"

log "Publishing 127.0.0.1:$LOCAL_HTTP_PORT as https://$NODE_DNS (port $FUNNEL_PORT)"

if ! tailscale funnel --bg --https="$FUNNEL_PORT" "http://127.0.0.1:$LOCAL_HTTP_PORT"; then
    die "funnel failed to start

Most likely one of the prerequisites is missing. In the Tailscale admin console:
  * DNS       -> enable MagicDNS and HTTPS Certificates
  * Access controls -> grant this node the funnel attribute, e.g.

      \"nodeAttrs\": [
        { \"target\": [\"$NODE_DNS\"], \"attr\": [\"funnel\"] }
      ]"
fi

log "Public."
cat <<EOF

  public URL   https://$NODE_DNS
    https://$NODE_DNS/healthz
    https://$NODE_DNS/v1/vessels
    wss://$NODE_DNS/ws

  Point the frontend at that hostname. See README.md for why
  sdr-ais.alnitak.cxcc.nl cannot be served directly by Funnel.

  withdraw     ./scripts/funnel-down.sh
EOF
