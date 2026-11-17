# Protocol

Everything here lives in `mirrorwire-proto` (constants, framing) and `mirrorwire-net`
(handshake, URI, mDNS). If you change any of it, follow §4.

## 1. Connection

- QUIC over UDP, default port **47800** (`DEFAULT_PORT`), ALPN **`mirrorwire/1`** (`ALPN`,
  fixed; the protocol version is `hello.protocol`, §4).
  The presenter port is configurable (`--port`, `MIRRORWIRE_PORT`, saved `settings.json`).
  A changed port is advertised in mDNS (`ServiceInfo` port) and in the URI (`mirrorwire://ip:port?…`);
  the viewer always uses the advertised port, never a hard-coded 47800.
- TLS 1.3 with self-signed certificates on both sides (`rcgen`, ring provider).
  - The viewer's verifier accepts the presenter's cert **only** if its SHA-256 fingerprint
    (64 lowercase hex chars) equals the pinned one from the URI/mDNS.
  - The presenter accepts any client cert at the TLS layer, then authorizes in the handshake:
    the fingerprint is in `paired_viewers.json`, or the `Hello` carries the right pairing code
    (then the fingerprint is added), or pairing is off (`--no-pairing`).
- Idle timeout 10 s. Datagram send buffer 2 MiB (`DATAGRAM_SEND_BUFFER`); when it is too full
  for a delta frame, the presenter drops that frame and forces a keyframe instead of queueing
  (keyframes are always queued). Pacing 500 Mbit/s (`lan_cc.rs`); on top, the send loop spreads
  a keyframe over time at 1.25 × the bitrate budget and the frames that waited behind it at
  1.5 ×, at most 100 ms per frame (`rate::pace_spread_us`), so a keyframe does not overflow a
  shallow bottleneck buffer. Not a wire change: the viewer sees the same packets, later.

## 2. Control stream

One bidirectional stream opened by the viewer. Each message is a 4-byte big-endian length
followed by a JSON body (`serde`, `tag = "type"`, `snake_case`). Bodies over
`MAX_CONTROL_MESSAGE` (64 KiB) are an error.

| `type` | Direction | Fields |
|---|---|---|
| `hello` | viewer → presenter | `protocol` (u32, must equal `PROTOCOL_VERSION`, currently **3**; read first, §4), `name`, `pairing_code` (string or null) |
| `rejected` | presenter → viewer | `reason` (shown to the user) |
| `stream_info` | presenter → viewer | `width`, `height`, `fps`, `codec`, `encoder`, `bitrate_kbps`, `intra_refresh`, `cursor` (`separate` = §3a, `embedded` = in the video, `hidden`), `fec` (the presenter's FEC setting `off`/`auto`/`20%`, for display). Sent after `hello`, and **again** whenever it changes |
| `request_keyframe` | viewer → presenter | `reason`. The presenter forces at most one keyframe per 2 × RTT (250 ms–1 s) per viewer and ignores requests in between. The viewer asks at once, then backs off 250 ms, 500 ms, 1 s, 1 s … until a keyframe arrives (`KeyframeBackoff`), so a lost keyframe is still replaced without a request storm; frames that arrive while it waits are still counted in `link_report`. The encoder drops a request that arrives when the previous frame sent was already an IDR (`Controls::take_keyframe_request`: no forced IDR directly after an IDR) |
| `link_report` | viewer → presenter | `loss_permille`, `packets`, `recovered`, `unrecoverable` over the last ~0.5 s, and `queue`: `{mean_us, max_us, trend_us_per_s}` (u32, u32, i32), the one-way queueing delay of the window's frames (mean, max) and its least-squares slope (§3c), or null when no frame arrived. Not sent for a window without packets |
| `ping` / `pong` | viewer ↔ presenter | `t_viewer_us`, `t_presenter_us` (clock offset + RTT) |
| `bye` | either | — |

Every field is required (a missing or mistyped one is a parse error that closes the session);
unknown fields are ignored. Any change to a message bumps `PROTOCOL_VERSION` (§4).

## 3. Video datagrams

One packet format, with or without FEC (`mirrorwire-proto/src/fragment.rs`). Each encoded frame
(H.264 Annex B access unit) is cut into `data_count` equal data shards of `shard_len` bytes
(even) that fit `conn.max_datagram_size()`; the last data shard goes out unpadded. While the
presenter's FEC setting isn't `off`, `parity_count` parity shards follow (§3b); with `off` it is
0. Data packets go out first, then parity (paced by the congestion controller like everything
else). Every packet starts with a **44-byte big-endian header** (`HEADER_LEN`):

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | wire version (`WIRE_VERSION` = 2); other values are dropped |
| 1 | 1 | flags: bit 0 = keyframe; other bits reserved, send 0 |
| 2 | 4 | `frame_id` (u32, increments per frame) |
| 6 | 2 | `index`: `0..data_count` data, then `data_count..data_count+parity_count` parity |
| 8 | 2 | `data_count` (≥ 1) |
| 10 | 2 | `parity_count` |
| 12 | 4 | `frame_len` (bytes of the encoded frame, ≤ 64 MiB) |
| 16 | 2 | `shard_len` |
| 18 | 1 | `blocks` (§3b; 1..=255, ≤ `data_count`) |
| 19 | 1 | reserved, 0 |
| 20 | 8 | `capture_us` (presenter clock) |
| 28 | 8 | `encoded_us` |
| 36 | 8 | `sent_us` |

Receivers drop packets whose header is inconsistent (zero blocks, `frame_len` not matching the
counts, payload length not what the index implies) or whose layout differs from the first packet
seen for that frame, and keep at most 64 partial frames. The receiver reassembles by
`frame_id`, drops incomplete frames that are overtaken, reports the loss, and asks for a
keyframe unless the stream uses intra refresh.

The header costs 10 bytes more per packet than the old version-1 fragments (34 bytes), about
0.7 % of a 1400-byte packet, in exchange for a single receive path.

## 3a. Cursor as coordinates (`stream_info.cursor` = `separate`)

All in `mirrorwire-proto/src/cursor.rs`.

**Pointer datagram** (unreliable, one per change, not tied to video frames). The first byte
`0xC1` can't collide with a video packet (whose first byte is `WIRE_VERSION` = 2).
Exactly 32 bytes, big endian (other lengths are dropped; a later tool such as a pen with
pressure/tilt changes the layout and bumps `PROTOCOL_VERSION`):

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | kind `0xC1` |
| 1 | 1 | version 1 (other values dropped) |
| 2 | 1 | flags: bit 0 visible |
| 3 | 1 | tool: 0 = cursor (future: pen, touch) |
| 4 | 4 | `seq` (u32, wrapping; the viewer drops older/duplicate ones) |
| 8 | 4 | `x` (i32, hotspot position in captured-frame pixels) |
| 12 | 4 | `y` (i32) |
| 16 | 2 | `frame_w` (captured frame width, before encoder scaling) |
| 18 | 2 | `frame_h` |
| 20 | 4 | `image_id` (0 = none yet) |
| 24 | 8 | `capture_us` (presenter clock) |

**Cursor image**: one unidirectional QUIC stream per shape (presenter → viewer), at most
`MAX_IMAGE_STREAM` bytes: magic `MWCI`, version 1, 3 reserved, `id` u32, `width` u16, `height`
u16 (each ≤ 512), `hotspot_x` i32, `hotspot_y` i32, 4 reserved, then `width*height*4` bytes of
premultiplied RGBA. 0×0 = invisible cursor. The viewer caches the last 32 images by id; a new
viewer gets every cached image and the last position on connect.

**Drawing**: `cursor_rect()` maps `(x - hotspot, y - hotspot)` and the image size through the
rectangle the video is drawn in (`video.w / frame_w`), so fit, letterbox and 1:1 modes all
line up. The same message is meant to carry the future pen/Wacom mode.

## 3b. Forward error correction

Parity shards in the packets of §3, while the presenter's FEC setting isn't `off`. Code in
`mirrorwire-proto/src/fec.rs` (erasure code: `reed-solomon-simd`, MIT AND BSD-3-Clause).

Data shard `i` belongs to block `i % blocks`, parity shard `j` to block `j % blocks`
(`blocks = ceil(data_count / 64)`, at most 255), so consecutive packets hit different blocks and
a burst costs each block one or two packets. Each block is a Reed-Solomon code over its data
shards (zero-padded to `shard_len` for coding) and parity shards: **any** `k_b` of its
`k_b + m_b` shards rebuild it, immediately, without a round trip.

**Overhead** (`FecMode::overhead`): `auto` gives delta frames 10 % + 3 × loss + boost (10–35 %)
parity and keyframes 25 % + 5 × loss + 1.5 × boost (25–60 %), from the loss the viewer reports in
`link_report` (smoothed: rises at once, decays slowly). The boost (0–15 points) grows by 5 for
every report with unrecoverable frames and shrinks by 1 per clean report, because bursty loss
needs more parity than its average suggests; it does not grow while the adaptive bitrate sees
congestion (parity would only add load). A fixed setting `N%` gives delta frames N % and
keyframes 1.5 × N %. At least one parity packet per block (two for keyframes). Parity is paid
from the bitrate budget: the encoder target is the video budget / (1 + delta overhead); while
the adaptive bitrate probes (below), delta frames carry extra parity so that video + parity
fill the whole budget (`rate::probe_overhead`, at most 60 %; keyframes keep theirs).

**Adaptive bitrate** (presenter; `mirrorwire-proto/src/rate.rs`): the same `link_report`
(loss and the queueing delay of §3c) and the presenter's backlog drops drive an AIMD
controller (the QUIC RTT is shown in the status line but not used). Its ceiling is the bitrate setting, its floor
3 Mbit/s. Congestion cuts the budget ×0.85 (×0.75 at ≥ 10 points excess loss, ×0.6 at ≥ 20),
at most once per second. Congestion is: a backlog drop; loss ≥ 3 points above the learned
background loss in two reports in a row, or ≥ 10 points once; or a queue:
one report with a queue building (mean ≥ 25 ms or max ≥ 50 ms, and rising ≥ 30 ms/s) or standing
(mean ≥ 60 ms, not within 2 s of a cut while it drains) is enough, and that cut also goes to
≤ 90 % of the wire rate sent in the last second. Loss with no frame waiting ≥ 40 ms in this or
the previous report counts as random (no cut, FEC covers it) unless it is ≥ 20 points. A clean link (< 1.5 points excess, no queue) adds 4 % of the ceiling
per second from 4 s after the last cut. Each cut remembers what the link did not carry (the
budget, or the wire rate sent if less); from 90 % of it up, loss above the background counts
even with a flat queue, and up to 115 % of it the budget grows by a parity probe: a quarter of
the normal step, up to 3 % above the video budget, filled with extra FEC parity; after 3 s
of clean reports (any report with loss ≥ 1.5 points excess or a queue restarts the 3 s) the
video budget follows. A cut lowers both. Keyframes are paced on the video budget. A new viewer starts at 10 Mbit/s (or the ceiling if
lower) in slow start: ×1.25 per second until the first cut, through loss below the cut
threshold. A viewer whose link failed before (same certificate fingerprint and address, within
10 min, presenter memory only) instead starts at min(the budget it left with, 60 % of the
budget that failed last), keeps that failure for the hysteresis and slow-starts only to 90 % of
it (`AdaptiveBitrate::resume`). Presenter-side only; no wire change. The budget only grows while the presenter sends ≥ 80 % of it (wire rate, video +
parity, per 1 s send window), so it stays near what has actually been sent. Loss that stays at ≥ 70 % of its level for 3 s after a
cut is learned as background loss (random Wi-Fi loss that FEC covers). x264 takes the new rate
live (no keyframe); VAAPI/FilmCraft reopen (keyframe): down by ≥ 10 % at most every 1 s, up by
≥ 15 % at most every 5 s.

**Loss accounting**: the viewer counts, per frame, packets sent (`data_count + parity_count`)
against packets received, once the frame is 30 frames old (so late packets still count). Frames
that never arrive at all are not in the packet count; they show up as unrecoverable frames.

## 3c. One-way queueing delay (`mirrorwire-proto/src/delay.rs`)

The viewer measures, for the **first packet that arrives of each new frame**, `arrival − sent_us`
(its clock minus the presenter's; the unknown clock offset is constant). The lowest value of the
last 30 s (per-second minima) is the empty-queue baseline; `delay − baseline` is the time the
packet waited in a queue. The last packet is not used: it also waits for the frame's own packets
to cross the link, which is not a queue left by earlier frames. A value above 2 s is taken as a
clock step and restarts the baseline; clock drift (ppm) is absorbed by the 30 s window. Per
`link_report` window: mean and max of the frames' queueing delay, and the least-squares slope over
arrival time (µs per s). The viewer overlay shows it as `queue N ms (max N, +N ms/s)`. No wire
change for video packets; `sent_us` was always there.

## 4. Versioning rules

There is **no backwards compatibility** (decisions.md D6): presenter and viewer must speak
the same `PROTOCOL_VERSION` (`lib.rs`), and any incompatible change (a control message or
field, a datagram layout, handshake or auth semantics, mDNS TXT keys, URI parameters) bumps
it. Don't add optional fields, capabilities or fallbacks to keep an older peer working:
change the format and delete the old path.

The presenter reads `hello.protocol` before parsing the rest of the hello
(`control::hello_protocol`), so a viewer of any version gets a `rejected` message the viewer
GUI shows ("version mismatch: the presenter speaks protocol X, the viewer Y; update the
viewer …"), not a timeout or a parse error. That check is the one thing that must keep working
across versions: `hello` stays a JSON object with `"type":"hello"` and a numeric `protocol`.
The ALPN stays `mirrorwire/1` (D5); an ALPN mismatch would fail inside TLS with no readable
message.

In the same commit: update this file, the README if users see it, add a test in
`mirrorwire-proto` / `mirrorwire-net` that pins the new layout (encode → bytes → decode), and list
the break for the owner in the commit message.

## 5. Discovery and pairing

- **mDNS:** service `_mirrorwire._udp.local.`, instance name = presenter name (hostname by
  default), TXT `fp` (fingerprint), `v` (the presenter's `PROTOCOL_VERSION` in decimal; since
  protocol 3, before that always `1`), `res` (`WxH`). The viewer's list (desktop, Android and
  `mirrorwire discover`) still shows a presenter whose `v` differs or is missing, marked "older/
  newer MirrorWire …: update the presenter/this viewer"; connecting to it gets the `hello`
  rejection (§4).
- **URI / QR:** `mirrorwire://<ip>:<port>?fp=<64 hex>&code=<6 digits>&name=<name>`. `fp` is
  required; `code` and `name` are optional. IPv6 hosts are bracketed.
- **Pairing code:** 6 decimal digits from `getrandom`, new on each presenter start, shown in the
  presenter window and terminal. Only needed on the first connection of a viewer.
