# Rewrite the Amsterdam Vessel Map

Three things are wrong with the current app: vessel markers do not line up with the
map underneath them, the UI is cluttered with detail nobody asked for, and the map
cannot be zoomed or panned. All three have the same root cause, so please read the
diagnosis before changing anything.

## Diagnosis: the map is hand-rolled, and that is the bug

The current build does not use a mapping library. It draws its own map out of raw
OpenStreetMap tile images in a CSS grid, and positions vessel markers with its own
arithmetic. There are two coordinate systems in the code and they do not agree.

**System 1 — choosing which tiles to fetch.** This part is correct. It uses proper
Web Mercator:

```js
function eE(lat, lon, z) {
  const n = 2 ** z;
  const x = Math.floor((lon + 180) / 360 * n);
  const y = Math.floor((1 - Math.asinh(Math.tan(lat * Math.PI / 180)) / Math.PI) / 2 * n);
  return { x, y };
}
```

**System 2 — positioning the vessel markers.** This part is wrong. It is a linear
transform with two hardcoded degree spans:

```js
const xa = { latitude: 52.3728, longitude: 4.9001 };
function vc(lat, lon) {
  return {
    x: 50 + (lon - xa.longitude) / 0.12  * 100,
    y: 50 - (lat - xa.latitude)  / 0.065 * 100
  };
}
```

The tiles are then rendered as a 4x4 grid at zoom 13 with a hand-tuned CSS fudge
factor, `scale-[1.16]`, trying to force the two systems to agree.

They cannot agree, and here is the arithmetic:

| | longitude span | latitude span |
|---|---|---|
| What a 4x4 grid of z13 tiles actually covers | 0.17578 deg | 0.10732 deg |
| What `vc()` assumes the viewport covers | 0.12000 deg | 0.06500 deg |
| Error | **+46.5%** | **+65.1%** |

On top of that, the aspect ratio is wrong. At latitude 52.3728, `cos(lat)` is 0.61052,
so an undistorted view needs a longitude-to-latitude span ratio of `1 / 0.61052 =
1.6379`. The code uses `0.12 / 0.065 = 1.8462`, which stretches everything
horizontally by a further 12.7%.

The practical result: **a vessel 1 km east of the centre is drawn roughly 465 m away
from where it actually is**, and the error grows with distance from the centre. The
`scale-[1.16]` makes it look approximately right near the middle of the screen, which
is why it is not obviously broken at first glance.

It also explains the other two complaints. Because the map is a static CSS grid rather
than a map component, there is nothing to implement zoom or pan, and the surrounding
React dashboard grew to fill the space.

**Do not try to fix the constants.** Do not adjust `0.12`, `0.065`, or `scale-[1.16]`.
Delete `vc()`, `eE()`, the tile grid, and the CSS transform entirely, and use a real
mapping library that does the projection for you.

## What to build instead

Use **Leaflet 1.9.4**. It handles Web Mercator, tile loading, zoom, pan, and marker
positioning correctly, and it is what the reference implementation uses. `react-leaflet`
is fine if you prefer idiomatic React, but plain Leaflet in a `useEffect` is simpler and
avoids a wrapper library.

### 1. Map setup

```js
const map = L.map('map').setView([52.38, 4.90], 13);
L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
  maxZoom: 19,
  attribution: '&copy; OpenStreetMap'
}).addTo(map);
```

Leave every interaction at its default. Do **not** pass `zoomControl: false`,
`scrollWheelZoom: false`, `dragging: false`, `touchZoom: false`, or
`doubleClickZoom: false`. Scroll to zoom, drag to pan, and pinch to zoom on touch must
all work. Keep the OpenStreetMap attribution — it is required by the tile usage policy.

The map container needs an explicit height, or it renders 0 pixels tall. Full viewport:

```css
#map { width: 100vw; height: 100vh; }
body { margin: 0; }
```

If the map is inside a flex or grid parent that sizes after mount, call
`map.invalidateSize()` once after layout settles.

### 2. Data source

Connect to the WebSocket and nothing else:

```js
const ws = new WebSocket('wss://alnitak.tail36e7a.ts.net/ws');
```

Four constraints, all of which will silently break the app if ignored:

- **Only `/ws` exists publicly.** There is no REST API reachable from the internet.
  Requests to `/v1/vessels`, `/healthz`, or any other path return 404 from the tunnel.
  Do not add a fetch-then-subscribe pattern; there is nothing to fetch.
- **No initial fetch is needed.** The server replays its entire current vessel table
  immediately on connect, then streams live updates. Just render what arrives.
- **The origin is enforced.** The server only accepts WebSocket upgrades from
  `https://amsterdam-vessel-map--bcleenders.replit.app`. Any other origin gets a 403
  at the upgrade. Do not change the deployment domain, and do not proxy the socket
  through a server-side route, or the origin will no longer match.
- **Reconnect on close**, or the map dies permanently after any network blip:

```js
ws.onclose = () => setTimeout(connectWs, 2000);
```

### 3. Message format

One JSON object per message, one AIS position report:

```json
{
  "timestamp": "2026-08-29 20:09:02",
  "channel": "B",
  "mmsi": 244660510,
  "lat": 52.40025,
  "lon": 4.891535,
  "speed_kn": 0.1,
  "course_deg": 282.5,
  "heading": 281,
  "nav_status": "UnderWayUsingEngine",
  "ship_name": "EXAMPLE",
  "signal_level": 7.06
}
```

**Optional fields are omitted entirely, not sent as null.** Only `timestamp`,
`channel`, `mmsi` and `signal_level` are always present. `lat`, `lon`, `speed_kn`,
`course_deg`, `heading`, `nav_status` and `ship_name` may each be absent, because AIS
message types carry different fields. Guard every one of them.

Skip any message without both `lat` and `lon` — but still count it as received.
`mmsi` is the unique vessel key. `nav_status` is a free-form string such as
`UnderWayUsingEngine`, `Moored`, `AtAnchor`, `Undefined`.

### 4. Markers

Use `L.circleMarker`, which takes real `[lat, lon]` coordinates and lets Leaflet do the
projection. Update existing markers in place rather than removing and re-adding, so
they do not flicker on every update:

```js
const ships = new Map();  // mmsi -> { marker, lastSeen }

function shipColor(status) {
  if (!status) return '#2196F3';
  if (status.includes('Moored')) return '#9E9E9E';
  if (status.includes('Anchor')) return '#FF9800';
  if (status.includes('Sailing')) return '#4CAF50';
  return '#2196F3';
}

const existing = ships.get(d.mmsi);
if (existing) {
  existing.marker.setLatLng([d.lat, d.lon]);
  existing.marker.setStyle({ color: shipColor(d.nav_status), fillColor: shipColor(d.nav_status) });
  existing.marker.setTooltipContent(tooltip(d));
  existing.lastSeen = Date.now();
} else {
  const marker = L.circleMarker([d.lat, d.lon], {
    radius: 5, weight: 1,
    color: shipColor(d.nav_status),
    fillColor: shipColor(d.nav_status),
    fillOpacity: 0.8
  }).addTo(map);
  marker.bindTooltip(tooltip(d));
  ships.set(d.mmsi, { marker, lastSeen: Date.now() });
}
```

Tooltip on hover, built only from fields that are present:

```js
function tooltip(d) {
  let t = 'MMSI: ' + d.mmsi;
  if (d.ship_name)         t += '\n' + d.ship_name;
  if (d.speed_kn != null)  t += '\nSpeed: '  + d.speed_kn.toFixed(1) + ' kn';
  if (d.course_deg != null)t += '\nCourse: ' + d.course_deg.toFixed(0) + '°';
  if (d.nav_status)        t += '\n' + d.nav_status;
  return t;
}
```

### 5. Drop stale vessels

A ship that stops transmitting should disappear rather than sit frozen on the map
forever. Every 30 seconds, remove anything not heard from in 5 minutes:

```js
setInterval(() => {
  const cutoff = Date.now() - 5 * 60 * 1000;
  for (const [mmsi, s] of ships) {
    if (s.lastSeen < cutoff) { s.marker.remove(); ships.delete(mmsi); }
  }
}, 30000);
```

### 6. Fit the view once

Once 10 vessels are known, fit the map to them once, then never move the camera again —
so it does not fight the user while they are panning:

```js
if (!hasFitted && ships.size >= 10) {
  hasFitted = true;
  map.fitBounds(L.latLngBounds([...ships.values()].map(s => s.marker.getLatLng())).pad(0.1));
}
```

### 7. Strip the UI back

Remove the top bar and the dashboard chrome entirely. The map should be the whole page.
Specifically, delete:

- the top navigation / header bar
- the vessel detail side panel and its "Close vessel details" control
- the "Activity" / "AIS packets" cards and any Card, CardHeader, CardContent,
  CardTitle, CardDescription or CardFooter wrappers around map content
- any statistics beyond the two below
- the Inter font preconnect and stylesheet, which are not needed

Keep exactly one small overlay, top-right, showing two numbers: how many ships are
currently tracked, and how many messages have arrived since page load.

```html
<div id="stats">Ships: <b id="ship-count">0</b> | Messages: <b id="msg-count">0</b></div>
```

```css
#stats {
  position: absolute; top: 10px; right: 10px; z-index: 1000;
  background: rgba(255,255,255,0.9); padding: 8px 14px;
  border-radius: 6px; font: 13px/1.5 system-ui, sans-serif;
  box-shadow: 0 1px 4px rgba(0,0,0,0.3);
}
#stats b { font-variant-numeric: tabular-nums; }
```

`z-index: 1000` matters — Leaflet's own panes sit below that, so a lower value puts the
overlay behind the map.

Vessel identity belongs in the hover tooltip, not in a panel. Do not add a click-to-open
detail view, a vessel list, a search box, filters, or a legend.

## Definition of done

- Scroll wheel zooms, drag pans, pinch-zoom works on touch.
- Markers stay locked to the correct geographic position at every zoom level. Verify by
  zooming to a moored vessel and confirming it sits on a quay, not in open water.
- No hardcoded degree spans, no `scale-[...]` on the map, no manual percentage
  positioning of markers anywhere in the codebase.
- The page is a full-viewport map with one small stats overlay and nothing else.
- Ships appear within a second of load, because the server replays its table on connect.
- Killing the network and restoring it reconnects automatically within a few seconds.
