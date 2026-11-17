# HANDOFF

## Goal
Item 0b.5, fifth part: split the big files (presenter.rs).

## Done
- 7897ce7: `presenter.rs` -> `presenter/mod.rs` + `presenter/status.rs` (PresentPhase, EncodeHud, PresentStatus; pure move). No unsafe added. Breaks: none. roadmap.md updated. GUI window not run (no display).
- Earlier: 6c14e7b encode/rc.rs, 4bd0440 encode/vaapi.rs, ef1e7cb config split, 3e0c0d5 needs_new_encoder, eabf95a perf budgets, f454021 unsafe_code lint, 6b5584e xtask ci.

## Decisions
- Pure code move, no decisions.md change.

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- Is 60 ms right for CI runners? Loopback p95 here is ~18 ms; tighten after the first CI run.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
7897ce7 (plus this file)

## Next step
presenter/mod.rs still ~1250 lines: move PresentArgs (needs pub(super) fields) and CaptureCtl to their own files; gui.rs (1321) split; or 0c: FFmpeg unsafe behind one safe module + Decoder trait.
