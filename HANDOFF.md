# HANDOFF

## Goal
Owner decision 2026-11-18: nightly ASan + cargo-fuzz CI job (D18).

## Done
- `fuzz/` crate (outside workspace; targets film_decode, proto_parse, reassembler) and `.github/workflows/fuzz.yml` (nightly cron + manual; ASan media/film tests, 60 s per target). Nightly only there. D18 + README/testing/roadmap updated. CI, Build, CodeQL green.
- First fuzz.yml run: ASan job green (~22 min). **Fuzz found a real upstream bug**: craft-libs h264 `decoder.rs:532` "attempt to subtract with overflow" on corrupt input (release wraps; FilmDecoder catches panics, so no app crash). The film_decode step is `continue-on-error` until FilmCraft fixes it. A re-run of fuzz.yml (workflow_dispatch) was started after that change; check `gh run list --workflow fuzz.yml`.

## Decisions
- D18 (libfuzzer-sys NCSA licence accepted because fuzz/ is unshipped). No new unsafe. Breaks: none.

## Open questions
- Owner: report the overflow to storytold/craft-libs (needs their repo), then bump the rev in mirrorwire-film + media and drop continue-on-error.
- Extend: Flatpak portal-version check for VIRTUAL untested.
- Not verified at run time: Flatpak ffmpeg-full extension providing libx264; AppImage on Ubuntu FFmpeg 6.1 (never run on hardware). build-ffmpeg.sh has an unused Linux branch (x264).
- Windows: nothing ran on a real Windows machine; only compile, clippy, tests in CI (zip: --version smoke). Presenter, installer, signing not decided. BtbN `latest` asset names rotate.
- FFmpeg-free build only compiled, clippy'd and unit-tested; never run with a GUI/hardware.
- cargo `unused dependency crossbeam-channel/tokio` warnings for mirrorwire-media in the FFmpeg-free build (harmless).

## Verify
cargo xtask ci; cargo ndk -t arm64-v8a --platform 26 clippy --release --locked -p mirrorwire-android -- -D warnings; cd fuzz && cargo +nightly fuzz run proto_parse -- -max_total_time=20; gh run list --branch main --limit 3

## Written at commit
see `git log -1`

## Next step
Protocol 8: viewer display size (and scale) in hello for Extend (owner YES, see answers.md); update Android viewer too, bump PROTOCOL_VERSION, protocol.md.
