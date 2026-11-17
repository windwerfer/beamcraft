# HANDOFF

## Goal
Roadmap item 5: bottleneck hint line on the Android HUD.

## Done
- Android HUD shows `Bottleneck: stage - rating: hint` (session.rs status field, app.rs text). Not tested on a device.
- Earlier: item 4 runtime + packaging (see git log).
- Runtime part (earlier): `no_encoder_message`, Auto falls back VA-API -> x264.
- Packaging switch: Flatpak manifest has no x264/FFmpeg modules, adds the ffmpeg-full extension; AppImage CI job builds against Ubuntu's FFmpeg dev packages, links dynamically, `build.sh` excludes libav*/libsw*. macOS unchanged. docs updated.
- Gates: cargo xtask ci, Android clippy; CI, Build (Flatpak, AppImage built green), CodeQL all green.

## Decisions
- D8 (2026-11-17). Rejected: keep bundling x264 (GPL binaries).

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- 60 ms loopback budget for CI runners: tighten after first CI run.
- Extend: viewer size in hello (protocol 8)? Flatpak portal-version check for VIRTUAL untested.
- Not verified at run time: Flatpak ffmpeg-full extension actually providing libx264 to the app; AppImage on Ubuntu FFmpeg 6.1 (built OK, never run on hardware). build-ffmpeg.sh still has a Linux branch (x264), now unused.
- Windows build item (answers.md) not started; not yet in roadmap/decisions.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
see `git log -1`

## Next step
Windows CI viewer build (windows-latest, MSVC; add roadmap item + decision first), or viewer size in hello for Extend.
