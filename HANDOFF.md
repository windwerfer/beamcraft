# HANDOFF

## Goal
Item 0b.5, second part: split encode.rs into modules.

## Done
- ef1e7cb: `encode/config.rs` holds EncoderBackend, RateControl, EncoderConfig, parse_size (pure move, re-exported; encode.rs is now `encode/mod.rs`, 1456 lines). No unsafe added, none moved. Breaks: none. roadmap.md updated.
- Earlier: 3e0c0d5 needs_new_encoder, eabf95a perf budgets, f454021 unsafe_code lint, 6b5584e xtask ci.

## Decisions
- Config split only; the FFmpeg parts stay together because they share private types and the `unsafe impl Send` blocks. Not a decisions.md change.

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- Is 60 ms right for CI runners? Loopback p95 here is ~15 ms; tighten after the first CI run.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
ef1e7cb (plus this file)

## Next step
Split encode/mod.rs further (VaapiVpp, DMA-BUF import into `encode/vaapi.rs`; rate-control helpers into `encode/rc.rs`), then presenter.rs (1354 lines); or 0c FFmpeg unsafe behind one safe module + Decoder trait.
