# HANDOFF

## Goal
Owner order 2026-11-18 11:25: (1) live VAAPI bitrate (blocked, see Open questions), (2) presenter remote.

## Done
- Presenter remote, protocol 9: `Control::RemoteKey{key}` (closed enum next/previous/space/escape) on the control stream, rate-limited 10/s in the presenter (`TapGate`), `ViewerSession::send_remote_key`; unit tests (wire form, hostile values, rate limit, loopback).
- Linux presenter: `media/src/source/remote.rs` taps keysyms through the RemoteDesktop portal (keyboard only, lazy session, restore token `remote-desktop.token`); status line in the presenter window (`CaptureCtl.remote_note`). No new unsafe, no new crate, no new Flatpak permission.
- Desktop viewer: R toggles Previous/Next/Click/Escape buttons. Android: same buttons in the tap overlay.
- Docs: decisions.md log, protocol.md (`remote_key`, version 9), roadmap, README.
- Breaks: PROTOCOL_VERSION 8 -> 9.

## Decisions
- See decisions.md log 2026-11-18 (presenter remote). Rejected: cursor datagram path, sharing the ScreenCast session, raw keycodes.

## Open questions
- Roadmap 0.6 live VAAPI bitrate: needs new `unsafe` (direct libva) in media; owner decision (earlier rejected cros-codecs).
- Presenter remote never run against a real portal, KDE/GNOME dialog, Flatpak, or a slideshow app; Android/desktop buttons only compiled, not seen. Owner: UI sketch for the buttons when available.
- RemoteDesktop portal availability on the owner's KDE (xdg-desktop-portal-kde) unverified; a denial shows in the presenter window.
- Extend does not use `viewer_display` yet (KDE-specific path; may need NEED_MANAGER).
- Loopback p95 close to the 40 ms budget on loaded boxes (this run 16.5 ms); watch CI.
- Owner: report the craft-libs h264 decoder.rs:532 overflow upstream, bump rev, drop continue-on-error in fuzz.yml.
- Flatpak portal-version check for VIRTUAL untested; Flatpak ffmpeg-full x264 and AppImage on FFmpeg 6.1 unverified on hardware.
- Windows: nothing ran on real Windows; presenter/installer/signing undecided.

## Verify
cargo xtask ci; cargo ndk -t arm64-v8a --platform 26 clippy --release --locked -p mirrorwire-android -- -D warnings; gh run list --branch main --limit 3

## Written at commit
see `git log -1`

## Next step
Roadmap 2 turned out already done (marked in roadmap.md). Remaining: Extend sizing (KDE path; likely NEED_MANAGER: KWin's `zkde_screencast stream_virtual_output` is reserved to the portal backend and `org.kde.KWin` D-Bus would need a new Flatpak `--talk-name`, forbidden by rule 1.2), desktop camera QR. Input features beyond the presenter remote are not yet wanted.
