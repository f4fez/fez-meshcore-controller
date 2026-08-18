# MQTT broker forwarding

The daemon can forward mesh activity to one or more MQTT brokers. Configuration lives under
`[[mqtt_brokers]]` in `config.toml` (see [`config.example.toml`](../config.example.toml) for a
commented template) and can be set up interactively via `fez-mesh-controller setup`.

Multiple brokers can be configured; each is independent (its own connection, topics, and
enable/disable switches).

## Configuration reference

| Field | Type | Default | Description |
|---|---|---|---|
| `name` | string | — | Internal identification for this broker, shown in the TUI's "Observer node" block. |
| `host` | string | — | Broker hostname or IP. |
| `port` | number | `1883` | Broker port. |
| `auth_method` | `"passwd"` \| `"device"` \| `"none"` | `"passwd"` | How to authenticate with this broker. `"passwd"` uses `username`/`password` below (optionally both left unset, for an anonymous connection under this mode too); `"device"` uses a MeshCore device-signed token instead — required by LetsMesh and MeshMapper's public brokers, see [Device-signed authentication](#device-signed-authentication-letsmesh-meshmapper) below; `"none"` always connects anonymously, ignoring `username`/`password` even if set. |
| `username` / `password` | string, optional | none | Broker credentials. Stored in **plaintext** in `config.toml`, like the rest of this file. Only used when `auth_method = "passwd"`. |
| `jwt_ttl_secs` | number | `3600` | Lifetime, in seconds, of a device-signed auth token before it's refreshed (6 minutes before expiry). Only used when `auth_method = "device"`. |
| `jwt_audience` | string, optional | `host` | `aud` claim for a device-signed auth token. Defaults to `host`, which is what LetsMesh/MeshMapper expect. Only used when `auth_method = "device"`. |
| `topic_prefix` | string | `"meshcore"` | Prefix substituted for `{prefix}` in every topic route below. |
| `status_topic` | string | `"{prefix}/status"` | Topic route for the status topic (see below). |
| `status_refresh_interval_secs` | number | `300` | How often (seconds) to republish the retained status message while connected, so its `timestamp` stays fresh. `0` disables the periodic republish (status is still published on connect/disconnect). |
| `enable_high_level_messages` | bool | `true` | Publishes the status topic and every decoded-event topic (see below). `false` mutes this broker entirely — it still connects, but publishes nothing. |
| `enable_packet_trafic_messages` | bool | `true` | Publishes the rich raw-packet capture topic (`packet_trafic_topic`). |
| `packet_trafic_topic` | string | `"{prefix}/packets"` | Topic route for the rich raw-packet capture topic. |
| `enable_raw_messages` | bool | `false` | Publishes the minimal raw-packet envelope topic (`raw_topic`). Off by default — config-file-only, not offered in the setup wizard. |
| `raw_topic` | string | `"{prefix}/raw"` | Topic route for the minimal raw-packet envelope topic. |
| `transport_protocol` | `"tcp"` \| `"websocket"` | `"tcp"` | Connection protocol to the broker. |
| `websocket_path` | string | `"/mqtt"` | URL path for the WebSocket connection (e.g. `/mqtt`, `/ws`) — only used when `transport_protocol = "websocket"`. Broker-specific; there's no universal default across providers. |
| `tls_enabled` | bool | `false` | Enables TLS. With `transport_protocol = "tcp"` this is plain TLS (`mqtts`-style); with `"websocket"` it's `wss`. |
| `tls_ca_cert` | path, optional | none | Custom CA certificate to trust, in addition to the system trust store. |
| `tls_client_cert` / `tls_client_key` | path, optional | none | Client certificate/key for mutual TLS. Both must be set together, and require `tls_ca_cert`. |

### Topic route placeholders

`status_topic`, `packet_trafic_topic` and `raw_topic` are templates. Two placeholders are
substituted at publish time:

- `{prefix}` — this broker's `topic_prefix`.
- `{public_key}` — this node's own public key, uppercase hex (falls back to `DEVICE` if not yet
  known, e.g. before the mesh connection is established).

Some consumers require a node-id segment in every topic, including status — e.g.
[`yellowcooln/meshcore-mqtt-live-map`](https://github.com/yellowcooln/meshcore-mqtt-live-map)
expects exactly `meshcore/<area-code>/<node-id>/<kind>` (4 segments; `kind` one of `status`,
`internal`, `packets`). To match that shape: set `topic_prefix = "meshcore/<area-code>"` and
`status_topic`/`packet_trafic_topic` to `"{prefix}/{public_key}/status"` /
`"{prefix}/{public_key}/packets"`.

Every decoded-event topic below is fixed at `{prefix}/<name>` (not independently configurable).

### Device-signed authentication (LetsMesh, MeshMapper)

Some public community MQTT brokers — verified against
[`Colorado-Mesh/mesh-client`](https://github.com/Colorado-Mesh/mesh-client)'s
`docs/letsmesh-mqtt-auth.md`, not assumed — require **device-signed** auth rather than a static
username/password: the MQTT username is `v1_<node public key, uppercase hex>`, and the password
is a short-lived JWT-style token signed by the node's own private key. Set `auth_method = "device"`
to use this instead of `username`/`password` (the setup wizard offers it as a third "MeshCore Auth
Token" option alongside "Username/Password" and "Anonymous", which map to `auth_method = "passwd"`
and `auth_method = "none"` respectively).

The node's private key **never leaves the device**: this daemon builds the token's header/payload
and asks the node to sign those bytes over the existing companion link (the same mechanism the
node's own firmware exposes for this purpose), then assembles the final token. The token is
automatically refreshed 6 minutes before `jwt_ttl_secs` expires, forcing a reconnect so the
broker sees the fresh credentials (MQTT 3.1.1 has no in-band re-authentication).

Known presets:

| Preset | Host | Port | WebSocket path |
|---|---|---|---|
| LetsMesh (US) | `mqtt-us-v1.letsmesh.net` | `443` | `/ws` |
| LetsMesh (EU) | `mqtt-eu-v1.letsmesh.net` | `443` | `/ws` |
| MeshMapper | `mqtt.meshmapper.net` | `443` | `/ws` |

To point a broker entry at one of these: `host` = the hostname above, `port = 443`,
`transport_protocol = "websocket"`, `websocket_path = "/ws"`, `tls_enabled = true`,
`auth_method = "device"`.

**MeshMapper requires a specific topic structure** — verified against their own
[`wiki.meshmapper.net/mqtt-python`](https://wiki.meshmapper.net/mqtt-python/), not assumed:
`meshcore/{IATA}/{PUBLIC_KEY}/status` and `meshcore/{IATA}/{PUBLIC_KEY}/packets`, where `{IATA}` is
a 3-letter IATA code (e.g. `SEA`, `LAX`, `YOW`, `LON`) identifying which MeshMapper region your
observer belongs to — pick your own region's code, this daemon has no way to infer it. This
project's existing `{prefix}`/`{public_key}` topic template placeholders (see above) already cover
this shape: set `topic_prefix = "meshcore/<your IATA code>"` and `status_topic`/
`packet_trafic_topic` to `"{prefix}/{public_key}/status"` / `"{prefix}/{public_key}/packets"`, as
in the ready-to-use entry below. LetsMesh has no equivalent region-in-topic requirement — its
packet logger topic (`{topicPrefix}/{pubKey}/packets` or `{topicPrefix}/meshcore/packets`) matches
this daemon's own default `<prefix>/packets` shape closely enough to leave `topic_prefix` at its
default, though the exact envelope isn't guaranteed identical — check against your operator's
current docs before relying on it.

Ready-to-use `[[mqtt_brokers]]` entries:

```toml
[[mqtt_brokers]]
name = "LetsMesh (US)"
host = "mqtt-us-v1.letsmesh.net"
port = 443
auth_method = "device"
transport_protocol = "websocket"
websocket_path = "/ws"
tls_enabled = true

[[mqtt_brokers]]
name = "LetsMesh (EU)"
host = "mqtt-eu-v1.letsmesh.net"
port = 443
auth_method = "device"
transport_protocol = "websocket"
websocket_path = "/ws"
tls_enabled = true

[[mqtt_brokers]]
name = "MeshMapper"
host = "mqtt.meshmapper.net"
port = 443
auth_method = "device"
transport_protocol = "websocket"
websocket_path = "/ws"
tls_enabled = true
topic_prefix = "meshcore/SEA"                     # replace SEA with your own 3-letter IATA code
status_topic = "{prefix}/{public_key}/status"
packet_trafic_topic = "{prefix}/{public_key}/packets"
```

Only one broker is needed to reach a given operator — add all three only if you actually want to
forward to every one of them. `jwt_ttl_secs`/`jwt_audience` are left at their defaults (3600s,
`aud` = `host`) above; override them only if an operator's requirements differ.

## Topics published

### Status topic (`status_topic`, default `<prefix>/status`, retained)

Published when the mesh node connects or reconnects, when it disconnects, on MQTT
(re)connection, and periodically every `status_refresh_interval_secs` while the MQTT connection
is up — gated by `enable_high_level_messages`. Also set as the broker's MQTT Last Will (published
automatically by the broker if the daemon disconnects uncleanly), using the compact 4-key shape
shown below.

Matches the format used by the community
[`agessaman/meshcore-packet-capture`](https://github.com/agessaman/meshcore-packet-capture)
bridge (`stats` is intentionally omitted — it would require querying the connected device for
firmware statistics commands not yet implemented in this project's MeshCore client library).

**Nothing is ever published to this topic before the mesh node's `origin`/`origin_id` are
known** (i.e. before the daemon has connected to the node at least once and fetched its
identity) — no message with placeholder/undefined values is ever sent.

```json
{
  "status": "online",
  "timestamp": "2026-08-14T07:53:47.871080+00:00",
  "origin": "F4FEZ_BRIDGE",
  "origin_id": "81364A93AB7221A1280DA26D91E7A702C6773925045A49C7782567EFA39544D0",
  "model": "Seeed Xiao-nrf52",
  "firmware_version": "v1.16.0-07a3ca9 (Build: 06-Jun-2026)",
  "radio": "869.618,62.5,8,8",
  "client_version": "fez-mesh-controller/0.1.0"
}
```

- `status`: `"online"` only when the mesh node is currently connected **and** its `model`/
  `firmware_version` were successfully fetched; `"offline"` otherwise.
- `origin` / `origin_id`: this node's name / public key (uppercase hex). Always present — this
  is the publish gate (see above).
- `model` / `firmware_version`: from the connected device's own info query — a separate,
  best-effort query that can fail even when the node itself is connected. **Omitted entirely**
  (not placeholdered) if never successfully fetched. Once fetched, they keep their last-known
  value even after a disconnect (`status` still flips to `"offline"`) — the daemon never resets
  them to placeholders on disconnect, only the connection-dependent `status` field changes.
- `radio`: `"<freq_mhz>,<bandwidth_khz>,<spreading_factor>,<coding_rate>"`. Comes from the same
  node-identity query as `origin`/`origin_id`, so it's always present whenever they are.

**Last Will payload** (published by the broker itself, not the daemon, on unclean disconnect) —
a strict subset, only 4 keys. Registered once when this broker's MQTT connection is first
established; if the mesh node hasn't connected yet at that exact moment, this specific payload
can fall back to `"unknown"`/`"DEVICE"` (a rumqttc limitation — the Last Will can't be updated on
a live connection) and keeps that fallback for the lifetime of the connection. This is the one
place those placeholders can still appear, and only if the MQTT connection *also* drops
uncleanly during that narrow startup window:

```json
{
  "status": "offline",
  "timestamp": "2026-08-14T07:53:47.871080+00:00",
  "origin": "F4FEZ_BRIDGE",
  "origin_id": "81364A93AB7221A1280DA26D91E7A702C6773925045A49C7782567EFA39544D0"
}
```

### Decoded-event topics

Gated by `enable_high_level_messages`. Every decoded mesh event this daemon recognizes gets
forwarded to one of the following fixed topics, using the same topic structure and JSON envelope
as the community [`ipnet-mesh/meshcore-mqtt`](https://github.com/ipnet-mesh/meshcore-mqtt)
bridge:

| Topic | Event |
|---|---|
| `<prefix>/events/connection` | Connected / Disconnected |
| `<prefix>/login` | Login success / failure |
| `<prefix>/device_info` | Device info query response |
| `<prefix>/battery` | Battery/storage status |
| `<prefix>/new_contact` | A new contact was learned |
| `<prefix>/advertisement` | Node advertisement overheard |
| `<prefix>/telemetry` | Telemetry response |
| `<prefix>/contacts` | Full contact list |
| `<prefix>/self_info` | This node's own info |
| `<prefix>/channel_info` | Channel info |
| `<prefix>/traceroute/unknown` | Trace route response |
| `<prefix>/message/direct/<sender_prefix_hex>` | Direct text message received |
| `<prefix>/message/channel/<channel_idx>` | Channel text message received |

Every message on these topics shares the same envelope:

```json
{
  "type": "EventType.CONTACT_MSG_RECV",
  "payload": { "...": "event-specific fields" },
  "attributes": { "...": "a small subset of payload, for MQTT topic filtering" }
}
```

Example (`<prefix>/message/direct/aabbccddeeff`):

```json
{
  "type": "EventType.CONTACT_MSG_RECV",
  "payload": {
    "pubkey_prefix": "aabbccddeeff",
    "path_len": 2,
    "txt_type": 0,
    "sender_timestamp": 1700000000,
    "text": "hello",
    "SNR": 5.5
  },
  "attributes": { "pubkey_prefix": "aabbccddeeff", "txt_type": 0 }
}
```

Internal command/response plumbing (`Ok`, `Error`, and any event type not listed above) is never
forwarded.

### `<prefix>/packets` — packet capture (rich schema)

Gated by `enable_packet_trafic_messages` (on by default), route configurable via
`packet_trafic_topic`. Publishes **every** overheard packet, regardless of whether this
project's own decoders recognize its payload type — this is the raw promiscuous capture stream,
independent of the decoded-event topics above. Same publish gate as `<prefix>/status`: nothing
is published (on either this topic or `<prefix>/raw` below) before `origin`/`origin_id` are
known — in practice this is never actually reachable before that, since packets can only be
overheard while the mesh node is connected in the first place.

Matches the format documented by
[`Colorado-Mesh/mesh-client`](https://github.com/Colorado-Mesh/mesh-client)'s
`docs/letsmesh-mqtt-auth.md` ("Packet logger" topic), which is field-for-field identical to
`agessaman/meshcore-packet-capture`'s own `packets` topic:

```json
{
  "origin": "F4FEZ_BRIDGE",
  "origin_id": "81364A93AB7221A1280DA26D91E7A702C6773925045A49C7782567EFA39544D0",
  "timestamp": "2026-08-14T07:53:47.871080+00:00",
  "type": "PACKET",
  "direction": "rx",
  "time": "07:53:47",
  "date": "14/08/2026",
  "len": 42,
  "packet_type": 2,
  "route": "direct",
  "payload_len": 30,
  "raw": "0a021111deadbeef...",
  "SNR": 5.5,
  "RSSI": -90,
  "hash": "a1b2c3d4e5f6a7b8"
}
```

- `direction`: always `"rx"` — this project only observes overheard traffic, it never captures
  its own transmissions.
- `time` / `date`: UTC (not the host's local timezone).
- `packet_type`: the raw numeric MeshCore payload type byte.
- `route`: `"direct"` for directly-routed packets, or the hop count as a string (e.g. `"2"`) for
  flood-routed packets.
- `len`: total reconstructed packet length in bytes (header + optional transport code + path +
  payload) — reflects the true size even when `raw` below is truncated.
- `raw`: the reconstructed over-the-air packet bytes, lowercase hex, **truncated to 2048
  characters**.
- `hash`: a locally-computed 8-byte SHA-256 prefix of the raw bytes, for deduplication
  convenience — **not** a MeshCore protocol field.

### `<prefix>/raw` — packet capture (minimal envelope)

Gated by `enable_raw_messages` (**off by default** — config-file-only, not offered in the setup
wizard), route configurable via `raw_topic`. A lighter-weight alternative to `<prefix>/packets`
that carries only the raw packet hex, no metadata.

Matches `agessaman/meshcore-packet-capture`'s own separate, opt-in-only `raw` topic:

```json
{
  "origin": "F4FEZ_BRIDGE",
  "origin_id": "81364A93AB7221A1280DA26D91E7A702C6773925045A49C7782567EFA39544D0",
  "timestamp": "2026-08-14T07:53:47.871080+00:00",
  "type": "RAW",
  "data": "0A021111DEADBEEF..."
}
```

- `data`: the reconstructed over-the-air packet bytes, **uppercase** hex, not truncated.

## QoS and retention

Every topic publishes at QoS 0 (at-most-once), except `<prefix>/status`, which is retained so a
newly-connecting subscriber immediately sees the last known state.
