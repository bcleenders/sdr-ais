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

# Capture output so a failure can be diagnosed rather than guessed at. The two
# realistic causes need completely different fixes.
# Mount ONLY /ws. Anything else 404s at the Tailscale layer and never
# reaches the microVM: /healthz and /v1/vessels stay loopback-only.
# The target carries the /ws path because serve strips the mount prefix
# before proxying, so a bare :8080 target would arrive as /.
if ! output="$(tailscale funnel --bg --https="$FUNNEL_PORT" --set-path=/ws \
        "http://127.0.0.1:$LOCAL_HTTP_PORT/ws" 2>&1)"; then
    printf '%s\n' "$output" >&2
    echo >&2

    if grep -qiE "access denied|operator|permission" <<<"$output"; then
        die "the tailscale CLI will not change serve config as a non-root user.

Register yourself as the operator once, then re-run this script:

    sudo tailscale set --operator=\$USER

scripts/setup-host.sh does this for you."
    fi

    if grep -qiE "funnel|attr|not allowed" <<<"$output"; then
        die "this node is not permitted to use Funnel.

In the Tailscale admin console:
  * DNS             -> enable MagicDNS and HTTPS Certificates
  * Access controls -> grant the funnel attribute. The target must be a tag,
                       a user, a group or *, NOT a MagicDNS name:

      \"nodeAttrs\": [
        { \"target\": [\"$(tailscale status --json 2>/dev/null \
            | sed -n 's/.*\"LoginName\": *\"\([^\"]*\)\".*/\1/p' | head -1)\"],
          \"attr\": [\"funnel\"] }
      ]"
    fi

    die "funnel failed to start; see the output above"
fi

log "Public."
cat <<EOF

  public   wss://$NODE_DNS/ws     <- the only thing exposed

  Everything else 404s at the Tailscale layer and never reaches the VM.
  These stay reachable on alnitak's loopback only, for debugging:
    curl http://127.0.0.1:$LOCAL_HTTP_PORT/healthz
    curl http://127.0.0.1:$LOCAL_HTTP_PORT/v1/vessels

  Point the frontend at that hostname. See README.md for why
  sdr-ais.alnitak.cxcc.nl cannot be served directly by Funnel.

  withdraw     ./scripts/funnel-down.sh
EOF
