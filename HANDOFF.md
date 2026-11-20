# HANDOFF (branch miracast-spike)

## Goal
Miracast sink spike (Android tablet TB336FU = source, Fedora KDE laptop = sink), rootless via NetworkManager D-Bus WifiP2P + Polkit. Clean-room, new crates only, never touch main. Steps 1-6 one per session.

## Done
- Step 1: `docs/agents/miracast.md` (packages, Polkit, firewall, P2P network structure, WFD IE, M1-M7 flow, media formats, Android expectations, Intel 8265 notes, spike architecture, test plan, commands to measure the T480s).

- Step 2: miracast.md section 8a: mac80211_hwsim/virtual radios NOT usable without root here (no module tree, no Wi-Fi stack; userns works but cannot create radios). Skipped; tests use mocks + loopback.

- Step 3: crates `miracast-nm` (P2pProvider trait, MockP2pProvider, NmApi/NmP2pProvider + MockNm, zbus backend `ZbusNm`, wfd IE encode/parse, Sink API), `miracast-rtsp` and `miracast-rtp` (type skeletons). In xtask LAYERS. 17 tests, clippy -D warnings and `cargo xtask ci --no-loopback` green. No unsafe, no new dependency (zbus already in the workspace). Details: miracast.md "Crate skeleton".

- Step 4: `miracast-rtsp` sink M1-M7 state machine (`message`, `params`, `SinkSession`), 21 tests, clippy -D warnings and `cargo xtask ci --no-loopback` green. No unsafe, no dependencies. Details: miracast.md "RTSP sink (step 4)".

- Step 5: `miracast-rtp` RTP parse + reorder, TS demux (PAT/PMT/PES/CC), `Receiver`, `MediaSink` loss -> `need_keyframe`, `ts::synth` stream builders, 16 tests incl. a real ffmpeg mpegts stream; clippy -D warnings green, no unsafe, no deps. Details: miracast.md "RTP/TS receive (step 5)". Decoder-feeding example binary NOT done (moved to step 6).

- Step 6: crate `miracast-loopback` (real-socket sink driver, `TestSource` speaking M1-M7 + RTP/TS, 3 e2e tests incl. loss -> M13 IDR request -> recovery, hardware test binary `miracast-sink-demo`). `cargo xtask ci --no-loopback` green, clippy -D warnings green, no unsafe, no new dependency. Decoder example NOT done (kept spike crates free of mirrorwire-* deps; `ffplay` on the dumped stream instead). Details: miracast.md "Loopback end-to-end (step 6)".

## Decisions
See miracast.md section 7 (sink first, sans-IO parsers, no HDCP/UIBC, audio demuxed not played, rejected: root helpers, direct wpa_supplicant D-Bus, GStreamer/FFmpeg demux, GPL code).

## Open questions
- zbus backend stubs (Unsupported): publishing WFD IEs via NM (R2), reading group addresses after activation, PeerAdded/GroupFormed signals. Need hardware to settle (R1/R2).
- The research box is a VM without Wi-Fi/iw/NM: nothing about the Intel 8265 was measured; the doc lists what to run on the T480s (section 9). Facts marked [verify] in miracast.md (WFD IE bits, wfd_* masks, NM incoming GO negotiation = risk R1) come from memory of the specs and are unconfirmed.

## Verify
`git log miracast-spike`; `cargo test -p miracast-nm -p miracast-rtsp -p miracast-rtp`; `cargo clippy -p miracast-nm -p miracast-rtsp -p miracast-rtp --all-targets -- -D warnings`; `cargo xtask layers`.

## Next step
Spike code is done; what remains needs the owner's hardware: run the HARDWARE TEST CHECKLIST below and send the logs back. Then settle R1/R2 (miracast.md section 10), wire `miracast-nm` to `miracast-loopback`'s sink loop, and optionally feed access units to MirrorWire's decoder.

## HARDWARE TEST CHECKLIST (T480s Fedora KDE Wayland + Lenovo TB336FU)
Facts marked [verify] come from memory of the specs/NM docs, not measured. Nothing here needs root for the spike code itself; stop at any step that asks for sudo and report it instead (the spike must stay rootless).

**0. Build and baseline (T480s)**
```sh
git fetch origin && git checkout miracast-spike
cargo build --release -p miracast-loopback --bin miracast-sink-demo
cargo test -p miracast-loopback            # 3 tests must pass on real hardware too
rpm -q NetworkManager NetworkManager-wifi wpa_supplicant linux-firmware
nmcli device                               # expect wlp.. and p2p-dev-wlp.. (type wifi-p2p)
iw list | grep -A12 'Supported interface modes'    # want P2P-client, P2P-GO, P2P-device
iw list | grep -B2 -A10 'valid interface combinations'
nmcli -f all device show | grep -i -B2 -A8 p2p
pkaction --verbose --action-id org.freedesktop.NetworkManager.network-control
```
Save all output to `miracast-baseline.txt`. If `nmcli device` shows no `p2p-dev-*`, stop: report it (Intel 8265 + this NM/wpa_supplicant do not expose P2P).

**1. Sink first, listening before any group exists (T480s)**
```sh
sudo -n true 2>/dev/null; echo "(no sudo used below)"
./target/release/miracast-sink-demo --out stream.h264 --secs 900 2> sink-trace.log
```
(firewall: the Wi-Fi Direct group interface may be in a restricted zone; see miracast.md section 1. Report what `firewall-cmd --get-active-zones` says; if the tablet cannot connect, ask the owner before opening 7236/tcp and 19000/udp, e.g. `firewall-cmd --add-port=7236/tcp --add-port=19000/udp` for the runtime config only.)

**2. Discovery and group (T480s, second terminal) [verify all]**
```sh
P2P=$(nmcli -t -f DEVICE,TYPE device | awk -F: '$2=="wifi-p2p"{print $1}')
busctl --system introspect org.freedesktop.NetworkManager $(nmcli -g GENERAL.UDI device show $P2P) | grep -i -E 'StartFind|Peers|WfdIEs'
nmcli connection add type wifi-p2p con-name mw-wfd ifname "$P2P" peer <TABLET_P2P_MAC> wifi-p2p.wfd-ies 00000600111c440032 wifi-p2p.wps-method 4
```
The WFD IE bytes `00 00 06 0011 1c44 0032` = device-info subelement: primary sink, session available, RTSP port 7236, 50 Mbit/s (`miracast_nm::wfd::sink_device_info`). Open question R2: whether NM publishes this IE to the tablet at all; R1: whether NM accepts the tablet's incoming connection request.

**3. On the TB336FU**
1. Settings -> Connected devices (or Connection preferences) -> Cast -> menu -> **Enable wireless display**.
2. Open Cast; wait up to 30 s. **Note: is a device named "MirrorWire" (or the laptop's name) listed? (yes/no)**
3. If listed: tap it. Accept any "connect" prompt. Note: does the tablet show "Connecting" then fail, or connect?
4. If not listed: on the T480s run `busctl --system call ... StartFind` / `nmcli device show $P2P` and check whether the *tablet* appears as a peer (`busctl ... get-property ... Peers`); then `nmcli connection up mw-wfd` to connect from the laptop side. Note which of the two directions worked.
5. After connecting, note: who became GO (`ip addr show` on the T480s: `192.168.49.1` on our side = we are GO, else the tablet is), the tablet's IP, whether video appears in `sink-trace.log`.
6. Rotate the tablet once (resolution change mid-stream) and note the log lines; lock the screen, then wake it (loss/IDR); finally tap Disconnect (teardown M8).

**4. Check the dump (T480s)**
```sh
ffplay -f h264 stream.h264        # or: mpv --demuxer-lavf-format=h264 stream.h264
ffprobe -hide_banner stream.h264
```

**5. Logs to send back**
`miracast-baseline.txt`, `sink-trace.log` (full RTSP trace both directions with timestamps plus per-second media stats; the `done: SinkReport {..}` line at the end shows close reason, loss and IDR counts), `journalctl -u NetworkManager --since "-30 min" > nm.log`, `journalctl -u wpa_supplicant --since "-30 min" > wpa.log`, `nmcli -f all device show $P2P`, `ip addr` and `ip route` during the session, and answers to the **bold** questions above (listed? which direction? who was GO? video yes/no, audio demanded?). Optional if allowed: `tcpdump -i <p2p-iface> -w mw.pcap 'tcp port 7236 or udp port 19000'` (needs root normally: skip if so and say so).

**What a good result looks like:** `sink-trace.log` shows M1..M7 in order, "start media on UDP 19000", rising video AU counts, no `close:` until you disconnect. Also useful: which `wfd_video_formats`/`wfd_audio_codecs` the tablet's M4 picked, and whether it rejected `wfd_audio_codecs: none` (then an LPCM offer is needed: miracast.md section 5).

## Commit
see `git log -1` on miracast-spike
