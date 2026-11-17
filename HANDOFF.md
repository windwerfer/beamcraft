# HANDOFF

## Goal
Roadmap item 5 remainder: the presenter shows the viewer's bottleneck verdict.

## Done
- New control message `bottleneck {stage, rating}` (viewer -> presenter, sent on change), PROTOCOL_VERSION 7. Stage/rating enums in mirrorwire-proto (app re-exports them). Presenter: `LinkState.viewer_verdict`, `viewer sees: ...` appended to `bitrate_line()`, reset per viewer. Tests: proto wire pin + malformed input, link.rs unit test. protocol.md, architecture.md, roadmap updated. Breaks: protocol 6 peers rejected with the update message; Android viewer does not send it yet.
- Gates: xtask ci (loopback p95 17 ms), app --all-features clippy, Android clippy ok. GUI window not run (no display).
- Earlier: lints (workspace), 0b, 0c, item 5 viewer HUD half.

## Decisions
- Viewer sends the verdict (stage+rating), not raw stats: same verdict on both sides, no second classifier; hint text stays on the viewer. Rejected: sending raw stats (duplicates the classifier and hysteresis).
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
Android viewer sends `bottleneck` too (needs a classifier there). Then 6 (extend mode, testable parts), 4 (system FFmpeg).
