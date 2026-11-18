# HANDOFF

## Goal
Optional FFmpeg (D17, roadmap 7a), step 2: cargo feature `ffmpeg`, FFmpeg-free build.

## Done
- `mirrorwire-media`/`mirrorwire-app` feature `ffmpeg` (default on; `static-ffmpeg` implies it). `ffmpeg-next` optional.
- `encode/mod.rs` = config + `path` + `DmaBufError` + `Encoder` enum (`Ffmpeg(FfmpegEncoder)` in `encode/ffmpeg.rs`, `Film(FilmEncoder)` in `filmcraft.rs`). `decode.rs`, `hw.rs`, `frame.rs::to_ffmpeg`, `init()` gated.
- Pure-Rust `convert::yuv_to_rgba` (viewer RGBA without swscale); `convert.rs` RGB->I420 optimised (FilmCraft loopback capture_to_encoded 13 ms vs 16 ms swscale; naive version was 20 ms).
- `cargo build/clippy/test --no-default-features --features filmcraft` works; added to `cargo xtask ci` and ci.yml. Scripts (loopback, android) now pass `--features ffmpeg`.
- Gates green locally (xtask ci incl. loopback, Android clippy); CI + Build green on main.

## Decisions
- D17 step 2 in decisions.md. FilmCraft encoder always uses convert.rs (no swscale).
- Breaks: `--encoder filmcraft` no longer scales (capture size must equal `--size`; readable error).

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- 60 ms loopback budget for CI runners: tighten after first CI run.
- Extend: viewer size in hello (protocol 8)? Flatpak portal-version check for VIRTUAL untested.
- Not verified at run time: Flatpak ffmpeg-full extension providing libx264; AppImage on Ubuntu FFmpeg 6.1 (never run on hardware). build-ffmpeg.sh has an unused Linux branch (x264).
- Windows: nothing ran on a real Windows machine; only compile, clippy, tests in CI. Windows presenter, packaging and firewall hint not done. BtbN `latest` asset names rotate.
- FFmpeg-free build only compiled, clippy'd and unit-tested (FilmCraft round trip); never run with a GUI/hardware.
- cargo `unused dependency crossbeam-channel/tokio` warnings for mirrorwire-media in the FFmpeg-free build (harmless).

## Verify
cargo xtask ci; cargo ndk -t arm64-v8a --platform 26 clippy --release --locked -p mirrorwire-android -- -D warnings; gh run list --branch main --limit 3

## Written at commit
see `git log -1`

## Next step
Windows step 2: package the viewer (zip + LGPL FFmpeg DLLs, or an FFmpeg-free `--no-default-features --features gui,filmcraft` viewer zip with no DLLs; check decode speed first). Then remaining roadmap items (0b/0c, etc.) per answers.md order.
