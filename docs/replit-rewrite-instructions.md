# Rewrite the Amsterdam Vessel Map with Leaflet

Markers don't line up with the map, the UI is cluttered, and the map can't be zoomed or
panned. All three have one cause: the app doesn't use a mapping library. It builds its
own map from raw OSM tile images in a CSS grid and positions markers with its own
arithmetic.

**Replace all of it with Leaflet 1.9.4.**

## Why not just fix the constants

There are two coordinate systems and they disagree. Tile selection uses correct Web
Mercator. Marker placement uses a linear transform over hardcoded spans:

```js
const xa = { latitude: 52.3728, longitude: 4.9001 };
x = 50 + (lon - xa.longitude) / 0.12  * 100
y = 50 - (lat - xa.latitude)  / 0.065 * 100
```

But a 4x4 grid of zoom-13 tiles covers 0.17578 deg longitude by 0.10732 deg latitude —
46% and 65% more than those constants assume. A hand-tuned `scale-[1.16]` on the tile
grid papers over it near the centre, which is why it looks almost right. **A vessel 1 km
east of centre renders about 465 m from where it actually is**, and the error grows
outward.

So: don't adjust `0.12`, `0.065`, or `scale-[1.16]`. Delete the tile grid, the CSS
transform, and both projection functions. Leaflet does this correctly.

## Map

```js
const map = L.map('map').setView([52.38, 4.90], 13);
L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
  maxZoom: 19,
  attribution: '&copy; OpenStreetMap'
}).addTo(map);
```

Leave all interaction at defaults — do not pass `zoomControl: false`,
`scrollWheelZoom: false`, `dragging: false`, `touchZoom: false` or
`doubleClickZoom: false`. Keep the attribution; the tile policy requires it.

The container needs an explicit height or it renders 0px tall:

```css
body { margin: 0; }
#map { width: 100vw; height: 100vh; }
```

## Data

```js
const ws = new WebSocket('wss://alnitak.tail36e7a.ts.net/ws');
ws.onclose = () => setTimeout(connectWs, 2000);   // or it dies on any blip
```

Four constraints that will silently break the app if ignored:

- **Only `/ws` is public.** `/v1/vessels` and everything else return 404 from the
  tunnel. There is no REST API to fetch.
- **No initial fetch needed.** The server replays its whole vessel table on connect,
  then streams updates.
- **Optional fields are omitted, not null.** Only `timestamp`, `channel`, `mmsi` and
  `signal_level` always appear. `lat`, `lon`, `speed_kn`, `course_deg`, `heading`,
  `nav_status`, `ship_name` may each be absent — guard every one. Skip messages with no
  `lat`/`lon`, but still count them.
- **Origin is enforced.** Upgrades are only accepted from
  `https://amsterdam-vessel-map--bcleenders.replit.app`. Don't change the deployment
  domain, and don't proxy the socket through a server-side route, or it 403s.

One message per position report:

```json
{"timestamp":"2026-08-29 20:09:02","channel":"B","mmsi":244660510,"lat":52.40025,
 "lon":4.891535,"speed_kn":0.1,"course_deg":282.5,"nav_status":"UnderWayUsingEngine",
 "ship_name":"EXAMPLE","signal_level":7.06}
```

`mmsi` is the vessel key. `nav_status` is free-form: `UnderWayUsingEngine`, `Moored`,
`AtAnchor`, `Undefined`.

## Markers

`L.circleMarker` takes real coordinates and lets Leaflet project them. Update in place
so markers don't flicker:

```js
const ships = new Map();  // mmsi -> { marker, lastSeen }

function shipColor(status) {
  if (!status) return '#2196F3';
  if (status.includes('Moored')) return '#9E9E9E';
  if (status.includes('Anchor')) return '#FF9800';
  if (status.includes('Sailing')) return '#4CAF50';
  return '#2196F3';
}

function tooltip(d) {
  let t = 'MMSI: ' + d.mmsi;
  if (d.ship_name)          t += '\n' + d.ship_name;
  if (d.speed_kn != null)   t += '\nSpeed: '  + d.speed_kn.toFixed(1) + ' kn';
  if (d.course_deg != null) t += '\nCourse: ' + d.course_deg.toFixed(0) + '°';
  if (d.nav_status)         t += '\n' + d.nav_status;
  return t;
}

const s = ships.get(d.mmsi);
if (s) {
  s.marker.setLatLng([d.lat, d.lon]);
  s.marker.setStyle({ color: shipColor(d.nav_status), fillColor: shipColor(d.nav_status) });
  s.marker.setTooltipContent(tooltip(d));
  s.lastSeen = Date.now();
} else {
  const marker = L.circleMarker([d.lat, d.lon], {
    radius: 5, weight: 1,
    color: shipColor(d.nav_status), fillColor: shipColor(d.nav_status), fillOpacity: 0.8
  }).addTo(map).bindTooltip(tooltip(d));
  ships.set(d.mmsi, { marker, lastSeen: Date.now() });
}
```

Drop vessels that stop transmitting, so they don't sit frozen forever:

```js
setInterval(() => {
  const cutoff = Date.now() - 5 * 60 * 1000;
  for (const [mmsi, s] of ships) {
    if (s.lastSeen < cutoff) { s.marker.remove(); ships.delete(mmsi); }
  }
}, 30000);
```

Fit the view once at 10 vessels, then never move the camera again — otherwise it fights
the user while they pan:

```js
if (!hasFitted && ships.size >= 10) {
  hasFitted = true;
  map.fitBounds(L.latLngBounds([...ships.values()].map(s => s.marker.getLatLng())).pad(0.1));
}
```

## UI

The map is the whole page. Delete the top bar, the vessel detail panel and its
"Close vessel details" control, the "Activity" / "AIS packets" cards, every Card wrapper
around map content, and the Inter font preconnect.

Keep one overlay, top-right, with two numbers:

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

`z-index: 1000` matters — Leaflet's panes sit below that. Vessel identity goes in the
hover tooltip; don't add a click-through detail view, vessel list, search, filters or
legend.

## Done when

- Scroll zooms, drag pans, pinch-zoom works on touch.
- Zooming in on a moored vessel puts it on a quay, not in open water.
- No hardcoded degree spans, no `scale-[...]` on the map, no percentage-positioned
  markers anywhere.
- Full-viewport map, one stats overlay, nothing else.
- Ships appear within a second of load.
- Killing and restoring the network reconnects within a few seconds.
