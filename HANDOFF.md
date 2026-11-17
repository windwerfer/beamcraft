# HANDOFF

## Goal
Item 0b.5, fifth part: split the big files (presenter.rs).

## Done
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
(see git log; this commit follows the capture.rs move)

## Next step
Move PresentArgs (needs pub(super) fields) out of presenter/mod.rs (~1170 lines); gui.rs (1321) split; or 0c: FFmpeg unsafe behind one safe module + Decoder trait.
