# HANDOFF

## Goal
Roadmap item 4 (system FFmpeg, owner decision in answers.md), runtime part.

## Done
- `no_encoder_message` (media/encode/mod.rs): when no backend opens, the error lists every backend's reason and, for a libx264-less FFmpeg (Fedora ffmpeg-free), the install command. Auto already falls back VA-API -> x264. Unit-tested.
- decisions.md D8 updated: no bundled FFmpeg/x264 on Linux; Flatpak uses the ffmpeg-full extension, AppImage links system FFmpeg.
- Gates: cargo xtask ci, Android clippy (cargo ndk). Not run on an ffmpeg-free host.

## Decisions
- D8 (2026-11-17). Rejected: keep bundling x264 (GPL binaries).

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- 60 ms loopback budget for CI runners: tighten after first CI run.
- Extend: viewer size in hello (protocol 8)? Flatpak portal-version check for VIRTUAL untested.
- Item 4 packaging not done: Flatpak manifest still builds x264+FFmpeg; build.yml Linux job still builds static FFmpeg. Needs: drop x264/ffmpeg modules, add the ffmpeg-full extension, link system FFmpeg dynamically (no static-ffmpeg on Linux). Which FFmpeg majors ffmpeg-next 9 supports (7.x/8?) was not verified against distro packages on this box.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
see `git log -1`

## Next step
Item 4 packaging switch (Flatpak manifest, build.yml Linux job, AppImage dynamic link), or Android HUD bottleneck line / viewer size in hello for Extend.
