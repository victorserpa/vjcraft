# Remote API (HTTP + WebSocket)

Control VJCraft from scripts, web pages or other apps. Off by default.

Enable in Preferences > Remote API. REST listens on port **P** (default 8090), the
WebSocket on **P + 1** (default 8091). Default bind is `127.0.0.1` (this computer only).

## Web remote

`GET /` on the REST port serves a phone-friendly page (clip grid with thumbnails, trigger clip/column,
layer opacity/bypass/solo/clear, master, crossfader, BPM/tap/resync, black-out with confirm, decks,
dashboard dials, live updates over the WebSocket). Open `http://<this-computer>:8090/` from a phone,
tablet or another PC (enable "Expose to the network" first). Preferences > Remote API lists the URLs
(LAN IPv4, `name.local`) and a QR code (it carries `?pin=` when a PIN is set). The page is one embedded
HTML file: no CDN, no external request, CSP `default-src 'none'`. It connects to the socket on port + 1.

- `GET /api/v1/web` returns `{"pin_required": bool}` (open).
- `GET /api/v1/clip/{layer}/{column}/thumb` returns the clip's thumbnail as `image/png` (`Cache-Control:
  private, max-age=30`); 404 for cells that are empty, not file clips, or not decoded yet (the decode
  starts on the first request; retry).
- Added addresses: `/comp/layer/N/bypass`, `/comp/layer/N/solo` (0/1) and `/comp/tempo/resync`.
- `GET /api/v1/composition` also returns `dashboard` (`[{name, value}]`).

## PIN

Preferences > Remote API > "Web remote PIN" (empty = none, stored in plain text in the preferences file).
When set, every `POST` needs it as header `X-Pin: <pin>`, `Authorization: Bearer <pin>` or `?pin=`
(401 otherwise), and the WebSocket upgrade needs `?pin=` or `X-Pin` (401). Compared in constant time.
Reads (`GET`, the page itself) stay open. A PIN is not encryption: it travels in clear over HTTP, so use
it on a network you trust.

## Security warning

Without a PIN there is **no authentication**. Anyone who can reach the port can trigger clips, change
opacity/BPM and clear the composition.

- Keep the default (loopback) unless you need another machine.
- "Expose to the network" binds `0.0.0.0`: everyone on that network gets full control unless a PIN is set.
  Use it only on a network you trust, or behind a firewall/VPN.
- Against web pages in your own browser: POST needs `Content-Type: application/json` (this
  forces a CORS preflight, which is never answered; no `Access-Control-*` header is ever sent), and
  WebSocket upgrades carrying an `Origin` other than localhost/127.0.0.1/[::1] are refused. When exposed
  to the network the `Origin` host must equal the `Host` the socket was reached at (same site only).
- Against DNS rebinding: on loopback (default) REST and WebSocket accept only `Host`
  127.0.0.1, localhost or [::1] (with the API port, or none); anything else gets 403.
  When exposed to the network the `Host` must be an IP literal, `localhost`, or this computer's name
  (`name` or `name.local`); other names get 403.
- Limits: 64 KB per body/message, 16 concurrent HTTP requests, 8 WebSocket clients,
  256 subscribed paths.

## Addresses

Same space as OSC and shortcuts: `/comp/layer/1/opacity`, `/comp/master/opacity`,
`/comp/tempo/bpm`, `/comp/crossfader`, `/comp/group/1/bypass`, `/comp/layer/2/clip/3/connect`,
`/comp/column/3/connect`, `/comp/clear`... Layers and groups are numbered from the bottom,
columns and clips from 1. Values are normalized (0..1) except `bpm`. Changes go through the
same commands as the UI, so they are undoable and mark the project as modified (triggers do not).

Transform: `/comp/transform/F` (composition), `/comp/layer/N/transform/F` and
`/comp/layer/N/clip/transform/F` (clip playing on the layer), with F one of `x`, `y`, `scale`,
`scale_w`, `scale_h`, `rotation`, `anchor_x`, `anchor_y`. Values are normalized over the field's
range (position and anchor +-16384 px, scale 0..10, rotation +-360 degrees).

Layer fields: `/comp/layer/N/master` (0..1, scales picture and sound; `opacity` only fades the
picture), `/comp/layer/N/auto_size` (0 Off, 1/3 Fill, 2/3 Fit, 1 Stretch), `/comp/layer/N/width` and
`/comp/layer/N/height` (0..1 of 16384 px, 0 = composition size).

Clip fields (clip playing on the layer): `/comp/layer/N/clip/duration` (0 = free speed, else
value x 600 s), `/comp/layer/N/clip/alpha` (>= 0.5 premultiplied), `/comp/layer/N/clip/freeze` (>= 0.5),
`/comp/layer/N/clip/stop_motion` (value x 60 fps, 0 = off), `/comp/layer/N/clip/autopilot`
(round(value x 5): 0 = the layer's, then Off, Next, Random, Previous, First/last),
`/comp/layer/N/clip/autopilot_loops` (value x 64, at least 1) and
`/comp/layer/N/clip/refresh_thumb` (trigger: decode the thumbnail again).

## REST

Errors: 4xx/5xx with `{"error": "..."}` (400 malformed, 404 unknown route/address, 405 method,
401 PIN missing/wrong, 413 too large, 415 content type, 422 address unknown or value without effect, 503 app busy).

```sh
curl http://127.0.0.1:8090/api/v1/composition
curl 'http://127.0.0.1:8090/api/v1/address?path=/comp/layer/1/opacity'
curl -X POST -H 'Content-Type: application/json' \
  -d '{"path":"/comp/layer/1/opacity","value":0.5}' http://127.0.0.1:8090/api/v1/address
curl -X POST -H 'Content-Type: application/json' \
  -d '{"layer":1,"column":2}' http://127.0.0.1:8090/api/v1/trigger   # one clip
curl -X POST -H 'Content-Type: application/json' \
  -d '{"column":2}' http://127.0.0.1:8090/api/v1/trigger             # whole column
```

`GET /api/v1/composition` returns name, size, fps, `bpm`, `master`, `crossfader`, `layers`
(number, name, opacity, bypass, solo, group, active_column, active_columns), `groups`, `columns` and `decks`
(with their clips: layer, column, name, kind).

## WebSocket

`ws://127.0.0.1:8091/api/v1/ws` (the path must match). Client messages (JSON text):

```json
{"set": "/comp/layer/1/opacity", "value": 0.25}
{"trigger": {"layer": 1, "column": 2}}
{"trigger": {"column": 2}}
{"subscribe": ["/comp/layer/1/opacity", "/comp/tempo/bpm"]}
```

`subscribe` replaces the list and the server answers right away with each current value.
Server messages:

```json
{"path": "/comp/layer/1/opacity", "value": 0.25}
{"event": "ClipStarted", "path": "/comp/layer/1/clip/2/connect"}
{"event": "BPM", "value": 128.0}
{"error": "..."}
```

Values are checked about 20 times per second; events go to every client without subscribing.

```sh
websocat ws://127.0.0.1:8091/api/v1/ws
{"subscribe":["/comp/master/opacity"]}
{"set":"/comp/master/opacity","value":0.5}
```
