# HANDOFF

## Goal
Session 2026-11-18 (hardware findings): fixed VAAPI LP open and the x264 post-outage freeze (see Done 'Hardware findings').

Latest session: Extend size dropdown (Auto = viewer display + common sizes) driving the presenter hint; session-only, hint-only. Next: desktop camera QR scan.

Owner order 2026-11-18 11:25: (1) live VAAPI bitrate (blocked, see Open questions), (2) presenter remote.

## Done
- Presenter remote, protocol 9: `Control::RemoteKey{key}` (closed enum next/previous/space/escape) on the control stream, rate-limited 10/s in the presenter (`TapGate`), `ViewerSession::send_remote_key`; unit tests (wire form, hostile values, rate limit, loopback).
- Linux presenter: `media/src/source/remote.rs` taps keysyms through the RemoteDesktop portal (keyboard only, lazy session, restore token `remote-desktop.token`); status line in the presenter window (`CaptureCtl.remote_note`). No new unsafe, no new crate, no new Flatpak permission.
- Desktop viewer: R toggles Previous/Next/Click/Escape buttons. Android: same buttons in the tap overlay.
- Docs: decisions.md log, protocol.md (`remote_key`, version 9), roadmap, README.
- Breaks: PROTOCOL_VERSION 8 -> 9.

- Hardware findings: (1) VAAPI retries with `low_power=1`; (2) Reassembler livelock after an outage fixed (decisions.md 2026-11-18 'hardware findings'), unit regression test. Live bitrate on the LP path is untested (the reopen path is used as before).
- MANAGER RERUN: scripts/android-wifi-run.sh 45 with both `--encoder vaapi` and `--encoder x264` on the T480s + TB336FU; expect VAAPI to open (log 'retrying with low_power=1') and the viewer to resume after the 12.5->5 Mbit/s cut. The livelock cause is inferred from the logs, not reproduced on hardware.

- Extend size dropdown (14:13 decision): done as UI + hint everywhere (`ExtendSize`, unit test). NOT done: applying the size via KWin on non-Flatpak builds, and using the chosen size as encode target (both noted in decisions.md; KWin route needs D-Bus, left for a later session).

## Decisions
- See decisions.md log 2026-11-18 (presenter remote). Rejected: cursor datagram path, sharing the ScreenCast session, raw keycodes.

## Open questions
- Roadmap 0.6 live VAAPI bitrate: needs new `unsafe` (direct libva) in media; owner decision (earlier rejected cros-codecs).
- Presenter remote never run against a real portal, KDE/GNOME dialog, Flatpak, or a slideshow app; Android/desktop buttons only compiled, not seen. Owner: UI sketch for the buttons when available.
- RemoteDesktop portal availability on the owner's KDE (xdg-desktop-portal-kde) unverified; a denial shows in the presenter window.
- Extend sizing is hint-only (owner option 2): the window shows the viewer's size; not seen on a real KDE desktop.
- Loopback p95 close to the 40 ms budget on loaded boxes (this run 16.5 ms); watch CI.
- Owner: report the craft-libs h264 decoder.rs:532 overflow upstream, bump rev, drop continue-on-error in fuzz.yml.
- Flatpak portal-version check for VIRTUAL untested; Flatpak ffmpeg-full x264 and AppImage on FFmpeg 6.1 unverified on hardware.
- Windows: nothing ran on real Windows; presenter/installer/signing undecided.

## Verify
cargo xtask ci; cargo ndk -t arm64-v8a --platform 26 clippy --release --locked -p mirrorwire-android -- -D warnings; gh run list --branch main --limit 3

## Written at commit
see `git log -1`

## Next step
Roadmap 2 turned out already done (marked in roadmap.md). Remaining: desktop camera QR scan (no camera on the box: unit tests on sample images only). Input features beyond the presenter remote are not yet wanted.
