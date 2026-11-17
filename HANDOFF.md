# HANDOFF

## Goal
Item 0b.5, third part: split encode/mod.rs into modules.

## Done
- 4bd0440: `encode/vaapi.rs` holds VaapiVpp, VppInput, alloc_vaapi_buffersrc, alloc_hw_filter, DrmHolder, map_dmabuf (pure move; mod.rs 1456 -> 1151 lines). No unsafe added; the existing unsafe blocks moved with the code (all in mirrorwire-media). Breaks: none. roadmap.md updated. VAAPI paths not run (no GPU here).
- Fixed red Build (Android clippy): `#[no_mangle]` on android_main tripped the `deny(unsafe_code)` from f454021; now `#[allow(unsafe_code)]` on that one fn (an unsafe attribute, not an unsafe block). Also 4bd0440 had forgotten vaapi.rs; added in the next commit.
- Earlier: ef1e7cb config split, 3e0c0d5 needs_new_encoder, eabf95a perf budgets, f454021 unsafe_code lint, 6b5584e xtask ci.

## Decisions
- Pure code move, no decisions.md change. DmaImport/Upload/Encoder stay in mod.rs (they share private fields and the unsafe impl Send blocks).

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- Is 60 ms right for CI runners? Loopback p95 here is ~14 ms; tighten after the first CI run.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
4bd0440 (plus this file)

## Next step
Rate-control helpers (effective_rc, apply_rate_control, apply_vaapi_opts, describe_rc) into `encode/rc.rs`; then presenter.rs (1354 lines); or 0c: FFmpeg unsafe behind one safe module + Decoder trait.
