# HANDOFF

## Goal
Owner order 20:13, item 2: camera QR scan step 2 (desktop viewer).

## Done (this session)
- `media/src/source/camera.rs`: `extract_luma` (pure, unit-tested) + `CameraScanner<T>` (Linux, feature `portal`): Camera portal -> PipeWire stream (role Camera, no node id) -> throttled newest luma -> worker thread running `pairing::scan_qr_luma`. ashpd feature `camera` added (no new crate). `app/src/gui/scan.rs` + "Scan QR with camera" button next to Connect (hidden without portal). Raw formats only; MJPEG-only cameras show "cannot read" in the window.
- Gates: cargo xtask ci + Android clippy green. No new unsafe/crate/Flatpak permission. Breaks: none. Decision logged in decisions.md; roadmap/README updated.

## Manager must check (camera hardware, Linux desktop viewer)
Click "Scan QR with camera" with a camera attached: portal dialog appears, status shows format/size, holding the presenter's QR connects. Also: does AUTOCONNECT with no node id pick the camera over the portal remote? Does a YUY2/NV12 camera work; what happens with an MJPEG-only one (status text)?

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
Nothing hardware-free is left; the loop waits for hardware checks (combined session, pen, camera scan). Reply NOTHING_LEFT.

(Superseded:)
Loop waits for owner hardware checks (pen/touch, Extend dropdown, encode-size cap, KWin sizing). No hardware-free item is left except those parked/undecided.

(Earlier:)
Pen: hardware check by the manager; then (if wanted) unify capture + input into one RemoteDesktop session for Extend. Otherwise as below.

(Previous next step:)
Camera QR step 2 (needs a camera box). KWin sizing is parked (see decisions.md); revisit only with a KDE desktop to test on. Otherwise remaining mirroring items (0.4 hysteresis, native resolution, connection diagnostics) if not yet done per roadmap.

(Older next step:)
Camera QR scan step 2 (needs a camera box): portal Camera + PipeWire video-input source in media, 'Scan' button in the desktop viewer feeding `scan_qr_luma` with the luma plane (YUY2/NV12/MJPEG decode). Not testable here. Hardware check deferred. (Old note follows.)
Roadmap 2 turned out already done (marked in roadmap.md). Remaining: desktop camera QR scan (no camera on the box: unit tests on sample images only). Input features beyond the presenter remote are not yet wanted.
