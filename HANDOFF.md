# HANDOFF

## Goal
Roadmap item 0b finished: indexing_slicing everywhere plus `[workspace.lints]`.

## Done
- `indexing_slicing` denied in media (19 sites), film (1), android (11) via get()/chunks; short buffers error or skip. Lints now in `[workspace.lints.clippy]`, each product crate `[lints] workspace = true` (xtask opts out); integration tests (`tests/*.rs`) carry an explicit `#![allow]`. decisions.md entry added. Breaks: none. Gates: xtask ci (loopback p95 17 ms), app clippy --all-features, Android clippy; CI, CodeQL, Build green.
- `clippy::indexing_slicing` denied in the app crate (6 sites: QR image, advertised addrs). Breaks: none. Gates: xtask ci, Android clippy ok.
- 0b finished for app: `deny(unwrap_used, expect_used, panic)` in app main.rs; 46 mutex `.expect`s replaced by `sync::LockExt::lock_ok` (poison-tolerant, unit test); presenter thread-spawn failure now shows as Failed + message in the GUI; viewer rgba converter uses get_or_insert_with. Breaks: none. Gates: xtask ci (incl. loopback p95 29 ms), Android clippy ok.
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
Item 5 remainder: viewer stats message + presenter status line (protocol bump, protocol.md). Then 6 (extend mode, testable parts), 4 (system FFmpeg).
