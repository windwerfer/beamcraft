# HANDOFF (branch miracast-spike)

## Goal
Miracast sink spike (Android tablet TB336FU = source, Fedora KDE laptop = sink), rootless via NetworkManager D-Bus WifiP2P + Polkit. Clean-room, new crates only, never touch main. Steps 1-6 one per session.

## Done
- Step 1: `docs/agents/miracast.md` (packages, Polkit, firewall, P2P network structure, WFD IE, M1-M7 flow, media formats, Android expectations, Intel 8265 notes, spike architecture, test plan, commands to measure the T480s).

- Step 2: miracast.md section 8a: mac80211_hwsim/virtual radios NOT usable without root here (no module tree, no Wi-Fi stack; userns works but cannot create radios). Skipped; tests use mocks + loopback.

- Step 3: crates `miracast-nm` (P2pProvider trait, MockP2pProvider, NmApi/NmP2pProvider + MockNm, zbus backend `ZbusNm`, wfd IE encode/parse, Sink API), `miracast-rtsp` and `miracast-rtp` (type skeletons). In xtask LAYERS. 17 tests, clippy -D warnings and `cargo xtask ci --no-loopback` green. No unsafe, no new dependency (zbus already in the workspace). Details: miracast.md "Crate skeleton".

- Step 4: `miracast-rtsp` sink M1-M7 state machine (`message`, `params`, `SinkSession`), 21 tests, clippy -D warnings and `cargo xtask ci --no-loopback` green. No unsafe, no dependencies. Details: miracast.md "RTSP sink (step 4)".

- Step 5: `miracast-rtp` RTP parse + reorder, TS demux (PAT/PMT/PES/CC), `Receiver`, `MediaSink` loss -> `need_keyframe`, `ts::synth` stream builders, 16 tests incl. a real ffmpeg mpegts stream; clippy -D warnings green, no unsafe, no deps. Details: miracast.md "RTP/TS receive (step 5)". Decoder-feeding example binary NOT done (moved to step 6).

## Decisions
See miracast.md section 7 (sink first, sans-IO parsers, no HDCP/UIBC, audio demuxed not played, rejected: root helpers, direct wpa_supplicant D-Bus, GStreamer/FFmpeg demux, GPL code).

## Open questions
- zbus backend stubs (Unsupported): publishing WFD IEs via NM (R2), reading group addresses after activation, PeerAdded/GroupFormed signals. Need hardware to settle (R1/R2).
- The research box is a VM without Wi-Fi/iw/NM: nothing about the Intel 8265 was measured; the doc lists what to run on the T480s (section 9). Facts marked [verify] in miracast.md (WFD IE bits, wfd_* masks, NM incoming GO negotiation = risk R1) come from memory of the specs and are unconfirmed.

## Verify
`git log miracast-spike`; `cargo test -p miracast-nm -p miracast-rtsp -p miracast-rtp`; `cargo clippy -p miracast-nm -p miracast-rtsp -p miracast-rtp --all-targets -- -D warnings`; `cargo xtask layers`.

## Next step
Step 6: loopback end-to-end: a minimal test source (RTSP client side M1-M7 against `SinkSession` over TCP on localhost + `ts::synth`/ffmpeg TS over RTP UDP to the sink's port, `Receiver` -> `MediaSink`, `need_keyframe` -> `request_idr`), optionally decode the access units with the existing MirrorWire decoder in an example binary; then write the HARDWARE TEST CHECKLIST here (T480s commands, what to tap on the TB336FU, logs to send back) and reply NOTHING_LEFT.

## Commit
see `git log -1` on miracast-spike
