# HANDOFF (branch miracast-spike)

## Goal
Miracast sink spike (Android tablet TB336FU = source, Fedora KDE laptop = sink), rootless via NetworkManager D-Bus WifiP2P + Polkit. Clean-room, new crates only, never touch main. Steps 1-6 one per session.

## Done
- Step 1: `docs/agents/miracast.md` (packages, Polkit, firewall, P2P network structure, WFD IE, M1-M7 flow, media formats, Android expectations, Intel 8265 notes, spike architecture, test plan, commands to measure the T480s).

## Decisions
See miracast.md section 7 (sink first, sans-IO parsers, no HDCP/UIBC, audio demuxed not played, rejected: root helpers, direct wpa_supplicant D-Bus, GStreamer/FFmpeg demux, GPL code).

## Open questions
- The research box is a VM without Wi-Fi/iw/NM: nothing about the Intel 8265 was measured; the doc lists what to run on the T480s (section 9). Facts marked [verify] in miracast.md (WFD IE bits, wfd_* masks, NM incoming GO negotiation = risk R1) come from memory of the specs and are unconfirmed.

## Verify
`git log miracast-spike`; read docs/agents/miracast.md. No code yet.

## Next step
Step 2: check mac80211_hwsim / virtual Wi-Fi without root on this machine, document in miracast.md.

## Commit
see `git log -1` on miracast-spike
