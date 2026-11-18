# HANDOFF

## Goal
Roadmap 0b/0c: lint enforcement for unsafe.

## Done
- `clippy::undocumented_unsafe_blocks = deny` in `[workspace.lints.clippy]` (was media only); fixed 3 comments in app yuv_glow.rs and android gpu.rs. Roadmap 0b updated.
- Gates green locally (xtask ci incl. loopback, Android clippy).

## Decisions
- No new unsafe. Breaks: none.

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- 60 ms loopback budget for CI runners: tighten after first CI run.
- Extend: viewer size in hello (protocol 8)? Flatpak portal-version check for VIRTUAL untested.
- Not verified at run time: Flatpak ffmpeg-full extension providing libx264; AppImage on Ubuntu FFmpeg 6.1 (never run on hardware). build-ffmpeg.sh has an unused Linux branch (x264).
- Windows zip: never run on Windows (only --version smoke in CI); no installer/signing.
- Windows: nothing ran on a real Windows machine; only compile, clippy, tests in CI. Windows presenter, packaging and firewall hint not done. BtbN `latest` asset names rotate.
- FFmpeg-free build only compiled, clippy'd and unit-tested (FilmCraft round trip); never run with a GUI/hardware.
- cargo `unused dependency crossbeam-channel/tokio` warnings for mirrorwire-media in the FFmpeg-free build (harmless).

## Verify
cargo xtask ci; cargo ndk -t arm64-v8a --platform 26 clippy --release --locked -p mirrorwire-android -- -D warnings; gh run list --branch main --limit 3

## Written at commit
see `git log -1`

## Next step
0b/0c lint part done; remaining there needs nightly (ASan/cargo-fuzz, owner question). Next: item 6 extend testable parts.
