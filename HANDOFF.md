# HANDOFF

## Goal
Windows build, step 1 (roadmap 7, D16): CI check of the viewer on windows-latest (MSVC).

## Done
- New CI job `Check (Windows)`: clippy (-D warnings) and full tests of the workspace (no portal) against a BtbN shared FFmpeg 8.1 via FFMPEG_DIR. Green. CI, Build, CodeQL green.
- Fixed non-Linux dead-code lints (net firewall helpers, media DMA-BUF types, presenter import) and gated 3 Linux-only firewall tests.
- Roadmap item 7 and decision D16 added.
- Earlier: bottleneck line on Android HUD (not tested on device); item 4 (system FFmpeg) done.

## Decisions
- D16: Windows viewer first; FFmpeg DLLs may be bundled, LGPL preferred. The CI job uses the GPL build only because tests need libx264 (nothing distributed). Rejected: building FFmpeg in CI.
- D8 (2026-11-17). Rejected: keep bundling x264 (GPL binaries).

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
Windows step 2: build a release viewer artifact (exe + LGPL FFmpeg DLLs + licence notes) in build.yml; or viewer size in hello for Extend.
