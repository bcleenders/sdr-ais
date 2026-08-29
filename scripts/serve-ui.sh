#!/usr/bin/env bash
#
# Serve the Leaflet map locally, pointed at the live API on alnitak.
#
# The page hardcodes wss://alnitak.tail36e7a.ts.net/ws, so this needs no
# arguments — but the guest must be allowing this origin, which is opt-in:
#
#     SDR_AIS_EXTRA_ORIGINS=http://localhost:8000 ./scripts/vm-up.sh   (on alnitak)
#     ./scripts/serve-ui.sh                                            (anywhere)
#
# Without that the WebSocket upgrade is refused with a 403 and the map stays
# empty. Runs on loopback only; this is a development tool, not a deployment.

source "$(dirname "${BASH_SOURCE[0]}")/config.sh"

command -v python3 >/dev/null || die "python3 not found (needed for the static server)"

PAGE="$REPO_ROOT/crates/sdr-ais/src/index.html"
[[ -f "$PAGE" ]] || die "page not found: $PAGE"

# Stage the single file so the server does not also expose the Rust sources
# sitting next to it.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp "$PAGE" "$STAGE/index.html"

api_url="$(sed -n "s/.*const API_URL = '\([^']*\)'.*/\1/p" "$PAGE")"

log "Serving the map on http://localhost:$UI_PORT"
printf '    page talks to : %s\n' "${api_url:-unknown}"
printf '    origin sent   : http://localhost:%s\n' "$UI_PORT"
printf '\n    If the map stays empty, the guest is not allowing that origin.\n'
printf '    Restart the VM on alnitak with:\n'
printf '      SDR_AIS_EXTRA_ORIGINS=http://localhost:%s ./scripts/vm-up.sh\n\n' "$UI_PORT"

cd "$STAGE"
python3 -m http.server "$UI_PORT" --bind 127.0.0.1
