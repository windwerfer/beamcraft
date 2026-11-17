# HANDOFF

## Goal
Item 0b.5, first part: merge the duplicated encoder-config comparison in media pipeline.rs.

## Done
- 3e0c0d5: `needs_new_encoder()` replaces the two copies of the eight-field comparison in `maybe_reopen` / `maybe_replace_encoder`. No behaviour change, no unsafe. Breaks: none. roadmap.md updated.
- Earlier: eabf95a perf budgets, f454021 unsafe_code lint per crate, 6b5584e xtask ci gate.

## Decisions
- Kept the two functions separate: their bitrate rules differ (initial open vs live/reopen rate limits). Not a decisions.md change.

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- Is 60 ms right for CI runners? Check the first CI run's p95 and tighten.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
3e0c0d5 (plus this file)

## Next step
0b.5 rest: split encode.rs (1632 lines) and presenter.rs (1354 lines) into modules; or 0c FFmpeg unsafe behind one safe module + `Decoder` trait; or workspace clippy unwrap lints.
