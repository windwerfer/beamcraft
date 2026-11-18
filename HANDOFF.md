# HANDOFF

## Goal
"Finish mirroring first" list (owner 2026-11-18 11:24), item 1-3.

## Done
- Item 1 (live VAAPI bitrate, roadmap 0.6): not doable without new unsafe; see Open questions.
- Item 2 (0.4 hysteresis/probing): already done and sim-verified earlier (roadmap 0.4); nothing to do.
- Item 3 (roadmap 1): native size was already the default; added limits in `EncoderConfig::resolved_size` (even edges, aspect kept when one of width/height is set, cap 4096 edge / 4096x2304 area, 5K -> 4096x2304) with a unit test. Not run on a 4K/5K display. Decision logged in decisions.md. No new unsafe, no protocol break.

## Decisions
- See decisions.md log 2026-11-18 (native size and limits).

## Open questions
- Roadmap 0.6 live VAAPI bitrate: FFmpeg sends rate control only with an IDR; only a direct libva path (new `unsafe` in media, new dependency or hand-written FFI; owner earlier rejected cros-codecs) could do it. Owner decision needed.
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
Roadmap 2 (GUI connection diagnostics per step), then Extend sizing (item 5, KDE path; may need NEED_MANAGER), then desktop camera QR.
