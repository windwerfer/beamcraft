# HANDOFF

## Goal
Item 0c: review remaining unsafe, FFmpeg decode hostile-input test.

## Done
- 0c review: `VideoDecoder` trait + `YuvImage` already exist; portal.rs borrow_raw and glow calls have no safe alternative (documented). Added `decode::tests::corrupt_h264_never_panics_and_decoder_recovers` (bit flips, truncation, junk, recovery on keyframe; x264). No unsafe, Breaks: none. xtask ci + Android clippy ok.
- gui/mod.rs split by screen: gui/present.rs (present_ui + quality_controls), gui/video.rs (video_ui + draw_cursor); mod.rs ~570 lines. Pure move, no unsafe, Breaks: none. xtask ci + Android clippy ok.
- gui.rs -> gui/mod.rs + gui/widgets.rs (stateless widgets/panels, pure move). No unsafe. Breaks: none. Loopback flaked once (lost 1), passed on rerun (p95 16 ms); Android clippy ok.
- PresentArgs/SourceKind moved to `presenter/args.rs` (pure move; presenter/mod.rs now ~1020 lines). No unsafe. Breaks: none.
- CaptureCtl/CaptureStatus moved to `presenter/capture.rs` (pure move, fields pub(super)). No unsafe. Breaks: none. roadmap.md updated.
- Earlier: 7897ce7 presenter/status.rs, 6c14e7b encode/rc.rs, 4bd0440 encode/vaapi.rs, ef1e7cb config split, 3e0c0d5, eabf95a, f454021, 6b5584e.
- Gates run locally: cargo xtask ci (loopback p95 18 ms), clippy --features gui, Android clippy via cargo ndk. GUI window not run (no display).

## Decisions
- Pure code move, no decisions.md change.

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- Is 60 ms right for CI runners? Loopback p95 here is ~18 ms; tighten after the first CI run.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
see `git log -1` of the gui/present.rs commit

## Next step
0c remaining needs the owner (ASan nightly job, cargo-fuzz). Next: item 5 (bottleneck line), then 6 / 4.
