# HANDOFF

## Goal
Owner decision 2026-11-18: protocol 8, viewer display size in hello (D19).

## Done
- `PROTOCOL_VERSION` 8; `hello.display {width,height,scale_milli}` (proto `DisplayInfo`, sanitised, tested). Desktop and Android viewers send egui content rect x ppp. Presenter exposes `Presenter::viewer_display` (watch). Test: presenter learns display from hello. protocol.md, D19, roadmap updated.
- Earlier this chain: fuzz/ASan job (D18), loopback budget 40 ms.

## Decisions
- D19. No new unsafe. Breaks: PROTOCOL_VERSION 7 -> 8.

## Open questions
- Extend does not use `viewer_display` yet: the ScreenCast portal has no size parameter; matching virtual monitor needs KDE/GNOME-specific work (owner decision on approach).
- Loopback p95 measured 38.9 ms in the last local run against the 40 ms budget (loaded box?); watch for flakiness in CI and raise if needed.
- Owner: report the craft-libs h264 decoder.rs:532 overflow upstream, bump rev, drop continue-on-error in fuzz.yml.
- Flatpak portal-version check for VIRTUAL untested; Flatpak ffmpeg-full x264 and AppImage on FFmpeg 6.1 unverified on hardware.
- Windows: nothing ran on real Windows; presenter/installer/signing undecided.
- FFmpeg-free build never run with GUI/hardware.

## Verify
cargo xtask ci; cargo ndk -t arm64-v8a --platform 26 clippy --release --locked -p mirrorwire-android -- -D warnings; gh run list --branch main --limit 3

## Written at commit
see `git log -1`

## Next step
Remaining listed items are done or need owner decisions; Extend virtual monitor sizing (needs owner approach) is the main open one.
