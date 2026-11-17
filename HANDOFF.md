# HANDOFF

## Goal
Item 0b.4: machine-checked perf budgets (loopback half).

## Done
- eabf95a: `perf-budgets.toml` + `cargo xtask loopback [secs]` (p95 capture→decoded ≤ 60 ms, lost / mismatches / decode errors / unrecoverable frames = 0); used by `xtask ci` and the CI x264 loopback step. Unit-tested parser/checker. No unsafe. Breaks: none.
- 62d1542: roadmap.md and testing.md.
- Earlier: f454021 unsafe_code lint per crate, 6b5584e xtask ci gate.

## Decisions
- Budgets are loose (shared CI VMs); the ABR simulation rows are already asserted by the `sim_*` tests in rate.rs, so no TOML for them. Not a decisions.md change.

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- Is 60 ms right for CI runners? Check the first CI run's p95 and tighten.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
62d1542 (plus this file)

## Next step
0b.5 merge `maybe_reopen`/`maybe_replace_encoder` in media pipeline.rs and split encode.rs/presenter.rs; or 0c FFmpeg unsafe into one safe module + `Decoder` trait; or workspace clippy unwrap lints.
