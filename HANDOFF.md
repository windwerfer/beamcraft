# HANDOFF

## Goal
Owner order 20:13, item 1: pen/touch in Extend via ONE RemoteDesktop session for capture + input.

## Done (this session)
- `portal.rs::start_combined`: RemoteDesktop CreateSession + SelectDevices KEYBOARD|POINTER + ScreenCast SelectSources on the same session, one Start, OpenPipeWireRemote; injection on the portal thread (`remote.rs::inject` shared). Fallback to plain ScreenCast + the old separate Mirror-only injector, with the reason shown in the presenter window. Per-mode input tokens `portal_restore_token[_extend]_input`. `RemoteRoute` (presenter/mod.rs) keeps one stable input sender across source reopens. `FrameSource::remote_input/input_note`.
- Tests: token names, Extend pointer maps on stream size not encode cap, separate-session mapping. Gates: cargo xtask ci + Android clippy green. No new unsafe/crate/permission. Breaks: none on the wire.
- NOT done: item 2 (camera QR step 2, portal Camera + PipeWire source, Scan button) is next.

## Manager must check on T480s (KDE Plasma Wayland) + TB336FU
1. Mirror capture still starts; count portal dialogs (expect one, covering screen + keyboard/pointer; none on second start with token). 2. Pen in Mirror. 3. Pen in Extend (VIRTUAL monitor). 4. Remote keys (Next/Previous) in both modes. 5. If the combined dialog is denied/unsupported: window shows "could not carry input ..." and Mirror still captures; pen then Mirror only.

## Earlier work (summary)
Pen/touch protocol 10, presenter remote protocol 9, Extend size cap, hardware findings fixes; see decisions.md.

## Decisions
- See decisions.md log 2026-11-18 (presenter remote). Rejected: cursor datagram path, sharing the ScreenCast session, raw keycodes.

## Open questions
- Pen: never run against a real portal/pen/touch screen; the combined session is untested on KDE. Whether KDE accepts a MONITOR source selection on the RemoteDesktop session with the old keyboard-only restore token (expected: one new dialog) is unverified. Manager hardware check: Mirror mode, tap Pen, drag with finger/pen on the tablet.
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
Camera QR scan step 2 (owner order 20:13 item 2), then NOTHING_LEFT if nothing hardware-free remains.

(Superseded:)
Loop waits for owner hardware checks (pen/touch, Extend dropdown, encode-size cap, KWin sizing). No hardware-free item is left except those parked/undecided.

(Earlier:)
Pen: hardware check by the manager; then (if wanted) unify capture + input into one RemoteDesktop session for Extend. Otherwise as below.

(Previous next step:)
Camera QR step 2 (needs a camera box). KWin sizing is parked (see decisions.md); revisit only with a KDE desktop to test on. Otherwise remaining mirroring items (0.4 hysteresis, native resolution, connection diagnostics) if not yet done per roadmap.

(Older next step:)
Camera QR scan step 2 (needs a camera box): portal Camera + PipeWire video-input source in media, 'Scan' button in the desktop viewer feeding `scan_qr_luma` with the luma plane (YUY2/NV12/MJPEG decode). Not testable here. Hardware check deferred. (Old note follows.)
Roadmap 2 turned out already done (marked in roadmap.md). Remaining: desktop camera QR scan (no camera on the box: unit tests on sample images only). Input features beyond the presenter remote are not yet wanted.
