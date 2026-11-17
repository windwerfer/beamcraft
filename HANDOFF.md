# HANDOFF

## Goal
Item 0b.5, fourth part: split encode/mod.rs into modules.

## Done
- 6c14e7b: `encode/rc.rs` holds effective_rc, apply_rate_control, apply_vaapi_opts, describe_rc (pure move; mod.rs 1151 -> 1023 lines). No unsafe added; the existing SAFETY-commented blocks (AVCodecContext integer fields, in mirrorwire-media) moved with it. Breaks: none. roadmap.md updated. VAAPI/VideoToolbox paths not run (no GPU).
- Earlier: 4bd0440 encode/vaapi.rs, ef1e7cb config split, 3e0c0d5 needs_new_encoder, eabf95a perf budgets, f454021 unsafe_code lint, 6b5584e xtask ci.

## Decisions
- Pure code move, no decisions.md change.

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- Is 60 ms right for CI runners? Loopback p95 here is ~15 ms; tighten after the first CI run.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
6c14e7b (plus this file)

## Next step
presenter.rs (1354 lines) split; or 0c: FFmpeg unsafe behind one safe module + Decoder trait.
