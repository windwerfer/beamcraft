# HANDOFF

## Goal
Optional FFmpeg (D17, roadmap 7a), step 1: pure-Rust RGB -> I420 converter.

## Done
- `mirrorwire-media/src/convert.rs`: safe scalar BT.601 limited-range converter, bounds-checked, tested against swscale (luma +-3, chroma +-20). Not wired in anywhere yet.
- D17 and roadmap 7a recorded. Gates (`cargo xtask ci`, Android clippy) green locally.

## Decisions
- D17: scalar converter, no SIMD until measured. Step 2 design in decisions.md.
- Earlier: D16 Windows viewer first; D8 no bundled FFmpeg/x264 on Linux.

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- 60 ms loopback budget for CI runners: tighten after first CI run.
- Extend: viewer size in hello (protocol 8)? Flatpak portal-version check for VIRTUAL untested.
- Not verified at run time: Flatpak ffmpeg-full extension actually providing libx264 to the app; AppImage on Ubuntu FFmpeg 6.1 (built OK, never run on hardware). build-ffmpeg.sh still has a Linux branch (x264), now unused.
- Windows: nothing ran on a real Windows machine; only compile, clippy, tests in CI. Windows presenter, packaging (zip + LGPL FFmpeg DLLs + licence notes) and the firewall hint (no-op there) not done. BtbN `latest` asset names rotate; the URL in ci.yml may need updating.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
see `git log -1`

## Next step
Optional FFmpeg step 2: cargo feature `ffmpeg` (default on) in media/app; split `Encoder` so FilmCraft takes planes (use `convert.rs`) instead of `ffmpeg_next::frame::Video`; cfg out decode/encode/hw and lib.rs init; CI builds `--no-default-features --features filmcraft`. Then Windows step 2.
