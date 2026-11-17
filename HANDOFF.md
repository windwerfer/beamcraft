# HANDOFF

## Goal
Roadmap item 0b: unwrap/expect/panic lints in media, film, android.

## Done
- 0b: `deny(clippy::unwrap_used, expect_used, panic)` in media, film, android (7 expects removed in media pipeline/decode/encode; android ViewerApp::new returns Result for the tokio runtime). Only app crate stays open (~79 sites). Breaks: none. Gates: xtask ci, Android clippy ok.
- Earlier: item 5 viewer half (bottleneck.rs HUD line).
- `crates/mirrorwire-app/src/bottleneck.rs`: classifier (network/viewer/driver, good/OK/weak), 3-window hysteresis, 7 unit tests; shown as a HUD line in gui/video.rs via `ViewerStatus.bottleneck`. Thresholds in architecture.md, roadmap item 5 updated. No unsafe, Breaks: none.
- Gates: cargo xtask ci, clippy --features gui, Android clippy ok. GUI window not run (no display).
- Earlier: 0c unsafe review + corrupt-H.264 decode test, gui/presenter splits.

## Decisions
- Viewer-only first: the presenter-side line needs a viewer->presenter stats message (protocol bump), left for a later session.
- A hardware encoder with low fps gives no verdict (idle screen is normal).

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- Is 60 ms right for CI runners? Loopback p95 here is ~18 ms; tighten after the first CI run.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
see `git log -1`

## Next step
App crate unwrap/expect/panic lints (79 sites), then Item 5 remainder: viewer stats message + presenter status line (protocol bump, protocols.md). Then lints/xtask gate, 6 (extend mode), 4.
